//! Recording what users do, for the activity log on the admin page.
//!
//! An audit event is an ordinary `tracing` event with the target `"audit"`
//! and an `action` field such as `"file.download"`. The server stores these
//! (and every warning and error) in the activity log, adding the user and the
//! client address of the request the event happened in. Modules only call
//! [`audit!`](crate::audit!):
//!
//! ```ignore
//! audit!("file.download", name = %node.name, "file downloaded");
//! audit!(warn, "auth.login_failed", user = login, "failed login");
//! ```

/// The `tracing` target of audit events.
pub const AUDIT_TARGET: &str = "audit";

/// Records an audit event: an action code, optional fields, and a message.
/// `warn` as the first argument makes it a warning (e.g. a failed login).
#[macro_export]
macro_rules! audit {
    (warn, $action:literal, $($rest:tt)+) => {
        ::tracing::warn!(target: "audit", action = $action, $($rest)+)
    };
    ($action:literal, $($rest:tt)+) => {
        ::tracing::info!(target: "audit", action = $action, $($rest)+)
    };
}
