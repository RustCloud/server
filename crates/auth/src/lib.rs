//! Accounts, login methods and sessions.
//!
//! The module is built around three ideas:
//!
//! 1. **Login methods are strategies.** A [`CredentialProvider`] checks
//!    credentials and returns an [`Identity`]. Passwords are one provider;
//!    LDAP would be another. Redirect-based methods (OIDC) produce an
//!    [`Identity`] in their callback and log in with
//!    [`Auth::sign_in_external`], which also creates accounts on first login.
//! 2. **One way into a session.** [`Auth::sign_in`] turns any identity into a
//!    session, so rules like "disabled users cannot log in" live in one place.
//! 3. **Sessions in the database.** A browser session is a random token; the
//!    database keeps only its hash. Client apps (devices) get a refresh token
//!    the same way and short-lived access tokens (JWT) for requests; every
//!    request checks that the session still exists, so sessions can be
//!    revoked at any time (logout, password change, account disabled).
//!
//! Handlers of other modules get the caller through the [`CurrentUser`] and
//! [`AdminUser`] extractors.

mod access;
mod app_passwords;
mod devices;
mod external;
mod http;
mod limiter;
mod provider;
mod secret;
mod service;
mod settings;

use service::IP_FAILURE_FACTOR;

pub use app_passwords::{Access, AppPassword, AppPasswordUser, CreatedAppPassword, NewAppPassword, Scope};
pub use devices::{check_redirect_uri, AuthorizeRequest, DeviceTokens, SessionInfo, REFRESH_DAYS};
pub use external::{ExternalLogin, LinkedLogin};
pub use http::{
    basic_credentials, from_a_web_page, router, session_cookie, user_agent, AdminUser, CurrentUser, CSRF_HEADER,
    SESSION_COOKIE,
};
pub use limiter::AttemptLimiter;
pub use provider::{CredentialProvider, Identity, PasswordProvider};
pub use secret::{hmac_sha256, same};
pub use service::{AccountUpdate, Auth, AuthStatus, NewAccount, NewSession, ProfileUpdate, SessionUser, User};
pub use settings::AccountSettings;
