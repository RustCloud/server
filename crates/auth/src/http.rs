//! HTTP side of the auth module: request extractors and the `/api/v1/auth`
//! and `/api/v1/admin` routes.
//!
//! Other modules get the caller by adding [`CurrentUser`] (or [`AdminUser`])
//! to a handler's arguments; they never deal with tokens or cookies.
//!
//! A client proves its session in one of three ways:
//! - browsers: the HttpOnly cookie [`SESSION_COOKIE`] set at login;
//! - client apps (devices): `Authorization: Bearer <access token>`, a JWT
//!   from `/api/v1/auth/token` (see [`crate::devices`]);
//! - scripts: `Authorization: Bearer <token>`, the token from the reply to
//!   login. Web pages don't get it there: a script on the page (say,
//!   through an XSS hole) could otherwise take the session elsewhere.
//!
//! Cookie-authenticated requests that change something must also send the
//! header `X-Requested-With` (any value). Browsers do not let other sites add
//! custom headers to cross-site requests without CORS permission, so this,
//! together with `SameSite=Lax`, blocks CSRF.

use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts, Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_core::{ClientInfo, Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::devices::{check_redirect_uri, AuthorizeRequest, Grant, SessionInfo};
use crate::external::LinkedLogin;
use crate::groups::{Group, GroupName};
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
        let ip = parts.extensions.get::<ClientInfo>().and_then(|client| client.ip);
        let session = auth
            .authenticate(&token, ip)
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
        current.0.browser_only()?;
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
        .route("/api/v1/auth/logins", get(linked_logins))
        .route("/api/v1/auth/logins/{id}", delete(unlink))
        .route("/api/v1/auth/sessions", get(sessions))
        .route("/api/v1/auth/sessions/{id}", delete(end_session))
        .route("/api/v1/auth/authorize", get(authorize_page).post(authorize))
        .route("/api/v1/auth/token", post(token))
        .route("/api/v1/admin/users", get(list_users).post(create_user))
        .route("/api/v1/admin/users/{id}", patch(update_user))
        .route("/api/v1/admin/groups", get(list_groups).post(create_group))
        .route("/api/v1/admin/groups/{id}", patch(rename_group).delete(delete_group))
        .route("/api/v1/admin/groups/{id}/members/{user_id}", put(add_member).delete(remove_member))
        .route("/api/v1/groups", get(my_groups))
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
    auth.describe_session(session.session_id, user_agent(&headers), client.ip).await;
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
    auth.describe_session(session.session_id, user_agent(&headers), client.ip).await;
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
    let cookie = session_cookie(&session, client);
    let token = (!from_a_web_page(headers)).then_some(session.token);
    (CookieJar::new().add(cookie), Json(SessionReply { token, expires_at: session.expires_at, user: session.user }))
}

/// The session cookie of a browser that just logged in.
pub fn session_cookie(session: &NewSession, client: ClientInfo) -> Cookie<'static> {
    let max_age = (session.expires_at - Utc::now()).num_seconds();
    Cookie::build((SESSION_COOKIE, session.token.clone()))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(client.https)
        .path("/")
        .max_age(time::Duration::seconds(max_age))
        .build()
}

async fn logout(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<impl IntoResponse> {
    auth.logout(current.0.session_id).await?;
    // An expired cookie with the same name and path makes the browser drop it.
    let mut expired = Cookie::build((SESSION_COOKIE, "")).path("/").build();
    expired.make_removal();
    let jar = CookieJar::new().add(expired);
    Ok((jar, StatusCode::NO_CONTENT))
}

/// The `User-Agent` of a request, to tell its session apart in the list.
pub fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::USER_AGENT).and_then(|value| value.to_str().ok())
}

/// Ends all sessions of the caller except this one.
async fn logout_others(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<LoggedOut>> {
    current.0.browser_only()?;
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
    current.0.browser_only()?;
    auth.change_password(&current.0, &change.current_password, &change.new_password, change.revoke_app_passwords)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The ways to log in to the caller's account (password, external services).
async fn linked_logins(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<Vec<LinkedLogin>>> {
    Ok(Json(auth.linked_logins(current.id()).await?))
}

async fn unlink(State(auth): State<Arc<Auth>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    current.0.browser_only()?;
    auth.unlink_identity(current.user(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The caller's sessions: browsers and devices.
async fn sessions(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<Vec<SessionInfo>>> {
    Ok(Json(auth.sessions(&current.0).await?))
}

async fn end_session(State(auth): State<Arc<Auth>>, current: CurrentUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    auth.end_session(&current.0, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/auth/authorize?…`: the authorization endpoint a client app
/// opens in the browser. The page that asks the user is the web UI's, so
/// this only checks the request and passes it on there.
async fn authorize_page(RawQuery(query): RawQuery) -> Response {
    let query = query.unwrap_or_default();
    let request: std::result::Result<AuthorizeRequest, _> = serde_urlencoded::from_str(&query);
    // A wrong redirect URI must not get anything sent to it.
    let valid = request.as_ref().is_ok_and(|r| check_redirect_uri(&r.redirect_uri).is_ok());
    if !valid {
        return Redirect::to("/#/connect?invalid=1").into_response();
    }
    Redirect::to(&format!("/#/connect?{query}")).into_response()
}

#[derive(Serialize)]
struct Code {
    code: String,
}

/// `POST /api/v1/auth/authorize`: the user allowed the device on the
/// consent page; the page sends the browser to the app with this code.
async fn authorize(
    State(auth): State<Arc<Auth>>,
    current: CurrentUser,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<Code>> {
    Ok(Json(Code { code: auth.authorize(&current.0, &request)? }))
}

/// A request to the token endpoint (`application/x-www-form-urlencoded`,
/// as OAuth 2.0 says; JSON works too).
#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    refresh_token: Option<String>,
}

/// `POST /api/v1/auth/token`: a code or a refresh token for a new pair of
/// tokens. Errors are in the OAuth 2.0 format (`{"error": "invalid_grant"}`).
async fn token(
    State(auth): State<Arc<Auth>>,
    client: ClientInfo,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    let request: std::result::Result<TokenRequest, String> = match json {
        true => serde_json::from_slice(&body).map_err(|err| err.to_string()),
        false => serde_urlencoded::from_bytes(&body).map_err(|err| err.to_string()),
    };
    let result = match request {
        Err(err) => Err(Error::bad_request("invalid_request", err)),
        Ok(request) => auth.token_request(request.into_grant(), client.ip).await,
    };
    let no_store = [(header::CACHE_CONTROL, "no-store"), (header::PRAGMA, "no-cache")];
    match result {
        Ok(tokens) => (no_store, Json(tokens)).into_response(),
        Err(err) => {
            let code = match err.code {
                "invalid_request" | "invalid_grant" | "unsupported_grant_type" => err.code,
                "TOO_MANY_ATTEMPTS" => "slow_down",
                _ if err.kind == rustcloud_core::ErrorKind::Internal => "server_error",
                _ => "invalid_grant",
            };
            let status = err.kind.status();
            // Details of the server's own failures stay in its log.
            let description = if code == "server_error" { "Internal error" } else { err.message.as_str() };
            let body = serde_json::json!({"error": code, "error_description": description});
            (status, no_store, Json(body)).into_response()
        }
    }
}

impl TokenRequest {
    fn into_grant(self) -> Grant {
        match self.grant_type.as_str() {
            "authorization_code" => Grant::Code {
                code: self.code.unwrap_or_default(),
                verifier: self.code_verifier.unwrap_or_default(),
                redirect_uri: self.redirect_uri.unwrap_or_default(),
            },
            "refresh_token" => Grant::Refresh(self.refresh_token.unwrap_or_default()),
            other => Grant::Unsupported(other.to_owned()),
        }
    }
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

async fn list_groups(State(auth): State<Arc<Auth>>, _admin: AdminUser) -> Result<Json<Vec<Group>>> {
    Ok(Json(auth.groups().await?))
}

#[derive(Deserialize)]
struct GroupForm {
    name: String,
}

async fn create_group(
    State(auth): State<Arc<Auth>>,
    _admin: AdminUser,
    Json(form): Json<GroupForm>,
) -> Result<(StatusCode, Json<Group>)> {
    Ok((StatusCode::CREATED, Json(auth.create_group(&form.name).await?)))
}

async fn rename_group(
    State(auth): State<Arc<Auth>>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
    Json(form): Json<GroupForm>,
) -> Result<StatusCode> {
    auth.rename_group(id, &form.name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_group(State(auth): State<Arc<Auth>>, _admin: AdminUser, Path(id): Path<Uuid>) -> Result<StatusCode> {
    auth.delete_group(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_member(
    State(auth): State<Arc<Auth>>,
    _admin: AdminUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    auth.add_member(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(auth): State<Arc<Auth>>,
    _admin: AdminUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    auth.remove_member(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/groups`: the groups the caller is in, to share files with.
async fn my_groups(State(auth): State<Arc<Auth>>, current: CurrentUser) -> Result<Json<Vec<GroupName>>> {
    Ok(Json(auth.my_groups(current.user()).await?))
}

async fn update_user(
    State(auth): State<Arc<Auth>>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<Uuid>,
    Json(update): Json<AccountUpdate>,
) -> Result<Json<User>> {
    Ok(Json(auth.update_account(admin.user(), user_id, update).await?))
}
