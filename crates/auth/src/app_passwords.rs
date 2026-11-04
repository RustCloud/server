//! App passwords: one per device or program that logs in with HTTP Basic
//! authentication (CalDAV, CardDAV, WebDAV clients), so it never gets the
//! account password. Each has its own scopes — which apps, read or write —
//! and is revoked on its own, like access tokens of Git hostings.
//!
//! The password is random and shown once; the database keeps its SHA-256.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use rustcloud_core::{audit, Error, ErrorKind, Result};
use rustcloud_db::app_password;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::service::{Auth, User};
use crate::{secret, IP_FAILURE_FACTOR};

/// Most app passwords per user.
const MAX_PER_USER: usize = 50;
/// `last_used_at` is written at most this often, not on every request.
const LAST_USED_STEP: chrono::Duration = chrono::Duration::minutes(10);

/// What a password may do in one app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

/// One app a password opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub app: String,
    pub access: Access,
}

/// An app password as listed to its owner (without the password).
#[derive(Debug, Clone, Serialize)]
pub struct AppPassword {
    pub id: Uuid,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A new app password, as the user fills it in.
#[derive(Debug, Clone, Deserialize)]
pub struct NewAppPassword {
    pub name: String,
    pub scopes: Vec<Scope>,
}

/// A freshly created app password: the only time the password is shown.
#[derive(Debug, Clone, Serialize)]
pub struct CreatedAppPassword {
    pub password: String,
    #[serde(flatten)]
    pub info: AppPassword,
}

/// The user behind an app password, with what the password allows.
#[derive(Debug, Clone)]
pub struct AppPasswordUser {
    pub user: User,
    pub password_id: Uuid,
    pub scopes: Vec<Scope>,
}

impl AppPasswordUser {
    /// What the password may do in `app`; `None`: nothing.
    pub fn access(&self, app: &str) -> Option<Access> {
        self.scopes.iter().find(|scope| scope.app == app).map(|scope| scope.access)
    }
}

impl Auth {
    /// The user's app passwords, newest first.
    pub async fn app_passwords(&self, user: &User) -> Result<Vec<AppPassword>> {
        let rows = app_password::Entity::find()
            .filter(app_password::Column::UserId.eq(user.id))
            .order_by_desc(app_password::Column::CreatedAt)
            .all(&self.db)
            .await?;
        rows.into_iter().map(AppPassword::try_from).collect()
    }

    /// Creates an app password. The caller checks that the scoped apps exist.
    pub async fn create_app_password(&self, user: &User, new: NewAppPassword) -> Result<CreatedAppPassword> {
        let name = new.name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err(Error::bad_request("INVALID_NAME", "Give the password a name of up to 100 characters"));
        }
        if new.scopes.is_empty() {
            return Err(Error::bad_request("INVALID_SCOPES", "Choose at least one app"));
        }
        for (i, scope) in new.scopes.iter().enumerate() {
            let valid = !scope.app.is_empty() && !scope.app.contains([' ', ':']);
            if !valid || new.scopes[..i].iter().any(|other| other.app == scope.app) {
                return Err(Error::bad_request("INVALID_SCOPES", "Each app may be listed once"));
            }
        }
        if self.app_passwords(user).await?.len() >= MAX_PER_USER {
            return Err(Error::bad_request("TOO_MANY_APP_PASSWORDS", "Revoke an unused app password first"));
        }

        let password = secret::new_token();
        let row = app_password::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(user.id),
            name: Set(name.to_owned()),
            token_hash: Set(secret::hash_token(&password)),
            scopes: Set(scopes_to_string(&new.scopes)),
            created_at: Set(Utc::now()),
            last_used_at: Set(None),
        }
        .insert(&self.db)
        .await?;
        let scopes = row.scopes.clone();
        audit!("app_password.create", name = row.name, scopes, "app password created");
        Ok(CreatedAppPassword { password, info: row.try_into()? })
    }

    /// Revokes one of the user's app passwords.
    pub async fn revoke_app_password(&self, user: &User, id: Uuid) -> Result<()> {
        let row = app_password::Entity::find_by_id(id)
            .filter(app_password::Column::UserId.eq(user.id))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("APP_PASSWORD_NOT_FOUND", "App password not found"))?;
        app_password::Entity::delete_by_id(id).exec(&self.db).await?;
        audit!("app_password.revoke", name = row.name, "app password revoked");
        Ok(())
    }

    /// Checks Basic credentials: a username and one of that user's app
    /// passwords. Wrong ones are `Ok(None)` and count as failed logins.
    pub async fn app_password_user(
        &self,
        username: &str,
        password: &str,
        ip: Option<IpAddr>,
    ) -> Result<Option<AppPasswordUser>> {
        let max_failures = self.settings.read(|s| s.max_login_failures);
        let account_key = format!("app-password:{}", username.trim().to_ascii_lowercase());
        let ip_key = ip.map(|ip| format!("ip:{ip}"));
        let ip_allowed = ip_key
            .as_deref()
            .is_none_or(|key| self.limiter.is_allowed(key, max_failures.saturating_mul(IP_FAILURE_FACTOR)));
        if !ip_allowed || !self.limiter.is_allowed(&account_key, max_failures) {
            return Err(Error::new(
                ErrorKind::TooManyRequests,
                "TOO_MANY_ATTEMPTS",
                "Too many failed logins, try again later",
            ));
        }

        let row = app_password::Entity::find()
            .filter(app_password::Column::TokenHash.eq(secret::hash_token(password)))
            .one(&self.db)
            .await?;
        let user = match &row {
            Some(row) => Some(self.find_user(row.user_id).await?),
            None => None,
        };
        let (Some(row), Some(user)) = (row, user.filter(|user| user.username.eq_ignore_ascii_case(username.trim())))
        else {
            let ip = ip.map(|ip| ip.to_string());
            audit!(warn, "auth.login_failed", user = username, ip, method = "app-password", "failed login");
            self.limiter.record_failure(&account_key);
            if let Some(key) = &ip_key {
                self.limiter.record_failure(key);
            }
            return Ok(None);
        };
        if user.disabled {
            return Ok(None);
        }
        self.limiter.record_success(&account_key);

        let password_id = row.id;
        let scopes = parse_scopes(&row.scopes)?;
        if row.last_used_at.is_none_or(|at| Utc::now() - at > LAST_USED_STEP) {
            let mut row = row.into_active_model();
            row.last_used_at = Set(Some(Utc::now()));
            row.update(&self.db).await?;
        }
        Ok(Some(AppPasswordUser { user, password_id, scopes }))
    }
}

impl TryFrom<app_password::Model> for AppPassword {
    type Error = Error;

    fn try_from(row: app_password::Model) -> Result<Self> {
        Ok(Self {
            scopes: parse_scopes(&row.scopes)?,
            id: row.id,
            name: row.name,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        })
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
        })
    }
}

impl FromStr for Access {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            _ => Err(Error::internal(format!("unknown app password access `{s}`"))),
        }
    }
}

/// Stored form of scopes: `calendar:write notes:read`.
fn scopes_to_string(scopes: &[Scope]) -> String {
    scopes.iter().map(|scope| format!("{}:{}", scope.app, scope.access)).collect::<Vec<_>>().join(" ")
}

fn parse_scopes(text: &str) -> Result<Vec<Scope>> {
    text.split_whitespace()
        .map(|item| {
            let (app, access) =
                item.split_once(':').ok_or_else(|| Error::internal(format!("invalid app password scope `{item}`")))?;
            Ok(Scope { app: app.to_owned(), access: access.parse()? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_survive_storage() {
        let scopes = vec![
            Scope { app: "calendar".into(), access: Access::Write },
            Scope { app: "notes".into(), access: Access::Read },
        ];
        let text = scopes_to_string(&scopes);
        assert_eq!(text, "calendar:write notes:read");
        assert_eq!(parse_scopes(&text).unwrap(), scopes);
    }
}
