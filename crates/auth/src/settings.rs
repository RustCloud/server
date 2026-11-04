//! The `[accounts]` settings section.

use rustcloud_core::{Error, Field, Result, Settings};
use serde::{Deserialize, Serialize};

/// Registration, sessions and login protection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountSettings {
    /// Whether anyone may create an account. The very first account can always
    /// be created (it becomes the administrator).
    pub open_registration: bool,
    /// How long a login lasts, in days.
    pub session_days: u32,
    /// Storage limit given to new accounts, in MiB; `None` is unlimited.
    pub default_quota_mib: Option<u64>,
    /// Failed logins allowed per account within 15 minutes. One IP address
    /// may fail five times as often, across all accounts.
    pub max_login_failures: u32,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self { open_registration: false, session_days: 14, default_quota_mib: None, max_login_failures: 10 }
    }
}

impl AccountSettings {
    /// Quota for new accounts in bytes.
    pub fn default_quota_bytes(&self) -> Option<i64> {
        self.default_quota_mib.map(|mib| i64::try_from(mib.saturating_mul(1024 * 1024)).unwrap_or(i64::MAX))
    }
}

impl Settings for AccountSettings {
    const SECTION: &'static str = "accounts";

    fn fields() -> Vec<Field> {
        vec![
            Field::bool("open_registration"),
            Field::number("session_days", "days"),
            Field::optional_number("default_quota_mib", "MiB"),
            Field::number("max_login_failures", "attempts"),
        ]
    }

    fn validate(&self) -> Result<()> {
        if !(1..=3650).contains(&self.session_days) {
            return Err(Error::bad_request("INVALID_SETTINGS", "session_days must be between 1 and 3650"));
        }
        if !(1..=1000).contains(&self.max_login_failures) {
            return Err(Error::bad_request("INVALID_SETTINGS", "max_login_failures must be between 1 and 1000"));
        }
        Ok(())
    }
}
