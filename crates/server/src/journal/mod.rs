//! The activity log ("journal") on the admin page: who logged in, downloaded
//! or deleted what, failed logins, and every warning and error of the server.
//!
//! Entries come from `tracing` (see [`layer`]): audit events recorded with
//! [`rustcloud_core::audit!`] and all events at WARN or ERROR. A background
//! task writes them to the `log_entries` table in batches and removes entries
//! older than `[journal] keep_days`.

mod layer;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustcloud_core::{Error, Field, Live, Result, Settings};
use rustcloud_db::log_entry;
use sea_orm::sea_query::{Expr, Func, LikeExpr};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, ExprTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use layer::WRITER_TARGET;
pub use layer::{JournalLayer, NewEntry};

/// Events waiting to be written; beyond this they are dropped (and counted).
const QUEUE_SIZE: usize = 10_000;
/// Entries written in one database statement.
const BATCH_SIZE: usize = 200;
/// Entries returned per page.
const PAGE_SIZE: u64 = 100;
/// How often old entries are removed.
const CLEANUP_EVERY: Duration = Duration::from_secs(3600);

/// The `[journal]` settings section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JournalSettings {
    /// Entries older than this are removed, in days.
    pub keep_days: u32,
}

impl Default for JournalSettings {
    fn default() -> Self {
        Self { keep_days: 90 }
    }
}

impl Settings for JournalSettings {
    const SECTION: &'static str = "journal";

    fn fields() -> Vec<Field> {
        vec![Field::number("keep_days", "days")]
    }

    fn validate(&self) -> Result<()> {
        if !(1..=3650).contains(&self.keep_days) {
            return Err(Error::bad_request("INVALID_SETTINGS", "keep_days must be between 1 and 3650"));
        }
        Ok(())
    }
}

/// Where the layer's events wait for the writer.
pub struct JournalQueue {
    receiver: mpsc::Receiver<NewEntry>,
    dropped: Arc<AtomicU64>,
}

/// Creates the `tracing` layer and the queue it fills. Install the layer
/// first thing at startup; give the queue to [`Journal::start`] once the
/// database is open. Events in between wait in the queue.
pub fn channel() -> (JournalLayer, JournalQueue) {
    let (sender, receiver) = mpsc::channel(QUEUE_SIZE);
    let dropped = Arc::new(AtomicU64::new(0));
    (JournalLayer { sender, dropped: dropped.clone() }, JournalQueue { receiver, dropped })
}

/// An entry as the admin page shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: i64,
    pub time: DateTime<Utc>,
    pub level: String,
    pub action: Option<String>,
    pub message: String,
    pub username: Option<String>,
    pub ip: Option<String>,
    pub details: Value,
}

impl From<log_entry::Model> for Entry {
    fn from(row: log_entry::Model) -> Self {
        Self {
            id: row.id,
            time: row.time,
            level: row.level,
            action: row.action,
            message: row.message,
            username: row.username,
            ip: row.ip,
            details: serde_json::from_str(&row.details).unwrap_or(Value::Null),
        }
    }
}

/// What to show: newest first, optionally only problems or matching a text.
#[derive(Debug, Default, Deserialize)]
pub struct EntryFilter {
    /// Only warnings and errors.
    #[serde(default)]
    pub problems: bool,
    /// Text to look for in the message, action, user, address and details.
    pub q: Option<String>,
    /// Entries older than this id (the next page).
    pub before: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    /// Events lost since startup because the queue was full.
    pub dropped: u64,
}

/// Reads the journal and keeps it written and trimmed.
pub struct Journal {
    db: DatabaseConnection,
    dropped: Arc<AtomicU64>,
}

impl Journal {
    /// Starts the background writer and returns the reader.
    pub fn start(db: DatabaseConnection, queue: JournalQueue, settings: Arc<Live<JournalSettings>>) -> Self {
        let JournalQueue { receiver, dropped } = queue;
        tokio::spawn(write_entries(db.clone(), receiver, settings));
        Self { db, dropped }
    }

    pub async fn entries(&self, filter: EntryFilter) -> Result<EntryPage> {
        let mut query = log_entry::Entity::find().order_by_desc(log_entry::Column::Id).limit(PAGE_SIZE);
        if filter.problems {
            query = query.filter(log_entry::Column::Level.is_in(["WARN", "ERROR"]));
        }
        if let Some(before) = filter.before {
            query = query.filter(log_entry::Column::Id.lt(before));
        }
        if let Some(text) = filter.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            let escaped = text.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let contains = |column: log_entry::Column| {
                Expr::expr(Func::lower(Expr::col(column))).like(LikeExpr::new(format!("%{escaped}%")).escape('\\'))
            };
            query = query.filter(
                Condition::any()
                    .add(contains(log_entry::Column::Message))
                    .add(contains(log_entry::Column::Action))
                    .add(contains(log_entry::Column::Username))
                    .add(contains(log_entry::Column::Ip))
                    .add(contains(log_entry::Column::Details)),
            );
        }
        let entries = query.all(&self.db).await?.into_iter().map(Entry::from).collect();
        Ok(EntryPage { entries, dropped: self.dropped.load(Ordering::Relaxed) })
    }
}

/// The writer task: stores queued events in batches and trims old entries.
async fn write_entries(
    db: DatabaseConnection,
    mut receiver: mpsc::Receiver<NewEntry>,
    settings: Arc<Live<JournalSettings>>,
) {
    let mut cleanup = tokio::time::interval(CLEANUP_EVERY);
    loop {
        tokio::select! {
            first = receiver.recv() => {
                let Some(first) = first else { return };
                let mut batch = vec![first];
                while batch.len() < BATCH_SIZE {
                    match receiver.try_recv() {
                        Ok(entry) => batch.push(entry),
                        Err(_) => break,
                    }
                }
                let rows = batch.into_iter().map(to_row);
                if let Err(err) = log_entry::Entity::insert_many(rows).exec(&db).await {
                    tracing::error!(target: WRITER_TARGET, %err, "could not write the activity log");
                }
            }
            _ = cleanup.tick() => {
                let keep_days = settings.read(|s| s.keep_days);
                if let Err(err) = remove_older_than(&db, keep_days).await {
                    tracing::error!(target: WRITER_TARGET, %err, "could not trim the activity log");
                }
            }
        }
    }
}

async fn remove_older_than(db: &DatabaseConnection, days: u32) -> Result<u64> {
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(days));
    let result = log_entry::Entity::delete_many().filter(log_entry::Column::Time.lt(cutoff)).exec(db).await?;
    Ok(result.rows_affected)
}

fn to_row(entry: NewEntry) -> log_entry::ActiveModel {
    log_entry::ActiveModel {
        time: Set(entry.time),
        level: Set(entry.level.to_string()),
        action: Set(entry.action),
        message: Set(entry.message),
        username: Set(entry.username),
        ip: Set(entry.ip),
        details: Set(Value::Object(entry.details).to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use rustcloud_db::testing::test_db;
    use sea_orm::ActiveModelTrait;
    use tracing::Level;

    use super::*;

    fn entry(level: Level, action: Option<&str>, message: &str, user: Option<&str>) -> NewEntry {
        NewEntry {
            time: Utc::now(),
            level,
            action: action.map(str::to_owned),
            message: message.to_owned(),
            username: user.map(str::to_owned),
            ip: Some("192.0.2.1".to_owned()),
            details: serde_json::from_value(serde_json::json!({ "name": "Report_2024.pdf" })).unwrap(),
        }
    }

    #[tokio::test]
    async fn entries_are_filtered_and_paged_newest_first() {
        let db = test_db().await;
        let (_layer, queue) = channel();
        let journal = Journal { db: db.conn.clone(), dropped: queue.dropped };
        for row in [
            entry(Level::INFO, Some("auth.login"), "logged in", Some("alice")),
            entry(Level::WARN, Some("auth.login_failed"), "failed login", Some("mallory")),
            entry(Level::ERROR, None, "internal error: disk on fire", None),
            entry(Level::INFO, Some("file.download"), "file downloaded", Some("bob")),
        ] {
            to_row(row).insert(&db.conn).await.unwrap();
        }

        let all = journal.entries(EntryFilter::default()).await.unwrap().entries;
        let messages: Vec<&str> = all.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["file downloaded", "internal error: disk on fire", "failed login", "logged in"]);
        assert_eq!(all[0].details["name"], "Report_2024.pdf");

        let problems = journal.entries(EntryFilter { problems: true, ..Default::default() }).await.unwrap();
        assert_eq!(problems.entries.len(), 2);

        let search = |q: &str| EntryFilter { q: Some(q.into()), ..Default::default() };
        assert_eq!(journal.entries(search("MALLORY")).await.unwrap().entries.len(), 1, "case-insensitive");
        assert_eq!(journal.entries(search("report_")).await.unwrap().entries.len(), 4, "details are searched");
        assert_eq!(journal.entries(search("%")).await.unwrap().entries.len(), 0, "wildcards are literal");

        let older = EntryFilter { before: Some(all[1].id), ..Default::default() };
        assert_eq!(journal.entries(older).await.unwrap().entries.len(), 2);
    }

    #[tokio::test]
    async fn old_entries_are_removed() {
        let db = test_db().await;
        let mut old = to_row(entry(Level::INFO, Some("auth.login"), "long ago", None));
        old.time = Set(Utc::now() - chrono::Duration::days(100));
        old.insert(&db.conn).await.unwrap();
        to_row(entry(Level::INFO, Some("auth.login"), "today", None)).insert(&db.conn).await.unwrap();

        assert_eq!(remove_older_than(&db.conn, 90).await.unwrap(), 1);
        assert_eq!(log_entry::Entity::find().all(&db.conn).await.unwrap()[0].message, "today");
    }

    #[tokio::test]
    async fn the_writer_stores_what_the_layer_queues() {
        let db = test_db().await;
        let (layer, queue) = channel();
        let journal = Journal::start(db.conn.clone(), queue, Arc::default());
        layer.sender.send(entry(Level::INFO, Some("auth.login"), "logged in", Some("alice"))).await.unwrap();

        // Up to 10 s: a busy CI database can be slow.
        for _ in 0..1000 {
            if !journal.entries(EntryFilter::default()).await.unwrap().entries.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the entry was not written");
    }
}
