//! Sessions of devices — desktop and phone clients — and how they get one:
//! OAuth 2.0 authorization code with PKCE for native apps (RFC 8252).
//!
//! 1. The client opens `/api/v1/auth/authorize?…` in the browser with a
//!    PKCE challenge and a loopback `redirect_uri` (`http://127.0.0.1:<port>/…`)
//!    or one of its private scheme (`com.example.app:/…`). The user logs in
//!    any way they like and allows the device; the page gets a one-time code
//!    ([`Auth::authorize`]) and sends the browser to `redirect_uri` with it.
//! 2. The client exchanges the code and its PKCE verifier for an access
//!    token and a refresh token ([`Auth::exchange_code`]). Only the client
//!    that started the login has the verifier, and the code goes to the
//!    device the browser runs on: a link someone else sent gets them nothing.
//! 3. The access token (a JWT, [`crate::access`]) works for ten minutes; the
//!    refresh token gets a new pair ([`Auth::refresh`]) and is replaced every
//!    time. Using a replaced one again means it was copied: the session ends.
//!
//! A device session lives [`REFRESH_DAYS`] from its last renewal, and never
//! longer than the login it came from allows (`not_after`, set by login
//! services). Devices can't do what would take over the account: see
//! [`SessionUser::device`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_core::{audit, Error, ErrorKind, Result};
use rustcloud_db::session;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::access::ACCESS_TIME;
use crate::secret;
use crate::service::{Auth, SessionUser};

/// The `kind` of device sessions (browser sessions have none).
pub(crate) const DEVICE: &str = "device";
/// How long a device session lives after its last renewal.
pub const REFRESH_DAYS: i64 = 90;
/// How long a code waits to be exchanged.
const CODE_TIME: Duration = Duration::from_secs(5 * 60);
/// Most codes waiting at once.
const MAX_CODES: usize = 10_000;
/// A refresh token replaced this recently still works once more: the client
/// may not have got the answer that replaced it (the connection broke).
const RETRY_TIME: chrono::Duration = chrono::Duration::seconds(30);
/// `last_used_at` is written at most this often.
pub(crate) const LAST_USED_STEP: chrono::Duration = chrono::Duration::minutes(10);
/// Longest name of a device.
const MAX_NAME: usize = 100;

/// What the consent page asks for, from the client's authorization request.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeRequest {
    /// The app ("rustcloud-desktop"); its name when it gives none.
    pub client_id: String,
    /// The device ("Anna's laptop"), shown in the list of sessions.
    #[serde(default)]
    pub device_name: Option<String>,
    pub redirect_uri: String,
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: Option<String>,
}

/// The tokens of a device, in the shape of an OAuth 2.0 token response.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceTokens {
    pub access_token: String,
    pub token_type: &'static str,
    /// Seconds the access token works.
    pub expires_in: i64,
    pub refresh_token: String,
    /// When the refresh token stops working unless used.
    pub refresh_expires_at: DateTime<Utc>,
}

/// A session as its owner sees it in the settings.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: Uuid,
    /// A client app rather than a browser.
    pub device: bool,
    /// The device's name, or the browser (User-Agent).
    pub name: Option<String>,
    /// How its login was made: `password`, `oidc:<id>`.
    pub method: Option<String>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    /// The session of this request.
    pub current: bool,
}

/// A code waiting to be exchanged.
pub(crate) struct PendingCode {
    user_id: Uuid,
    /// The browser session that allowed the device.
    session_id: Uuid,
    challenge: String,
    redirect_uri: String,
    name: String,
    at: Instant,
}

/// Codes by their hash.
pub(crate) type Codes = std::sync::Mutex<HashMap<String, PendingCode>>;

impl Auth {
    /// Allows a device: the user of a browser session agreed on the consent
    /// page. Returns the one-time code for the device.
    pub fn authorize(&self, user: &SessionUser, request: &AuthorizeRequest) -> Result<String> {
        user.browser_only()?;
        let redirect_uri = check_redirect_uri(&request.redirect_uri)?.to_owned();
        if request.code_challenge_method.as_deref() != Some("S256") {
            return Err(invalid_request("code_challenge_method must be S256"));
        }
        if !is_pkce_value(&request.code_challenge) || request.code_challenge.len() != 43 {
            return Err(invalid_request("code_challenge must be the base64url SHA-256 of the verifier"));
        }
        let name = device_name(request)?;
        let code = secret::new_token();
        let pending = PendingCode {
            user_id: user.user.id,
            session_id: user.session_id,
            challenge: request.code_challenge.clone(),
            redirect_uri,
            name,
            at: Instant::now(),
        };
        let mut codes = self.codes.lock().unwrap_or_else(|e| e.into_inner());
        codes.retain(|_, code| code.at.elapsed() < CODE_TIME);
        if codes.len() >= MAX_CODES {
            return Err(crate::service::too_many_attempts("Too many devices logging in at once, try again later"));
        }
        codes.insert(secret::hash_token(&code), pending);
        Ok(code)
    }

    /// Exchanges a code for the tokens of a new device session.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        ip: Option<IpAddr>,
    ) -> Result<DeviceTokens> {
        let pending = self
            .codes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&secret::hash_token(code))
            .filter(|pending| pending.at.elapsed() < CODE_TIME)
            .ok_or_else(|| invalid_grant("The code is unknown, used or expired"))?;
        if pending.redirect_uri != redirect_uri {
            return Err(invalid_grant("redirect_uri differs from the one of the authorization request"));
        }
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        if !is_pkce_value(verifier) || !secret::same(challenge.as_bytes(), pending.challenge.as_bytes()) {
            return Err(invalid_grant("The code_verifier doesn't match the code_challenge"));
        }
        // The browser session that allowed it must still be there: logging
        // out there in the meantime takes the permission back.
        let now = Utc::now();
        let browser = session::Entity::find_by_id(pending.session_id)
            .filter(session::Column::UserId.eq(pending.user_id))
            .filter(session::Column::ExpiresAt.gt(now))
            .one(&self.db)
            .await?
            .ok_or_else(|| invalid_grant("The browser session that allowed the device has ended"))?;
        let user = self.find_user(pending.user_id).await?;
        if user.disabled {
            return Err(invalid_grant("The account is disabled"));
        }

        // A login with a time limit gives the device the same span.
        let not_after = browser.not_after.map(|end| now + (end - browser.created_at));
        let refresh_token = secret::new_token();
        let expires_at = refresh_until(now, not_after);
        let row = session::ActiveModel {
            id: Set(Uuid::new_v4()),
            token_hash: Set(secret::hash_token(&refresh_token)),
            user_id: Set(user.id),
            created_at: Set(now),
            expires_at: Set(expires_at),
            kind: Set(Some(DEVICE.to_owned())),
            name: Set(Some(pending.name.clone())),
            method: Set(browser.method.clone()),
            ip: Set(ip.map(|ip| ip.to_string())),
            last_used_at: Set(Some(now)),
            not_after: Set(not_after),
            previous_hash: Set(None),
            rotated_at: Set(None),
        }
        .insert(&self.db)
        .await?;
        audit!("auth.device_add", user = user.username, device = pending.name, "device connected");
        Ok(self.device_tokens(user.id, row.id, refresh_token, expires_at, now))
    }

    /// A new pair of tokens for a refresh token, which stops working. A
    /// refresh token used again after it was replaced ends its session:
    /// someone has a copy.
    pub async fn refresh(&self, refresh_token: &str, ip: Option<IpAddr>) -> Result<DeviceTokens> {
        let hash = secret::hash_token(refresh_token);
        let now = Utc::now();
        let device = || session::Entity::find().filter(session::Column::Kind.eq(DEVICE));
        let (row, retry) = match device().filter(session::Column::TokenHash.eq(&hash)).one(&self.db).await? {
            Some(row) => (row, false),
            None => {
                let replaced = device()
                    .filter(session::Column::PreviousHash.eq(&hash))
                    .one(&self.db)
                    .await?
                    .ok_or_else(|| invalid_grant("Unknown refresh token"))?;
                if replaced.rotated_at.is_some_and(|at| now - at < RETRY_TIME) {
                    (replaced, true)
                } else {
                    session::Entity::delete_by_id(replaced.id).exec(&self.db).await?;
                    let user = self.find_user(replaced.user_id).await.map(|u| u.username).unwrap_or_default();
                    let device = replaced.name.unwrap_or_default();
                    audit!(warn, "auth.token_reuse", user, device, "a replaced refresh token was used: session ended");
                    return Err(invalid_grant("The refresh token was already used; log in again"));
                }
            }
        };
        if row.expires_at <= now {
            return Err(invalid_grant("The session has expired; log in again"));
        }
        let user = self.find_user(row.user_id).await?;
        if user.disabled {
            return Err(invalid_grant("The account is disabled"));
        }

        let new_token = secret::new_token();
        let expires_at = refresh_until(now, row.not_after);
        let mut update = session::Entity::update_many()
            .col_expr(session::Column::TokenHash, Expr::value(secret::hash_token(&new_token)))
            .col_expr(session::Column::ExpiresAt, Expr::value(expires_at))
            .col_expr(session::Column::LastUsedAt, Expr::value(now))
            .col_expr(session::Column::Ip, Expr::value(ip.map(|ip| ip.to_string())))
            .filter(session::Column::Id.eq(row.id))
            // Two renewals at once: only one replaces the token.
            .filter(session::Column::TokenHash.eq(row.token_hash.clone()));
        if !retry {
            update = update
                .col_expr(session::Column::PreviousHash, Expr::value(hash))
                .col_expr(session::Column::RotatedAt, Expr::value(now));
        }
        if update.exec(&self.db).await?.rows_affected != 1 {
            return Err(invalid_grant("The refresh token was just used; try again"));
        }
        Ok(self.device_tokens(user.id, row.id, new_token, expires_at, now))
    }

    fn device_tokens(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        refresh_token: String,
        refresh_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> DeviceTokens {
        DeviceTokens {
            access_token: self.access.issue(user_id, session_id, now),
            token_type: "Bearer",
            expires_in: ACCESS_TIME.num_seconds(),
            refresh_token,
            refresh_expires_at,
        }
    }

    /// The user of an access token, if its session still exists.
    pub(crate) async fn device_user(&self, token: &str, ip: Option<IpAddr>) -> Result<Option<SessionUser>> {
        let now = Utc::now();
        let Some(claims) = self.access.check(token, now) else {
            return Ok(None);
        };
        let Some(row) = session::Entity::find_by_id(claims.session_id)
            .filter(session::Column::UserId.eq(claims.user_id))
            .filter(session::Column::Kind.eq(DEVICE))
            .filter(session::Column::ExpiresAt.gt(now))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let user = self.find_user(row.user_id).await?;
        if user.disabled {
            return Ok(None);
        }
        self.touch(&row, ip).await;
        Ok(Some(SessionUser { session_id: row.id, user, device: true }))
    }

    /// Notes that a session was used now, from `ip` (not on every request).
    pub(crate) async fn touch(&self, row: &session::Model, ip: Option<IpAddr>) {
        let ip = ip.map(|ip| ip.to_string());
        let now = Utc::now();
        if row.last_used_at.is_some_and(|at| now - at < LAST_USED_STEP) && (ip.is_none() || ip == row.ip) {
            return;
        }
        let mut update = session::Entity::update_many()
            .col_expr(session::Column::LastUsedAt, Expr::value(now))
            .filter(session::Column::Id.eq(row.id));
        if ip.is_some() {
            update = update.col_expr(session::Column::Ip, Expr::value(ip));
        }
        if let Err(err) = update.exec(&self.db).await {
            tracing::warn!("could not note the use of a session: {err}");
        }
    }

    /// The sessions of the caller, the newest first.
    pub async fn sessions(&self, current: &SessionUser) -> Result<Vec<SessionInfo>> {
        let rows = session::Entity::find()
            .filter(session::Column::UserId.eq(current.user.id))
            .filter(session::Column::ExpiresAt.gt(Utc::now()))
            .order_by_desc(session::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| SessionInfo {
                current: row.id == current.session_id,
                device: row.kind.as_deref() == Some(DEVICE),
                id: row.id,
                name: row.name,
                method: row.method,
                ip: row.ip,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
                expires_at: row.expires_at,
            })
            .collect())
    }

    /// Ends one session of the caller (from the settings).
    pub async fn end_session(&self, current: &SessionUser, id: Uuid) -> Result<()> {
        current.browser_only()?;
        let row = session::Entity::find_by_id(id)
            .filter(session::Column::UserId.eq(current.user.id))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("SESSION_NOT_FOUND", "No such session"))?;
        session::Entity::delete_by_id(row.id).exec(&self.db).await?;
        let name = row.name.unwrap_or_default();
        audit!("auth.session_end", device = name, "session ended");
        Ok(())
    }

    /// Ends every session made through a login method (its service was removed).
    pub async fn end_sessions_of(&self, method: &str) -> Result<u64> {
        let deleted = session::Entity::delete_many().filter(session::Column::Method.eq(method)).exec(&self.db).await?;
        Ok(deleted.rows_affected)
    }

    /// Notes what a new browser session runs in and where from.
    pub async fn describe_session(&self, session_id: Uuid, user_agent: Option<&str>, ip: Option<IpAddr>) {
        let name: Option<String> =
            user_agent.map(|agent| agent.chars().filter(|c| !c.is_control()).take(200).collect());
        let done = session::Entity::update_many()
            .col_expr(session::Column::Name, Expr::value(name))
            .col_expr(session::Column::Ip, Expr::value(ip.map(|ip| ip.to_string())))
            .filter(session::Column::Id.eq(session_id))
            .exec(&self.db)
            .await;
        if let Err(err) = done {
            tracing::warn!("could not describe a session: {err}");
        }
    }
}

/// What a client asks the token endpoint for.
pub(crate) enum Grant {
    Code { code: String, verifier: String, redirect_uri: String },
    Refresh(String),
    Unsupported(String),
}

/// Failed requests to the token endpoint one IP address may make in
/// [`crate::service::LOGIN_WINDOW`]: codes and refresh tokens are long and
/// random, so this only keeps a flood of guesses from piling up.
const TOKEN_FAILURES: u32 = 30;

impl Auth {
    /// Answers the token endpoint.
    pub(crate) async fn token_request(&self, grant: Grant, ip: Option<IpAddr>) -> Result<DeviceTokens> {
        let key = format!("token:{}", ip.map(|ip| ip.to_string()).unwrap_or_default());
        if !self.limiter.try_start(&key, TOKEN_FAILURES) {
            return Err(crate::service::too_many_attempts("Too many failed requests, try again later"));
        }
        let result = match grant {
            Grant::Code { code, verifier, redirect_uri } => {
                self.exchange_code(&code, &verifier, &redirect_uri, ip).await
            }
            Grant::Refresh(token) => self.refresh(&token, ip).await,
            Grant::Unsupported(kind) => {
                Err(Error::bad_request("unsupported_grant_type", format!("grant_type {kind:?} is not supported")))
            }
        };
        if result.is_ok() {
            self.limiter.forgive(&key);
        }
        result
    }
}

impl SessionUser {
    /// Refuses devices: what would take over the account (password, login
    /// methods, app passwords, other sessions, administration) is done in
    /// the browser only, so a stolen device token can't lock the owner out.
    pub fn browser_only(&self) -> Result<()> {
        match self.device {
            true => Err(Error::forbidden("Do this in the browser").with_code("BROWSER_ONLY")),
            false => Ok(()),
        }
    }
}

/// When a refresh token made `now` stops working.
fn refresh_until(now: DateTime<Utc>, not_after: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let end = now + chrono::Duration::days(REFRESH_DAYS);
    not_after.map_or(end, |limit| end.min(limit))
}

/// The redirect URI of a native app, if it is one (RFC 8252): a loopback
/// address on any port, or a private-use scheme with a dot (reverse domain
/// name). Anything else could send the code to someone else's server.
pub fn check_redirect_uri(uri: &str) -> Result<&str> {
    let invalid =
        || invalid_request("redirect_uri must be http://127.0.0.1:<port>/… or a private scheme (com.example.app:/…)");
    if uri.len() > 512 || uri.chars().any(|c| c.is_whitespace() || c.is_control() || c == '#') {
        return Err(invalid());
    }
    for host in ["http://127.0.0.1:", "http://[::1]:", "http://localhost:"] {
        if let Some(rest) = uri.strip_prefix(host) {
            let (port, path) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
            let port_ok = !port.is_empty() && port.len() <= 5 && port.parse::<u16>().is_ok_and(|p| p > 0);
            return match port_ok && (path.is_empty() || path.starts_with('/') || path.starts_with('?')) {
                true => Ok(uri),
                false => Err(invalid()),
            };
        }
    }
    let (scheme, rest) = uri.split_once(':').ok_or_else(invalid)?;
    let scheme_ok = scheme.contains('.')
        && scheme.starts_with(|c: char| c.is_ascii_lowercase())
        && scheme.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '+'));
    // `com.example.app:/path`: one slash, no host that could mean a server.
    match scheme_ok && rest.starts_with('/') && !rest.starts_with("//") {
        true => Ok(uri),
        false => Err(invalid()),
    }
}

/// A PKCE verifier or challenge: 43–128 characters of base64url (RFC 7636).
fn is_pkce_value(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
}

/// The name of a device in the list of sessions.
fn device_name(request: &AuthorizeRequest) -> Result<String> {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .chars()
            .take(MAX_NAME)
            .collect()
    };
    let client = clean(&request.client_id);
    if client.is_empty() {
        return Err(invalid_request("client_id is required"));
    }
    Ok(match request.device_name.as_deref().map(clean).filter(|name| !name.is_empty()) {
        Some(device) => format!("{device} ({client})"),
        None => client,
    })
}

/// OAuth `invalid_request`: the request itself is wrong.
fn invalid_request(message: &str) -> Error {
    Error::bad_request("invalid_request", message.to_owned())
}

/// OAuth `invalid_grant`: the code or refresh token doesn't work.
fn invalid_grant(message: &str) -> Error {
    Error::new(ErrorKind::BadRequest, "invalid_grant", message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_redirects_to_this_device_are_allowed() {
        for good in [
            "http://127.0.0.1:53682/callback",
            "http://127.0.0.1:8080",
            "http://[::1]:1234/cb?x=1",
            "http://localhost:4000/",
            "org.rustcloud.app:/oauth",
        ] {
            assert!(check_redirect_uri(good).is_ok(), "{good}");
        }
        for bad in [
            "https://evil.example/callback",
            "http://127.0.0.1.evil.example:80/",
            "http://127.0.0.1/no-port",
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:80@evil.example/",
            "http://localhost:80#frag",
            "rustcloud:/no-dot",
            "javascript:alert(1)",
            "com.example.app://host/path",
            "Com.Example:/upper",
            "",
        ] {
            assert!(check_redirect_uri(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn device_names_say_device_and_app() {
        let request = |device: Option<&str>| AuthorizeRequest {
            client_id: " RustCloud Desktop ".into(),
            device_name: device.map(str::to_owned),
            redirect_uri: String::new(),
            code_challenge: String::new(),
            code_challenge_method: None,
        };
        assert_eq!(device_name(&request(None)).unwrap(), "RustCloud Desktop");
        assert_eq!(device_name(&request(Some("Anna\n laptop"))).unwrap(), "Anna  laptop (RustCloud Desktop)");
        assert!(device_name(&AuthorizeRequest { client_id: "  ".into(), ..request(None) }).is_err());
    }
}
