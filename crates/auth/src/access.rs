//! Access tokens of devices: short-lived JWTs (HS256) that a client sends as
//! `Authorization: Bearer …` and renews with its refresh token.
//!
//! Only this server reads them, so the format is fixed: the header is always
//! the same bytes (its `alg` is never read, so no "none" or other algorithms
//! sneak in), the key is random and lives in memory — after a restart old
//! access tokens stop working and clients renew them, which they do every few
//! minutes anyway.
//!
//! A token says only who and which session; rights come from the database on
//! every request, and the session must still exist, so revoking it works at
//! once.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::secret::{hmac_sha256, same};

/// How long an access token works.
pub const ACCESS_TIME: chrono::Duration = chrono::Duration::minutes(10);
/// `{"alg":"HS256","typ":"JWT"}`, the only header there is.
const HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
/// The audience: tokens of this server's API.
const AUDIENCE: &str = "rustcloud-api";

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    sid: Uuid,
    aud: String,
    iat: i64,
    exp: i64,
}

/// Who an access token is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessClaims {
    pub user_id: Uuid,
    pub session_id: Uuid,
}

/// Makes and checks access tokens.
pub struct AccessTokens {
    key: [u8; 32],
}

impl AccessTokens {
    /// With a new random key.
    pub fn new_random() -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self { key }
    }

    /// A token for `session_id` of `user_id`, valid for [`ACCESS_TIME`] from `now`.
    pub fn issue(&self, user_id: Uuid, session_id: Uuid, now: DateTime<Utc>) -> String {
        let claims = Claims {
            sub: user_id,
            sid: session_id,
            aud: AUDIENCE.to_owned(),
            iat: now.timestamp(),
            exp: (now + ACCESS_TIME).timestamp(),
        };
        // Serialising a struct of plain fields can't fail.
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
        let signed = format!("{HEADER}.{payload}");
        let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(&self.key, signed.as_bytes()));
        format!("{signed}.{signature}")
    }

    /// The claims of a token this server made that hasn't run out; `None`
    /// for anything else.
    pub fn check(&self, token: &str, now: DateTime<Utc>) -> Option<AccessClaims> {
        let (signed, signature) = token.rsplit_once('.')?;
        let (header, payload) = signed.split_once('.')?;
        if header != HEADER {
            return None;
        }
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        if !same(&signature, &hmac_sha256(&self.key, signed.as_bytes())) {
            return None;
        }
        let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        (claims.aud == AUDIENCE && claims.exp > now.timestamp())
            .then_some(AccessClaims { user_id: claims.sub, session_id: claims.sid })
    }
}

/// Whether a bearer token looks like an access token (a JWT) rather than an
/// opaque session token.
pub fn is_access_token(token: &str) -> bool {
    token.starts_with(HEADER) && token.matches('.').count() == 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_work_until_they_run_out_and_only_with_their_key() {
        let tokens = AccessTokens::new_random();
        let (user, session, now) = (Uuid::new_v4(), Uuid::new_v4(), Utc::now());
        let token = tokens.issue(user, session, now);
        assert!(is_access_token(&token));
        assert_eq!(tokens.check(&token, now), Some(AccessClaims { user_id: user, session_id: session }));
        assert_eq!(tokens.check(&token, now + ACCESS_TIME), None, "ran out");
        assert_eq!(AccessTokens::new_random().check(&token, now), None, "another key");

        // Any change breaks the signature, including another algorithm.
        let (_, rest) = token.split_once('.').unwrap();
        let none = format!("{}.{rest}", URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#));
        assert_eq!(tokens.check(&none, now), None);
        let (signed, _) = token.rsplit_once('.').unwrap();
        assert_eq!(tokens.check(&format!("{signed}."), now), None);
        let other = tokens.issue(Uuid::new_v4(), session, now);
        let (_, other_payload) = other.split_once('.').unwrap();
        let (_, signature) = token.rsplit_once('.').unwrap();
        let (other_payload, _) = other_payload.split_once('.').unwrap();
        assert_eq!(tokens.check(&format!("{HEADER}.{other_payload}.{signature}"), now), None);
        assert!(!is_access_token("plain-session-token"));
    }

    #[test]
    fn the_header_is_what_it_says() {
        let header = URL_SAFE_NO_PAD.decode(HEADER).unwrap();
        assert_eq!(header, br#"{"alg":"HS256","typ":"JWT"}"#);
    }
}
