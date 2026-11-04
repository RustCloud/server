//! The plugins of this server as the administrator manages them: which there
//! are, which run, their limits (`/api/v1/admin/plugins`, the Plugins tab).
//!
//! Every plugin found at startup is listed, also one that failed to load,
//! with the reason. A running plugin can be disabled: its pages, routes and
//! discovery addresses disappear until it is enabled again; users' data
//! stays. `files` is part of the server and always runs.
//!
//! The administrator installs plugins from a zip archive into
//! `<data_dir>/apps` and removes them from there. A new plugin starts
//! disabled: enabling it approves what it may ask users for. An update keeps
//! it running only if it asks for nothing more than before.
//!
//! A plugin may declare settings for the whole server in its manifest
//! (`[[server_settings]]`: an API key, a model name). The administrator fills
//! them in here; the plugin reads them through its `settings` import. Secrets
//! are never sent back to the browser.
//!
//! What a plugin may use is three multiples of a base ([`Scale`]: memory,
//! time, batches of jobs at a time), declared in its manifest's `[limits]`;
//! the administrator may set other ones. Everything else follows from them
//! ([`rustcloud_plugin_api::limits`]). Plugins with background jobs
//! ([`plugin_jobs`](crate::plugin_jobs)) run them in batches; the
//! administrator sees how many jobs are left and failed.
//!
//! Each plugin has its own storage on the server ([`PluginStorage`]). Files
//! its manifest lists in `[[downloads]]` (a model) are downloaded there in
//! the background once it runs; the administrator sees the progress, can
//! try again after a failure and can clear the storage.
//!
//! What the administrator sets is kept in the database (`plugin_configs`);
//! the defaults for all plugins are the `[plugins]` settings section.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::StreamExt;
use rustcloud_auth::AdminUser;
use rustcloud_core::{audit, Error, Field, Live, Result, Settings};
use rustcloud_db::{plugin_config, DatabaseConnection};
use rustcloud_plugin_api::limits::{self, SCALES};
use rustcloud_plugin_api::{Download, Manifest, Scale, SettingKind};
use rustcloud_wasm_host::{read_app_dir, AppConfig, AppStorage, Limits, StoredFile, WasmHost};
use sea_orm::sea_query::OnConflict;
use sea_orm::{EntityTrait, Set};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::apps::{app_folders, Apps};
use crate::plugin_jobs::{self, Counts};
use crate::plugin_storage::{PluginStorage, Usage};
use crate::AppState;

/// The id of the plugin that is part of the server.
const FILES: &str = "files";
/// Largest plugin archive that can be installed.
pub const ARCHIVE_LIMIT: usize = 64 * 1024 * 1024;
/// Largest `manifest.toml` in an archive.
const MANIFEST_LIMIT: u64 = 64 * 1024;
/// Longest text setting, and most lines of a list setting.
const TEXT_LIMIT: usize = 10_000;
const LIST_LIMIT: usize = 1_000;

/// `[plugins]`: what all plugins together may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginSettings {
    /// Batches of background jobs that run at a time on the whole server;
    /// 0: half the processor cores. Takes effect after a restart.
    pub workers: u32,
    /// The most memory one plugin gets, as a multiple of 64 MiB, whatever its
    /// manifest or the administrator asks for.
    pub max_memory: u32,
    /// Services of plugins (`[[proxy]]`): WebSocket connections at a time on
    /// the whole server, and for one user.
    pub proxy_connections: u32,
    pub proxy_connections_per_user: u32,
    /// Other requests to them at a time, for one user.
    pub proxy_requests_per_user: u32,
    /// The largest body a request to them may carry, MiB.
    pub proxy_body_mib: u32,
    /// Before every plugin had multiples of its own: read and forgotten.
    #[serde(default, skip_serializing)]
    memory_mib: Option<u32>,
    #[serde(default, skip_serializing)]
    time_seconds: Option<u32>,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            workers: 0,
            max_memory: 16,
            proxy_connections: 256,
            proxy_connections_per_user: 8,
            proxy_requests_per_user: 16,
            proxy_body_mib: 256,
            memory_mib: None,
            time_seconds: None,
        }
    }
}

impl PluginSettings {
    /// How many batches of jobs run at a time.
    pub fn workers(&self) -> usize {
        match self.workers {
            0 => std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 2).max(1)),
            workers => workers as usize,
        }
    }
}

impl Settings for PluginSettings {
    const SECTION: &'static str = "plugins";

    fn fields() -> Vec<Field> {
        vec![
            Field::number("workers", "batches"),
            Field::number("max_memory", "x64MiB"),
            Field::number("proxy_connections", "connections"),
            Field::number("proxy_connections_per_user", "connections"),
            Field::number("proxy_requests_per_user", "requests"),
            Field::number("proxy_body_mib", "MiB"),
        ]
    }

    fn validate(&self) -> Result<()> {
        if self.workers > 64 {
            return Err(Error::bad_request("INVALID_SETTINGS", "workers must be from 0 to 64"));
        }
        let proxy = [
            ("proxy_connections", self.proxy_connections, 100_000),
            ("proxy_connections_per_user", self.proxy_connections_per_user, 1_000),
            ("proxy_requests_per_user", self.proxy_requests_per_user, 1_000),
            ("proxy_body_mib", self.proxy_body_mib, 64 * 1024),
        ];
        for (name, value, most) in proxy {
            if !(1..=most).contains(&value) {
                return Err(Error::bad_request("INVALID_SETTINGS", format!("{name} must be from 1 to {most}")));
            }
        }
        if !SCALES.contains(&self.max_memory) {
            return Err(Error::bad_request("INVALID_SETTINGS", format!("max_memory must be one of {SCALES:?}")));
        }
        Ok(())
    }
}

/// A service of a running plugin and the paths of the server that go to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRoute {
    pub app_id: String,
    /// The service's origin: `http://collabora:9980`.
    pub target: String,
    pub paths: Vec<String>,
}

/// Where a plugin comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Part of the server (`files`).
    BuiltIn,
    /// Shipped with the server (notes, calendar): `builtin_apps_dir`.
    Shipped,
    /// Added by the administrator: `<data_dir>/apps`.
    Installed,
}

/// Whether a plugin runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Running,
    Disabled,
    /// It can't run: invalid, made for another API, or in conflict with another plugin.
    Failed {
        error: String,
    },
}

/// A plugin as the admin page shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    pub id: String,
    pub source: Source,
    /// None if the manifest couldn't be read.
    pub manifest: Option<Manifest>,
    pub status: Status,
    /// Whether the administrator wants it to run.
    pub enabled: bool,
    /// The multiples the administrator chose; `None`: as the manifest says.
    pub limits: Option<Scale>,
    /// What it gets: [`Self::limits`] or the manifest's, within `[plugins]`.
    pub effective: Effective,
    /// Values of its server settings, secrets left out.
    pub settings: Map<String, Value>,
    /// Keys of the secrets that are filled in.
    pub secrets_set: Vec<String>,
    /// All values, secrets too: only for the plugin itself.
    #[serde(skip)]
    values: Map<String, Value>,
    /// Its jobs left and failed, by kind.
    pub queue: BTreeMap<String, Counts>,
    /// What it keeps in its storage.
    pub storage: Usage,
    /// The files of its `[[downloads]]` being fetched, or why that failed;
    /// none when there is nothing to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadState>,
    /// Why it didn't load at startup; it can't run until the server restarts.
    #[serde(skip)]
    load_error: Option<String>,
}

/// What a plugin gets, for the admin page: its multiples and what follows
/// from them for one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Effective {
    pub scale: Scale,
    pub memory_mib: u64,
    /// Most data one call moves at once: a file read whole, a request body.
    pub data_mib: u64,
    pub time_seconds: u64,
    pub wall_seconds: u64,
    /// The most its background jobs take at once: `workers` batches of its
    /// largest kind; 0 without jobs.
    pub jobs_mib: u64,
}

impl Effective {
    fn of(scale: Scale, manifest: Option<&Manifest>) -> Self {
        let limits = scale.limits();
        let kinds = manifest.map(|m| m.jobs.as_slice()).unwrap_or_default();
        let largest = kinds.iter().map(|kind| limits::combine(scale.memory, kind.scale().0)).max();
        let batch_mib = largest.map_or(0, |memory| Limits::scaled(memory, 1).memory >> 20);
        Self {
            scale,
            memory_mib: limits.memory >> 20,
            data_mib: limits.data >> 20,
            time_seconds: limits.time.as_secs(),
            wall_seconds: limits.wall.as_secs(),
            jobs_mib: batch_mib * u64::from(scale.workers),
        }
    }
}

/// Where the downloads of a plugin are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadState {
    /// `path` is being downloaded; `left` files, this one included.
    Running { path: String, done: u64, total: Option<u64>, left: usize },
    /// Stopped at `path`; the administrator may try again.
    Failed { path: String, error: String },
}

/// The downloads of one plugin and the switch that stops them.
struct DownloadJob {
    state: DownloadState,
    cancel: Arc<AtomicBool>,
}

/// A file in a plugin's storage, as the admin page lists it.
#[derive(Debug, Clone, Serialize)]
pub struct StorageFile {
    pub path: String,
    pub size: u64,
    pub updated_at: String,
}

impl From<StoredFile> for StorageFile {
    fn from(file: StoredFile) -> Self {
        Self { path: file.path, size: file.size, updated_at: file.updated_at }
    }
}

impl Plugin {
    /// Sets the values of its server settings and what the browser may see of them.
    fn set_values(&mut self, values: Map<String, Value>) {
        let secret = |key: &str| {
            self.manifest.iter().flat_map(|m| &m.server_settings).any(|s| s.key == key && s.kind == SettingKind::Secret)
        };
        self.settings = values.iter().filter(|(key, _)| !secret(key)).map(|(k, v)| (k.clone(), v.clone())).collect();
        self.secrets_set = values.keys().filter(|key| secret(key)).cloned().collect();
        self.values = values;
    }
}

/// What the administrator sets for a plugin.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginUpdate {
    pub enabled: bool,
    /// None: as the manifest says.
    #[serde(default)]
    pub limits: Option<Scale>,
}

/// The plugins of the running server.
pub struct Plugins {
    db: DatabaseConnection,
    host: Arc<WasmHost>,
    storage: Arc<PluginStorage>,
    /// Running or failed downloads, by plugin id.
    downloads: Mutex<HashMap<String, DownloadJob>>,
    defaults: Arc<Live<PluginSettings>>,
    /// Where installed plugins go: `<data_dir>/apps`.
    install_dir: PathBuf,
    state: RwLock<Registry>,
    /// One change at a time: install, remove, configure.
    changing: tokio::sync::Mutex<()>,
}

struct Registry {
    plugins: Vec<Plugin>,
    /// The running ones, in the order of the side panel.
    running: Arc<Apps>,
}

impl Plugins {
    /// Reads the plugins in `dirs` (in that order) and compiles them into
    /// `host`; the built-in `files` comes first. A plugin that can't run is
    /// listed as failed, so a broken one never stops the server. Plugins
    /// installed later go to `install_dir`.
    pub async fn load(
        db: DatabaseConnection,
        host: Arc<WasmHost>,
        storage: Arc<PluginStorage>,
        defaults: Arc<Live<PluginSettings>>,
        dirs: &[(PathBuf, Source)],
        install_dir: PathBuf,
    ) -> Result<Self> {
        let configs = plugin_config::Entity::find().all(&db).await?;
        let config = |id: &str| configs.iter().find(|c| c.app_id == id);

        let files = rustcloud_files::manifest();
        let mut plugins = vec![new_plugin(FILES.to_owned(), Source::BuiltIn, Some(files))];
        for (dir, source) in dirs {
            for folder in app_folders(dir) {
                let folder_name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let mut plugin = match read_app_dir(&folder) {
                    Ok((manifest, wasm)) => {
                        let mut plugin = new_plugin(manifest.id.clone(), *source, Some(manifest.clone()));
                        plugin.load_error = if plugins.iter().any(|p| p.id == manifest.id) {
                            Some(format!("two plugins have the id \"{}\"", manifest.id))
                        } else if let Err(err) = Apps::new(vec![manifest.clone()]) {
                            Some(err.0)
                        } else {
                            host.load(&manifest, &wasm).err()
                        };
                        plugin
                    }
                    Err(err) => Plugin { load_error: Some(err), ..new_plugin(folder_name, *source, None) },
                };
                if let Some(row) = config(&plugin.id) {
                    plugin.enabled = row.enabled;
                    plugin.limits = row.limits.as_deref().and_then(|limits| serde_json::from_str(limits).ok());
                    plugin.set_values(serde_json::from_str(&row.settings).unwrap_or_default());
                }
                plugins.push(plugin);
            }
        }
        let registry = Registry::new(plugins);
        for plugin in &registry.plugins {
            match &plugin.status {
                Status::Failed { error } => tracing::warn!("plugin \"{}\" can't run: {error}", plugin.id),
                Status::Disabled => tracing::info!("plugin \"{}\" is disabled", plugin.id),
                Status::Running => tracing::info!("plugin \"{}\" runs", plugin.id),
            }
        }
        Ok(Self {
            db,
            host,
            storage,
            downloads: Mutex::default(),
            defaults,
            install_dir,
            state: RwLock::new(registry),
            changing: tokio::sync::Mutex::new(()),
        })
    }

    /// The running plugins (`GET /api/v1/apps`).
    pub fn running(&self) -> Arc<Apps> {
        self.read().running.clone()
    }

    /// The manifest of a running plugin.
    pub fn get(&self, id: &str) -> Option<Manifest> {
        self.read().running.get(id).cloned()
    }

    /// The manifests of the running plugins.
    pub fn running_manifests(&self) -> Vec<Manifest> {
        let state = self.read();
        state.plugins.iter().filter(|p| p.status == Status::Running).filter_map(|p| p.manifest.clone()).collect()
    }

    /// Every plugin, also disabled and failed ones.
    pub fn list(&self) -> Vec<Plugin> {
        self.read().plugins.clone()
    }

    /// What one request of a plugin gets: its limits, server settings,
    /// network addresses and native tools (approved when the administrator
    /// enabled it).
    pub fn config(&self, id: &str) -> AppConfig {
        let state = self.read();
        let plugin = state.plugins.iter().find(|p| p.id == id);
        let settings = plugin.map(|p| Value::Object(p.values.clone()).to_string()).unwrap_or_else(|| "{}".to_owned());
        let approved = plugin.filter(|p| p.status == Status::Running).and_then(|p| p.manifest.as_ref());
        let setting = |key: &str| plugin?.values.get(key)?.as_str().map(str::to_owned);
        let network = approved.map(|m| m.permissions.network_rules_with(setting)).unwrap_or_default();
        let uses = approved.map(|m| m.permissions.uses.clone()).unwrap_or_default();
        drop(state);
        AppConfig { limits: self.limits(id), settings, network, uses, self_url: None }
    }

    /// Origins of the services the panel may show for a running plugin's
    /// page (`permissions.frame`), with addresses from its settings.
    pub fn frame_origins(&self, id: &str) -> Vec<String> {
        let state = self.read();
        let Some(plugin) = state.plugins.iter().find(|p| p.id == id && p.status == Status::Running) else {
            return Vec::new();
        };
        let setting = |key: &str| plugin.values.get(key)?.as_str().map(str::to_owned);
        plugin.manifest.as_ref().map(|m| m.permissions.frame_origins(setting)).unwrap_or_default()
    }

    /// The services running plugins show through the server's address
    /// (`[[proxy]]`), with addresses from their settings; one whose setting is
    /// empty is left out.
    pub fn proxies(&self) -> Vec<ProxyRoute> {
        let state = self.read();
        let mut routes = Vec::new();
        for plugin in state.plugins.iter().filter(|p| p.status == Status::Running) {
            let setting = |key: &str| plugin.values.get(key)?.as_str().map(str::to_owned);
            for proxy in plugin.manifest.iter().flat_map(|m| &m.proxy) {
                if let Some(target) = proxy.target_origin(setting) {
                    routes.push(ProxyRoute { app_id: plugin.id.clone(), target, paths: proxy.paths.clone() });
                }
            }
        }
        routes
    }

    /// The multiples a plugin gets: as the administrator set them, else as
    /// its manifest says, with no more memory than `[plugins] max_memory`.
    pub fn scale(&self, id: &str) -> Scale {
        let max_memory = self.defaults.read(|d| d.max_memory);
        let state = self.read();
        let plugin = state.plugins.iter().find(|p| p.id == id);
        scale_of(plugin, max_memory)
    }

    /// What one request of a plugin may use.
    pub fn limits(&self, id: &str) -> Limits {
        self.scale(id).limits()
    }

    /// Batches of a plugin's jobs that run at a time.
    pub fn workers(&self, id: &str) -> usize {
        self.scale(id).workers as usize
    }

    /// The batch size of a kind of jobs and what one batch gets: the
    /// plugin's multiples times the kind's.
    pub fn job_config(&self, id: &str, kind: &str) -> (u32, AppConfig) {
        let mut config = self.config(id);
        let declared = self.get(id).and_then(|m| m.jobs.into_iter().find(|k| k.kind == kind));
        let batch = declared.as_ref().map_or(1, |k| k.batch);
        let (memory, time) = declared.as_ref().map_or((1, 1), |k| k.scale());
        let plugin = self.scale(id);
        // A kind of job may need more than a request, but never more memory
        // than the administrator allows plugins at all.
        let max_memory = self.defaults.read(|d| d.max_memory);
        let memory = limits::combine(plugin.memory, memory).min(max_memory);
        config.limits = Limits::scaled(memory, limits::combine(plugin.time, time));
        (batch, config)
    }

    /// Adds a plugin that is compiled into the server, like `files` (tests
    /// use it for plugins without code).
    pub fn add_built_in(&self, manifest: Manifest) {
        let mut state = self.write();
        let mut plugins = state.plugins.clone();
        plugins.push(new_plugin(manifest.id.clone(), Source::BuiltIn, Some(manifest)));
        *state = Registry::new(plugins);
    }

    /// Enables or disables a plugin and sets its limits. Enabling fails if
    /// the plugin can't run (e.g. another one has its discovery address).
    pub async fn configure(&self, id: &str, update: PluginUpdate) -> Result<Plugin> {
        if let Some(problem) = update.limits.as_ref().and_then(Scale::problem) {
            return Err(invalid_settings(problem));
        }
        let max_memory = self.defaults.read(|d| d.max_memory);
        if update.limits.is_some_and(|scale| scale.memory > max_memory) {
            return Err(invalid_settings(format!("memory must be at most {max_memory} ([plugins] max_memory)")));
        }
        let _changing = self.changing.lock().await;
        let (plugin, registry) = {
            let state = self.read();
            let mut plugins = state.plugins.clone();
            let plugin = plugins.iter_mut().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
            if plugin.source == Source::BuiltIn && !update.enabled {
                return Err(
                    Error::forbidden(format!("The plugin \"{id}\" is part of the server")).with_code("PLUGIN_REQUIRED")
                );
            }
            plugin.enabled = update.enabled;
            plugin.limits = update.limits;
            let registry = Registry::new(plugins);
            let plugin = registry.plugins.iter().find(|p| p.id == id).cloned().ok_or_else(plugin_not_found)?;
            if let (true, Status::Failed { error }) = (update.enabled, &plugin.status) {
                return Err(Error::conflict("PLUGIN_CANT_RUN", error.clone()));
            }
            (plugin, registry)
        };

        let row = plugin_config::ActiveModel {
            app_id: Set(id.to_owned()),
            enabled: Set(update.enabled),
            limits: Set(limits_json(update.limits)?),
            settings: Set("{}".to_owned()),
            updated_at: Set(Utc::now()),
        };
        plugin_config::Entity::insert(row)
            .on_conflict(
                OnConflict::column(plugin_config::Column::AppId)
                    .update_columns([
                        plugin_config::Column::Enabled,
                        plugin_config::Column::Limits,
                        plugin_config::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        *self.write() = registry;
        audit!(
            "plugin.configure",
            app = id,
            enabled = update.enabled,
            limits = update.limits.map(|l| format!("memory ×{}, time ×{}, workers {}", l.memory, l.time, l.workers)),
            "plugin settings changed"
        );
        Ok(plugin)
    }

    /// Saves the server settings of a plugin. A secret that is missing or
    /// empty keeps its value, `null` clears it; other settings missing or
    /// `null` are cleared.
    pub async fn save_settings(&self, id: &str, new: Map<String, Value>) -> Result<Plugin> {
        let _changing = self.changing.lock().await;
        let mut plugin = self.list().into_iter().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
        let declared = plugin.manifest.as_ref().map(|m| m.server_settings.clone()).unwrap_or_default();
        if let Some(unknown) = new.keys().find(|key| !declared.iter().any(|s| &s.key == *key)) {
            return Err(invalid_settings(format!("the plugin has no setting \"{unknown}\"")));
        }
        let mut values = Map::new();
        for setting in &declared {
            let key = setting.key.as_str();
            let value = match (setting.kind, new.get(key)) {
                (SettingKind::Secret, None) => plugin.values.get(key).cloned(),
                (SettingKind::Secret, Some(Value::String(text))) if text.is_empty() => plugin.values.get(key).cloned(),
                (_, None | Some(Value::Null)) => None,
                (kind, Some(value)) => Some(checked(kind, key, value)?),
            };
            if let Some(value) = value {
                values.insert(key.to_owned(), value);
            }
        }
        let changed: Vec<&str> =
            declared.iter().map(|s| s.key.as_str()).filter(|key| values.get(*key) != plugin.values.get(*key)).collect();

        let row = plugin_config::ActiveModel {
            app_id: Set(id.to_owned()),
            enabled: Set(plugin.enabled),
            limits: Set(limits_json(plugin.limits)?),
            settings: Set(Value::Object(values.clone()).to_string()),
            updated_at: Set(Utc::now()),
        };
        plugin_config::Entity::insert(row)
            .on_conflict(
                OnConflict::column(plugin_config::Column::AppId)
                    .update_columns([plugin_config::Column::Settings, plugin_config::Column::UpdatedAt])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        audit!("plugin.settings", app = id, changed = changed.join(", "), "plugin settings saved");
        plugin.set_values(values);
        let mut state = self.write();
        if let Some(slot) = state.plugins.iter_mut().find(|p| p.id == id) {
            *slot = plugin.clone();
        }
        Ok(plugin)
    }

    /// Installs a plugin from a zip archive with `manifest.toml` and
    /// `app.wasm` (at the top or in one folder), or updates an installed one
    /// with the same id. A new plugin is disabled until the administrator
    /// enables it.
    pub async fn install(&self, archive: Bytes) -> Result<Plugin> {
        let (text, wasm) = tokio::task::spawn_blocking(move || unpack(&archive)).await.map_err(Error::internal)??;
        let manifest = Manifest::from_toml(&text).map_err(|err| invalid_plugin(err.0))?;
        Apps::new(vec![manifest.clone()]).map_err(|err| invalid_plugin(err.0))?;
        let id = manifest.id.clone();

        let _changing = self.changing.lock().await;
        let previous = self.list().into_iter().find(|p| p.id == id);
        if previous.as_ref().is_some_and(|p| p.source != Source::Installed) {
            return Err(Error::conflict(
                "PLUGIN_EXISTS",
                format!("The plugin \"{id}\" comes with the server and can't be replaced"),
            ));
        }
        // An update that asks users for more needs the administrator's approval again.
        let enabled = previous
            .as_ref()
            .is_some_and(|p| p.enabled && p.manifest.as_ref().is_some_and(|old| asks_for_the_same(old, &manifest)));

        // Compiled from a hidden folder, then put in place: a failed install
        // leaves the old version.
        let staging = self.install_dir.join(format!(".{id}.new"));
        remove_folder(&staging).await?;
        tokio::fs::create_dir_all(&staging).await?;
        tokio::fs::write(staging.join("manifest.toml"), text).await?;
        tokio::fs::write(staging.join("app.wasm"), &wasm).await?;
        let host = self.host.clone();
        let compiled = manifest.clone();
        let loaded = tokio::task::spawn_blocking(move || host.load(&compiled, &wasm)).await.map_err(Error::internal)?;
        if let Err(err) = loaded {
            remove_folder(&staging).await?;
            return Err(invalid_plugin(err));
        }
        let folder = self.install_dir.join(&id);
        remove_folder(&folder).await?;
        tokio::fs::rename(&staging, &folder).await?;

        let row = plugin_config::ActiveModel {
            app_id: Set(id.clone()),
            enabled: Set(enabled),
            limits: Set(None),
            settings: Set("{}".to_owned()),
            updated_at: Set(Utc::now()),
        };
        plugin_config::Entity::insert(row)
            .on_conflict(
                OnConflict::column(plugin_config::Column::AppId)
                    .update_columns([plugin_config::Column::Enabled, plugin_config::Column::UpdatedAt])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        let mut plugin = new_plugin(id.clone(), Source::Installed, Some(manifest.clone()));
        plugin.enabled = enabled;
        if let Some(previous) = &previous {
            plugin.limits = previous.limits;
            plugin.set_values(previous.values.clone());
        }
        let mut plugins = self.list();
        match plugins.iter_mut().find(|p| p.id == id) {
            Some(slot) => *slot = plugin,
            None => plugins.push(plugin),
        }
        let registry = Registry::new(plugins);
        let installed = registry.plugins.iter().find(|p| p.id == id).cloned().ok_or_else(plugin_not_found)?;
        *self.write() = registry;
        audit!(
            "plugin.install",
            app = id,
            version = manifest.version.to_string(),
            update = previous.is_some(),
            enabled,
            "plugin installed"
        );
        Ok(installed)
    }

    /// Removes an installed plugin with its storage (models, caches: what
    /// it can fetch again). Users' data (AppData) stays, so installing it
    /// again brings everything back.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let _changing = self.changing.lock().await;
        let plugin = self.list().into_iter().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
        if plugin.source != Source::Installed {
            return Err(Error::forbidden(format!("The plugin \"{id}\" comes with the server; disable it instead"))
                .with_code("PLUGIN_NOT_REMOVABLE"));
        }
        self.host.unload(id);
        self.stop_downloads(id);
        self.storage.clear(id).await?;
        plugin_jobs::forget_app(&self.db, id).await?;
        remove_folder(&self.install_dir.join(id)).await?;
        plugin_config::Entity::delete_by_id(id.to_owned()).exec(&self.db).await?;
        let plugins = self.list().into_iter().filter(|p| p.id != id).collect();
        *self.write() = Registry::new(plugins);
        audit!("plugin.remove", app = id, "plugin removed");
        Ok(())
    }

    /// The compiled plugins.
    pub fn host(&self) -> &WasmHost {
        &self.host
    }

    /// `plugins` with what they keep in storage and how their downloads go.
    pub async fn view(&self, mut plugins: Vec<Plugin>) -> Result<Vec<Plugin>> {
        // Downloads first: one that ends meanwhile is then in the storage already.
        let downloads: HashMap<String, DownloadState> =
            self.jobs().iter().map(|(id, job)| (id.clone(), job.state.clone())).collect();
        let usage = self.storage.usage().await?;
        let mut queues = plugin_jobs::counts(&self.db).await?;
        let max_memory = self.defaults.read(|d| d.max_memory);
        for plugin in &mut plugins {
            plugin.effective = Effective::of(scale_of(Some(plugin), max_memory), plugin.manifest.as_ref());
            plugin.queue = queues.remove(&plugin.id).unwrap_or_default();
            plugin.storage = usage.get(&plugin.id).copied().unwrap_or_default();
            plugin.download = downloads.get(&plugin.id).cloned();
        }
        Ok(plugins)
    }

    async fn view_one(&self, plugin: Plugin) -> Result<Plugin> {
        Ok(self.view(vec![plugin]).await?.remove(0))
    }

    /// The files in a plugin's storage.
    pub async fn storage_files(&self, id: &str) -> Result<Vec<StorageFile>> {
        self.list().into_iter().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
        Ok(self.storage.list(id, "").await?.into_iter().map(StorageFile::from).collect())
    }

    /// Deletes everything in a plugin's storage; the files of its
    /// `[[downloads]]` come again if it runs.
    pub async fn clear_storage(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        let plugin = self.list().into_iter().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
        self.stop_downloads(id);
        self.storage.clear(id).await?;
        audit!("plugin.storage_clear", app = id, "plugin storage cleared");
        self.start_downloads(id);
        self.view_one(plugin).await
    }

    /// Starts the downloads of every running plugin (at startup).
    pub fn start_all_downloads(self: &Arc<Self>) {
        for plugin in self.list().into_iter().filter(|p| p.status == Status::Running) {
            self.start_downloads(&plugin.id);
        }
    }

    /// Fetches the files of a running plugin's `[[downloads]]` it doesn't
    /// have yet, in the background; nothing if they are being fetched.
    pub fn start_downloads(self: &Arc<Self>, id: &str) {
        let Some(manifest) = self.get(id).filter(|m| !m.downloads.is_empty()) else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.jobs();
            if jobs.get(id).is_some_and(|job| matches!(job.state, DownloadState::Running { .. })) {
                return;
            }
            let first = &manifest.downloads[0];
            let state = DownloadState::Running {
                path: first.path.clone(),
                done: 0,
                total: first.size,
                left: manifest.downloads.len(),
            };
            jobs.insert(id.to_owned(), DownloadJob { state, cancel: cancel.clone() });
        }
        let plugins = self.clone();
        tokio::spawn(async move { plugins.download_all(manifest, cancel).await });
    }

    /// Stops a plugin's downloads (it is disabled, updated or removed).
    pub fn stop_downloads(&self, id: &str) {
        if let Some(job) = self.jobs().remove(id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    async fn download_all(self: Arc<Self>, manifest: Manifest, cancel: Arc<AtomicBool>) {
        let id = manifest.id.as_str();
        let mut missing = Vec::new();
        for download in &manifest.downloads {
            match self.storage.stat(id, &download.path).await {
                Ok(file) if file.sha256.eq_ignore_ascii_case(&download.sha256) => {}
                _ => missing.push(download),
            }
        }
        let rules = manifest.permissions.network_rules();
        for (index, download) in missing.iter().enumerate() {
            let path = download.path.clone();
            let left = missing.len() - index;
            self.set_download(id, &cancel, DownloadState::Running { path, done: 0, total: download.size, left });
            tracing::info!(app = id, "downloading {} from {}", download.path, download.url);
            if let Err(error) = self.download_one(id, download, &rules, &cancel).await {
                if !cancel.load(Ordering::Relaxed) {
                    audit!(warn, "plugin.download", app = id, path = download.path, error, "plugin download failed");
                    let path = download.path.clone();
                    self.set_download(id, &cancel, DownloadState::Failed { path, error });
                }
                return;
            }
        }
        let mut jobs = self.jobs();
        if jobs.get(id).is_some_and(|job| Arc::ptr_eq(&job.cancel, &cancel)) {
            jobs.remove(id);
        }
        if !missing.is_empty() {
            audit!("plugin.download", app = id, files = missing.len(), "plugin files downloaded");
        }
    }

    /// Downloads one file into the plugin's storage, keeping its progress.
    async fn download_one(
        self: &Arc<Self>,
        id: &str,
        download: &Download,
        rules: &[rustcloud_plugin_api::NetworkRule],
        cancel: &Arc<AtomicBool>,
    ) -> std::result::Result<(), String> {
        let (size, data) = self.host.download(rules, &download.url).await?;
        let total = download.size.or(size);
        let stop = cancel.clone();
        let data = data.map(move |chunk| match stop.load(Ordering::Relaxed) {
            true => Err(std::io::Error::other("stopped")),
            false => chunk,
        });
        let (plugins, app, cancel) = (self.clone(), id.to_owned(), cancel.clone());
        let progress = move |done| {
            let mut jobs = plugins.jobs();
            if let Some(job) = jobs.get_mut(&app).filter(|job| Arc::ptr_eq(&job.cancel, &cancel)) {
                if let DownloadState::Running { done: shown, total: known, .. } = &mut job.state {
                    (*shown, *known) = (done, total);
                }
            }
        };
        self.storage
            .save(id, &download.path, Box::pin(data), Some(&download.sha256), progress)
            .await
            .map(|_| ())
            .map_err(|err| err.message)
    }

    /// Changes the state of a plugin's downloads, unless they were stopped.
    fn set_download(&self, id: &str, cancel: &Arc<AtomicBool>, state: DownloadState) {
        if let Some(job) = self.jobs().get_mut(id).filter(|job| Arc::ptr_eq(&job.cancel, cancel)) {
            job.state = state;
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<String, DownloadJob>> {
        self.downloads.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl Registry {
    /// Works out which plugins run: the enabled ones that loaded, in order,
    /// unless one asks for a discovery address an earlier one has.
    fn new(mut plugins: Vec<Plugin>) -> Self {
        let mut running = Apps::new(Vec::new()).expect("an empty list is valid");
        for plugin in &mut plugins {
            plugin.status = match (&plugin.load_error, &plugin.manifest) {
                (Some(error), _) => failed(error.clone()),
                (None, None) => failed("no manifest".to_owned()),
                (None, Some(_)) if !plugin.enabled => Status::Disabled,
                (None, Some(manifest)) => match running.add(manifest.clone()) {
                    Ok(()) => Status::Running,
                    Err(err) => failed(err.0),
                },
            };
        }
        Self { plugins, running: Arc::new(running) }
    }
}

fn new_plugin(id: String, source: Source, manifest: Option<Manifest>) -> Plugin {
    Plugin {
        id,
        source,
        manifest,
        status: Status::Running,
        enabled: true,
        limits: None,
        effective: Effective::default(),
        settings: Map::new(),
        secrets_set: Vec::new(),
        values: Map::new(),
        queue: BTreeMap::new(),
        storage: Usage::default(),
        download: None,
        load_error: None,
    }
}

/// A setting's value if it fits the setting's kind.
fn checked(kind: SettingKind, key: &str, value: &Value) -> Result<Value> {
    let fits = match (kind, value) {
        (SettingKind::Bool, Value::Bool(_)) => true,
        (SettingKind::Number, Value::Number(n)) => n.is_i64(),
        (SettingKind::Text | SettingKind::Secret, Value::String(text)) => text.chars().count() <= TEXT_LIMIT,
        (SettingKind::List, Value::Array(lines)) => {
            lines.len() <= LIST_LIMIT
                && lines.iter().all(|line| line.as_str().is_some_and(|line| line.chars().count() <= TEXT_LIMIT))
        }
        _ => false,
    };
    if !fits {
        return Err(invalid_settings(format!("\"{key}\" is not a valid {kind:?} value").to_lowercase()));
    }
    Ok(value.clone())
}

fn invalid_settings(message: String) -> Error {
    Error::bad_request("INVALID_SETTINGS", message)
}

fn failed(error: String) -> Status {
    Status::Failed { error }
}

/// Whether an update asks users for the same as the installed version.
fn asks_for_the_same(old: &Manifest, new: &Manifest) -> bool {
    let jobs_within = new.jobs.iter().all(|job| {
        old.jobs.iter().any(|before| {
            let ((memory, time), (old_memory, old_time)) = (job.scale(), before.scale());
            before.kind == job.kind && memory <= old_memory && time <= old_time && job.batch <= before.batch
        })
    });
    old.permissions == new.permissions
        && old.well_known == new.well_known
        && old.proxy == new.proxy
        && old.downloads == new.downloads
        && old.server_settings == new.server_settings
        && (old.large_bodies || !new.large_bodies)
        && jobs_within
        && new.limits.within(&old.limits)
}

/// The multiples of a plugin (see [`Plugins::scale`]).
fn scale_of(plugin: Option<&Plugin>, max_memory: u32) -> Scale {
    let declared = plugin.and_then(|p| p.manifest.as_ref()).map(|m| m.limits).unwrap_or_default();
    let mut scale = plugin.and_then(|p| p.limits).unwrap_or(declared);
    scale.memory = scale.memory.min(max_memory);
    scale
}

/// The administrator's multiples as kept in the database.
fn limits_json(limits: Option<Scale>) -> Result<Option<String>> {
    limits.map(|limits| serde_json::to_string(&limits).map_err(Error::internal)).transpose()
}

/// `manifest.toml` and `app.wasm` from a zip archive: at its top or in one folder.
fn unpack(archive: &[u8]) -> Result<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|err| invalid_plugin(format!("not a zip archive: {err}")))?;
    let (mut manifest, mut wasm) = (None, None);
    for index in 0..zip.len() {
        let file = zip.by_index(index).map_err(|err| invalid_plugin(format!("broken archive: {err}")))?;
        let Some(path) = file.enclosed_name().filter(|path| !file.is_dir() && path.components().count() <= 2) else {
            continue;
        };
        let (found, limit) = match path.file_name().and_then(|name| name.to_str()) {
            Some("manifest.toml") => (&mut manifest, MANIFEST_LIMIT),
            Some("app.wasm") => (&mut wasm, ARCHIVE_LIMIT as u64),
            _ => continue,
        };
        if found.is_some() {
            return Err(invalid_plugin(format!("the archive has two files named {}", path.display())));
        }
        let mut data = Vec::new();
        file.take(limit + 1).read_to_end(&mut data).map_err(|err| invalid_plugin(format!("broken archive: {err}")))?;
        if data.len() as u64 > limit {
            return Err(invalid_plugin(format!("{} is too large", path.display())));
        }
        *found = Some(data);
    }
    let manifest = manifest.ok_or_else(|| invalid_plugin("the archive has no manifest.toml".to_owned()))?;
    let wasm = wasm.ok_or_else(|| invalid_plugin("the archive has no app.wasm".to_owned()))?;
    let manifest = String::from_utf8(manifest).map_err(|_| invalid_plugin("manifest.toml is not UTF-8".to_owned()))?;
    Ok((manifest, wasm))
}

async fn remove_folder(folder: &FsPath) -> Result<()> {
    match tokio::fs::remove_dir_all(folder).await {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

fn invalid_plugin(message: String) -> Error {
    Error::bad_request("INVALID_PLUGIN", message)
}

fn plugin_not_found() -> Error {
    Error::not_found("PLUGIN_NOT_FOUND", "There is no such plugin")
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/plugins", get(list).post(install).layer(DefaultBodyLimit::max(ARCHIVE_LIMIT)))
        .route("/api/v1/admin/plugins/{id}", put(configure).delete(remove))
        .route("/api/v1/admin/plugins/{id}/settings", put(save_settings))
        .route("/api/v1/admin/plugins/{id}/storage", get(storage_files).delete(clear_storage))
        .route("/api/v1/admin/plugins/{id}/downloads", post(retry_downloads))
}

/// `GET /api/v1/admin/plugins/{id}/storage`: the files in the plugin's storage.
async fn storage_files(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<StorageFile>>> {
    Ok(Json(plugins.storage_files(&id).await?))
}

/// `DELETE /api/v1/admin/plugins/{id}/storage`: clears the plugin's storage.
async fn clear_storage(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Plugin>> {
    Ok(Json(plugins.clear_storage(&id).await?))
}

/// `POST /api/v1/admin/plugins/{id}/downloads`: tries the downloads again.
async fn retry_downloads(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Plugin>> {
    let plugin = plugins.list().into_iter().find(|p| p.id == id).ok_or_else(plugin_not_found)?;
    plugins.start_downloads(&id);
    Ok(Json(plugins.view_one(plugin).await?))
}

/// `PUT /api/v1/admin/plugins/{id}/settings`: the plugin's server settings.
async fn save_settings(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
    Json(values): Json<Map<String, Value>>,
) -> Result<Json<Plugin>> {
    let plugin = plugins.save_settings(&id, values).await?;
    Ok(Json(plugins.view_one(plugin).await?))
}

/// `POST /api/v1/admin/plugins`: installs or updates a plugin from a zip archive (the body).
async fn install(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    archive: Bytes,
) -> Result<(StatusCode, Json<Plugin>)> {
    let plugin = plugins.install(archive).await?;
    // An update may download other files.
    plugins.stop_downloads(&plugin.id);
    plugins.start_downloads(&plugin.id);
    Ok((StatusCode::CREATED, Json(plugins.view_one(plugin).await?)))
}

/// `DELETE /api/v1/admin/plugins/{id}`: removes an installed plugin.
async fn remove(State(plugins): State<Arc<Plugins>>, _admin: AdminUser, Path(id): Path<String>) -> Result<StatusCode> {
    plugins.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/admin/plugins`: every plugin with its state.
async fn list(State(plugins): State<Arc<Plugins>>, _admin: AdminUser) -> Result<Json<Vec<Plugin>>> {
    Ok(Json(plugins.view(plugins.list()).await?))
}

/// `PUT /api/v1/admin/plugins/{id}`: enables or disables a plugin, sets its limits.
async fn configure(
    State(plugins): State<Arc<Plugins>>,
    _admin: AdminUser,
    Path(id): Path<String>,
    Json(update): Json<PluginUpdate>,
) -> Result<Json<Plugin>> {
    let plugin = plugins.configure(&id, update).await?;
    match plugin.status {
        Status::Running => plugins.start_downloads(&id),
        _ => plugins.stop_downloads(&id),
    }
    Ok(Json(plugins.view_one(plugin).await?))
}

#[cfg(test)]
mod tests {
    use rustcloud_db::testing::test_db;
    use rustcloud_storage_local::LocalBlobStore;
    use rustcloud_wasm_host::testing::install_echo;

    use super::*;

    async fn load(db: &DatabaseConnection, dir: &std::path::Path) -> Plugins {
        let blobs = Arc::new(LocalBlobStore::open(dir.join(".blobs")).await.unwrap());
        let storage = Arc::new(PluginStorage::new(db.clone(), blobs));
        let host = Arc::new(WasmHost::new().unwrap().with_storage(storage.clone()));
        let defaults = Arc::new(Live::new(PluginSettings::default()));
        let dirs = [(dir.to_owned(), Source::Installed)];
        Plugins::load(db.clone(), host, storage, defaults, &dirs, dir.to_owned()).await.unwrap()
    }

    #[tokio::test]
    async fn what_the_administrator_sets_survives_a_restart() {
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        install_echo(dir.path());

        let plugins = load(&db.conn, dir.path()).await;
        assert_eq!(plugins.limits("echo"), Limits::default());
        let scale = Scale { memory: 2, time: 1, workers: 3 };
        let update = PluginUpdate { enabled: false, limits: Some(scale) };
        plugins.configure("echo", update).await.unwrap();
        assert!(plugins.get("echo").is_none(), "disabled at once");

        let restarted = load(&db.conn, dir.path()).await;
        let echo = restarted.list().into_iter().find(|p| p.id == "echo").unwrap();
        assert_eq!((echo.status, echo.limits), (Status::Disabled, Some(scale)));
        assert_eq!(restarted.limits("echo"), Limits::scaled(2, 1));
        assert_eq!(restarted.workers("echo"), 3);

        let too_much = PluginUpdate { enabled: false, limits: Some(Scale { memory: 32, ..scale }) };
        let err = restarted.configure("echo", too_much).await.unwrap_err();
        assert!(err.message.contains("max_memory"), "{err:?}");
        let odd = PluginUpdate { enabled: false, limits: Some(Scale { time: 3, ..scale }) };
        assert_eq!(restarted.configure("echo", odd).await.unwrap_err().code, "INVALID_SETTINGS");
    }

    #[tokio::test]
    async fn a_discovery_address_goes_to_the_first_plugin_that_asks() {
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let plugins = load(&db.conn, dir.path()).await;
        let calendar = |id: &str| {
            let manifest = format!(
                "id = \"{id}\"\nversion = \"1.0.0\"\napi = \"={}\"\n[name]\nen = \"{id}\"\n[well_known]\ncaldav = \"/dav/\"\n",
                rustcloud_plugin_api::API_VERSION
            );
            Manifest::from_toml(&manifest).unwrap()
        };
        plugins.add_built_in(calendar("one"));
        plugins.add_built_in(calendar("two"));
        let states: Vec<Status> = plugins.list().into_iter().skip(1).map(|p| p.status).collect();
        assert_eq!(states[0], Status::Running);
        assert!(matches!(&states[1], Status::Failed { error } if error.contains("already has")));
    }
}
