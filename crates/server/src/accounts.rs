//! `DELETE /api/v1/admin/users/{id}`: an administrator deletes an account
//! with all its files, without opening them. Lives here because it takes
//! both the accounts and the files.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::delete;
use axum::Router;
use rustcloud_auth::AdminUser;
use rustcloud_core::Result;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/admin/users/{id}", delete(delete_account))
}

async fn delete_account(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<Uuid>,
) -> Result<StatusCode> {
    // The keys first: once the account is gone, so are the rows that name them.
    let blobs = state.files.account_blobs(user_id).await?;
    state.auth.delete_account(admin.user(), user_id).await?;
    state.files.remove_account_blobs(blobs).await;
    Ok(StatusCode::NO_CONTENT)
}
