//! Routes of the files module: `/api/v1/files`, `/api/v1/shares`, `/api/v1/usage`
//! and the public `/api/v1/public/{token}`.

use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use rustcloud_auth::{AdminUser, Auth, CurrentUser};
use rustcloud_core::{ByteStream, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::content;
use crate::service::{Files, Node, NodeUpdate, Usage};
use crate::shares::{PublicView, Share};
use crate::storage::{SpeedTestResult, StorageReport};
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
        .route("/api/v1/files/trash", get(trash).delete(empty_trash))
        .route("/api/v1/files/{id}", get(details).patch(update).delete(remove))
        .route("/api/v1/files/{id}/children", get(children))
        .route("/api/v1/files/{id}/folders", post(create_folder))
        .route("/api/v1/files/{id}/upload", post(upload))
        .route("/api/v1/files/{id}/content", get(download).put(replace_content))
        .route("/api/v1/files/{id}/copy", post(copy))
        .route("/api/v1/files/{id}/restore", post(restore))
        .route("/api/v1/files/{id}/lock", delete(unlock))
        .route("/api/v1/files/{id}/versions", get(versions))
        .route("/api/v1/files/{id}/versions/{version}/content", get(download_version))
        .route("/api/v1/files/{id}/versions/{version}/restore", post(restore_version))
        .route("/api/v1/usage", get(usage))
        .route("/api/v1/shares", get(list_shares).post(create_share))
        .route("/api/v1/shares/{id}", delete(delete_share))
        .route("/api/v1/public/{token}", get(public_view))
        .route("/api/v1/public/{token}/content", get(public_download))
        .route("/api/v1/admin/storage", get(storage))
        .route("/api/v1/admin/storage/speed-test", post(speed_test))
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
}

/// Uploads a file: the request body is the raw file content.
async fn upload(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(parent_id): Path<Uuid>,
    Query(params): Query<UploadParams>,
    body: Body,
) -> Result<impl IntoResponse> {
    let node = files.upload(current.user(), parent_id, &params.name, byte_stream(body), params.overwrite).await?;
    Ok((StatusCode::CREATED, Json(node)))
}

/// Replaces the content; with `If-Match: "<etag>"`, only if the file still has it.
async fn replace_content(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Node>> {
    let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()).map(|v| v.trim().trim_matches('"'));
    Ok(Json(files.replace_content(current.user(), id, byte_stream(body), if_match).await?))
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
    content::serve(node, &headers, params.download.is_some(), |range| files.download(user, id, range)).await
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

async fn usage(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Usage>> {
    Ok(Json(files.usage(current.user()).await?))
}

async fn list_shares(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Vec<Share>>> {
    Ok(Json(files.shares(current.user()).await?))
}

#[derive(Deserialize)]
struct NewShare {
    node_id: Uuid,
    expires_at: Option<DateTime<Utc>>,
}

async fn create_share(
    State(files): State<Arc<Files>>,
    current: CurrentUser,
    Json(share): Json<NewShare>,
) -> Result<impl IntoResponse> {
    let share = files.create_share(current.user(), share.node_id, share.expires_at).await?;
    Ok((StatusCode::CREATED, Json(share)))
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
) -> Result<Json<PublicView>> {
    Ok(Json(files.public_view(&token, params.node).await?))
}

async fn public_download(
    State(files): State<Arc<Files>>,
    Path(token): Path<String>,
    Query(params): Query<PublicParams>,
    headers: HeaderMap,
) -> Result<Response> {
    let node = files.public_file(&token, params.node).await?;
    content::serve(node, &headers, params.download.is_some(), |range| files.public_download(&token, params.node, range))
        .await
}

/// The request body as a stream of byte chunks.
fn byte_stream(body: Body) -> ByteStream {
    Box::pin(body.into_data_stream().map_err(io::Error::other))
}
