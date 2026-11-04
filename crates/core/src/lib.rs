//! Shared building blocks of RustCloud.
//!
//! Everything here is independent of a concrete database, storage backend or
//! login method:
//!
//! - [`Error`] — the single error type used by every crate; it knows its HTTP
//!   status and renders itself as a JSON API error.
//! - [`BlobStore`] — strategy trait for *where file bytes live* (local disk now,
//!   S3 or anything else later).
//! - [`validate`] — input validation shared by services (file names, usernames).
//! - [`settings`] — settings sections that modules expose for runtime changes.
//! - [`ClientInfo`] — the client's IP address and whether it uses HTTPS.
//! - [`audit!`] — recording user actions for the activity log.

pub mod audit;
pub mod blob;
pub mod client;
pub mod error;
pub mod settings;
pub mod validate;

pub use audit::AUDIT_TARGET;
pub use blob::{open_as_file, BlobStore, ByteRange, ByteStream, Capacity, SpeedTest, StorageInfo};
pub use client::ClientInfo;
pub use error::{Error, ErrorKind, Result};
pub use settings::{Field, FieldKind, Live, Settings, SettingsSection};
