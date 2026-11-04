//! `/api/v1/grants`: access users give apps to their files.
//!
//! The rules live in `rustcloud_files` (see `Files::create_grant`); here the
//! app's manifest supplies what the app is allowed to ask for.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rustcloud_auth::CurrentUser;
use rustcloud_core::{Error, Result};
use rustcloud_files::{Files, Grant, NewGrant};
use serde::Deserialize;
use uuid::Uuid;

use crate::plugin_jobs::Jobs;
use crate::plugins::Plugins;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/grants", get(list).post(create))
        .route("/api/v1/grants/{id}", delete(revoke))
        .route("/api/v1/grants/{id}/keep-alive", post(keep_alive))
}

/// `GET /api/v1/grants`: the caller's access grants in force.
async fn list(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<Vec<Grant>>> {
    Ok(Json(files.grants(current.user()).await?))
}

/// `POST /api/v1/grants`: the caller gives an app access to a file or folder.
/// Files of a folder given for good start the app's `file.granted` jobs.
async fn create(
    State(files): State<Arc<Files>>,
    State(plugins): State<Arc<Plugins>>,
    State(jobs): State<Arc<Jobs>>,
    current: CurrentUser,
    Json(grant): Json<NewGrant>,
) -> Result<(StatusCode, Json<Grant>)> {
    let app = plugins
        .get(&grant.app_id)
        .ok_or_else(|| Error::not_found("APP_NOT_FOUND", format!("There is no app \"{}\"", grant.app_id)))?;
    let permissions = app.permissions.clone();
    let grant = files.create_grant(current.user(), grant, &permissions).await?;
    jobs.on_grant(current.user().clone(), grant.clone());
    Ok((StatusCode::CREATED, Json(grant)))
}

/// `POST /api/v1/grants/{id}/keep-alive`: the file is still open in the app.
async fn keep_alive(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    files.keep_grant_alive(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RevokeParams {
    #[serde(default)]
    closing: bool,
}

/// `DELETE /api/v1/grants/{id}`: revoked by the user, or the app was closed.
/// Once the user has given the app no access at all, its background jobs
/// for the user end too.
///
/// `?closing=true`: the page with the file was closed; the session lasts a
/// minute more for a service the app works with to finish.
async fn revoke(
    State(files): State<Arc<Files>>,
    State(jobs): State<Arc<Jobs>>,
    current: CurrentUser,
    Path(id): Path<Uuid>,
    Query(params): Query<RevokeParams>,
) -> Result<StatusCode> {
    let user = current.user();
    if params.closing {
        files.close_grant(user, id).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let app_id = files.grants(user).await?.into_iter().find(|g| g.id == id).map(|g| g.app_id);
    files.revoke_grant(user, id).await?;
    if let Some(app_id) = app_id {
        if !files.grants(user).await?.iter().any(|g| g.app_id == app_id) {
            jobs.forget_user(&app_id, user.id).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
