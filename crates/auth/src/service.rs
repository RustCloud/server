//! [`Auth`]: accounts, login and sessions.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustcloud_core::{audit, validate, Error, ErrorKind, Live, Result};
use rustcloud_db::{app_password, identity, session, user};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::limiter::AttemptLimiter;
use crate::provider::{CredentialProvider, Identity, PasswordProvider};
use crate::secret;
use crate::settings::AccountSettings;

/// How long failed logins are remembered.
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// One IP address may fail this many times more often than one account
/// (it may be shared by many users, e.g. behind NAT).
pub(crate) const IP_FAILURE_FACTOR: u32 = 5;
/// Failed logins of one account that cost nothing; after them every next
/// attempt waits longer.
pub(crate) const FREE_LOGIN_FAILURES: u32 = 3;

/// A user account as returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
    pub quota_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
}

impl From<user::Model> for User {
    fn from(row: user::Model) -> Self {
        Self {
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            email: row.email,
            is_admin: row.is_admin,
            disabled: row.disabled,
            quota_bytes: row.quota_bytes,
            created_at: row.created_at,
        }
    }
}

/// Data for a new account.
#[derive(Debug, Clone, Deserialize)]
pub struct NewAccount {
    pub username: String,
    pub password: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
}

/// Changes a user may make to their own profile. Missing fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileUpdate {
    pub display_name: Option<String>,
    /// An empty string removes the email.
    pub email: Option<String>,
}

/// Changes an administrator may make to an account. Missing fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AccountUpdate {
    pub display_name: Option<String>,
    pub is_admin: Option<bool>,
    pub disabled: Option<bool>,
    /// `Some(None)` (JSON `null`) removes the limit.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub quota_bytes: Option<Option<i64>>,
    pub password: Option<String>,
}

/// What a client needs before login: which forms to show.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    /// No accounts exist yet; the first registration creates the administrator.
    pub needs_setup: bool,
    pub registration_open: bool,
    /// Names of the available login methods.
    pub providers: Vec<String>,
}

/// A freshly created session. The token is shown to the client once and never stored.
#[derive(Debug, Clone, Serialize)]
pub struct NewSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub user: User,
}

/// The user behind a valid session token.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub session_id: Uuid,
    pub user: User,
}

/// Accounts, login methods and sessions.
pub struct Auth {
    pub(crate) db: DatabaseConnection,
    pub(crate) settings: Arc<Live<AccountSettings>>,
    passwords: Arc<PasswordProvider>,
    providers: Vec<Arc<dyn CredentialProvider>>,
    pub(crate) limiter: AttemptLimiter,
    /// Serialises registrations so that exactly one first user becomes admin.
    registration: Mutex<()>,
}

impl Auth {
    /// Creates the auth service with password login enabled. `settings` may
    /// change at any time; every request uses the current values.
    pub fn new(db: DatabaseConnection, settings: Arc<Live<AccountSettings>>) -> Self {
        let passwords = Arc::new(PasswordProvider::new(db.clone()));
        Self {
            limiter: AttemptLimiter::new(LOGIN_WINDOW),
            providers: vec![passwords.clone()],
            passwords,
            registration: Mutex::new(()),
            settings,
            db,
        }
    }

    /// Adds another login method (e.g. LDAP).
    pub fn with_provider(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    pub async fn status(&self) -> Result<AuthStatus> {
        let needs_setup = user::Entity::find().count(&self.db).await? == 0;
        Ok(AuthStatus {
            needs_setup,
            registration_open: needs_setup || self.settings.read(|s| s.open_registration),
            providers: self.providers.iter().map(|p| p.name().to_owned()).collect(),
        })
    }

    /// Self-service registration. The first account ever becomes an
    /// administrator; after that, registration must be enabled in the config.
    pub async fn register(&self, account: NewAccount) -> Result<User> {
        let _guard = self.registration.lock().await;
        let is_first = user::Entity::find().count(&self.db).await? == 0;
        if !is_first && !self.settings.read(|s| s.open_registration) {
            return Err(Error::forbidden("Registration is closed").with_code("REGISTRATION_CLOSED"));
        }
        self.create_user(account, is_first).await
    }

    /// Creates the first administrator, if there are no accounts yet (from
    /// the server's environment, so nobody has to race to the setup page).
    /// `None` when accounts already exist.
    pub async fn create_first_admin(&self, account: NewAccount) -> Result<Option<User>> {
        let _guard = self.registration.lock().await;
        if user::Entity::find().count(&self.db).await? > 0 {
            return Ok(None);
        }
        self.create_user(account, true).await.map(Some)
    }

    /// Creates an account with a password (used by registration and by admins).
    pub async fn create_user(&self, account: NewAccount, is_admin: bool) -> Result<User> {
        let username = validate::username(&account.username)?;
        validate::password(&account.password)?;
        let display_name = clean_display_name(account.display_name.as_deref())?.unwrap_or_else(|| username.clone());
        let email = clean_email(account.email.as_deref())?;

        let txn = self.db.begin().await?;
        let row = user::ActiveModel {
            id: Set(Uuid::new_v4()),
            username: Set(username.clone()),
            display_name: Set(display_name),
            email: Set(email),
            is_admin: Set(is_admin),
            disabled: Set(false),
            quota_bytes: Set(self.settings.read(AccountSettings::default_quota_bytes)),
            created_at: Set(Utc::now()),
        }
        .insert(&txn)
        .await
        .map_err(|err| match Error::from(err) {
            e if e.kind == ErrorKind::Conflict => Error::conflict("USERNAME_TAKEN", "This username is taken"),
            e => e,
        })?;
        self.passwords.set_password(&txn, row.id, &username, &account.password).await?;
        txn.commit().await?;

        audit!("account.create", account = username, is_admin, "account created");
        Ok(row.into())
    }

    /// Logs in with a login method (`"password"` unless another is configured).
    ///
    /// Failed attempts are counted per account and per client IP (when known).
    /// After three failures an account's next attempts wait longer and
    /// longer (up to a minute); an address over its limit is blocked for a
    /// while.
    pub async fn login(&self, provider: &str, login: &str, secret: &str, ip: Option<IpAddr>) -> Result<NewSession> {
        let provider = self
            .providers
            .iter()
            .find(|p| p.name() == provider)
            .ok_or_else(|| Error::bad_request("UNKNOWN_PROVIDER", "Unknown login method"))?;

        // An IP address is blocked for a while after too many failures; an
        // account only makes its next attempts wait (see
        // `AttemptLimiter::try_start_delayed`), so nobody can lock its owner out.
        let max_failures = self.settings.read(|s| s.max_login_failures);
        let account_key = format!("{}:{}", provider.name(), login.trim().to_ascii_lowercase());
        let ip_key = ip.map(|ip| format!("ip:{ip}"));
        if let Some(key) = &ip_key {
            if !self.limiter.try_start(key, max_failures.saturating_mul(IP_FAILURE_FACTOR)) {
                return Err(too_many_attempts("Too many failed logins, try again later"));
            }
        }
        if let Err(left) = self.limiter.try_start_delayed(&account_key, FREE_LOGIN_FAILURES) {
            if let Some(key) = &ip_key {
                self.limiter.forgive(key);
            }
            let seconds = left.as_secs_f64().ceil();
            return Err(too_many_attempts(format!("Too many failed logins, try again in {seconds} s")));
        }

        let wrong = || Error::unauthorized("Wrong username or password").with_code("INVALID_CREDENTIALS");
        let Some(identity) = provider.verify(login, secret).await? else {
            // Logged with the address, so tools like fail2ban can act on it.
            let ip = ip.map(|ip| ip.to_string());
            audit!(warn, "auth.login_failed", user = login, ip, "failed login");
            return Err(wrong());
        };
        self.limiter.record_success(&account_key);
        if let Some(key) = &ip_key {
            self.limiter.forgive(key);
        }
        // A disabled account answers like a wrong password: whether the
        // password was right would help someone who is guessing it.
        let session = match self.sign_in(&identity).await {
            Err(err) if err.code == "ACCOUNT_DISABLED" => return Err(wrong()),
            other => other?,
        };
        let ip = ip.map(|ip| ip.to_string());
        audit!("auth.login", user = session.user.username, ip, method = provider.name(), "logged in");
        Ok(session)
    }

    /// Starts a session for a verified identity. Every login method ends here.
    pub async fn sign_in(&self, identity: &Identity) -> Result<NewSession> {
        let account = identity::Entity::find()
            .filter(identity::Column::Provider.eq(&identity.provider))
            .filter(identity::Column::Subject.eq(&identity.subject))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::unauthorized("No account for this login").with_code("INVALID_CREDENTIALS"))?;
        let user = self.find_user(account.user_id).await?;
        if user.disabled {
            return Err(Error::forbidden("This account is disabled").with_code("ACCOUNT_DISABLED"));
        }

        // Drop this user's expired sessions while we are here.
        session::Entity::delete_many()
            .filter(session::Column::UserId.eq(user.id))
            .filter(session::Column::ExpiresAt.lt(Utc::now()))
            .exec(&self.db)
            .await?;

        let token = secret::new_token();
        let session_days = self.settings.read(|s| s.session_days);
        let expires_at = Utc::now() + chrono::Duration::days(i64::from(session_days));
        session::ActiveModel {
            id: Set(Uuid::new_v4()),
            token_hash: Set(secret::hash_token(&token)),
            user_id: Set(user.id),
            created_at: Set(Utc::now()),
            expires_at: Set(expires_at),
        }
        .insert(&self.db)
        .await?;

        Ok(NewSession { token, expires_at, user })
    }

    /// Finds the user of a session token. Expired sessions and disabled users
    /// give `None`.
    pub async fn session_user(&self, token: &str) -> Result<Option<SessionUser>> {
        let Some(session) = session::Entity::find()
            .filter(session::Column::TokenHash.eq(secret::hash_token(token)))
            .filter(session::Column::ExpiresAt.gt(Utc::now()))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let user = self.find_user(session.user_id).await?;
        if user.disabled {
            return Ok(None);
        }
        Ok(Some(SessionUser { session_id: session.id, user }))
    }

    /// The user of a session that is still valid, by the session's id (links
    /// for app pages carry it, never the token).
    pub async fn session_by_id(&self, session_id: Uuid) -> Result<Option<SessionUser>> {
        let Some(session) = session::Entity::find_by_id(session_id)
            .filter(session::Column::ExpiresAt.gt(Utc::now()))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let user = self.find_user(session.user_id).await?;
        Ok((!user.disabled).then_some(SessionUser { session_id: session.id, user }))
    }

    /// Ends one session.
    pub async fn logout(&self, session_id: Uuid) -> Result<()> {
        session::Entity::delete_by_id(session_id).exec(&self.db).await?;
        audit!("auth.logout", "logged out");
        Ok(())
    }

    pub async fn find_user(&self, user_id: Uuid) -> Result<User> {
        user::Entity::find_by_id(user_id)
            .one(&self.db)
            .await?
            .map(User::from)
            .ok_or_else(|| Error::not_found("USER_NOT_FOUND", "User not found"))
    }

    pub async fn update_profile(&self, user_id: Uuid, update: ProfileUpdate) -> Result<User> {
        let mut row: user::ActiveModel = self.find_row(user_id).await?.into();
        if let Some(name) = clean_display_name(update.display_name.as_deref())? {
            row.display_name = Set(name);
        }
        if let Some(email) = update.email.as_deref() {
            row.email = Set(clean_email(Some(email))?);
        }
        Ok(row.update(&self.db).await?.into())
    }

    /// Changes the caller's own password after checking the current one.
    /// All other sessions of the user are ended, and with
    /// `revoke_app_passwords` all app passwords too.
    /// Wrong current passwords count like failed logins, so a stolen session
    /// can't be used to guess the password.
    pub async fn change_password(
        &self,
        user: &SessionUser,
        current: &str,
        new: &str,
        revoke_app_passwords: bool,
    ) -> Result<()> {
        let key = format!("password-change:{}", user.user.id);
        if !self.limiter.try_start(&key, self.settings.read(|s| s.max_login_failures)) {
            return Err(too_many_attempts("Too many wrong passwords, try again later"));
        }
        let confirmed = self.passwords.verify(&user.user.username, current).await?;
        if confirmed.is_none() {
            return Err(Error::bad_request("WRONG_PASSWORD", "The current password is wrong"));
        }
        self.limiter.record_success(&key);
        validate::password(new)?;
        let txn = self.db.begin().await?;
        self.passwords.set_password(&txn, user.user.id, &user.user.username, new).await?;
        end_other_sessions(&txn, user).await?;
        if revoke_app_passwords {
            app_password::Entity::delete_many()
                .filter(app_password::Column::UserId.eq(user.user.id))
                .exec(&txn)
                .await?;
        }
        txn.commit().await?;
        audit!("auth.password_change", revoke_app_passwords, "password changed");
        Ok(())
    }

    /// Ends all sessions of the user except the current one ("log out on
    /// other devices"); returns how many were ended.
    pub async fn logout_others(&self, user: &SessionUser) -> Result<u64> {
        let ended = end_other_sessions(&self.db, user).await?;
        audit!("auth.logout_others", ended, "logged out on other devices");
        Ok(ended)
    }

    /// All accounts, oldest first (admin only).
    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows = user::Entity::find().order_by_asc(user::Column::CreatedAt).all(&self.db).await?;
        Ok(rows.into_iter().map(User::from).collect())
    }

    /// Changes an account (admin only). Admins cannot disable or demote
    /// themselves, so the server never loses its last way in by accident.
    pub async fn update_account(&self, actor: &User, user_id: Uuid, update: AccountUpdate) -> Result<User> {
        let current = self.find_row(user_id).await?;
        let is_self = actor.id == user_id;
        if is_self && (update.disabled == Some(true) || update.is_admin == Some(false)) {
            return Err(Error::bad_request("CANNOT_CHANGE_SELF", "You cannot disable or demote your own account"));
        }
        if let Some(password) = &update.password {
            validate::password(password)?;
        }
        if let Some(Some(quota)) = update.quota_bytes {
            if quota < 0 {
                return Err(Error::bad_request("INVALID_QUOTA", "Quota must not be negative"));
            }
        }

        let username = current.username.clone();
        let mut row: user::ActiveModel = current.into();
        if let Some(name) = clean_display_name(update.display_name.as_deref())? {
            row.display_name = Set(name);
        }
        if let Some(is_admin) = update.is_admin {
            row.is_admin = Set(is_admin);
        }
        if let Some(disabled) = update.disabled {
            row.disabled = Set(disabled);
        }
        if let Some(quota) = update.quota_bytes {
            row.quota_bytes = Set(quota);
        }

        let txn = self.db.begin().await?;
        let updated = row.update(&txn).await?;
        if let Some(password) = &update.password {
            self.passwords.set_password(&txn, user_id, &username, password).await?;
        }
        if update.disabled == Some(true) || update.password.is_some() {
            session::Entity::delete_many().filter(session::Column::UserId.eq(user_id)).exec(&txn).await?;
        }
        txn.commit().await?;
        // Only the changed fields are recorded (`None` leaves a field out).
        let quota = update.quota_bytes.map(|quota| quota.map_or_else(|| "unlimited".to_owned(), |b| b.to_string()));
        audit!(
            "account.update",
            account = updated.username,
            is_admin = update.is_admin,
            disabled = update.disabled,
            quota_bytes = quota.as_deref(),
            password_reset = update.password.is_some().then_some(true),
            "account changed"
        );
        Ok(updated.into())
    }

    async fn find_row(&self, user_id: Uuid) -> Result<user::Model> {
        user::Entity::find_by_id(user_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("USER_NOT_FOUND", "User not found"))
    }
}

/// Deletes the sessions of `user` other than the current one.
async fn end_other_sessions(db: &impl sea_orm::ConnectionTrait, user: &SessionUser) -> Result<u64> {
    let deleted = session::Entity::delete_many()
        .filter(session::Column::UserId.eq(user.user.id))
        .filter(session::Column::Id.ne(user.session_id))
        .exec(db)
        .await?;
    Ok(deleted.rows_affected)
}

/// The error of a refused attempt (HTTP 429).
pub(crate) fn too_many_attempts(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::TooManyRequests, "TOO_MANY_ATTEMPTS", message)
}

/// Trims a display name; `None` or blank means "not given".
fn clean_display_name(name: Option<&str>) -> Result<Option<String>> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(Error::bad_request("INVALID_DISPLAY_NAME", "Display name is invalid or too long"));
    }
    Ok(Some(name.to_owned()))
}

/// Trims an email; blank means "no email". Only a light sanity check is done:
/// the address is not used for anything yet.
fn clean_email(email: Option<&str>) -> Result<Option<String>> {
    let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(None);
    };
    let plausible = email.len() <= 254
        && email.split_once('@').is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    if !plausible {
        return Err(Error::bad_request("INVALID_EMAIL", "Email address is invalid"));
    }
    Ok(Some(email.to_owned()))
}

/// Lets `Option<Option<T>>` tell a missing field (`None`) from `null` (`Some(None)`).
fn deserialize_some<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}
