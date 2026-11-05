//! The plugin catalog as the administrator sees it (`/api/v1/admin/catalog`):
//! plugins from the internet with what can be done with each here — install,
//! update, nothing (the newest is installed) — and installing one.
//!
//! The catalog is one JSON file (`[plugins] catalog_url`, format in
//! `docs/dev/plugin-catalog.md` and [`rustcloud_plugin_api::catalog`]). It is
//! downloaded when the administrator opens the catalog and kept for a few
//! minutes. Installing downloads the release's archive, checks its size,
//! SHA-256 and manifest against the catalog, then installs it like an
//! archive uploaded by hand: a new plugin starts disabled.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use rustcloud_auth::AdminUser;
use rustcloud_core::{audit, Error, ErrorKind, Live, Result};
use rustcloud_plugin_api::catalog::{Catalog, CatalogPlugin, Release};
use rustcloud_plugin_api::{api_version, Version};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::plugins::{Plugin, PluginSettings, Plugins, Source};
use crate::AppState;

/// How long a downloaded catalog is used before it is downloaded again.
const KEEP: Duration = Duration::from_secs(10 * 60);
/// Largest catalog file.
const CATALOG_LIMIT: usize = 4 * 1024 * 1024;
/// Most time for the catalog file and for an archive.
const CATALOG_TIME: Duration = Duration::from_secs(30);
const ARCHIVE_TIME: Duration = Duration::from_secs(5 * 60);

/// The catalog of this server: where it is and the last copy.
pub struct PluginCatalog {
    settings: Arc<Live<PluginSettings>>,
    client: reqwest::Client,
    cached: Mutex<Option<Cached>>,
}

struct Cached {
    url: String,
    at: Instant,
    catalog: Arc<Catalog>,
}

/// What can be done with a plugin of the catalog on this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Not installed; `release` can be.
    Install,
    /// Installed; `release` is newer.
    Update,
    /// Installed, and nothing newer works here.
    Latest,
    /// No version works with this server's plugin API.
    NeedsNewerServer,
    /// A plugin with this id comes with the server: it can't be replaced.
    BuiltIn,
}

/// A plugin of the catalog as the administrator sees it.
#[derive(Debug, Serialize)]
pub struct CatalogItem {
    #[serde(flatten)]
    pub plugin: CatalogPlugin,
    /// The version to install or update to: the newest that works here.
    pub release: Option<Release>,
    /// The installed version, if any.
    pub installed: Option<Version>,
    pub action: Action,
}

#[derive(Debug, Serialize)]
pub struct CatalogView {
    /// The address of the catalog; empty when there is none.
    pub url: String,
    pub plugins: Vec<CatalogItem>,
    /// Entries of the catalog that were skipped, and why.
    pub skipped: Vec<String>,
}

impl PluginCatalog {
    pub fn new(settings: Arc<Live<PluginSettings>>) -> Result<Self> {
        // Redirects are followed (GitHub serves release assets from another
        // host), but never from https:// to http://.
        let redirects = reqwest::redirect::Policy::custom(|attempt| {
            let downgrade = attempt.url().scheme() != "https"
                && attempt.previous().first().is_some_and(|first| first.scheme() == "https");
            if downgrade {
                attempt.error("redirected from https:// to http://")
            } else if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        });
        let client = reqwest::Client::builder()
            .redirect(redirects)
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("RustCloud/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::internal)?;
        Ok(Self { settings, client, cached: Mutex::new(None) })
    }

    fn url(&self) -> String {
        self.settings.read(|s| s.catalog_url.trim().to_owned())
    }

    /// The catalog: the copy kept unless it is older than [`KEEP`], is of
    /// another address, or `fresh` is asked for.
    pub async fn catalog(&self, fresh: bool) -> Result<(String, Arc<Catalog>)> {
        let url = self.url();
        if url.is_empty() {
            return Err(Error::not_found("CATALOG_OFF", "The plugin catalog is turned off in the settings"));
        }
        if !fresh {
            let cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cached.as_ref().filter(|c| c.url == url && c.at.elapsed() < KEEP) {
                return Ok((url, cached.catalog.clone()));
            }
        }
        let parsed = Url::parse(&url).map_err(|err| unavailable(format!("{url}: {err}")))?;
        let text = self.download(parsed, CATALOG_LIMIT as u64, CATALOG_TIME).await?;
        let text = String::from_utf8(text.to_vec()).map_err(|_| unavailable("the catalog is not UTF-8 text"))?;
        let catalog = Arc::new(Catalog::from_json(&text).map_err(|err| unavailable(err.0))?);
        for skipped in &catalog.skipped {
            tracing::warn!("plugin catalog: skipped {skipped}");
        }
        let fresh = Cached { url: url.clone(), at: Instant::now(), catalog: catalog.clone() };
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = Some(fresh);
        Ok((url, catalog))
    }

    /// The catalog with what can be done with each plugin here.
    pub async fn view(&self, plugins: &Plugins, fresh: bool) -> Result<CatalogView> {
        let (url, catalog) = self.catalog(fresh).await?;
        let here: BTreeMap<String, Plugin> = plugins.list().into_iter().map(|p| (p.id.clone(), p)).collect();
        let api = api_version();
        let items = catalog
            .plugins
            .iter()
            .map(|entry| {
                let release = entry.best_release(&api).cloned();
                let installed = here.get(&entry.id);
                let version = installed.and_then(|p| p.manifest.as_ref()).map(|m| m.version.clone());
                let action = match (installed, &release) {
                    (Some(p), _) if p.source != Source::Installed => Action::BuiltIn,
                    (_, None) => Action::NeedsNewerServer,
                    (None, Some(_)) => Action::Install,
                    (Some(_), Some(release)) if version.as_ref().is_none_or(|v| *v < release.version) => Action::Update,
                    (Some(_), Some(_)) => Action::Latest,
                };
                CatalogItem { plugin: entry.clone(), release, installed: version, action }
            })
            .collect();
        Ok(CatalogView { url, plugins: items, skipped: catalog.skipped.clone() })
    }

    /// Installs (or updates to) `version` of the catalog's plugin `id`.
    pub async fn install(&self, plugins: &Plugins, id: &str, version: &Version) -> Result<Plugin> {
        let (url, catalog) = self.catalog(false).await?;
        let entry = catalog
            .plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::not_found("PLUGIN_NOT_FOUND", format!("The catalog has no plugin \"{id}\"")))?;
        let release = entry.versions.iter().find(|r| r.version == *version).ok_or_else(|| {
            Error::not_found("CATALOG_VERSION_NOT_FOUND", format!("The catalog has no version {version} of \"{id}\""))
        })?;
        if !release.api.matches(&api_version()) {
            return Err(Error::bad_request(
                "NEEDS_NEWER_SERVER",
                format!("{id} {version} needs plugin API {}, this server has {}", release.api, api_version()),
            ));
        }
        let address = archive_url(&url, &release.url)?;
        let archive = self.download(address.clone(), release.size, ARCHIVE_TIME).await?;
        let sha256: String = Sha256::digest(&archive).iter().map(|b| format!("{b:02x}")).collect();
        release.check_archive(archive.len() as u64, &sha256).map_err(|err| invalid_archive(err.0))?;
        let installed = plugins
            .install_checked(archive, |manifest| {
                release.check_manifest(id, manifest).map_err(|err| invalid_archive(err.0))
            })
            .await?;
        audit!(
            "plugin.catalog_install",
            app = id,
            version = version.to_string(),
            url = address.to_string(),
            "plugin installed from the catalog"
        );
        Ok(installed)
    }

    /// Downloads `url`, at most `limit` bytes within `time`.
    async fn download(&self, url: Url, limit: u64, time: Duration) -> Result<Bytes> {
        let address = url.to_string();
        let work = async {
            let response = self.client.get(url).send().await.map_err(|err| unavailable(format!("{address}: {err}")))?;
            if !response.status().is_success() {
                return Err(unavailable(format!("{address}: the server answered {}", response.status())));
            }
            if response.content_length().is_some_and(|size| size > limit) {
                return Err(unavailable(format!("{address}: larger than {limit} bytes")));
            }
            let mut body = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|err| unavailable(format!("{address}: {err}")))?;
                if (body.len() + chunk.len()) as u64 > limit {
                    return Err(unavailable(format!("{address}: larger than {limit} bytes")));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Bytes::from(body))
        };
        tokio::time::timeout(time, work).await.map_err(|_| unavailable(format!("{address}: no answer in time")))?
    }
}

/// The address of an archive: absolute, or relative to the catalog. Only
/// https://, unless the catalog itself is on http:// (a mirror in a LAN).
fn archive_url(catalog: &str, archive: &str) -> Result<Url> {
    let base = Url::parse(catalog).map_err(|err| unavailable(format!("{catalog}: {err}")))?;
    let url = base.join(archive).map_err(|err| invalid_archive(format!("{archive}: {err}")))?;
    let allowed = url.scheme() == "https" || (url.scheme() == "http" && base.scheme() == "http");
    if !allowed {
        return Err(invalid_archive(format!("{url}: only https:// archives can be installed")));
    }
    Ok(url)
}

fn unavailable(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::BadGateway, "CATALOG_UNAVAILABLE", message)
}

fn invalid_archive(message: impl Into<String>) -> Error {
    Error::bad_request("INVALID_PLUGIN", message)
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/admin/catalog", get(view)).route("/api/v1/admin/catalog/{id}/install", post(install))
}

#[derive(Deserialize)]
struct ViewParams {
    /// Download the catalog again even if a copy is kept.
    #[serde(default)]
    fresh: bool,
}

/// `GET /api/v1/admin/catalog`: the plugins of the catalog.
async fn view(
    State(catalog): State<Arc<PluginCatalog>>,
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Query(params): Query<ViewParams>,
) -> Result<Json<CatalogView>> {
    Ok(Json(catalog.view(&plugins, params.fresh).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    /// The version shown to the administrator, so a catalog changed in the
    /// meantime installs nothing else.
    version: Version,
}

/// `POST /api/v1/admin/catalog/{id}/install`: installs or updates a plugin
/// from the catalog.
async fn install(
    State(catalog): State<Arc<PluginCatalog>>,
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
    Json(request): Json<InstallRequest>,
) -> Result<(StatusCode, Json<Plugin>)> {
    let plugin = catalog.install(&plugins, &id, &request.version).await?;
    // An update may download other files.
    plugins.stop_downloads(&plugin.id);
    plugins.start_downloads(&plugin.id);
    Ok((StatusCode::CREATED, Json(plugins.view_one(plugin).await?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_are_https_or_next_to_the_catalog() {
        let github = "https://raw.githubusercontent.com/RustCloud/_plugins/main/index.json";
        let lan = "http://192.0.2.1/catalog/index.json";
        let url = |catalog, archive| archive_url(catalog, archive).map(|u| u.to_string());
        assert_eq!(url(github, "https://example.com/a.zip").unwrap(), "https://example.com/a.zip");
        assert_eq!(url(lan, "a/a-0.1.0.zip").unwrap(), "http://192.0.2.1/catalog/a/a-0.1.0.zip");
        assert_eq!(url(lan, "https://example.com/a.zip").unwrap(), "https://example.com/a.zip");
        assert!(url(github, "http://example.com/a.zip").is_err(), "no http:// from an https:// catalog");
        assert!(url(github, "file:///etc/passwd").is_err());
        assert!(url(lan, "ftp://example.com/a.zip").is_err());
    }
}
