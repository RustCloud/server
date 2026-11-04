//! `/api/v1/app-shares`: folders of their apps' data users share with other
//! users (a calendar). The rules live in `rustcloud_files`
//! (see `Files::share_appdata`); here the apps of this server are checked.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use rustcloud_auth::CurrentUser;
use rustcloud_core::{Error, Result};
use rustcloud_files::{AppShare, AppShares, Files, NewAppShare};
use uuid::Uuid;

use crate::plugins::Plugins;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/app-shares", get(list).post(create)).route("/api/v1/app-shares/{id}", delete(revoke))
}

/// `GET /api/v1/app-shares`: what the caller shared and what was shared with them.
async fn list(State(files): State<Arc<Files>>, current: CurrentUser) -> Result<Json<AppShares>> {
    Ok(Json(files.appdata_shares(current.user()).await?))
}

/// `POST /api/v1/app-shares`: the caller shares a folder of an app's data.
async fn create(
    State(files): State<Arc<Files>>,
    State(plugins): State<Arc<Plugins>>,
    current: CurrentUser,
    Json(share): Json<NewAppShare>,
) -> Result<(StatusCode, Json<AppShare>)> {
    if plugins.get(&share.app_id).is_none() {
        return Err(Error::not_found("APP_NOT_FOUND", format!("There is no app \"{}\"", share.app_id)));
    }
    Ok((StatusCode::CREATED, Json(files.share_appdata(current.user(), share).await?)))
}

/// `DELETE /api/v1/app-shares/{id}`: the owner revokes it, or the other user gives it up.
async fn revoke(State(files): State<Arc<Files>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    files.revoke_appdata_share(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
