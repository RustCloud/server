//! HTTP side of the auth module: request extractors and the `/api/v1/auth`
//! and `/api/v1/admin` routes.
//!
//! Other modules get the caller by adding [`CurrentUser`] (or [`AdminUser`])
//! to a handler's arguments; they never deal with tokens or cookies.
//!
//! A client proves its session in one of two ways:
//! - browsers: the HttpOnly cookie [`SESSION_COOKIE`] set at login;
//! - API clients: `Authorization: Bearer <token>`, the token from the reply
//!   to login. Web pages don't get it there: a script on the page (say,
//!   through an XSS hole) could otherwise take the session elsewhere.
//!
//! Cookie-authenticated requests that change something must also send the
//! header `X-Requested-With` (any value). Browsers do not let other sites add
//! custom headers to cross-site requests without CORS permission, so this,
//! together with `SameSite=Lax`, blocks CSRF.

use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_core::{ClientInfo, Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::PasswordProvider;
use crate::service::{AccountUpdate, Auth, NewAccount, NewSession, ProfileUpdate, SessionUser, User};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "rustcloud_session";
/// Header required on cookie-authenticated requests that change state.
pub const CSRF_HEADER: &str = "x-requested-with";

/// The logged-in caller. Rejects the request with 401 if there is no valid session.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub SessionUser);

impl CurrentUser {
    pub fn user(&self) -> &User {
        &self.0.user
    }

    pub fn id(&self) -> Uuid {
        self.0.user.id
    }
}

impl<S> FromRequestParts<S> for CurrentUser
where
    Arc<Auth>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let (token, from_cookie) = session_token(parts).ok_or_else(|| Error::unauthorized("Not logged in"))?;
        if from_cookie && !parts.method.is_safe() && !parts.headers.contains_key(CSRF_HEADER) {
            return Err(Error::forbidden("Missing X-Requested-With header").with_code("CSRF_CHECK_FAILED"));
        }

        let auth = Arc::<Auth>::from_ref(state);
        let session = auth
            .session_user(&token)
            .await?
            .ok_or_else(|| Error::unauthorized("Session has expired, please log in again"))?;
        // Activity log entries of this request are attributed to this user.
        tracing::Span::current().record("user", session.user.username.as_str());
        Ok(Self(session))
    }
}

/// A logged-in administrator. Rejects others with 403.
#[derive(Debug, Clone)]
pub struct AdminUser(pub CurrentUser);

impl<S> FromRequestParts<S> for AdminUser
where
    Arc<Auth>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let current = CurrentUser::from_request_parts(parts, state).await?;
        if !current.user().is_admin {
            return Err(Error::forbidden("Administrator rights required"));
        }
        Ok(Self(current))
    }
}

/// The session token of a request and whether it came from the cookie.
fn session_token(parts: &Parts) -> Option<(String, bool)> {
    let bearer = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if let Some(token) = bearer {
        return Some((token.trim().to_owned(), false));
    }
    let jar = CookieJar::from_headers(&parts.headers);
    jar.get(SESSION_COOKIE).map(|cookie| (cookie.value().to_owned(), true))
}

/// Whether a request comes from a script or form of a web page, as opposed
/// to a program (curl, a CalDAV client) or the address bar: browsers send
/// `Origin` with such requests, and `Sec-Fetch-Site` other than `none`.
pub fn from_a_web_page(headers: &HeaderMap) -> bool {
    let site = headers.get("sec-fetch-site").and_then(|value| value.to_str().ok());
    headers.contains_key(header::ORIGIN) || site.is_some_and(|site| site != "none")
}

/// The username and password of `Authorization: Basic …`, if the request has one.
/// Programs that can't keep a session (CalDAV clients) log in this way, with an
/// app password.
pub fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let encoded = headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Basic ")?;
    let decoded = String::from_utf8(STANDARD.decode(encoded.trim()).ok()?).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_owned(), password.to_owned()))
}

/// Routes of the auth module.
pub fn router<S>() -> Router<S>
where
    Arc<Auth>: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/v1/auth/status", get(status))
        .route("/api/v1/auth/register", post(register))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/logout-others", post(logout_others))
        .route("/api/v1/auth/me", get(me).patch(update_me))
        .route("/api/v1/auth/password", post(change_password))
        .route("/api/v1/admin/users", get(list_users).post(create_user))
        .route("/api/v1/admin/users/{id}", patch(update_user))
}

async fn status(State(auth): State<Arc<Auth>>) -> Result<impl IntoResponse> {
    Ok(Json(auth.status().await?))
}

async fn register(
    State(auth): State<Arc<Auth>>,
    client: ClientInfo,
    headers: HeaderMap,
    Json(account): Json<NewAccount>,
) -> Result<impl IntoResponse> {
    let password = account.password.clone();
    let user = auth.register(account).await?;
    let session = auth.login(PasswordProvider::NAME, &user.username, &password, client.ip).await?;
    Ok((StatusCode::CREATED, start_session(session, client, &headers)))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    /// Login method; password login when omitted.
    provider: Option<String>,
}

async fn login(
    State(auth): State<Arc<Auth>>,
    client: ClientInfo,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse> {
    let provider = request.provider.as_deref().unwrap_or(PasswordProvider::NAME);
    let session = auth.login(provider, &request.username, &request.password, client.ip).await?;
    Ok(start_session(session, client, &headers))
}

/// The reply to login and registration.
#[derive(Serialize)]
struct SessionReply {
    /// For API clients only; web pages use the cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    expires_at: DateTime<Utc>,
    user: User,
}

/// Sets the session cookie and returns the session (with the token, for API
/// clients that aren't web pages).
fn start_session(session: NewSession, client: ClientInfo, headers: &HeaderMap) -> impl IntoResponse {
    let max_age = (session.expires_at - Utc::now()).num_seconds();
    let cookie = Cookie::build((SESSION_COOKIE, session.token.clone()))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(client.https)
        .path("/")
        .max_age(time::Duration::seconds(max_age));
    let token = (!from_a_web_page(headers)).then_some(session.token);
    (CookieJar::new().add(cookie), Json(SessionReply { token, expires_at: session.expires_at, user: session.user }))
}

async fn logout(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<impl IntoResponse> {
    auth.logout(current.0.session_id).await?;
    // An expired cookie with the same name and path makes the browser drop it.
    let mut expired = Cookie::build((SESSION_COOKIE, "")).path("/").build();
    expired.make_removal();
    let jar = CookieJar::new().add(expired);
    Ok((jar, StatusCode::NO_CONTENT))
}

/// Ends all sessions of the caller except this one.
async fn logout_others(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<LoggedOut>> {
    Ok(Json(LoggedOut { ended: auth.logout_others(&current.0).await? }))
}

#[derive(Serialize)]
struct LoggedOut {
    /// Sessions ended.
    ended: u64,
}

async fn me(current: CurrentUser) -> Json<User> {
    Json(current.0.user)
}

async fn update_me(
    State(auth): State<Arc<Auth>>,
    current: CurrentUser,
    Json(update): Json<ProfileUpdate>,
) -> Result<Json<User>> {
    Ok(Json(auth.update_profile(current.id(), update).await?))
}

#[derive(Deserialize)]
struct PasswordChange {
    current_password: String,
    new_password: String,
    /// Also revoke all app passwords (phones, calendars sign in again).
    #[serde(default)]
    revoke_app_passwords: bool,
}

async fn change_password(
    State(auth): State<Arc<Auth>>,
    current: CurrentUser,
    Json(change): Json<PasswordChange>,
) -> Result<StatusCode> {
    auth.change_password(&current.0, &change.current_password, &change.new_password, change.revoke_app_passwords)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_users(State(auth): State<Arc<Auth>>, _admin: AdminUser) -> Result<Json<Vec<User>>> {
    Ok(Json(auth.list_users().await?))
}

#[derive(Deserialize)]
struct CreateUserRequest {
    #[serde(flatten)]
    account: NewAccount,
    #[serde(default)]
    is_admin: bool,
}

async fn create_user(
    State(auth): State<Arc<Auth>>,
    _admin: AdminUser,
    Json(request): Json<CreateUserRequest>,
) -> Result<impl IntoResponse> {
    let user = auth.create_user(request.account, request.is_admin).await?;
    Ok((StatusCode::CREATED, Json(user)))
}

async fn update_user(
    State(auth): State<Arc<Auth>>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(update): Json<AccountUpdate>,
) -> Result<Json<User>> {
    Ok(Json(auth.update_account(admin.user(), user_id, update).await?))
}
