//! Password hashing and session tokens.

use std::sync::LazyLock;
use std::time::Duration;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rustcloud_core::{Error, ErrorKind, Result};
use sha2::{Digest, Sha256};

/// Argon2 runs at once: each takes 19 MiB and a core, so a flood of logins
/// must not start hundreds of them. One per core.
static HASHING: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(2, |n| n.get()).max(2)));

/// How long a login waits for its turn before it is turned away with 429.
const HASHING_WAIT: Duration = Duration::from_secs(3);

/// A turn to run Argon2; `SERVER_BUSY` when none is free in time.
async fn hashing_turn() -> Result<tokio::sync::SemaphorePermit<'static>> {
    match tokio::time::timeout(HASHING_WAIT, HASHING.acquire()).await {
        Ok(Ok(turn)) => Ok(turn),
        _ => Err(Error::new(ErrorKind::TooManyRequests, "SERVER_BUSY", "The server is busy, try again in a moment")),
    }
}

/// Hashes a password with Argon2id (default parameters: 19 MiB, 2 passes),
/// on a blocking thread so the async runtime is not stalled.
pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_owned();
    let _turn = hashing_turn().await?;
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(Error::internal)?;
        let hash = Argon2::default().hash_password(password.as_bytes(), &salt).map_err(Error::internal)?;
        Ok(hash.to_string())
    })
    .await
    .map_err(Error::internal)?
}

/// Checks a password against a stored hash.
///
/// When there is no stored hash (unknown user), a dummy hash is checked
/// instead, so that "no such user" and "wrong password" take the same time
/// and cannot be told apart.
pub async fn verify_password(password: &str, hash: Option<&str>) -> Result<bool> {
    let password = password.to_owned();
    let hash = hash.map(str::to_owned);
    let _turn = hashing_turn().await?;
    tokio::task::spawn_blocking(move || {
        let Some(hash) = hash else {
            let dummy = PasswordHash::new(&DUMMY_HASH).map_err(Error::internal)?;
            let _ = Argon2::default().verify_password(password.as_bytes(), &dummy);
            return Ok(false);
        };
        let parsed = PasswordHash::new(&hash).map_err(Error::internal)?;
        Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .map_err(Error::internal)?
}

/// A hash of a random password, computed once on first use.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).expect("16 bytes is a valid salt");
    let password: [u8; 16] = rand::random();
    Argon2::default().hash_password(&password, &salt).expect("hashing a fixed-size input cannot fail").to_string()
});

/// A new random session token: 32 random bytes, base64url-encoded (43 chars).
pub fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// The form in which a token is stored: hex SHA-256. Tokens are random and
/// long, so a fast hash is enough (no need for Argon2 here).
pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifies_the_right_password_only() {
        let hash = hash_password("correct horse").await.unwrap();
        assert!(verify_password("correct horse", Some(&hash)).await.unwrap());
        assert!(!verify_password("wrong horse", Some(&hash)).await.unwrap());
    }

    #[tokio::test]
    async fn unknown_user_never_verifies() {
        assert!(!verify_password("anything", None).await.unwrap());
    }

    #[test]
    fn tokens_are_random_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn token_hash_is_stable_hex() {
        assert_eq!(hash_token("abc"), hash_token("abc"));
        assert_eq!(hash_token("abc").len(), 64);
        assert_ne!(hash_token("abc"), hash_token("abd"));
    }
}
