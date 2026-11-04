//! The `[files]` settings section.

use rustcloud_core::{zero_is_none, Error, Field, Result, Settings};
use serde::{Deserialize, Serialize};

/// Limits of the files app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesSettings {
    /// Largest file that can be uploaded, in MiB; `None` (`0` in the config
    /// file) is unlimited.
    #[serde(deserialize_with = "zero_is_none")]
    pub max_upload_mib: Option<u64>,
    /// Days a deleted file stays in the trash; 0: until the user empties it.
    pub trash_days: u32,
    /// Earlier versions of a file kept in any case: the latest this many…
    pub versions_keep: u32,
    /// …and also all from this many days back. A version goes only when it is
    /// neither; both 0 turn versions off.
    pub versions_days: u32,
}

impl Default for FilesSettings {
    fn default() -> Self {
        Self { max_upload_mib: Some(4096), trash_days: 30, versions_keep: 10, versions_days: 30 }
    }
}

impl FilesSettings {
    /// [`max_upload_mib`](Self::max_upload_mib) in bytes.
    pub fn max_upload_bytes(&self) -> Option<i64> {
        self.max_upload_mib.map(|mib| i64::try_from(mib.saturating_mul(1024 * 1024)).unwrap_or(i64::MAX))
    }
}

impl Settings for FilesSettings {
    const SECTION: &'static str = "files";

    fn fields() -> Vec<Field> {
        vec![
            Field::optional_number("max_upload_mib", "MiB"),
            Field::number("trash_days", "days"),
            Field::number("versions_keep", "versions"),
            Field::number("versions_days", "days"),
        ]
    }

    fn validate(&self) -> Result<()> {
        if self.trash_days > 3650 {
            return Err(Error::bad_request("INVALID_SETTINGS", "trash_days must be from 0 to 3650"));
        }
        if self.versions_keep > 1000 {
            return Err(Error::bad_request("INVALID_SETTINGS", "versions_keep must be from 0 to 1000"));
        }
        if self.versions_days > 3650 {
            return Err(Error::bad_request("INVALID_SETTINGS", "versions_days must be from 0 to 3650"));
        }
        Ok(())
    }
}
