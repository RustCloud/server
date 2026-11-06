//! Accounts of external login methods (OpenID Connect): the first login
//! finds or creates the account, later logins find it by the identity.
//!
//! The method itself (redirects, tokens) lives elsewhere; it ends with an
//! [`ExternalLogin`] — "this service vouches for this person" — and
//! [`Auth::sign_in_external`] does the rest the same way for every service.

use chrono::{DateTime, Utc};
use rustcloud_core::{audit, validate, Error, ErrorKind, Result};
use rustcloud_db::{identity, user};
use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, TransactionTrait,
};
use serde::Serialize;
use uuid::Uuid;

use crate::provider::{Identity, PasswordProvider};
use crate::service::{clean_display_name, clean_email, Auth, NewSession, User};

/// A person as an external login service confirmed them.
#[derive(Debug, Clone, Default)]
pub struct ExternalLogin {
    /// `("oidc:<provider>", "<sub>")`.
    pub identity: Identity,
    /// What the service calls them (`preferred_username`); a wish for the
    /// username of a new account, nothing more.
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// Whether the service checked that the email is theirs.
    pub email_verified: bool,
    /// Create an account for someone unknown.
    pub auto_create: bool,
    /// Take the account with the same email, if the email is verified and
    /// exactly one account has it.
    pub link_by_email: bool,
    /// `Some` when the service decides administrator rights (by groups).
    pub admin: Option<bool>,
}

/// A way to log in to the caller's account, as the settings page lists it.
#[derive(Debug, Clone, Serialize)]
pub struct LinkedLogin {
    pub id: Uuid,
    /// `password` or `oidc:<provider>`.
    pub provider: String,
    pub created_at: DateTime<Utc>,
}

impl Auth {
    /// Logs in someone an external service vouches for: finds their account
    /// by the identity, or (first login) by email or by creating one, as the
    /// service's rules allow. Then applies the service's verdict on
    /// administrator rights and starts a session.
    pub async fn sign_in_external(&self, login: &ExternalLogin) -> Result<NewSession> {
        let method = login.identity.provider.as_str();
        let user_id = match self.identity_owner(&login.identity).await? {
            Some(user_id) => user_id,
            None => self.first_external_login(login).await?,
        };
        if let Some(admin) = login.admin {
            self.set_admin_from_service(user_id, admin, method).await?;
        }
        let session = self.sign_in(&login.identity).await?;
        audit!("auth.login", user = session.user.username, method, "logged in");
        Ok(session)
    }

    /// Adds an external identity to an account (the user asked for it from
    /// the settings page and then logged in at the service).
    pub async fn link_identity(&self, user_id: Uuid, identity: &Identity) -> Result<()> {
        match self.identity_owner(identity).await? {
            Some(owner) if owner == user_id => return Ok(()),
            Some(_) => {
                return Err(Error::conflict("LOGIN_TAKEN", "This login already belongs to another account"));
            }
            None => {}
        }
        insert_identity(&self.db, user_id, identity).await?;
        let user = self.find_user(user_id).await?;
        audit!("auth.link", account = user.username, method = identity.provider, "login method added");
        Ok(())
    }

    /// The ways to log in to an account, oldest first.
    pub async fn linked_logins(&self, user_id: Uuid) -> Result<Vec<LinkedLogin>> {
        let rows = identity::Entity::find()
            .filter(identity::Column::UserId.eq(user_id))
            .order_by_asc(identity::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| LinkedLogin { id: row.id, provider: row.provider, created_at: row.created_at })
            .collect())
    }

    /// Removes an external login from the caller's account. The password and
    /// the last way in can't be removed.
    pub async fn unlink_identity(&self, user: &User, id: Uuid) -> Result<()> {
        let txn = self.db.begin().await?;
        let row = identity::Entity::find_by_id(id)
            .filter(identity::Column::UserId.eq(user.id))
            .one(&txn)
            .await?
            .ok_or_else(|| Error::not_found("LOGIN_NOT_FOUND", "No such login method"))?;
        if row.provider == PasswordProvider::NAME {
            return Err(Error::bad_request("CANNOT_UNLINK_PASSWORD", "The password can't be removed here"));
        }
        let count = identity::Entity::find().filter(identity::Column::UserId.eq(user.id)).count(&txn).await?;
        if count < 2 {
            return Err(Error::bad_request("LAST_LOGIN", "This is the only way to log in to the account"));
        }
        identity::Entity::delete_by_id(id).exec(&txn).await?;
        txn.commit().await?;
        audit!("auth.unlink", account = user.username, method = row.provider, "login method removed");
        Ok(())
    }

    /// Forgets every identity of a login method (its service was removed).
    /// Accounts stay; their owners log in another way.
    pub async fn forget_identities(&self, provider: &str) -> Result<u64> {
        let deleted =
            identity::Entity::delete_many().filter(identity::Column::Provider.eq(provider)).exec(&self.db).await?;
        Ok(deleted.rows_affected)
    }

    async fn identity_owner(&self, identity: &Identity) -> Result<Option<Uuid>> {
        Ok(identity::Entity::find()
            .filter(identity::Column::Provider.eq(&identity.provider))
            .filter(identity::Column::Subject.eq(&identity.subject))
            .one(&self.db)
            .await?
            .map(|row| row.user_id))
    }

    /// The account of someone logging in through a service for the first time.
    async fn first_external_login(&self, login: &ExternalLogin) -> Result<Uuid> {
        // One at a time: two logins of the same person must not make two accounts.
        let _guard = self.registration.lock().await;
        if let Some(user_id) = self.identity_owner(&login.identity).await? {
            return Ok(user_id);
        }
        let email = clean_email(login.email.as_deref()).ok().flatten();
        let method = login.identity.provider.as_str();

        if let Some(email) = email.as_deref().filter(|_| login.link_by_email && login.email_verified) {
            let lower = Func::lower(Expr::col(user::Column::Email));
            let owners = user::Entity::find()
                .filter(Expr::expr(lower).eq(email.to_lowercase()))
                // Two are enough to tell "exactly one" from "several".
                .limit(2)
                .all(&self.db)
                .await?;
            if let [owner] = owners.as_slice() {
                insert_identity(&self.db, owner.id, &login.identity).await?;
                audit!("auth.link", account = owner.username, method, "login method added by email");
                return Ok(owner.id);
            }
        }
        if !login.auto_create {
            return Err(Error::forbidden("There is no account for this login").with_code("NO_ACCOUNT"));
        }

        let txn = self.db.begin().await?;
        let username = free_username(&txn, login).await?;
        let display_name = clean_display_name(login.display_name.as_deref()).ok().flatten();
        let row = user::ActiveModel {
            id: Set(Uuid::new_v4()),
            username: Set(username.clone()),
            display_name: Set(display_name.unwrap_or_else(|| username.clone())),
            email: Set(email),
            is_admin: Set(false),
            disabled: Set(false),
            quota_bytes: Set(self.settings.read(crate::AccountSettings::default_quota_bytes)),
            created_at: Set(Utc::now()),
        }
        .insert(&txn)
        .await?;
        insert_identity(&txn, row.id, &login.identity).await?;
        txn.commit().await?;
        audit!("account.create", account = username, method, "account created");
        Ok(row.id)
    }

    /// Makes an account an administrator or not, as its service decided.
    /// The last administrator is never demoted this way: the server would
    /// lose its last way to be managed.
    async fn set_admin_from_service(&self, user_id: Uuid, admin: bool, method: &str) -> Result<()> {
        let user = self.find_user(user_id).await?;
        if user.is_admin == admin {
            return Ok(());
        }
        if !admin {
            let admins = user::Entity::find()
                .filter(user::Column::IsAdmin.eq(true))
                .filter(user::Column::Disabled.eq(false))
                .count(&self.db)
                .await?;
            if admins <= 1 {
                audit!(warn, "account.update", account = user.username, method, "not demoted: the last administrator");
                return Ok(());
            }
        }
        let mut row: user::ActiveModel = self.find_row(user_id).await?.into();
        row.is_admin = Set(admin);
        row.update(&self.db).await?;
        audit!(
            "account.update",
            account = user.username,
            is_admin = admin,
            method,
            "account changed by its login service"
        );
        Ok(())
    }
}

async fn insert_identity(conn: &impl ConnectionTrait, user_id: Uuid, identity: &Identity) -> Result<()> {
    identity::ActiveModel {
        id: Set(Uuid::new_v4()),
        user_id: Set(user_id),
        provider: Set(identity.provider.clone()),
        subject: Set(identity.subject.clone()),
        secret_hash: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(conn)
    .await
    .map_err(|err| match Error::from(err) {
        e if e.kind == ErrorKind::Conflict => {
            Error::conflict("LOGIN_TAKEN", "This login already belongs to an account")
        }
        e => e,
    })?;
    Ok(())
}

/// A username for a new account: from what the service calls the person,
/// their email or name, made valid, with a number added if it is taken.
async fn free_username(conn: &impl ConnectionTrait, login: &ExternalLogin) -> Result<String> {
    let local_part = login.email.as_deref().and_then(|email| email.split('@').next());
    let wishes = [login.username.as_deref(), local_part, login.display_name.as_deref()];
    let base = wishes.into_iter().flatten().find_map(username_from).unwrap_or_else(|| "user".to_owned());
    for n in 1..=1000u32 {
        let candidate = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        let taken = user::Entity::find().filter(user::Column::Username.eq(&candidate)).count(conn).await? > 0;
        if !taken {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}-{}", &Uuid::new_v4().simple().to_string()[..8]))
}

/// `text` as a valid username (see [`validate::username`]) with room for a
/// `-<number>` suffix; `None` if too little of it is left.
pub(crate) fn username_from(text: &str) -> Option<String> {
    let mut name = String::new();
    for c in text.trim().to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' };
        if !(c == '-' && name.ends_with('-')) {
            name.push(c);
        }
    }
    let name: String = name.trim_matches(|c: char| !c.is_ascii_alphanumeric()).chars().take(27).collect();
    let name = name.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_owned();
    validate::username(&name).ok()
}

#[cfg(test)]
mod tests {
    use super::username_from;

    #[test]
    fn usernames_are_made_valid() {
        assert_eq!(username_from("Alice").as_deref(), Some("alice"));
        assert_eq!(username_from("john.smith").as_deref(), Some("john.smith"));
        assert_eq!(username_from("Иван Петров").as_deref(), None, "nothing valid is left");
        assert_eq!(username_from("Anna Maria O'Neil").as_deref(), Some("anna-maria-o-neil"));
        assert_eq!(username_from("__x__").as_deref(), None, "too short");
        assert_eq!(username_from(&"a".repeat(40)).map(|n| n.len()), Some(27));
    }
}
