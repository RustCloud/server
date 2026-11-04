//! `/api/v1/app-passwords`: passwords for CalDAV and other clients that log in
//! with Basic authentication. The rules live in `rustcloud_auth`
//! (see `Auth::create_app_password`); here the apps of this server are checked.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use rustcloud_auth::{AppPassword, Auth, CreatedAppPassword, CurrentUser, NewAppPassword};
use rustcloud_core::{Error, Result};
use uuid::Uuid;

use crate::plugins::Plugins;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/app-passwords", get(list).post(create))
        .route("/api/v1/app-passwords/{id}", delete(revoke))
}

/// `GET /api/v1/app-passwords`: the caller's app passwords, without the passwords.
async fn list(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<Vec<AppPassword>>> {
    Ok(Json(auth.app_passwords(current.user()).await?))
}

/// `POST /api/v1/app-passwords`: a new app password; the only response that shows it.
async fn create(
    State(auth): State<Arc<Auth>>,
    State(plugins): State<Arc<Plugins>>,
    current: CurrentUser,
    Json(new): Json<NewAppPassword>,
) -> Result<(StatusCode, Json<CreatedAppPassword>)> {
    if let Some(scope) = new.scopes.iter().find(|scope| plugins.get(&scope.app).is_none()) {
        return Err(Error::not_found("APP_NOT_FOUND", format!("There is no app \"{}\"", scope.app)));
    }
    Ok((StatusCode::CREATED, Json(auth.create_app_password(current.user(), new).await?)))
}

/// `DELETE /api/v1/app-passwords/{id}`: revokes it at once.
async fn revoke(State(auth): State<Arc<Auth>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    auth.revoke_app_password(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
