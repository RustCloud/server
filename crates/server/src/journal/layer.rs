//! The `tracing` layer that turns events into activity log entries.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rustcloud_core::AUDIT_TARGET;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Target of the journal's own failures; they are not written to the journal.
pub(super) const WRITER_TARGET: &str = "rustcloud_journal";

/// An event on its way to the database.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub time: DateTime<Utc>,
    pub level: Level,
    pub action: Option<String>,
    pub message: String,
    pub username: Option<String>,
    pub ip: Option<String>,
    pub details: Map<String, Value>,
}

/// Sends audit events, warnings and errors to the journal writer.
///
/// Events never wait for the database: they go into a bounded queue, and if
/// the queue is full (the database is down or very slow) they are counted as
/// dropped instead of slowing requests down.
pub struct JournalLayer {
    pub(super) sender: mpsc::Sender<NewEntry>,
    pub(super) dropped: Arc<AtomicU64>,
}

/// The `ip` and `user` fields of the request span (see `network::guard` and
/// the `CurrentUser` extractor, which records the user).
#[derive(Debug, Default, Clone)]
struct RequestFields {
    ip: Option<String>,
    user: Option<String>,
}

impl Visit for RequestFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "ip" => self.ip = Some(value.to_owned()),
            "user" => self.user = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}

/// All fields of an event: the well-known ones separately, the rest as JSON.
#[derive(Default)]
struct EventFields {
    message: String,
    action: Option<String>,
    request: RequestFields,
    details: Map<String, Value>,
}

impl EventFields {
    fn put(&mut self, field: &Field, value: Value) {
        let text = || match &value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        match field.name() {
            "message" => self.message = text(),
            "action" => self.action = Some(text()),
            "user" => self.request.user = Some(text()),
            "ip" => self.request.ip = Some(text()),
            name => {
                self.details.insert(name.to_owned(), value);
            }
        }
    }
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, Value::from(format!("{value:?}")));
    }
}

impl<S> Layer<S> for JournalLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().fields().field("ip").is_none() {
            return;
        }
        let mut fields = RequestFields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(fields) = span.extensions_mut().get_mut::<RequestFields>() {
                values.record(fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let is_audit = metadata.target() == AUDIT_TARGET;
        // More verbose levels compare greater: this keeps WARN and ERROR.
        if (!is_audit && *metadata.level() > Level::WARN) || metadata.target() == WRITER_TARGET {
            return;
        }

        let mut fields = EventFields::default();
        event.record(&mut fields);
        let request = ctx
            .event_scope(event)
            .and_then(|scope| scope.into_iter().find_map(|span| span.extensions().get::<RequestFields>().cloned()))
            .unwrap_or_default();

        let entry = NewEntry {
            time: Utc::now(),
            level: *metadata.level(),
            action: fields.action,
            message: fields.message,
            username: fields.request.user.or(request.user),
            ip: fields.request.ip.or(request.ip),
            details: fields.details,
        };
        if self.sender.try_send(entry).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn capture(f: impl FnOnce()) -> Vec<NewEntry> {
        let (sender, mut receiver) = mpsc::channel(100);
        let layer = JournalLayer { sender, dropped: Arc::default() };
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), f);
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn audit_events_take_user_and_ip_from_the_request_span() {
        let entries = capture(|| {
            let span = tracing::info_span!("request", ip = "203.0.113.7", user = tracing::field::Empty);
            let _entered = span.enter();
            span.record("user", "alice");
            rustcloud_core::audit!("file.download", name = "a.txt", size = 5u64, "file downloaded");
        });
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.action.as_deref(), Some("file.download"));
        assert_eq!(entry.message, "file downloaded");
        assert_eq!(entry.username.as_deref(), Some("alice"));
        assert_eq!(entry.ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(Value::Object(entry.details.clone()), serde_json::json!({ "name": "a.txt", "size": 5 }));
    }

    #[test]
    fn warnings_and_errors_are_kept_but_ordinary_info_is_not() {
        let entries = capture(|| {
            tracing::info!("server noise");
            tracing::debug!("more noise");
            tracing::warn!("disk is slow");
            tracing::error!(target: WRITER_TARGET, "journal write failed");
            rustcloud_core::audit!(warn, "auth.login_failed", user = "mallory", "failed login");
        });
        let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["disk is slow", "failed login"]);
        assert_eq!(entries[1].username.as_deref(), Some("mallory"), "an explicit user field wins");
        assert_eq!(entries[1].level, Level::WARN);
    }
}
