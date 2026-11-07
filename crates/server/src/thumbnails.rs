//! Thumbnails of photos and videos for the file lists and the viewer.
//!
//! The media engine makes them (any photo format it reads, RAW and HEIC
//! too; a frame of a video), in WebP. They are kept on disk under
//! `data_dir/thumbnails` by the SHA-256 of the content and the size, so the
//! same content is made once, and a file that changes gets a new one. One
//! that could not be made is remembered too, so a broken file doesn't keep
//! the engine busy. Thumbnails nobody asked for in [`KEEP_DAYS`] days go.
//!
//! - `GET /api/v1/files/{id}/thumbnail?size=small|large`
//! - `GET /api/v1/public/{token}/thumbnail?node=&size=` (a public link)
//!
//! Without the media engine (no ffmpeg) every answer is `404 NO_THUMBNAIL`
//! and the panel shows icons.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rustcloud_auth::CurrentUser;
use rustcloud_core::{Error, Result};
use rustcloud_files::{Node, LINK_COOKIE};
use rustcloud_media::{Fit, ImageFormat, Media, PictureOptions, Priority};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;

/// Thumbnails not asked for in this many days are removed.
const KEEP_DAYS: u64 = 30;
/// How often old thumbnails are looked for.
const CLEANUP_EVERY: Duration = Duration::from_secs(6 * 3600);
/// The largest thumbnail the engine may give.
const MAX_BYTES: u64 = 4 << 20;
/// A kept thumbnail is marked as used at most this often.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 3600);

/// The two sizes the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    /// A 256 px square for lists and grids.
    #[default]
    Small,
    /// Up to 1280 px on the longer side, for the viewer.
    Large,
}

impl Size {
    fn options(self) -> PictureOptions {
        let (side, fit) = match self {
            Self::Small => (256, Fit::Cover),
            Self::Large => (1280, Fit::Inside),
        };
        PictureOptions { max_width: side, max_height: side, fit, format: ImageFormat::Webp, quality: 80, at_ms: None }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Large => "large",
        }
    }
}

/// Thumbnails kept on disk.
pub struct Thumbnails {
    dir: PathBuf,
    media: Arc<Media>,
}

impl Thumbnails {
    pub fn new(dir: PathBuf, media: Arc<Media>) -> Self {
        Self { dir, media }
    }

    /// Whether thumbnails can be made here at all.
    pub fn available(&self) -> bool {
        self.media.status().available
    }

    /// Removes old thumbnails now and then, for as long as the server runs.
    pub fn start_cleanup(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let dir = this.dir.clone();
                let removed = tokio::task::spawn_blocking(move || remove_old(&dir)).await.unwrap_or(0);
                if removed > 0 {
                    tracing::info!("removed {removed} thumbnails nobody looked at for {KEEP_DAYS} days");
                }
                tokio::time::sleep(CLEANUP_EVERY).await;
            }
        });
    }

    /// The thumbnail of `node`, made from `file` if it isn't kept yet.
    async fn get(&self, node: &Node, file: std::fs::File, size: Size) -> Result<Vec<u8>> {
        let none = || Error::not_found("NO_THUMBNAIL", "There is no thumbnail of this file");
        let pictured =
            node.mime_type.as_deref().is_some_and(|mime| mime.starts_with("image/") || mime.starts_with("video/"));
        let (true, Some(sha256)) = (pictured && self.available(), node.etag.as_deref()) else {
            return Err(none());
        };
        let path = self.path(sha256, size, "webp");
        let failed = self.path(sha256, size, "none");
        if let Ok(bytes) = tokio::fs::read(&path).await {
            touch(path);
            return Ok(bytes);
        }
        if tokio::fs::try_exists(&failed).await.unwrap_or(false) {
            touch(failed);
            return Err(none());
        }
        match self.media.thumbnail(file, &size.options(), MAX_BYTES, Priority::Request).await {
            Ok(bytes) => {
                save(&path, &bytes).await;
                Ok(bytes)
            }
            // Busy is not broken: try again next time.
            Err(err) if err.code == "SERVER_BUSY" => Err(err),
            Err(err) => {
                tracing::debug!(file = %node.id, "no thumbnail: {err}");
                save(&failed, &[]).await;
                Err(none())
            }
        }
    }

    fn path(&self, sha256: &str, size: Size, extension: &str) -> PathBuf {
        let prefix = sha256.get(..2).unwrap_or("00");
        self.dir.join(prefix).join(format!("{sha256}-{}.{extension}", size.name()))
    }
}

/// Writes a kept thumbnail whole or not at all.
async fn save(path: &Path, bytes: &[u8]) {
    let write = async {
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let partial = path.with_extension(format!("{}.part", Uuid::new_v4().simple()));
        tokio::fs::write(&partial, bytes).await?;
        tokio::fs::rename(&partial, path).await
    };
    if let Err(err) = write.await {
        tracing::warn!("could not keep a thumbnail: {err}");
    }
}

/// Marks a kept thumbnail as used, so the cleanup leaves it.
fn touch(path: PathBuf) {
    tokio::task::spawn_blocking(move || {
        let Ok(file) = std::fs::File::options().write(true).open(&path) else { return };
        let old = file.metadata().and_then(|m| m.modified()).ok();
        if old.is_some_and(|old| old.elapsed().unwrap_or_default() > TOUCH_AFTER) {
            let _ = file.set_modified(SystemTime::now());
        }
    });
}

/// Removes thumbnails not used for [`KEEP_DAYS`] days; returns how many.
fn remove_old(dir: &Path) -> usize {
    let limit = Duration::from_secs(KEEP_DAYS * 24 * 3600);
    let mut removed = 0;
    let Ok(prefixes) = std::fs::read_dir(dir) else { return 0 };
    for prefix in prefixes.flatten() {
        let Ok(files) = std::fs::read_dir(prefix.path()) else { continue };
        for file in files.flatten() {
            let old =
                file.metadata().and_then(|m| m.modified()).is_ok_and(|at| at.elapsed().unwrap_or_default() > limit);
            if old && std::fs::remove_file(file.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/files/{id}/thumbnail", get(thumbnail))
        .route("/api/v1/public/{token}/thumbnail", get(public_thumbnail))
}

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    size: Size,
    /// A file inside a shared folder (public links).
    node: Option<Uuid>,
}

async fn thumbnail(
    State(state): State<AppState>,
    current: CurrentUser,
    UrlPath(id): UrlPath<Uuid>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Result<Response> {
    let (node, file) = state.files.open_for_media(current.user(), id).await?;
    answer(&state.thumbnails, &node, file, params.size, &headers).await
}

async fn public_thumbnail(
    State(state): State<AppState>,
    UrlPath(token): UrlPath<String>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Result<Response> {
    let key = link_key(&headers);
    let (node, file) = state.files.public_open_for_media(&token, key.as_deref(), params.node).await?;
    answer(&state.thumbnails, &node, file, params.size, &headers).await
}

async fn answer(
    thumbnails: &Thumbnails,
    node: &Node,
    file: std::fs::File,
    size: Size,
    headers: &HeaderMap,
) -> Result<Response> {
    let etag = format!("\"{}-{}\"", node.etag.as_deref().unwrap_or_default(), size.name());
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let bytes = thumbnails.get(node, file, size).await?;
    let headers = [
        (header::CONTENT_TYPE, "image/webp".to_owned()),
        (header::CACHE_CONTROL, "private, max-age=86400".to_owned()),
        (header::ETAG, etag),
    ];
    Ok((headers, bytes).into_response())
}

/// The key of an unlocked public link, from its cookie.
fn link_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == LINK_COOKIE)
        .map(|(_, value)| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_thumbnails_go_and_used_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ab");
        std::fs::create_dir_all(&sub).unwrap();
        let old = sub.join("abc-small.webp");
        let fresh = sub.join("abd-small.webp");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&fresh, b"y").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs((KEEP_DAYS + 1) * 24 * 3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(long_ago).unwrap();
        assert_eq!(remove_old(dir.path()), 1);
        assert!(!old.exists() && fresh.exists());
    }
}
