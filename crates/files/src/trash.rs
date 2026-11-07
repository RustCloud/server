//! The trash: each user's hidden folder `.trash/` in their root.
//!
//! Deleting a file or folder moves it there; what is deleted in the trash is
//! gone for good. Everything inside a deleted folder is marked too
//! (`trashed_at`), so search, public links and apps leave it out without
//! walking up the tree; the deleted node itself remembers where it came from
//! (`trashed_from`) for restoring. The trash counts against the quota, and
//! what has been in it longer than `[files] trash_days` is removed.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::node;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use uuid::Uuid;

use crate::changes::ChangeKind;
use crate::events::FileEventKind;
use crate::service::{Files, Node};
use crate::settings::FilesSettings;

/// Name of the hidden folder that holds what the user deleted.
pub const TRASH_FOLDER: &str = ".trash";
/// How often old things are removed from the trash.
const CLEANUP_EVERY: Duration = Duration::from_secs(3600);
/// Ids per `IN (…)` list: well under the database's limit of parameters.
const IDS_AT_ONCE: usize = 1000;

impl Files {
    /// The user's trash folder, created on first use.
    pub async fn trash(&self, user: &User) -> Result<Node> {
        let root = self.root(user).await?;
        Ok(self.trash_row(root.id).await?.into())
    }

    /// Restores something deleted to the folder it was deleted from, or to the
    /// root if that folder is gone or deleted too. A name taken there gets a
    /// number: "report (2).pdf".
    pub async fn restore(&self, user: &User, id: Uuid) -> Result<Node> {
        let _moving = self.moving.lock().await;
        let node = self.accessible(user, id).await?;
        if node.trashed_from.is_none() {
            let message =
                if node.trashed_at.is_some() { "Restore the folder it is in" } else { "This is not in the trash" };
            return Err(Error::bad_request("NOT_IN_TRASH", message));
        }
        let target = match node.trashed_from {
            Some(from) => match self.row(from).await {
                Ok(folder) if folder.owner_id == node.owner_id && !self.in_trash(&folder).await? => Some(folder),
                _ => None,
            },
            None => None,
        };
        let target = match target {
            Some(folder) => folder,
            None => self.find_root(node.owner_id).await?.ok_or_else(|| Error::internal("no root folder"))?,
        };
        let name = self.free_name(target.id, &node.name, node.is_dir).await?;
        let restored = self.take_out(&node, target.id, &name).await?;
        self.note(ChangeKind::Created, &restored).await;
        self.emit_for(FileEventKind::Created, restored.id, None).await;
        audit!("file.restore", name = restored.name, folder = restored.is_dir, "restored from the trash");
        Ok(restored.into())
    }

    /// Deletes everything in the user's trash for good.
    pub async fn empty_trash(&self, user: &User) -> Result<()> {
        let root = self.root(user).await?;
        let Some(trash) = self.child_by_name(root.id, TRASH_FOLDER).await? else {
            return Ok(());
        };
        let items = self.child_rows(trash.id).await?;
        for item in &items {
            self.remove(item).await?;
        }
        audit!("trash.empty", count = items.len(), "trash emptied");
        Ok(())
    }

    /// Removes for good what went to any trash before `before`.
    pub async fn purge_trash(&self, before: DateTime<Utc>) -> Result<usize> {
        let old = node::Entity::find()
            .filter(node::Column::TrashedFrom.is_not_null())
            .filter(node::Column::TrashedAt.lt(before))
            .all(&self.db)
            .await?;
        for item in &old {
            self.remove(item).await?;
        }
        Ok(old.len())
    }

    /// Every hour, removes what has been in the trash longer than
    /// `[files] trash_days` (none when 0), versions no longer kept and old
    /// entries of the change feed.
    pub fn start_trash_cleanup(self: &Arc<Self>) {
        let files = self.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(CLEANUP_EVERY);
            loop {
                every.tick().await;
                if let Err(err) = files.purge_locks().await {
                    tracing::warn!("locks: {err}");
                }
                if let Err(err) = files.purge_changes().await {
                    tracing::warn!("change feed: {err}");
                }
                match files.purge_versions().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!("versions: removed {count} no longer kept"),
                    Err(err) => tracing::warn!("versions: {err}"),
                }
                let days = files.settings.read(|s: &FilesSettings| s.trash_days);
                if days == 0 {
                    continue;
                }
                match files.purge_trash(Utc::now() - chrono::Duration::days(i64::from(days))).await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!("trash: removed {count} items older than {days} days"),
                    Err(err) => tracing::warn!("trash: {err}"),
                }
            }
        });
    }

    /// Moves `node` (already checked, not in the trash) to its owner's trash.
    pub(crate) async fn move_to_trash(&self, node: &node::Model) -> Result<node::Model> {
        let root = self.find_root(node.owner_id).await?.ok_or_else(|| Error::internal("no root folder"))?;
        let trash = self.trash_row(root.id).await?;
        let name = self.free_name(trash.id, &node.name, node.is_dir).await?;
        let ids: Vec<Uuid> = self.subtree(node).await?.iter().map(|n| n.id).collect();
        let now = Utc::now();

        let db = self.db.begin().await?;
        let mut row: node::ActiveModel = node.clone().into();
        row.parent_id = Set(Some(trash.id));
        row.name = Set(name);
        row.trashed_from = Set(node.parent_id);
        let moved = row.update(&db).await?;
        for ids in ids.chunks(IDS_AT_ONCE) {
            node::Entity::update_many()
                .col_expr(node::Column::TrashedAt, Expr::value(now))
                .filter(node::Column::Id.is_in(ids.to_vec()))
                .exec(&db)
                .await?;
        }
        db.commit().await?;
        self.note_deleted(node).await;
        Ok(node::Model { trashed_at: Some(now), ..moved })
    }

    /// Moves `node` out of the trash into `parent_id` as `name`, unmarking it
    /// and everything inside it.
    pub(crate) async fn take_out(&self, node: &node::Model, parent_id: Uuid, name: &str) -> Result<node::Model> {
        let ids: Vec<Uuid> = self.subtree(node).await?.iter().map(|n| n.id).collect();
        let db = self.db.begin().await?;
        let mut row: node::ActiveModel = node.clone().into();
        row.parent_id = Set(Some(parent_id));
        row.name = Set(name.to_owned());
        row.trashed_from = Set(None);
        row.trashed_at = Set(None);
        let restored = row.update(&db).await.map_err(crate::service::name_taken)?;
        for ids in ids.chunks(IDS_AT_ONCE) {
            node::Entity::update_many()
                .col_expr(node::Column::TrashedAt, Expr::value(Option::<DateTime<Utc>>::None))
                .filter(node::Column::Id.is_in(ids.to_vec()))
                .exec(&db)
                .await?;
        }
        db.commit().await?;
        Ok(restored)
    }

    /// Whether `node` is the trash folder or anything in it.
    pub(crate) async fn in_trash(&self, node: &node::Model) -> Result<bool> {
        Ok(node.trashed_at.is_some() || self.is_trash_folder(node).await?)
    }

    pub(crate) async fn is_trash_folder(&self, node: &node::Model) -> Result<bool> {
        if !node.is_dir || node.name != TRASH_FOLDER {
            return Ok(false);
        }
        match node.parent_id {
            Some(parent) => Ok(self.row(parent).await?.parent_id.is_none()),
            None => Ok(false),
        }
    }

    /// Fails for a folder in the trash: things get there only by deleting.
    pub(crate) async fn refuse_trash(&self, folder: &node::Model) -> Result<()> {
        if self.in_trash(folder).await? {
            return Err(Error::bad_request("IN_TRASH", "Nothing can be put in the trash but by deleting it"));
        }
        Ok(())
    }

    /// The trash folder in `root_id`, created on first use.
    async fn trash_row(&self, root_id: Uuid) -> Result<node::Model> {
        if let Some(trash) = self.child_by_name(root_id, TRASH_FOLDER).await? {
            return self.checked_trash(trash);
        }
        let root = self.row(root_id).await?;
        match self.insert_folder(&root, TRASH_FOLDER).await {
            Ok(trash) => Ok(trash),
            // Another request created it at the same moment: use that one.
            Err(_) => self
                .child_by_name(root_id, TRASH_FOLDER)
                .await?
                .ok_or_else(|| Error::internal("trash folder could not be created"))
                .and_then(|trash| self.checked_trash(trash)),
        }
    }

    fn checked_trash(&self, trash: node::Model) -> Result<node::Model> {
        if !trash.is_dir {
            return Err(Error::conflict("NAME_TAKEN", "A file named .trash is in the way of the trash"));
        }
        Ok(trash)
    }

    /// `name`, or with a number if it is taken in the folder: "report (2).pdf".
    async fn free_name(&self, folder_id: Uuid, name: &str, is_dir: bool) -> Result<String> {
        if self.child_by_name(folder_id, name).await?.is_none() {
            return Ok(name.to_owned());
        }
        for number in 2..1000 {
            let candidate = numbered(name, is_dir, number);
            if self.child_by_name(folder_id, &candidate).await?.is_none() {
                return Ok(candidate);
            }
        }
        Ok(numbered(name, is_dir, Utc::now().timestamp_millis()))
    }
}

/// "report.pdf" → "report (2).pdf"; "notes" → "notes (2)"; ".env" → ".env (2)";
/// a folder "photos.2024" → "photos.2024 (2)".
fn numbered(name: &str, is_dir: bool, number: i64) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !is_dir => format!("{stem} ({number}).{extension}"),
        _ => format!("{name} ({number})"),
    }
}

#[cfg(test)]
mod tests {
    use super::numbered;

    #[test]
    fn taken_names_get_a_number_before_the_extension() {
        assert_eq!(numbered("report.pdf", false, 2), "report (2).pdf");
        assert_eq!(numbered("archive.tar.gz", false, 3), "archive.tar (3).gz");
        assert_eq!(numbered("notes", false, 2), "notes (2)");
        assert_eq!(numbered(".env", false, 2), ".env (2)");
        assert_eq!(numbered("photos.2024", true, 2), "photos.2024 (2)");
    }
}
