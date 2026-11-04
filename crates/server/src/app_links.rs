//! Files the server sends for apps itself, as a stream with `Range` and
//! `ETag`, whatever their size: links for an app's page (the `links` import,
//! `GET /api/v1/app-links/<token>`) and files an app answers with (the
//! `x-rustcloud-send` header). The file never passes through the app.
//!
//! An app's page runs in a sandboxed `<iframe>` whose requests carry no
//! cookie, so a link's token says who asks: the user's session, the app, the
//! file and its version, signed with a key the server makes when it starts
//! (links end with a restart; the page asks for new ones). Every request
//! checks again that the session is valid, the app runs, it may still read
//! the file for the user and the file is that version: a link ends with the
//! session, the access or the file.
//!
//! A link may also be a video's HLS stream (`links.video-stream`):
//! `<link>/index.m3u8`, `<link>/init.mp4` and `<link>/<n>.m4s`, made by the
//! media engine when the player asks ([`rustcloud_media`]'s `stream`).
//! The pieces and the init segment of a video are remembered for a while.
//!
//! An upload link (`links.upload`) goes the other way: the page `POST`s a
//! file to it, which is stored as a stream into a folder the user gave the
//! app to change, never passing through the app. The token names the
//! session, the app, the folder and the file's name; it serves one upload,
//! which must start within [`UPLOAD_WITHIN`]. The upload checks the session,
//! the app and its access again, and goes through the user's quota and the
//! server's upload limit like any other.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use futures_util::TryStreamExt;
use rustcloud_auth::User;
use rustcloud_core::{ByteRange, Error, Result};
use rustcloud_files::{content, Download, Files, Node};
use rustcloud_media::{Media, Piece, Priority, StreamMode};
use rustcloud_wasm_host::{AppStorage, LinkTarget};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_tokens::AppTokens;
use crate::plugin_storage::PluginStorage;
use crate::AppState;

/// A file the server sends for an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", content = "v")]
pub enum Target {
    /// A file the user gave the app.
    #[serde(rename = "f")]
    File(Uuid),
    /// A file of the user's AppData of the app.
    #[serde(rename = "a")]
    AppData(String),
    /// A file of the app's storage.
    #[serde(rename = "s")]
    Storage(String),
}

impl Target {
    /// What an app's `x-rustcloud-send` header names: `file:<id>`,
    /// `appdata:<path>` or `storage:<path>`.
    pub fn from_header(value: &str) -> Result<Self> {
        let invalid = || Error::internal(format!("the app asked to send \"{value}\""));
        let (kind, what) = value.split_once(':').ok_or_else(invalid)?;
        match kind {
            "file" => Ok(Self::File(Uuid::parse_str(what.trim()).map_err(|_| invalid())?)),
            "appdata" => Ok(Self::AppData(what.to_owned())),
            "storage" => Ok(Self::Storage(what.to_owned())),
            _ => Err(invalid()),
        }
    }

    fn from_wit(target: LinkTarget) -> Result<Self> {
        Ok(match target {
            LinkTarget::File(id) => {
                Self::File(Uuid::parse_str(&id).map_err(|_| Error::not_found("FILE_NOT_FOUND", "File not found"))?)
            }
            LinkTarget::Appdata(path) => Self::AppData(path),
            LinkTarget::Storage(path) => Self::Storage(path),
        })
    }
}

/// What a link's token holds; the signature comes after it.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// The session.
    s: Uuid,
    /// The app.
    a: String,
    t: Target,
    /// The file's ETag when the link was made.
    e: String,
    /// A stream of the video, not the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<StreamMode>,
    /// Until when the link works, in Unix seconds.
    x: i64,
}

/// What an upload link's token holds. Its fields differ from [`Claims`], so
/// neither kind of token passes for the other.
#[derive(Debug, Serialize, Deserialize)]
struct UploadClaims {
    /// The session.
    s: Uuid,
    /// The app.
    a: String,
    /// The folder.
    f: Uuid,
    /// The file's name.
    n: String,
    /// It may replace a file of that name.
    o: bool,
    /// Until when the upload may start, in Unix seconds.
    x: i64,
    /// Tells this link from any other: it serves one upload.
    r: u64,
}

/// What a link to replace a file's content holds. Its fields differ from
/// [`Claims`] and [`UploadClaims`], so no other token passes for it.
#[derive(Debug, Serialize, Deserialize)]
struct ReplaceClaims {
    /// The session.
    s: Uuid,
    /// The app.
    a: String,
    /// The file.
    i: Uuid,
    /// The ETag the file must still have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<String>,
    /// Until when the upload may start, in Unix seconds.
    x: i64,
    /// Tells this link from any other: it serves one upload.
    r: u64,
}

/// How long an upload link waits for its upload to start, in seconds.
pub const UPLOAD_WITHIN: i64 = 15 * 60;

/// How long a link to a file works at least, in seconds (also while the
/// session lives on): a link that leaked doesn't stay good for days. It
/// ends at a full hour, so all links made within an hour are the same and
/// the browser's cache keeps working.
pub const LINK_LIFETIME: i64 = 12 * 60 * 60;

/// When a link made now ends: 12 to 13 hours on, at a full hour.
fn link_end() -> i64 {
    let hour = 60 * 60;
    (Utc::now().timestamp() / hour + 1) * hour + LINK_LIFETIME
}

/// Videos remembered at most: their pieces and init segments.
const STREAMS_KEPT: usize = 256;

/// What is known of a video's stream: by its version and mode.
#[derive(Default)]
struct Streams {
    pieces: HashMap<(String, StreamMode), Arc<Vec<Piece>>>,
    inits: HashMap<(String, StreamMode), Arc<Vec<u8>>>,
}

pub struct AppLinks {
    /// Signs the links; made at startup.
    key: [u8; 32],
    /// Tokens apps give other servers.
    pub tokens: AppTokens,
    /// Where bodies of requests too large for apps are kept ([`crate::app_bodies`]).
    pub bodies: PathBuf,
    files: Arc<Files>,
    storage: Arc<PluginStorage>,
    media: Arc<Media>,
    streams: Mutex<Streams>,
    /// Upload links used or in use, with when they end.
    spent: Mutex<HashMap<u64, i64>>,
}

impl AppLinks {
    pub fn new(
        files: Arc<Files>,
        storage: Arc<PluginStorage>,
        media: Arc<Media>,
        tokens: AppTokens,
        bodies: PathBuf,
    ) -> Self {
        let (streams, spent) = (Mutex::default(), Mutex::default());
        Self { key: rand::random(), tokens, bodies, files, storage, media, streams, spent }
    }

    /// A link for the page of `app_id` in `session` to `target`, if the user
    /// may read it through the app; the file with it.
    pub async fn create(&self, session: Uuid, user: &User, app_id: &str, target: LinkTarget) -> Result<(String, Node)> {
        self.link(session, user, app_id, target, None).await
    }

    /// An HLS playlist of a video for the page of `app_id`, and the file.
    pub async fn create_stream(
        &self,
        session: Uuid,
        user: &User,
        app_id: &str,
        target: LinkTarget,
        mode: StreamMode,
    ) -> Result<(String, Node)> {
        if !self.media.status().available {
            return Err(Error::bad_request("MEDIA_UNSUPPORTED", "There is no ffmpeg on this server"));
        }
        let (link, file) = self.link(session, user, app_id, target, Some(mode)).await?;
        Ok((format!("{link}/index.m3u8"), file))
    }

    async fn link(
        &self,
        session: Uuid,
        user: &User,
        app_id: &str,
        target: LinkTarget,
        mode: Option<StreamMode>,
    ) -> Result<(String, Node)> {
        let target = Target::from_wit(target)?;
        let file = self.file(Some(user), app_id, &target).await?;
        let e = file.etag.clone().unwrap_or_default();
        let x = link_end();
        let claims = Claims { s: session, a: app_id.to_owned(), t: target, e, m: mode, x };
        Ok((format!("/api/v1/app-links/{}", self.token(&claims)?), file))
    }

    /// A link for the page of `app_id` in `session` to upload `name` into
    /// `folder`, if the app may.
    pub async fn create_upload(
        &self,
        session: Uuid,
        user: &User,
        app_id: &str,
        folder: Uuid,
        name: &str,
        overwrite: bool,
    ) -> Result<String> {
        self.files.app_upload_check(user, app_id, folder, name, overwrite).await?;
        let claims = UploadClaims {
            s: session,
            a: app_id.to_owned(),
            f: folder,
            n: name.to_owned(),
            o: overwrite,
            x: Utc::now().timestamp() + UPLOAD_WITHIN,
            r: rand::random(),
        };
        Ok(format!("/api/v1/app-links/{}", self.token(&claims)?))
    }

    /// A link for the page of `app_id` in `session` to upload a new content
    /// of `file`, if the app may change it.
    pub async fn create_replace(
        &self,
        session: Uuid,
        user: &User,
        app_id: &str,
        file: Uuid,
        if_match: Option<&str>,
    ) -> Result<String> {
        self.files.app_replace_check(user, app_id, file).await?;
        let claims = ReplaceClaims {
            s: session,
            a: app_id.to_owned(),
            i: file,
            m: if_match.map(str::to_owned),
            x: Utc::now().timestamp() + UPLOAD_WITHIN,
            r: rand::random(),
        };
        Ok(format!("/api/v1/app-links/{}", self.token(&claims)?))
    }

    /// Takes an upload link (`r`, valid until `x`) for its upload: false if
    /// it was taken already. Forgets the links that have ended.
    fn spend(&self, r: u64, x: i64) -> bool {
        let now = Utc::now().timestamp();
        let mut spent = self.spent.lock().unwrap_or_else(|e| e.into_inner());
        spent.retain(|_, until| *until >= now);
        spent.insert(r, x).is_none()
    }

    /// Gives an upload link back after its upload failed, to be tried again.
    fn give_back(&self, r: u64) {
        self.spent.lock().unwrap_or_else(|e| e.into_inner()).remove(&r);
    }

    /// `claims`, signed: a token.
    fn token(&self, claims: &impl Serialize) -> Result<String> {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(Error::internal)?);
        let signature = URL_SAFE_NO_PAD.encode(&self.sign(payload.as_bytes())[..16]);
        Ok(format!("{payload}.{signature}"))
    }

    /// What a token says, if this server signed it.
    fn open<T: DeserializeOwned>(&self, token: &str) -> Option<T> {
        let (payload, signature) = token.split_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let expected = self.sign(payload.as_bytes());
        if signature.len() != 16 || !same(&signature, &expected[..16]) {
            return None;
        }
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
    }

    /// The file `target` is now, if `user` (none: a request without one) may
    /// read it through the app.
    pub async fn file(&self, user: Option<&User>, app_id: &str, target: &Target) -> Result<Node> {
        let file = match (target, user) {
            (Target::Storage(path), _) => self.storage.file(app_id, path).await?,
            (Target::File(id), Some(user)) => self.files.app_get(user, app_id, *id).await?,
            (Target::AppData(path), Some(user)) => self.files.appdata_file(user, app_id, path).await?,
            (_, None) => return Err(Error::not_found("FILE_NOT_FOUND", "File not found")),
        };
        match file.is_dir {
            true => Err(Error::not_found("FILE_NOT_FOUND", "File not found")),
            false => Ok(file),
        }
    }

    /// Sends `target` for the app, answering `request`'s `Range` and `If-None-Match`.
    pub async fn send(
        &self,
        user: Option<&User>,
        app_id: &str,
        target: &Target,
        request: &HeaderMap,
    ) -> Result<Response> {
        let file = self.file(user, app_id, target).await?;
        content::serve(file, request, false, |range| self.download(user, app_id, target, range)).await
    }

    async fn download(
        &self,
        user: Option<&User>,
        app_id: &str,
        target: &Target,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        match (target, user) {
            (Target::Storage(path), _) => self.storage.download(app_id, path, range).await,
            (Target::File(id), Some(user)) => self.files.app_download(user, app_id, *id, range).await,
            (Target::AppData(path), Some(user)) => self.files.appdata_download(user, app_id, path, range).await,
            (_, None) => Err(Error::not_found("FILE_NOT_FOUND", "File not found")),
        }
    }

    /// The file opened for the media engine.
    async fn open_file(&self, user: &User, app_id: &str, target: &Target) -> Result<std::fs::File> {
        match target {
            Target::Storage(path) => self.storage.open(app_id, path).await,
            Target::File(id) => Ok(self.files.app_open(user, app_id, *id).await?.0),
            Target::AppData(path) => self.files.appdata_open(user, app_id, path).await,
        }
    }

    /// The pieces of a video's stream, remembered by its version.
    async fn pieces(&self, user: &User, claims: &Claims, mode: StreamMode) -> Result<Arc<Vec<Piece>>> {
        let key = (claims.e.clone(), mode);
        if let Some(pieces) = self.streams().pieces.get(&key) {
            return Ok(pieces.clone());
        }
        let file = self.open_file(user, &claims.a, &claims.t).await?;
        let pieces = Arc::new(self.media.stream_pieces(file, mode, Priority::Request).await?);
        let mut streams = self.streams();
        if streams.pieces.len() >= STREAMS_KEPT {
            streams.pieces.clear();
        }
        streams.pieces.insert(key, pieces.clone());
        Ok(pieces)
    }

    /// One piece of a video's stream: its init segment, remembered, and its media.
    async fn piece(
        &self,
        user: &User,
        claims: &Claims,
        mode: StreamMode,
        piece: Piece,
    ) -> Result<(Arc<Vec<u8>>, Vec<u8>)> {
        let file = self.open_file(user, &claims.a, &claims.t).await?;
        let (init, media) = self.media.stream_piece(file, mode, piece, Priority::Request).await?;
        let mut streams = self.streams();
        if streams.inits.len() >= STREAMS_KEPT {
            streams.inits.clear();
        }
        let init = streams.inits.entry((claims.e.clone(), mode)).or_insert_with(|| Arc::new(init)).clone();
        Ok((init, media))
    }

    fn streams(&self) -> std::sync::MutexGuard<'_, Streams> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// HMAC-SHA256 (RFC 2104) of `message` with the server's key.
    fn sign(&self, message: &[u8]) -> [u8; 32] {
        hmac_sha256(&self.key, message)
    }
}

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    match key.len() > 64 {
        true => block[..32].copy_from_slice(&Sha256::digest(key)),
        false => block[..key.len()].copy_from_slice(key),
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

/// Equal, in the same time whatever differs.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/app-links/{token}", get(follow).post(upload))
        .route("/api/v1/app-links/{token}/{name}", get(follow_stream))
}

fn gone() -> Error {
    Error::not_found("LINK_NOT_FOUND", "The link is no longer valid")
}

/// What a link says, and its user, while it is valid: signed here, the
/// session alive, the plugin running, the file readable and that version.
async fn check(state: &AppState, token: &str) -> Result<(Claims, User)> {
    let claims: Claims = state.links.open(token).ok_or_else(gone)?;
    if claims.x < Utc::now().timestamp() {
        return Err(gone());
    }
    let session = state.auth.session_by_id(claims.s).await?.ok_or_else(gone)?;
    if state.plugins.get(&claims.a).is_none() {
        return Err(gone());
    }
    let file = state.links.file(Some(&session.user), &claims.a, &claims.t).await.map_err(|_| gone())?;
    if file.etag.as_deref().unwrap_or_default() != claims.e {
        return Err(gone());
    }
    Ok((claims, session.user))
}

/// Headers of every answer to a link. The page that uses it is the app's,
/// in a unique origin: hls.js fetches it from there, without a cookie, and
/// there is nothing to protect from it but the file, which it may read.
fn for_pages(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}

/// `GET /api/v1/app-links/<token>`: the file of a link.
async fn follow(State(state): State<AppState>, Path(token): Path<String>, request: HeaderMap) -> Result<Response> {
    let (claims, user) = check(&state, &token).await?;
    if claims.m.is_some() {
        return Err(gone());
    }
    Ok(for_pages(state.links.send(Some(&user), &claims.a, &claims.t, &request).await?))
}

/// `POST /api/v1/app-links/<token>`: the file of an upload link, as the
/// body; answers with the new file.
async fn upload(State(state): State<AppState>, Path(token): Path<String>, body: Body) -> Result<Response> {
    if let Some(claims) = state.links.open::<ReplaceClaims>(&token) {
        return replace(state, claims, body).await;
    }
    let claims: UploadClaims = state.links.open(&token).ok_or_else(gone)?;
    if claims.x < Utc::now().timestamp() {
        return Err(gone());
    }
    let session = state.auth.session_by_id(claims.s).await?.ok_or_else(gone)?;
    if state.plugins.get(&claims.a).is_none() || !state.links.spend(claims.r, claims.x) {
        return Err(gone());
    }
    let stream = Box::pin(body.into_data_stream().map_err(io::Error::other));
    let uploaded = state.files.app_upload(&session.user, &claims.a, claims.f, &claims.n, stream, claims.o).await;
    match uploaded {
        Ok(file) => Ok(for_pages((StatusCode::CREATED, Json(file)).into_response())),
        Err(err) => {
            state.links.give_back(claims.r);
            Err(err)
        }
    }
}

/// The upload of a link to replace a file: answers with the file.
async fn replace(state: AppState, claims: ReplaceClaims, body: Body) -> Result<Response> {
    if claims.x < Utc::now().timestamp() {
        return Err(gone());
    }
    let session = state.auth.session_by_id(claims.s).await?.ok_or_else(gone)?;
    if state.plugins.get(&claims.a).is_none() || !state.links.spend(claims.r, claims.x) {
        return Err(gone());
    }
    let stream = Box::pin(body.into_data_stream().map_err(io::Error::other));
    let user = &session.user;
    match state.files.app_replace(user, &claims.a, claims.i, stream, claims.m.as_deref()).await {
        Ok(file) => Ok(for_pages(Json(file).into_response())),
        Err(err) => {
            state.links.give_back(claims.r);
            Err(err)
        }
    }
}

/// `GET /api/v1/app-links/<token>/index.m3u8 | init.mp4 | <n>.m4s`: a video's stream.
async fn follow_stream(State(state): State<AppState>, Path((token, name)): Path<(String, String)>) -> Result<Response> {
    let (claims, user) = check(&state, &token).await?;
    let mode = claims.m.ok_or_else(gone)?;
    if !state.plugins.config(&claims.a).uses.iter().any(|tool| tool == "ffmpeg") {
        return Err(gone());
    }
    let links = &state.links;
    let pieces = links.pieces(&user, &claims, mode).await?;
    let (kind, body) = match name.as_str() {
        "index.m3u8" => ("application/vnd.apple.mpegurl", playlist(&pieces).into_bytes()),
        "init.mp4" => ("video/mp4", links.piece(&user, &claims, mode, pieces[0]).await?.0.to_vec()),
        name => {
            let index: usize = name.strip_suffix(".m4s").and_then(|n| n.parse().ok()).ok_or_else(gone)?;
            let piece = *pieces.get(index).ok_or_else(gone)?;
            ("video/iso.segment", links.piece(&user, &claims, mode, piece).await?.1)
        }
    };
    // A link names one version of the file: what it gives never changes.
    let headers = [(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "private, max-age=86400")];
    Ok(for_pages((headers, body).into_response()))
}

/// The HLS playlist of a video's pieces.
fn playlist(pieces: &[Piece]) -> String {
    let longest = pieces.iter().map(|p| p.duration_ms).max().unwrap_or(0).div_ceil(1000);
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{longest}\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init.mp4\"\n"
    );
    for (index, piece) in pieces.iter().enumerate() {
        let _ = write!(text, "#EXTINF:{}.{:03},\n{index}.m4s\n", piece.duration_ms / 1000, piece.duration_ms % 1000);
    }
    text.push_str("#EXT-X-ENDLIST\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2: key "Jefe".
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn a_playlist_lists_the_pieces() {
        let piece = |start_ms, duration_ms| Piece { start_ms, duration_ms, until_ms: None };
        let pieces = [piece(0, 6000), piece(6000, 2500)];
        assert_eq!(
            playlist(&pieces),
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n\
             #EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init.mp4\"\n\
             #EXTINF:6.000,\n0.m4s\n#EXTINF:2.500,\n1.m4s\n#EXT-X-ENDLIST\n"
        );
    }

    #[test]
    fn upload_tokens_and_file_tokens_never_pass_for_each_other() {
        let (s, a) = (Uuid::new_v4(), "photos".to_owned());
        let file = Claims { s, a: a.clone(), t: Target::File(Uuid::new_v4()), e: "etag".into(), m: None, x: 0 };
        let upload = UploadClaims { s, a, f: Uuid::new_v4(), n: "a.jpg".into(), o: true, x: 0, r: 7 };
        let file = serde_json::to_vec(&file).unwrap();
        let upload = serde_json::to_vec(&upload).unwrap();
        assert!(serde_json::from_slice::<UploadClaims>(&file).is_err());
        assert!(serde_json::from_slice::<Claims>(&upload).is_err());
    }

    #[test]
    fn apps_name_what_to_send() {
        let id = Uuid::new_v4();
        assert_eq!(Target::from_header(&format!("file:{id}")).unwrap(), Target::File(id));
        assert_eq!(Target::from_header("appdata:thumbs/a.webp").unwrap(), Target::AppData("thumbs/a.webp".into()));
        assert_eq!(Target::from_header("storage:models/m.bin").unwrap(), Target::Storage("models/m.bin".into()));
        assert!(Target::from_header("/etc/passwd").is_err());
        assert!(Target::from_header("file:not-an-id").is_err());
    }
}
