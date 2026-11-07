//! The change feed of sync clients: which files and folders of a user were
//! created, changed, moved or deleted, in order, since a cursor.
//!
//! The feed holds ids and kinds only, never names: a client asks for the
//! node itself (`GET /files/{id}`) to see what it is now. One entry stands
//! for the node it names and nothing below it: a folder moved, copied,
//! deleted or restored is one entry, and the client lists the folder (or
//! drops it) with everything inside.
//!
//! Hidden folders (`.name`, among them AppData and the trash) and what is
//! in them are not in the feed: going to the trash is `deleted`, coming back
//! is `created`. Entries stay [`KEEP_DAYS`] days; an older cursor gets
//! `410 CURSOR_EXPIRED` and the client lists everything again.
//!
//! An entry is written right after the change it tells of, not in the same
//! transaction (most file operations are several writes, the content
//! outside the database). Entries are written one at a time, so their ids
//! grow in the order they become visible and a cursor never skips one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rustcloud_auth::User;
use rustcloud_core::{Error, ErrorKind, Result};
use rustcloud_db::{file_change, node};
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::service::Files;

/// How long entries stay in the feed.
pub const KEEP_DAYS: i64 = 30;
/// Most entries in one answer.
pub const CHANGES_AT_ONCE: u64 = 1000;
/// Most connections a user may hold open to hear of changes.
pub const MAX_WATCHERS: usize = 10;
/// A connection that hears of changes ends after this; the client opens a
/// new one (with a fresh token, if it uses short ones).
pub const WATCH_TIME: Duration = Duration::from_secs(10 * 60);
/// Hints of one user closer together than this become one.
pub const HINT_EVERY: Duration = Duration::from_secs(1);
/// Hints that wait for a slow connection before the oldest are lost.
pub(crate) const HINT_BACKLOG: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Created,
    /// Content or mtime.
    Changed,
    /// Moved to another folder or renamed.
    Moved,
    /// Deleted, moved to the trash or into a hidden folder.
    Deleted,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Changed => "changed",
            Self::Moved => "moved",
            Self::Deleted => "deleted",
        }
    }

    fn parse(kind: &str) -> Option<Self> {
        [Self::Created, Self::Changed, Self::Moved, Self::Deleted].into_iter().find(|k| k.as_str() == kind)
    }
}

/// One entry of the feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// The file or folder.
    pub id: Uuid,
    pub kind: ChangeKind,
}

/// A page of the feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangePage {
    /// Oldest first; the same node may come more than once.
    pub changes: Vec<Change>,
    /// Where the next request starts.
    pub cursor: String,
    /// More entries wait: ask again right away.
    pub more: bool,
}

/// The connections of users that hear of changes; see [`Files::watch_changes`].
#[derive(Default)]
pub(crate) struct Watchers {
    open: Mutex<HashMap<Uuid, usize>>,
}

/// A user's open connection that hears of their changes; counted until dropped.
pub struct ChangeWatch {
    pub hints: broadcast::Receiver<Uuid>,
    pub user_id: Uuid,
    watchers: Arc<Watchers>,
}

impl std::fmt::Debug for ChangeWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeWatch").field("user_id", &self.user_id).finish_non_exhaustive()
    }
}

impl Drop for ChangeWatch {
    fn drop(&mut self) {
        let mut open = self.watchers.open.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = open.get_mut(&self.user_id) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.user_id);
            }
        }
    }
}

impl Files {
    /// The user's changes after `cursor`. Without a cursor: no changes and
    /// the cursor of now, to start from (before listing the files).
    pub async fn changes(&self, user: &User, cursor: Option<&str>) -> Result<ChangePage> {
        let latest = self.latest_change().await?;
        let Some(cursor) = cursor else {
            return Ok(ChangePage { changes: Vec::new(), cursor: latest.to_string(), more: false });
        };
        let cursor: i64 = cursor
            .parse()
            .ok()
            .filter(|cursor| *cursor >= 0)
            .ok_or_else(|| Error::bad_request("INVALID_CURSOR", "The cursor is not one the server gave"))?;
        // Entries before the oldest kept one are gone; a cursor beyond the
        // latest comes from another database (a restored backup).
        let oldest = self.oldest_change().await?;
        if cursor > latest || oldest.is_some_and(|oldest| cursor < oldest - 1) {
            return Err(Error::new(ErrorKind::Gone, "CURSOR_EXPIRED", "The cursor is too old: list all files again"));
        }

        let mut rows = file_change::Entity::find()
            .filter(file_change::Column::OwnerId.eq(user.id))
            .filter(file_change::Column::Id.gt(cursor))
            .filter(file_change::Column::Id.lte(latest))
            .order_by_asc(file_change::Column::Id)
            .limit(CHANGES_AT_ONCE + 1)
            .all(&self.db)
            .await?;
        let more = rows.len() as u64 > CHANGES_AT_ONCE;
        rows.truncate(CHANGES_AT_ONCE as usize);
        let next = match (more, rows.last()) {
            (true, Some(last)) => last.id,
            _ => latest,
        };
        let changes = rows
            .into_iter()
            .filter_map(|row| Some(Change { id: row.node_id, kind: ChangeKind::parse(&row.kind)? }))
            .collect();
        Ok(ChangePage { changes, cursor: next.to_string(), more })
    }

    /// Opens a connection that hears when the user has new changes; at most
    /// [`MAX_WATCHERS`] per user (`TOO_MANY_WATCHERS`).
    pub fn watch_changes(&self, user: &User) -> Result<ChangeWatch> {
        let mut open = self.watchers.open.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = open.entry(user.id).or_default();
        if *count >= MAX_WATCHERS {
            return Err(Error::new(
                ErrorKind::TooManyRequests,
                "TOO_MANY_WATCHERS",
                "Too many connections wait for changes; close one",
            ));
        }
        *count += 1;
        Ok(ChangeWatch { hints: self.hints.subscribe(), user_id: user.id, watchers: self.watchers.clone() })
    }

    /// Notes that `node` (as it is now) was created or changed, if clients
    /// see it.
    pub(crate) async fn note(&self, kind: ChangeKind, node: &node::Model) {
        if self.visible(node).await {
            self.note_change(kind, node).await;
        }
    }

    /// Notes that `node` was deleted, moved to the trash or into a hidden
    /// folder; `before` is the node as it was, so clients saw it if it was
    /// visible then.
    pub(crate) async fn note_deleted(&self, before: &node::Model) {
        if self.visible(before).await {
            self.note_change(ChangeKind::Deleted, before).await;
        }
    }

    /// Notes a rename or move from `before` to `after`: a node that leaves
    /// what clients see is deleted for them, one that comes into it new.
    pub(crate) async fn note_moved(&self, before: &node::Model, after: &node::Model) {
        let kind = match (self.visible(before).await, self.visible(after).await) {
            (true, true) => ChangeKind::Moved,
            (true, false) => ChangeKind::Deleted,
            (false, true) => ChangeKind::Created,
            (false, false) => return,
        };
        self.note_change(kind, after).await;
    }

    /// Writes the entry and tells the user's open connections. The change
    /// is already made: a failure here is only logged.
    async fn note_change(&self, kind: ChangeKind, node: &node::Model) {
        let entry = file_change::ActiveModel {
            owner_id: Set(node.owner_id),
            node_id: Set(node.id),
            kind: Set(kind.as_str().to_owned()),
            time: Set(Utc::now()),
            ..Default::default()
        };
        let written = {
            let _in_order = self.noting.lock().await;
            entry.insert(&self.db).await
        };
        match written {
            // Nobody listening is not an error.
            Ok(_) => _ = self.hints.send(node.owner_id),
            Err(err) => tracing::error!(node = %node.id, "change feed: {err}"),
        }
    }

    /// Whether sync clients see `node`: not in the trash, and neither it nor
    /// a folder above it is hidden. Unsure (the database failed): no.
    async fn visible(&self, node: &node::Model) -> bool {
        if node.parent_id.is_none() || node.trashed_at.is_some() || is_hidden_folder(node) {
            return false;
        }
        match self.ancestor_rows(node).await {
            Ok(folders) => !folders.iter().any(is_hidden_folder),
            Err(err) => {
                tracing::warn!(node = %node.id, "change feed: {err}");
                false
            }
        }
    }

    /// Removes entries older than [`KEEP_DAYS`]; the latest one always stays,
    /// so cursors of a quiet server stay valid.
    pub async fn purge_changes(&self) -> Result<u64> {
        let latest = self.latest_change().await?;
        let before = Utc::now() - chrono::Duration::days(KEEP_DAYS);
        let deleted = file_change::Entity::delete_many()
            .filter(file_change::Column::Time.lt(before))
            .filter(file_change::Column::Id.lt(latest))
            .exec(&self.db)
            .await?;
        Ok(deleted.rows_affected)
    }

    /// Id of the latest entry of anyone; 0 for none.
    async fn latest_change(&self) -> Result<i64> {
        let latest: Option<Option<i64>> = file_change::Entity::find()
            .select_only()
            .column_as(Expr::from(Func::max(Expr::col(file_change::Column::Id))), "latest")
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(latest.flatten().unwrap_or(0))
    }

    /// Id of the oldest entry kept, if any.
    async fn oldest_change(&self) -> Result<Option<i64>> {
        let oldest: Option<Option<i64>> = file_change::Entity::find()
            .select_only()
            .column_as(Expr::from(Func::min(Expr::col(file_change::Column::Id))), "oldest")
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(oldest.flatten())
    }
}

/// A folder named `.name`: AppData, the trash, or one the user hid.
fn is_hidden_folder(node: &node::Model) -> bool {
    node.is_dir && node.name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::ChangeKind;

    #[test]
    fn kinds_are_stored_by_name() {
        for kind in [ChangeKind::Created, ChangeKind::Changed, ChangeKind::Moved, ChangeKind::Deleted] {
            assert_eq!(ChangeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ChangeKind::parse("renamed"), None);
    }
}
