//! `/.well-known/oauth-authorization-server` (RFC 8414): where OAuth
//! libraries of client apps find the endpoints for logging in a device
//! (see `rustcloud_auth`'s `devices` module).

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use rustcloud_core::ClientInfo;
use serde_json::{json, Value};

use crate::oidc::base_url;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/.well-known/oauth-authorization-server", get(metadata))
}

async fn metadata(State(state): State<AppState>, client: ClientInfo, headers: HeaderMap) -> Json<Value> {
    let base = base_url(&state.server, client, &headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/api/v1/auth/authorize"),
        "token_endpoint": format!("{base}/api/v1/auth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}
