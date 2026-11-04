//! Login methods ("credential providers") — the strategy for *how* someone
//! proves who they are.
//!
//! A provider checks credentials and answers with an [`Identity`]: "provider X
//! vouches that this is account Y". Turning an identity into a user and a
//! session is the same for every provider and is done by
//! [`Auth::sign_in`](crate::Auth::sign_in).
//!
//! - [`PasswordProvider`] — username + password, hashes stored in `identities`.
//! - An LDAP provider would implement [`CredentialProvider`] the same way.
//! - Redirect-based logins (OIDC) don't fit a "check these credentials" call;
//!   their callback handler builds an [`Identity`] from the ID token and calls
//!   `Auth::sign_in` directly.

use async_trait::async_trait;
use chrono::Utc;
use rustcloud_core::Result;
use rustcloud_db::identity;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::secret;

/// An account as confirmed by a login method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Name of the login method, e.g. `"password"` or `"oidc:company"`.
    pub provider: String,
    /// Id of the account within that method, e.g. a username or an OIDC `sub`.
    pub subject: String,
}

/// A login method that checks a login and a secret.
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// Name used in the API and stored in `identities.provider`.
    fn name(&self) -> &str;

    /// Checks the credentials. Wrong credentials are `Ok(None)`, not an error.
    async fn verify(&self, login: &str, secret: &str) -> Result<Option<Identity>>;
}

/// Username + password login. The subject of a password identity is the
/// (lower-case) username.
pub struct PasswordProvider {
    db: DatabaseConnection,
}

impl PasswordProvider {
    pub const NAME: &'static str = "password";

    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// Sets (or replaces) the password of a user. `conn` may be a transaction.
    pub async fn set_password(
        &self,
        conn: &impl ConnectionTrait,
        user_id: Uuid,
        username: &str,
        password: &str,
    ) -> Result<()> {
        let hash = secret::hash_password(password).await?;
        match find_identity(conn, username).await? {
            Some(existing) => {
                let mut row: identity::ActiveModel = existing.into();
                row.secret_hash = Set(Some(hash));
                row.update(conn).await?;
            }
            None => {
                identity::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    user_id: Set(user_id),
                    provider: Set(Self::NAME.to_owned()),
                    subject: Set(username.to_owned()),
                    secret_hash: Set(Some(hash)),
                    created_at: Set(Utc::now()),
                }
                .insert(conn)
                .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CredentialProvider for PasswordProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn verify(&self, login: &str, password: &str) -> Result<Option<Identity>> {
        let username = login.trim().to_ascii_lowercase();
        let stored = find_identity(&self.db, &username).await?;
        let hash = stored.as_ref().and_then(|row| row.secret_hash.as_deref());

        // Always run the (slow) check, even for unknown users: see verify_password.
        if !secret::verify_password(password, hash).await? {
            return Ok(None);
        }
        Ok(Some(Identity { provider: Self::NAME.to_owned(), subject: username }))
    }
}

async fn find_identity(conn: &impl ConnectionTrait, username: &str) -> Result<Option<identity::Model>> {
    Ok(identity::Entity::find()
        .filter(identity::Column::Provider.eq(PasswordProvider::NAME))
        .filter(identity::Column::Subject.eq(username))
        .one(conn)
        .await?)
}
