//! Login through external services with OpenID Connect (Keycloak, Authentik,
//! Google, …): the administrator adds the services, users log in with a
//! button or add a service to their account in the settings.
//!
//! The flow is the authorization code flow with PKCE:
//!
//! 1. `GET /api/v1/auth/oidc/{id}/login` (or `POST …/link` from the settings
//!    page) remembers a random `state`, `nonce` and PKCE verifier on the
//!    server and the `state` in a cookie of this browser, then sends the
//!    browser to the service.
//! 2. The service sends it back to `…/callback` with a code. The code is
//!    exchanged for an ID token, whose signature, issuer, audience, nonce and
//!    time are checked. Its `sub` is the identity `("oidc:<id>", sub)`.
//! 3. [`Auth::sign_in_external`] finds or creates the account, applies the
//!    groups → administrator rule, and the browser gets the session cookie.
//!
//! The cookie with `state` ties the callback to the browser that started
//! the login: a link with someone else's code logs nobody in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use rustcloud_auth::{session_cookie, user_agent, AdminUser, Auth, CurrentUser, ExternalLogin, Identity};
use rustcloud_core::{audit, ClientInfo, Error, Live, Result};
use rustcloud_db::{oidc_provider, DatabaseConnection};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;
use uuid::Uuid;

use crate::network::ServerSettings;
use crate::AppState;

/// The cookie with the `state` of a login in progress.
const FLOW_COOKIE: &str = "rustcloud_oidc";
/// Where the routes of the flow live (the cookie's path).
const FLOW_PATH: &str = "/api/v1/auth/oidc";
/// How long a login at the service may take.
const FLOW_TIME: Duration = Duration::from_secs(10 * 60);
/// Most logins in progress at once.
const MAX_FLOWS: usize = 10_000;
/// How long a service's discovery document and keys are used.
const KEEP_METADATA: Duration = Duration::from_secs(5 * 60);
/// Most time for one request to a service.
const REQUEST_TIME: Duration = Duration::from_secs(15);
/// The prefix of identities of these services.
const PREFIX: &str = "oidc:";

/// The services, the logins in progress and the HTTP client for both.
pub struct Oidc {
    db: DatabaseConnection,
    auth: Arc<Auth>,
    server: Arc<Live<ServerSettings>>,
    client: reqwest::Client,
    metadata: Mutex<HashMap<String, Discovered>>,
    flows: Mutex<HashMap<String, Flow>>,
}

struct Discovered {
    issuer: String,
    at: Instant,
    metadata: CoreProviderMetadata,
}

/// A login waiting for the service to send the browser back.
struct Flow {
    provider: String,
    metadata: CoreProviderMetadata,
    redirect_url: String,
    nonce: Nonce,
    pkce: PkceCodeVerifier,
    /// Add the identity to this account instead of logging in.
    link_to: Option<Uuid>,
    /// The page of the web UI to go back to after logging in (see [`return_path`]).
    return_to: Option<String>,
    at: Instant,
}

/// A service as the administrator sees it. The client secret is never sent back.
#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub client_id: String,
    pub has_secret: bool,
    pub scopes: String,
    pub enabled: bool,
    pub auto_create: bool,
    pub link_by_email: bool,
    pub groups_claim: String,
    pub user_groups: Vec<String>,
    pub admin_groups: Vec<String>,
    /// The longest a login through it lasts, devices included; 0: no limit.
    pub session_days: u32,
    pub created_at: DateTime<Utc>,
    /// The address to register at the service ("redirect URI").
    pub redirect_url: String,
}

/// A new service or new values of one. On a change `id` and `issuer` may be
/// left out (they never change) and an empty `client_secret` keeps the old one.
#[derive(Debug, Deserialize)]
pub struct ProviderInput {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub issuer: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub scopes: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub auto_create: bool,
    #[serde(default)]
    pub link_by_email: bool,
    #[serde(default)]
    pub groups_claim: String,
    #[serde(default)]
    pub user_groups: Vec<String>,
    #[serde(default)]
    pub admin_groups: Vec<String>,
    #[serde(default = "thirty")]
    pub session_days: u32,
}

fn yes() -> bool {
    true
}

fn thirty() -> u32 {
    30
}

/// A service on the login page.
#[derive(Debug, Serialize)]
pub struct LoginButton {
    pub id: String,
    pub name: String,
}

impl Oidc {
    pub fn new(db: DatabaseConnection, auth: Arc<Auth>, server: Arc<Live<ServerSettings>>) -> Result<Self> {
        // No redirects: a service could send the server's requests anywhere.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIME)
            .user_agent(concat!("RustCloud/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::internal)?;
        Ok(Self { db, auth, server, client, metadata: Mutex::new(HashMap::new()), flows: Mutex::new(HashMap::new()) })
    }

    /// The enabled services, for the login page.
    pub async fn buttons(&self) -> Result<Vec<LoginButton>> {
        let rows = oidc_provider::Entity::find()
            .filter(oidc_provider::Column::Enabled.eq(true))
            .order_by_asc(oidc_provider::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| LoginButton { id: row.id, name: row.name }).collect())
    }

    pub async fn list(&self, base: &str) -> Result<Vec<ProviderView>> {
        let rows = oidc_provider::Entity::find().order_by_asc(oidc_provider::Column::CreatedAt).all(&self.db).await?;
        Ok(rows.into_iter().map(|row| view(row, base)).collect())
    }

    /// Adds a service after checking that its discovery document can be read.
    pub async fn create(&self, input: ProviderInput, base: &str) -> Result<ProviderView> {
        let id = input.id.trim().to_owned();
        if !valid_id(&id) {
            return Err(invalid("The id must be 1-32 characters: a-z, 0-9 and '-'"));
        }
        if oidc_provider::Entity::find_by_id(&id).one(&self.db).await?.is_some() {
            return Err(Error::conflict("OIDC_PROVIDER_EXISTS", "A login service with this id exists"));
        }
        let issuer = clean_issuer(&input.issuer)?;
        let checked = Checked::from_input(&input)?;
        self.discover(&id, &issuer, true).await?;

        let row = oidc_provider::ActiveModel {
            id: Set(id.clone()),
            issuer: Set(issuer),
            created_at: Set(Utc::now()),
            ..checked.into_active(input.client_secret.trim().to_owned())
        }
        .insert(&self.db)
        .await?;
        audit!("oidc.provider_add", provider = id, "login service added");
        Ok(view(row, base))
    }

    pub async fn update(&self, id: &str, input: ProviderInput, base: &str) -> Result<ProviderView> {
        let current = self.find(id).await?;
        if !input.issuer.trim().is_empty() && clean_issuer(&input.issuer)? != current.issuer {
            return Err(invalid("The issuer of a login service can't be changed; add a new service instead"));
        }
        let checked = Checked::from_input(&input)?;
        let secret = match input.client_secret.trim() {
            "" => current.client_secret.clone(),
            new => new.to_owned(),
        };
        let mut row = checked.into_active(secret);
        row.id = Set(current.id.clone());
        row.issuer = Set(current.issuer.clone());
        row.created_at = Set(current.created_at);
        let row = row.update(&self.db).await?;
        audit!("oidc.provider_change", provider = id, "login service changed");
        Ok(view(row, base))
    }

    /// Removes a service and the logins of accounts through it.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.find(id).await?;
        oidc_provider::Entity::delete_by_id(id).exec(&self.db).await?;
        let method = format!("{PREFIX}{id}");
        let forgotten = self.auth.forget_identities(&method).await?;
        let sessions = self.auth.end_sessions_of(&method).await?;
        self.metadata.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        audit!("oidc.provider_remove", provider = id, logins = forgotten, sessions, "login service removed");
        Ok(())
    }

    async fn find(&self, id: &str) -> Result<oidc_provider::Model> {
        oidc_provider::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("OIDC_PROVIDER_NOT_FOUND", "No such login service"))
    }

    /// The service's discovery document and keys, kept for a few minutes.
    async fn discover(&self, id: &str, issuer: &str, fresh: bool) -> Result<CoreProviderMetadata> {
        if !fresh {
            let cache = self.metadata.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(found) = cache.get(id).filter(|d| d.issuer == issuer && d.at.elapsed() < KEEP_METADATA) {
                return Ok(found.metadata.clone());
            }
        }
        let issuer_url = IssuerUrl::new(issuer.to_owned()).map_err(|err| invalid(format!("{issuer}: {err}")))?;
        let metadata = CoreProviderMetadata::discover_async(issuer_url, &self.client).await.map_err(|err| {
            tracing::warn!("OIDC discovery of {issuer} failed: {}", error_chain(&err));
            Error::bad_request(
                "OIDC_DISCOVERY_FAILED",
                format!("Could not read {issuer}/.well-known/openid-configuration: {}", error_chain(&err)),
            )
        })?;
        let fresh = Discovered { issuer: issuer.to_owned(), at: Instant::now(), metadata: metadata.clone() };
        self.metadata.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), fresh);
        Ok(metadata)
    }

    /// Starts a login (or adding the service to `link_to`): the address of
    /// the service to send the browser to and the `state` for the cookie.
    async fn start(
        &self,
        id: &str,
        base: &str,
        link_to: Option<Uuid>,
        return_to: Option<String>,
    ) -> Result<(String, String)> {
        let row = self.find(id).await?;
        if !row.enabled {
            return Err(Error::not_found("OIDC_PROVIDER_NOT_FOUND", "No such login service"));
        }
        let metadata = self.discover(&row.id, &row.issuer, false).await?;
        let redirect_url = callback_url(base, &row.id);
        let client = client(&row, metadata.clone(), &redirect_url)?;
        let (challenge, pkce) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .set_pkce_challenge(challenge);
        for scope in row.scopes.split_whitespace().filter(|scope| *scope != "openid") {
            request = request.add_scope(Scope::new(scope.to_owned()));
        }
        let (url, state, nonce) = request.url();

        let flow =
            Flow { provider: row.id, metadata, redirect_url, nonce, pkce, link_to, return_to, at: Instant::now() };
        let mut flows = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        flows.retain(|_, flow| flow.at.elapsed() < FLOW_TIME);
        if flows.len() >= MAX_FLOWS {
            return Err(Error::new(
                rustcloud_core::ErrorKind::TooManyRequests,
                "TOO_MANY_ATTEMPTS",
                "Too many logins at once, try again later",
            ));
        }
        flows.insert(state.secret().clone(), flow);
        Ok((url.to_string(), state.secret().clone()))
    }

    /// Finishes a login: checks the answer of the service and logs in (or
    /// adds the service to the account that asked).
    async fn finish(&self, id: &str, answer: &Answer, cookie_state: Option<&str>) -> Result<Finished> {
        let state = answer.state.as_deref().unwrap_or_default();
        // The browser that started this login is the one that finishes it.
        if state.is_empty() || cookie_state != Some(state) {
            return Err(flow_error("OIDC_EXPIRED", "the login was not started in this browser or took too long"));
        }
        let flow = self
            .flows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(state)
            .filter(|flow| flow.at.elapsed() < FLOW_TIME && flow.provider == id)
            .ok_or_else(|| flow_error("OIDC_EXPIRED", "unknown or expired state"))?;
        if let Some(error) = &answer.error {
            let details = answer.error_description.as_deref().unwrap_or_default();
            return Err(flow_error("OIDC_CANCELLED", &format!("the service answered {error}: {details}")));
        }
        let code = answer.code.clone().ok_or_else(|| flow_error("OIDC_FAILED", "no code in the answer"))?;

        let row = self.find(id).await?;
        if !row.enabled {
            return Err(flow_error("OIDC_FAILED", "the service was turned off"));
        }
        let client = client(&row, flow.metadata.clone(), &flow.redirect_url)?;
        let failed = |what: &str, err: &dyn std::error::Error| {
            flow_error("OIDC_FAILED", &format!("{what}: {}", error_chain(err)))
        };
        let tokens = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|err| failed("no token endpoint", &err))?
            .set_pkce_verifier(flow.pkce)
            .request_async(&self.client)
            .await
            .map_err(|err| failed("exchanging the code", &err))?;
        let id_token = tokens.id_token().ok_or_else(|| flow_error("OIDC_FAILED", "no ID token in the answer"))?;
        let verified = id_token
            .claims(&client.id_token_verifier(), &flow.nonce)
            .map_err(|err| failed("checking the ID token", &err))?;
        let subject = verified.subject().as_str().to_owned();

        // Every claim of the (now verified) token, then what the userinfo
        // endpoint adds: some services put groups or email only there.
        let mut claims = token_payload(&id_token.to_string()).unwrap_or_default();
        let rules = Rules::of(&row);
        let wants_groups = rules.uses_groups() && claim(&claims, &row.groups_claim).is_none();
        let wants_profile = !claims.contains_key("email") || !claims.contains_key("preferred_username");
        if let (true, Some(endpoint)) = (wants_groups || wants_profile, flow.metadata.userinfo_endpoint()) {
            match self.userinfo(endpoint.url(), tokens.access_token().secret()).await {
                Ok(info) if info.get("sub").and_then(Value::as_str) == Some(subject.as_str()) => {
                    for (key, value) in info {
                        claims.entry(key).or_insert(value);
                    }
                }
                Ok(_) => tracing::warn!("OIDC {id}: userinfo is about another subject; ignored"),
                Err(err) => tracing::warn!("OIDC {id}: userinfo failed: {err}"),
            }
        }

        let groups = groups(&claims, &row.groups_claim);
        if !rules.may_log_in(&groups) {
            return Err(flow_error("NOT_ALLOWED", "not in the groups allowed to log in"));
        }
        let identity = Identity { provider: format!("{PREFIX}{id}"), subject };
        if let Some(user_id) = flow.link_to {
            self.auth.link_identity(user_id, &identity).await?;
            return Ok(Finished::Linked);
        }
        let text = |key: &str| claims.get(key).and_then(Value::as_str).map(str::to_owned);
        let login = ExternalLogin {
            identity,
            username: text("preferred_username"),
            display_name: text("name"),
            email: text("email"),
            email_verified: matches!(claims.get("email_verified"), Some(Value::Bool(true)))
                || claims.get("email_verified").and_then(Value::as_str) == Some("true"),
            auto_create: row.auto_create,
            link_by_email: row.link_by_email,
            admin: rules.admin(&groups),
            max_session_days: u32::try_from(row.session_days).ok().filter(|days| *days > 0),
        };
        let session = self.auth.sign_in_external(&login).await?;
        Ok(Finished::LoggedIn(Box::new(session), flow.return_to))
    }

    async fn userinfo(&self, url: &Url, access_token: &str) -> std::result::Result<Map<String, Value>, String> {
        let response = self
            .client
            .get(url.clone())
            .bearer_auth(access_token)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|err| error_chain(&err))?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        // A signed (JWT) answer is not read: the ID token has what matters.
        let body = response.bytes().await.map_err(|err| error_chain(&err))?;
        serde_json::from_slice(&body).map_err(|err| err.to_string())
    }
}

enum Finished {
    /// With the page to go back to.
    LoggedIn(Box<rustcloud_auth::NewSession>, Option<String>),
    Linked,
}

/// What a service's groups decide.
struct Rules {
    user_groups: Vec<String>,
    admin_groups: Vec<String>,
}

impl Rules {
    fn of(row: &oidc_provider::Model) -> Self {
        Self { user_groups: list(&row.user_groups), admin_groups: list(&row.admin_groups) }
    }

    fn uses_groups(&self) -> bool {
        !self.user_groups.is_empty() || !self.admin_groups.is_empty()
    }

    /// With user groups set, only their members (and administrators) log in.
    fn may_log_in(&self, groups: &[String]) -> bool {
        self.user_groups.is_empty() || self.member(&self.user_groups, groups) || self.member(&self.admin_groups, groups)
    }

    /// With administrator groups set, they decide who is an administrator.
    fn admin(&self, groups: &[String]) -> Option<bool> {
        (!self.admin_groups.is_empty()).then(|| self.member(&self.admin_groups, groups))
    }

    fn member(&self, wanted: &[String], groups: &[String]) -> bool {
        wanted.iter().any(|group| groups.contains(group))
    }
}

/// Values of a service checked for saving (all but id, issuer and secret).
struct Checked {
    name: String,
    client_id: String,
    scopes: String,
    groups_claim: String,
    user_groups: Vec<String>,
    admin_groups: Vec<String>,
    enabled: bool,
    auto_create: bool,
    link_by_email: bool,
    session_days: u32,
}

impl Checked {
    fn from_input(input: &ProviderInput) -> Result<Self> {
        let name = input.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
            return Err(invalid("The name must be 1-60 characters"));
        }
        let client_id = input.client_id.trim().to_owned();
        if client_id.is_empty() || client_id.len() > 255 || client_id.chars().any(char::is_whitespace) {
            return Err(invalid("The client id must be 1-255 characters without spaces"));
        }
        let mut scopes: Vec<&str> = vec!["openid"];
        let requested = if input.scopes.trim().is_empty() { "openid profile email" } else { input.scopes.as_str() };
        for scope in requested.split_whitespace() {
            if !scope.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\') {
                return Err(invalid(format!("`{scope}` is not a valid scope")));
            }
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        let groups_claim = match input.groups_claim.trim() {
            "" => "groups".to_owned(),
            claim if claim.len() <= 100 && claim.split('.').all(|part| !part.is_empty()) => claim.to_owned(),
            claim => return Err(invalid(format!("`{claim}` is not a claim name"))),
        };
        if input.session_days > 3650 {
            return Err(invalid("The session limit must be 0-3650 days"));
        }
        Ok(Self {
            name,
            client_id,
            scopes: scopes.join(" "),
            groups_claim,
            user_groups: clean_groups(&input.user_groups)?,
            admin_groups: clean_groups(&input.admin_groups)?,
            enabled: input.enabled,
            auto_create: input.auto_create,
            link_by_email: input.link_by_email,
            session_days: input.session_days,
        })
    }

    fn into_active(self, client_secret: String) -> oidc_provider::ActiveModel {
        oidc_provider::ActiveModel {
            name: Set(self.name),
            client_id: Set(self.client_id),
            client_secret: Set(client_secret),
            scopes: Set(self.scopes),
            enabled: Set(self.enabled),
            auto_create: Set(self.auto_create),
            link_by_email: Set(self.link_by_email),
            groups_claim: Set(self.groups_claim),
            session_days: Set(i32::try_from(self.session_days).unwrap_or(30)),
            user_groups: Set(serde_json::to_string(&self.user_groups).unwrap_or_default()),
            admin_groups: Set(serde_json::to_string(&self.admin_groups).unwrap_or_default()),
            ..Default::default()
        }
    }
}

fn clean_groups(groups: &[String]) -> Result<Vec<String>> {
    let mut clean = Vec::new();
    for group in groups.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
        if group.chars().count() > 200 || group.chars().any(char::is_control) {
            return Err(invalid("A group name is too long or has control characters"));
        }
        if !clean.iter().any(|g| g == group) {
            clean.push(group.to_owned());
        }
    }
    Ok(clean)
}

/// An `http(s)://` address without query or fragment, as typed (the
/// service compares it with its `iss` letter by letter, trailing `/` too).
fn clean_issuer(issuer: &str) -> Result<String> {
    let issuer = issuer.trim();
    let url = Url::parse(issuer).map_err(|err| invalid(format!("The issuer `{issuer}` is not a URL: {err}")))?;
    let plain = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty();
    if !plain {
        return Err(invalid(format!("The issuer `{issuer}` must look like https://login.example.com/realms/main")));
    }
    Ok(issuer.to_owned())
}

/// 1-32 characters of `a-z`, `0-9` and `-`, starting with a letter or digit.
fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn view(row: oidc_provider::Model, base: &str) -> ProviderView {
    ProviderView {
        redirect_url: callback_url(base, &row.id),
        has_secret: !row.client_secret.is_empty(),
        user_groups: list(&row.user_groups),
        admin_groups: list(&row.admin_groups),
        id: row.id,
        name: row.name,
        issuer: row.issuer,
        client_id: row.client_id,
        scopes: row.scopes,
        enabled: row.enabled,
        auto_create: row.auto_create,
        link_by_email: row.link_by_email,
        groups_claim: row.groups_claim,
        session_days: u32::try_from(row.session_days).unwrap_or(0),
        created_at: row.created_at,
    }
}

fn list(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn callback_url(base: &str, id: &str) -> String {
    format!("{base}{FLOW_PATH}/{id}/callback")
}

fn client(
    row: &oidc_provider::Model,
    metadata: CoreProviderMetadata,
    redirect_url: &str,
) -> Result<
    CoreClient<
        openidconnect::EndpointSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointNotSet,
        openidconnect::EndpointMaybeSet,
        openidconnect::EndpointMaybeSet,
    >,
> {
    let secret = (!row.client_secret.is_empty()).then(|| ClientSecret::new(row.client_secret.clone()));
    let redirect = RedirectUrl::new(redirect_url.to_owned()).map_err(Error::internal)?;
    Ok(CoreClient::from_provider_metadata(metadata, ClientId::new(row.client_id.clone()), secret)
        .set_redirect_uri(redirect))
}

/// The claims of a JWT, read without checking it: only for a token that
/// was checked already.
fn token_payload(jwt: &str) -> Option<Map<String, Value>> {
    let payload = jwt.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

/// A claim by name; dots go into objects (`realm_access.roles`).
fn claim<'a>(claims: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    if let Some(value) = claims.get(name) {
        return Some(value);
    }
    let mut parts = name.split('.');
    let mut value = claims.get(parts.next()?)?;
    for part in parts {
        value = value.get(part)?;
    }
    Some(value)
}

/// The groups in a claim: a list of names, or one name.
fn groups(claims: &Map<String, Value>, name: &str) -> Vec<String> {
    match claim(claims, name) {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    }
}

/// An error and its causes in one line.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn invalid(message: impl Into<String>) -> Error {
    Error::bad_request("INVALID_OIDC_PROVIDER", message)
}

/// A failed login at a service: the code goes to the browser, the details
/// only into the log.
fn flow_error(code: &'static str, details: &str) -> Error {
    Error::bad_request(code, details.to_owned())
}

/// The address users reach the server at: the public URL, or the address of
/// this request.
pub(crate) fn base_url(server: &Live<ServerSettings>, client: ClientInfo, headers: &HeaderMap) -> String {
    let public = server.read(|s| s.public_url.trim_end_matches('/').to_owned());
    if !public.is_empty() {
        return public;
    }
    let scheme = if client.https { "https" } else { "http" };
    format!("{scheme}://{}", request_host(headers))
}

/// The host (and port) a request was sent to.
fn request_host(headers: &HeaderMap) -> &str {
    headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost")
}

/// Whether a request came to the address of `base` (the flow cookie is set
/// for the host the browser is on; the service sends it back to `base`).
fn on_base_host(base: &str, headers: &HeaderMap) -> bool {
    let host = request_host(headers);
    Url::parse(base).ok().is_some_and(|url| {
        let expected = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        expected.eq_ignore_ascii_case(host)
    })
}

fn flow_cookie(state: String, client: ClientInfo) -> Cookie<'static> {
    Cookie::build((FLOW_COOKIE, state))
        .http_only(true)
        // Lax: the cookie comes along when the service sends the browser back.
        .same_site(SameSite::Lax)
        .secure(client.https)
        .path(FLOW_PATH)
        .max_age(time::Duration::seconds(FLOW_TIME.as_secs() as i64))
        .build()
}

fn removed_flow_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::build((FLOW_COOKIE, "")).path(FLOW_PATH).build();
    cookie.make_removal();
    cookie
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/oidc", get(login_buttons))
        .route("/api/v1/auth/oidc/{id}/login", get(login))
        .route("/api/v1/auth/oidc/{id}/link", post(link))
        .route("/api/v1/auth/oidc/{id}/callback", get(callback))
        .route("/api/v1/admin/oidc", get(list_providers).post(add_provider))
        .route("/api/v1/admin/oidc/{id}", axum::routing::put(change_provider).delete(remove_provider))
}

/// `GET /api/v1/auth/oidc`: the services to show on the login page.
async fn login_buttons(State(oidc): State<Arc<Oidc>>) -> Result<Json<Vec<LoginButton>>> {
    Ok(Json(oidc.buttons().await?))
}

#[derive(Deserialize)]
struct LoginParams {
    /// The page of the web UI to come back to, such as `/connect?…`.
    #[serde(rename = "return")]
    return_to: Option<String>,
}

/// `GET /api/v1/auth/oidc/{id}/login[?return=/page]`: sends the browser to the service.
async fn login(
    State(oidc): State<Arc<Oidc>>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    Query(params): Query<LoginParams>,
    client: ClientInfo,
    headers: HeaderMap,
) -> Response {
    if !valid_id(&id) {
        return back_to_page("login", "OIDC_UNAVAILABLE").into_response();
    }
    let base = base_url(&oidc.server, client, &headers);
    // Start where the service will send the browser back, or the cookie
    // would be on another host.
    if !on_base_host(&base, &headers) {
        let query = query.map(|q| format!("?{q}")).unwrap_or_default();
        return Redirect::to(&format!("{base}{FLOW_PATH}/{id}/login{query}")).into_response();
    }
    let return_to = params.return_to.as_deref().and_then(return_path);
    match oidc.start(&id, &base, None, return_to).await {
        Ok((url, state)) => (CookieJar::new().add(flow_cookie(state, client)), Redirect::to(&url)).into_response(),
        Err(err) => {
            tracing::warn!("OIDC {id}: login not started: {}", err.message);
            back_to_page("login", "OIDC_UNAVAILABLE").into_response()
        }
    }
}

#[derive(Serialize)]
struct GoTo {
    url: String,
}

/// `POST /api/v1/auth/oidc/{id}/link`: adds a service to the caller's
/// account; answers the address to send the browser to.
async fn link(
    State(oidc): State<Arc<Oidc>>,
    Path(id): Path<String>,
    current: CurrentUser,
    client: ClientInfo,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    current.0.browser_only()?;
    let base = base_url(&oidc.server, client, &headers);
    if !on_base_host(&base, &headers) {
        return Err(Error::bad_request(
            "OIDC_WRONG_ADDRESS",
            format!("Open the server at {base} to add this login method"),
        ));
    }
    let (url, state) = oidc.start(&id, &base, Some(current.id()), None).await?;
    Ok((CookieJar::new().add(flow_cookie(state, client)), Json(GoTo { url })))
}

/// The query of the service's answer.
#[derive(Debug, Deserialize)]
struct Answer {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// `GET /api/v1/auth/oidc/{id}/callback`: the service sends the browser
/// back here; it ends up on the files page, or on the page it came from
/// with an error code.
async fn callback(
    State(oidc): State<Arc<Oidc>>,
    Path(id): Path<String>,
    Query(answer): Query<Answer>,
    jar: CookieJar,
    client: ClientInfo,
    headers: HeaderMap,
) -> Response {
    let cookie_state = jar.get(FLOW_COOKIE).map(|c| c.value().to_owned());
    let linking = cookie_state
        .as_deref()
        .and_then(|state| oidc.flows.lock().unwrap_or_else(|e| e.into_inner()).get(state).map(|f| f.link_to.is_some()))
        .unwrap_or(false);
    let jar = CookieJar::new().add(removed_flow_cookie());
    match oidc.finish(&id, &answer, cookie_state.as_deref()).await {
        Ok(Finished::LoggedIn(session, return_to)) => {
            oidc.auth.describe_session(session.session_id, user_agent(&headers), client.ip).await;
            let jar = jar.add(session_cookie(&session, client));
            let page = return_to.map_or_else(|| "/".to_owned(), |path| format!("/#{path}"));
            (jar, Redirect::to(&page)).into_response()
        }
        Ok(Finished::Linked) => (jar, Redirect::to(&format!("/#/settings?oidc_linked={id}"))).into_response(),
        Err(err) => {
            let ip = client.ip.map(|ip| ip.to_string());
            let method = format!("{PREFIX}{id}");
            audit!(warn, "auth.login_failed", method, ip, reason = err.message, "failed login");
            let page = if linking { "settings" } else { "login" };
            let code = match err.code {
                "INVALID_CREDENTIALS" | "ACCOUNT_DISABLED" => "ACCOUNT_DISABLED",
                code => code,
            };
            (jar, back_to_page(page, code)).into_response()
        }
    }
}

/// A page of the web UI to come back to after logging in (`/connect?…`), if
/// it is one: it ends up after `/#`, so it can't lead to another site anyway.
fn return_path(path: &str) -> Option<String> {
    let fits = path.starts_with('/') && path.len() <= 2048 && !path.chars().any(|c| c.is_control());
    fits.then(|| path.to_owned())
}

/// A page of the web UI with an error code to show.
fn back_to_page(page: &str, code: &str) -> Redirect {
    let code: String = code.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    Redirect::to(&format!("/#/{page}?oidc_error={code}"))
}

async fn list_providers(
    State(oidc): State<Arc<Oidc>>,
    _admin: AdminUser,
    client: ClientInfo,
    headers: HeaderMap,
) -> Result<Json<Vec<ProviderView>>> {
    Ok(Json(oidc.list(&base_url(&oidc.server, client, &headers)).await?))
}

async fn add_provider(
    State(oidc): State<Arc<Oidc>>,
    _admin: AdminUser,
    client: ClientInfo,
    headers: HeaderMap,
    Json(input): Json<ProviderInput>,
) -> Result<impl IntoResponse> {
    let view = oidc.create(input, &base_url(&oidc.server, client, &headers)).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn change_provider(
    State(oidc): State<Arc<Oidc>>,
    _admin: AdminUser,
    Path(id): Path<String>,
    client: ClientInfo,
    headers: HeaderMap,
    Json(input): Json<ProviderInput>,
) -> Result<Json<ProviderView>> {
    Ok(Json(oidc.update(&id, input, &base_url(&oidc.server, client, &headers)).await?))
}

async fn remove_provider(
    State(oidc): State<Arc<Oidc>>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    oidc.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claims(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn groups_come_from_lists_single_names_and_nested_claims() {
        let c = claims(json!({ "groups": ["a", "b"], "role": "admin", "realm_access": { "roles": ["x"] } }));
        assert_eq!(groups(&c, "groups"), ["a", "b"]);
        assert_eq!(groups(&c, "role"), ["admin"]);
        assert_eq!(groups(&c, "realm_access.roles"), ["x"]);
        assert!(groups(&c, "missing").is_empty());
    }

    #[test]
    fn groups_decide_who_logs_in_and_who_is_an_administrator() {
        let open = Rules { user_groups: vec![], admin_groups: vec![] };
        assert!(open.may_log_in(&[]));
        assert_eq!(open.admin(&["admins".into()]), None, "no admin groups: rights are left alone");

        let rules = Rules { user_groups: vec!["staff".into()], admin_groups: vec!["admins".into()] };
        assert!(!rules.may_log_in(&["guests".into()]));
        assert!(rules.may_log_in(&["staff".into()]));
        assert!(rules.may_log_in(&["admins".into()]), "administrators may log in too");
        assert_eq!(rules.admin(&["staff".into()]), Some(false));
        assert_eq!(rules.admin(&["admins".into()]), Some(true));
    }

    #[test]
    fn scopes_always_include_openid() {
        let input = |scopes: &str| ProviderInput {
            id: "x".into(),
            name: "X".into(),
            issuer: String::new(),
            client_id: "c".into(),
            client_secret: String::new(),
            scopes: scopes.into(),
            enabled: true,
            auto_create: true,
            link_by_email: false,
            groups_claim: String::new(),
            user_groups: vec![],
            admin_groups: vec![],
            session_days: 30,
        };
        assert_eq!(Checked::from_input(&input("")).unwrap().scopes, "openid profile email");
        assert_eq!(Checked::from_input(&input("email groups")).unwrap().scopes, "openid email groups");
        assert!(Checked::from_input(&input("a\"b")).is_err());
    }

    #[test]
    fn requests_start_on_the_public_host() {
        let headers = |host: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, host.parse().unwrap());
            h
        };
        assert!(on_base_host("https://cloud.example.com", &headers("cloud.example.com")));
        assert!(on_base_host("http://192.168.1.5:8080", &headers("192.168.1.5:8080")));
        assert!(!on_base_host("https://cloud.example.com", &headers("192.168.1.5:8080")));
    }
}
