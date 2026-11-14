//! Tokens apps give other servers (since API 0.2.14).
//!
//! An editor such as Collabora Online calls the app's routes back (WOPI) for
//! the file the user opened: it has no session of the user. The app asks the
//! server for a token for that one file; the other server sends it as
//! `?access_token=…` or `Authorization: Bearer …`, and the server calls the app
//! as the user, reaching only that file.
//!
//! A token is signed with a key kept in the data folder, so it outlives a
//! restart. It works while it hasn't run out, the browser session it was
//! made in lasts, and the user's access that the app had for the file (the
//! grant) is in force; each call keeps a session of
//! an opened file alive, as its page would.

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_core::{Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_links::{hmac_sha256, same};

/// The longest a token lasts.
pub const TOKEN_MAX_SECONDS: u32 = 12 * 3600;

/// What a token holds. A prefix keeps it apart from the links' tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenClaims {
    /// The app.
    pub a: String,
    /// The user.
    pub u: Uuid,
    /// The grant the app reaches the file through.
    pub g: Uuid,
    /// The file.
    pub f: Uuid,
    /// It may change the file.
    pub w: bool,
    /// Until when, in Unix seconds.
    pub x: i64,
    /// The browser session it was made in, if any: logging out there (or a
    /// new password) ends the token too, as it does the links of the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<Uuid>,
}

impl TokenClaims {
    pub fn expires_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.x, 0).unwrap_or_default()
    }
}

/// Signs and checks tokens.
pub struct AppTokens {
    key: [u8; 32],
}

const PREFIX: &str = "rct1.";

impl AppTokens {
    /// With the key in `dir/app-tokens.key`, made on first start.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("app-tokens.key");
        if !path.exists() {
            create_key(dir, &path)?;
        }
        let bytes = std::fs::read(&path).map_err(|err| Error::internal(format!("{}: {err}", path.display())))?;
        let key = bytes.try_into().map_err(|_| Error::internal(format!("{} is not a key", path.display())))?;
        Ok(Self { key })
    }

    /// A key of its own, for tests.
    pub fn new_random() -> Self {
        Self { key: rand::random() }
    }

    pub fn issue(&self, claims: &TokenClaims) -> Result<String> {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(Error::internal)?);
        let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(&self.key, payload.as_bytes()));
        Ok(format!("{PREFIX}{payload}.{signature}"))
    }

    /// What a token holds, if it is one of ours and hasn't run out.
    pub fn check(&self, token: &str) -> Option<TokenClaims> {
        let (payload, signature) = token.strip_prefix(PREFIX)?.split_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        if !same(&signature, &hmac_sha256(&self.key, payload.as_bytes())) {
            return None;
        }
        let claims: TokenClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        (claims.x > Utc::now().timestamp()).then_some(claims)
    }
}

/// The token of a request: `?access_token=` (WOPI) or `Authorization: Bearer`,
/// if it looks like ours.
pub fn token_of(query: Option<&str>, authorization: Option<&str>) -> Option<String> {
    let from_query = query.and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(name, _)| name == "access_token")
            .map(|(_, v)| v.into_owned())
    });
    let from_header = authorization.and_then(|value| value.strip_prefix("Bearer ")).map(str::to_owned);
    from_query.or(from_header).filter(|token| token.starts_with(PREFIX))
}

/// Writes a new key to `path`, unless another start did it at the same
/// moment: the key is written in full to a file of its own first, then linked
/// into place, which never replaces a key already there.
fn create_key(dir: &Path, path: &Path) -> Result<()> {
    use std::io::Write;
    let failed = |err: std::io::Error| Error::internal(format!("{}: {err}", path.display()));
    let key: [u8; 32] = rand::random();
    // A new file is only readable by its owner (tempfile makes it 0600).
    let mut new = tempfile::NamedTempFile::new_in(dir).map_err(failed)?;
    new.write_all(&key).map_err(failed)?;
    new.as_file().sync_all().map_err(failed)?;
    match std::fs::hard_link(new.path(), path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(failed(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(x: i64) -> TokenClaims {
        TokenClaims { a: "office".into(), u: Uuid::new_v4(), g: Uuid::new_v4(), f: Uuid::new_v4(), w: true, x, s: None }
    }

    #[test]
    fn tokens_are_checked_and_run_out() {
        let tokens = AppTokens::new_random();
        let valid = claims(Utc::now().timestamp() + 60);
        let token = tokens.issue(&valid).unwrap();
        assert_eq!(tokens.check(&token), Some(valid));
        assert!(tokens.check(&tokens.issue(&claims(Utc::now().timestamp() - 1)).unwrap()).is_none(), "ran out");
        assert!(AppTokens::new_random().check(&token).is_none(), "another key");
        let mut forged = token.clone();
        forged.insert(10, 'x');
        assert!(tokens.check(&forged).is_none());
    }

    #[test]
    fn the_token_comes_from_the_query_or_the_header() {
        let header = Some("Bearer rct1.abc");
        assert_eq!(token_of(Some("a=1&access_token=rct1.xyz"), header).as_deref(), Some("rct1.xyz"));
        assert_eq!(token_of(None, header).as_deref(), Some("rct1.abc"));
        assert_eq!(token_of(Some("access_token=other"), Some("Bearer api-token")), None, "not ours");
    }

    #[test]
    fn starts_at_the_same_moment_share_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let token = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..8).map(|_| scope.spawn(|| AppTokens::open(dir.path()).unwrap())).collect();
            let tokens: Vec<AppTokens> = starts.into_iter().map(|start| start.join().unwrap()).collect();
            let token = tokens[0].issue(&claims(Utc::now().timestamp() + 60)).unwrap();
            assert!(tokens.iter().all(|tokens| tokens.check(&token).is_some()));
            token
        });
        assert!(AppTokens::open(dir.path()).unwrap().check(&token).is_some());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "only the key is left");
    }

    #[test]
    fn the_key_is_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let token = AppTokens::open(dir.path()).unwrap().issue(&claims(Utc::now().timestamp() + 60)).unwrap();
        assert!(AppTokens::open(dir.path()).unwrap().check(&token).is_some());
    }
}
