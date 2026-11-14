//! Routes of the files module: `/api/v1/files`, `/api/v1/shares`, `/api/v1/usage`
//! and the public `/api/v1/public/{token}`.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use rustcloud_auth::{AdminUser, Auth, CurrentUser};
use rustcloud_core::{ByteStream, ClientInfo, Error, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use uuid::Uuid;

use crate::archive::ZipDownload;
use crate::changes::{ChangePage, ChangeWatch, HINT_EVERY, WATCH_TIME};
use crate::content;
use crate::service::{Files, Node, NodeUpdate, Usage};
use crate::shares::{PublicView, Share, ShareOptions, LINK_COOKIE};
use crate::sharing::{Access, NewAccess, Received};
use crate::storage::{SpeedTestResult, StorageReport};
use crate::uploads::{NewUpload, Upload};
use crate::versions::Versions;

/// Routes of the files module.
pub fn router<S>() -> Router<S>
where
    Arc<Files>: FromRef<S>,
    Arc<Auth>: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/v1/files/root", get(root))
        .route("/api/v1/files/search", get(search))
        .route("/api/v1/files/changes", get(changes))
        .route("/api/v1/files/changes/watch", get(watch_changes))
        .route("/api/v1/files/trash", get(trash).delete(empty_trash))
        .route("/api/v1/files/shared", get(shared_with_me))
        .route("/api/v1/files/{id}/access", get(node_access).post(grant_access))
        .route("/api/v1/access/{id}", patch(change_access).delete(remove_access))
        .route("/api/v1/files/{id}", get(details).patch(update).delete(remove))
        .route("/api/v1/files/{id}/children", get(children))
        .route("/api/v1/files/{id}/folders", post(create_folder))
        .route("/api/v1/files/{id}/upload", post(upload))
        .route("/api/v1/files/{id}/uploads", post(start_upload))
        .route("/api/v1/files/uploads", get(uploads))
        .route("/api/v1/files/uploads/{upload}", get(upload_info).put(upload_part).delete(cancel_upload))
        .route("/api/v1/files/uploads/{upload}/finish", post(finish_upload))
        .route("/api/v1/files/{id}/content", get(download).put(replace_content))
        .route("/api/v1/files/{id}/zip", get(download_zip))
        .route("/api/v1/files/{id}/copy", post(copy))
        .route("/api/v1/files/{id}/restore", post(restore))
        .route("/api/v1/files/{id}/lock", delete(unlock))
        .route("/api/v1/files/{id}/versions", get(versions))
        .route("/api/v1/files/{id}/versions/{version}/content", get(download_version))
        .route("/api/v1/files/{id}/versions/{version}/restore", post(restore_version))
        .route("/api/v1/usage", get(usage))
        .route("/api/v1/shares", get(list_shares).post(create_share))
        .route("/api/v1/shares/{id}", delete(delete_share).patch(update_share))
        .route("/api/v1/public/{token}", get(public_view))
        .route("/api/v1/public/{token}/content", get(public_download))
        .route("/api/v1/public/{token}/zip", get(public_zip))
        .route("/api/v1/public/{token}/unlock", post(unlock_share))
        .route("/api/v1/public/{token}/upload", post(public_upload))
        .route("/api/v1/admin/storage", get(storage))
        .route("/api/v1/admin/storage/speed-test", post(speed_test))
        .merge(crate::dav::router())
}

async fn storage(State(files): State<Arc<Files>>, _admin: AdminUser) -> Result<Json<StorageReport>> {
    Ok(Json(files.storage_report().await?))
}

async fn speed_test(State(files): State<Arc<Files>>, _admin: AdminUser) -> Result<Json<SpeedTestResult>> {
    Ok(Json(files.speed_test().await?))
}

async fn root(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Node>> {
    Ok(Json(files.root(current.user()).await?))
}

/// A node together with the folders above it (for breadcrumbs).
#[derive(Serialize)]
struct NodeDetails {
    node: Node,
    path: Vec<Node>,
}

async fn details(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    let user = current.user();
    let node = files.get(user, id).await?;
    let path = files.ancestors(user, id).await?;
    Ok(Json(NodeDetails { node, path }))
}

async fn children(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Node>>> {
    Ok(Json(files.children(current.user(), id).await?))
}

#[derive(Deserialize)]
struct NewFolder {
    name: String,
}

async fn create_folder(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(parent_id): Path<Uuid>,
    Json(folder): Json<NewFolder>,
) -> Result<impl IntoResponse> {
    let node = files.create_folder(current.user(), parent_id, &folder.name).await?;
    Ok((StatusCode::CREATED, Json(node)))
}

#[derive(Deserialize)]
struct UploadParams {
    name: String,
    #[serde(default)]
    overwrite: bool,
    /// See [`parse_mtime`].
    mtime: Option<String>,
}

/// Uploads a file: the request body is the raw file content.
async fn upload(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(parent_id): Path<Uuid>,
    Query(params): Query<UploadParams>,
    body: Body,
) -> Result<impl IntoResponse> {
    let mtime = params.mtime.as_deref().map(parse_mtime).transpose()?;
    let body = byte_stream(body);
    let node = files.upload(current.user(), parent_id, &params.name, body, params.overwrite, mtime).await?;
    Ok((StatusCode::CREATED, Json(node)))
}

#[derive(Deserialize)]
struct StartUpload {
    #[serde(flatten)]
    upload: NewUpload,
    /// See [`parse_mtime`].
    mtime: Option<String>,
}

async fn start_upload(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(params): Json<StartUpload>,
) -> Result<impl IntoResponse> {
    let mtime = params.mtime.as_deref().map(parse_mtime).transpose()?;
    let upload = NewUpload { mtime, ..params.upload };
    Ok((StatusCode::CREATED, Json(files.start_upload(current.user(), folder_id, upload).await?)))
}

async fn uploads(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Vec<Upload>>> {
    Ok(Json(files.uploads(current.user()).await?))
}

async fn upload_info(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Upload>> {
    Ok(Json(files.upload_info(current.user(), id).await?))
}

#[derive(Deserialize)]
struct PartParams {
    offset: i64,
}

async fn upload_part(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Query(params): Query<PartParams>,
    body: Body,
) -> Result<Json<Upload>> {
    Ok(Json(files.upload_part(current.user(), id, params.offset, byte_stream(body)).await?))
}

#[derive(Deserialize)]
struct FinishUpload {
    sha256: String,
}

async fn finish_upload(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(params): Json<FinishUpload>,
) -> Result<impl IntoResponse> {
    Ok((StatusCode::CREATED, Json(files.finish_upload(current.user(), id, &params.sha256).await?)))
}

async fn cancel_upload(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    files.cancel_upload(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ContentParams {
    /// See [`parse_mtime`].
    mtime: Option<String>,
}

/// Replaces the content; with `If-Match: "<etag>"`, only if the file still has it.
async fn replace_content(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Query(params): Query<ContentParams>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Node>> {
    let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()).map(|v| v.trim().trim_matches('"'));
    let mtime = params.mtime.as_deref().map(parse_mtime).transpose()?;
    Ok(Json(files.replace_content(current.user(), id, byte_stream(body), if_match, mtime).await?))
}

/// The mtime of an uploaded file: Unix time in seconds (`1730985600`,
/// `1730985600.25`) or RFC 3339 (`2024-11-07T13:20:00Z`).
pub(crate) fn parse_mtime(text: &str) -> Result<DateTime<Utc>> {
    let invalid = || Error::bad_request("INVALID_MTIME", "mtime must be Unix seconds or an RFC 3339 time");
    let text = text.trim();
    if let Ok(seconds) = text.parse::<f64>() {
        if !seconds.is_finite() {
            return Err(invalid());
        }
        let micros = (seconds * 1e6).round();
        // Beyond ±292 000 years `as` would saturate; check_mtime refuses those anyway.
        return DateTime::from_timestamp_micros(micros as i64).ok_or_else(invalid);
    }
    DateTime::parse_from_rfc3339(text).map(|time| time.with_timezone(&Utc)).map_err(|_| invalid())
}

async fn unlock(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    files.unlock(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DownloadParams {
    /// `?download=1` forces "save as" instead of showing the file.
    #[serde(default)]
    download: Option<String>,
}

async fn download(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Query(params): Query<DownloadParams>,
    headers: HeaderMap,
) -> Result<Response> {
    let user = current.user();
    let node = files.get(user, id).await?;
    // A PDF is shown by the browser without a sandbox, on our address: one
    // someone else wrote (shared with the user, or put into their folder) is
    // only downloaded, as through public links.
    let is_pdf = node.mime_type.as_deref() == Some("application/pdf");
    let attachment = params.download.is_some() || (is_pdf && files.from_someone_else(user, id).await?);
    content::serve(node, &headers, attachment, |range| files.download(user, id, range)).await
}

async fn versions(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Versions>> {
    Ok(Json(files.versions(current.user(), id).await?))
}

async fn download_version(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path((id, version)): Path<(Uuid, Uuid)>,
    Query(params): Query<DownloadParams>,
    headers: HeaderMap,
) -> Result<Response> {
    let user = current.user();
    let node = files.version_node(user, id, version).await?;
    content::serve(node, &headers, params.download.is_some(), |range| files.download_version(user, id, version, range))
        .await
}

async fn restore_version(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path((id, version)): Path<(Uuid, Uuid)>,
) -> Result<Json<Node>> {
    Ok(Json(files.restore_version(current.user(), id, version).await?))
}

async fn update(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(update): Json<NodeUpdate>,
) -> Result<Json<Node>> {
    Ok(Json(files.update(current.user(), id, update).await?))
}

#[derive(Deserialize)]
struct CopyRequest {
    parent_id: Uuid,
    name: Option<String>,
}

async fn copy(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(request): Json<CopyRequest>,
) -> Result<impl IntoResponse> {
    let node = files.copy(current.user(), id, request.parent_id, request.name.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(node)))
}

async fn remove(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    files.delete(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<Json<Node>> {
    Ok(Json(files.restore(current.user(), id).await?))
}

async fn trash(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Node>> {
    Ok(Json(files.trash(current.user()).await?))
}

async fn empty_trash(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<StatusCode> {
    files.empty_trash(current.user()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
}

async fn search(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<Node>>> {
    Ok(Json(files.search(current.user(), &params.q).await?))
}

#[derive(Deserialize)]
struct ChangesParams {
    cursor: Option<String>,
}

async fn changes(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Query(params): Query<ChangesParams>,
) -> Result<Json<ChangePage>> {
    Ok(Json(files.changes(current.user(), params.cursor.as_deref()).await?))
}

/// Server-sent events: `changes` whenever the user's feed has new entries
/// (at most one per [`HINT_EVERY`]); the connection ends after
/// [`WATCH_TIME`]. The client reads the feed once connected and after each event.
async fn watch_changes(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<impl IntoResponse> {
    let watch = files.watch_changes(current.user())?;
    let deadline = tokio::time::Instant::now() + WATCH_TIME;
    let events = futures_util::stream::unfold(watch, move |mut watch| async move {
        let event = tokio::time::timeout_at(deadline, next_hint(&mut watch)).await.ok().flatten()?;
        Some((Ok::<_, Infallible>(event), watch))
    });
    // Proxies such as nginx would otherwise hold the events back.
    let headers = [("x-accel-buffering", "no"), (header::CACHE_CONTROL.as_str(), "no-store")];
    Ok((headers, Sse::new(events).keep_alive(KeepAlive::default())))
}

/// Waits for a hint for the watching user, lets those that follow within
/// [`HINT_EVERY`] join it, and makes it an event; `None` once the server stops.
async fn next_hint(watch: &mut ChangeWatch) -> Option<Event> {
    loop {
        match watch.hints.recv().await {
            Ok(user_id) if user_id != watch.user_id => continue,
            Ok(_) | Err(RecvError::Lagged(_)) => break,
            Err(RecvError::Closed) => return None,
        }
    }
    tokio::time::sleep(HINT_EVERY).await;
    while !matches!(watch.hints.try_recv(), Err(TryRecvError::Empty | TryRecvError::Closed)) {}
    Some(Event::default().event("changes").data("new"))
}

async fn usage(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Usage>> {
    Ok(Json(files.usage(current.user()).await?))
}

async fn shared_with_me(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Vec<Received>>> {
    Ok(Json(files.shared_with_me(current.user()).await?))
}

async fn node_access(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Access>>> {
    Ok(Json(files.node_access(current.user(), id).await?))
}

async fn grant_access(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(new): Json<NewAccess>,
) -> Result<(StatusCode, Json<Access>)> {
    Ok((StatusCode::CREATED, Json(files.grant_access(current.user(), id, new).await?)))
}

#[derive(Deserialize)]
struct AccessChange {
    write: bool,
}

async fn change_access(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(change): Json<AccessChange>,
) -> Result<Json<Access>> {
    Ok(Json(files.change_access(current.user(), id, change.write).await?))
}

async fn remove_access(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    files.remove_access(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_shares(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Vec<Share>>> {
    Ok(Json(files.shares(current.user()).await?))
}

#[derive(Deserialize)]
struct NewShare {
    node_id: Uuid,
    #[serde(flatten)]
    options: ShareOptions,
}

async fn create_share(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Json(share): Json<NewShare>,
) -> Result<impl IntoResponse> {
    let share = files.create_share(current.user(), share.node_id, share.options).await?;
    Ok((StatusCode::CREATED, Json(share)))
}

async fn update_share(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Json(options): Json<ShareOptions>,
) -> Result<Json<Share>> {
    Ok(Json(files.update_share(current.user(), id, options).await?))
}

async fn delete_share(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    files.delete_share(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PublicParams {
    /// A node inside a shared folder; the shared node itself when omitted.
    node: Option<Uuid>,
    #[serde(default)]
    download: Option<String>,
}

async fn public_view(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    Query(params): Query<PublicParams>,
    headers: HeaderMap,
) -> Result<Json<PublicView>> {
    Ok(Json(files.public_view(&token, link_key(&headers).as_deref(), params.node).await?))
}

#[derive(Deserialize)]
struct Unlock {
    password: String,
}

/// Checks a link's password; the answer sets the cookie that opens the
/// link, for its own path only.
async fn unlock_share(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    client: ClientInfo,
    Json(unlock): Json<Unlock>,
) -> Result<Response> {
    let key = files.unlock_share(&token, &unlock.password).await?;
    let secure = if client.https { "; Secure" } else { "" };
    let cookie = format!(
        "{LINK_COOKIE}={key}; Path=/api/v1/public/{token}; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
        LINK_COOKIE_DAYS * 24 * 3600
    );
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response())
}

#[derive(Deserialize)]
struct PublicUploadParams {
    name: String,
    /// The folder inside the shared one; the shared one when omitted.
    node: Option<Uuid>,
}

async fn public_upload(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    Query(params): Query<PublicUploadParams>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse> {
    let key = link_key(&headers);
    let node = files.public_upload(&token, key.as_deref(), params.node, &params.name, byte_stream(body)).await?;
    Ok((StatusCode::CREATED, Json(node)))
}

/// The key of an unlocked link, from its cookie.
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

async fn public_download(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    Query(params): Query<PublicParams>,
    headers: HeaderMap,
) -> Result<Response> {
    let key = link_key(&headers);
    let key = key.as_deref();
    let node = files.public_file(&token, key, params.node).await?;
    // A PDF is shown by the browser without a sandbox, on our address: one
    // from a stranger (anyone can make a public link) is only downloaded.
    let is_pdf = node.mime_type.as_deref() == Some("application/pdf");
    let attachment = params.download.is_some() || is_pdf;
    content::serve(node, &headers, attachment, |range| files.public_download(&token, key, params.node, range)).await
}

async fn download_zip(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<Response> {
    Ok(zip_response(files.download_zip(current.user(), id).await?))
}

async fn public_zip(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    Query(params): Query<PublicParams>,
    headers: HeaderMap,
) -> Result<Response> {
    Ok(zip_response(files.public_zip(&token, link_key(&headers).as_deref(), params.node).await?))
}

/// How long a browser keeps the key of an unlocked link.
const LINK_COOKIE_DAYS: u64 = 30;

/// A zip archive as it is written; its size is not known before.
fn zip_response(zip: ZipDownload) -> Response {
    let headers = [
        (header::CONTENT_TYPE, "application/zip".to_owned()),
        (header::CONTENT_DISPOSITION, content::content_disposition("attachment", &zip.name)),
        (header::CACHE_CONTROL, "no-store".to_owned()),
    ];
    (headers, Body::from_stream(zip.body)).into_response()
}

/// The request body as a stream of byte chunks.
pub(crate) fn byte_stream(body: Body) -> ByteStream {
    Box::pin(body.into_data_stream().map_err(io::Error::other))
}

#[cfg(test)]
mod tests {
    use super::parse_mtime;

    #[test]
    fn mtimes_are_unix_seconds_or_rfc3339() {
        let at = |text| parse_mtime(text).map(|time| time.to_rfc3339()).ok();
        assert_eq!(at("1730985600").as_deref(), Some("2024-11-07T13:20:00+00:00"));
        assert_eq!(at("1730985600.25").as_deref(), Some("2024-11-07T13:20:00.250+00:00"));
        assert_eq!(at("2024-11-07T16:20:00+03:00").as_deref(), Some("2024-11-07T13:20:00+00:00"));
        assert_eq!(at("yesterday"), None);
        assert_eq!(at("NaN"), None);
        assert_eq!(at("1e300"), None);
    }
}
