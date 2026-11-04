//! Accounts, login methods and sessions.
//!
//! The module is built around three ideas:
//!
//! 1. **Login methods are strategies.** A [`CredentialProvider`] checks
//!    credentials and returns an [`Identity`]. Passwords are one provider;
//!    LDAP would be another. Redirect-based methods (OIDC) produce an
//!    [`Identity`] in their callback instead.
//! 2. **One way into a session.** [`Auth::sign_in`] turns any identity into a
//!    session, so rules like "disabled users cannot log in" live in one place.
//! 3. **Opaque session tokens.** A session is a random token; the database
//!    keeps only its hash. Sessions can be revoked at any time (logout,
//!    password change, account disabled) — no JWT, no signing keys.
//!
//! Handlers of other modules get the caller through the [`CurrentUser`] and
//! [`AdminUser`] extractors.

mod app_passwords;
mod http;
mod limiter;
mod provider;
mod secret;
mod service;
mod settings;

use service::IP_FAILURE_FACTOR;

pub use app_passwords::{Access, AppPassword, AppPasswordUser, CreatedAppPassword, NewAppPassword, Scope};
pub use http::{basic_credentials, router, AdminUser, CurrentUser, CSRF_HEADER, SESSION_COOKIE};
pub use limiter::AttemptLimiter;
pub use provider::{CredentialProvider, Identity, PasswordProvider};
pub use service::{AccountUpdate, Auth, AuthStatus, NewAccount, NewSession, ProfileUpdate, SessionUser, User};
pub use settings::AccountSettings;
