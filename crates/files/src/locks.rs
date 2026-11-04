//! Locks apps hold on files while they edit them.
//!
//! An editor (a WOPI client such as Collabora, through its plugin) locks the
//! file it opens; while the lock lasts, only that app changes or deletes the
//! file: the panel, other apps and later WebDAV get `FILE_LOCKED`. Renaming
//! and moving still work. A lock ends when the app unlocks it, when it is not
//! renewed in time, or when the owner breaks it from the panel.

use chrono::{DateTime, Duration, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::file_lock;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Serialize;
use uuid::Uuid;

use crate::service::Files;

/// The longest a lock lasts without being renewed.
pub const LOCK_MAX: Duration = Duration::hours(1);
/// Ids per `IN (…)` list: well under the database's limit of parameters.
const IDS_AT_ONCE: usize = 1000;

/// A lock on a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileLock {
    /// What the app calls it.
    pub lock_id: String,
    pub app_id: String,
    /// The user it was taken for.
    pub user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

impl From<file_lock::Model> for FileLock {
    fn from(lock: file_lock::Model) -> Self {
        Self { lock_id: lock.lock_id, app_id: lock.app_id, user_id: lock.user_id, expires_at: lock.expires_at }
    }
}

/// What came of asking for a lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locking {
    /// The app holds it now, taken or renewed.
    Held(FileLock),
    /// Someone else holds another lock on the file: theirs.
    Taken(FileLock),
}

impl Files {
    /// Takes the lock `lock_id` on a granted file for the app, or renews it,
    /// for `seconds` (at most an hour).
    pub async fn app_lock(&self, user: &User, app_id: &str, id: Uuid, lock_id: &str, seconds: u32) -> Result<Locking> {
        if lock_id.is_empty() || lock_id.len() > 1024 {
            return Err(Error::bad_request("INVALID_LOCK", "A lock id has 1 to 1024 bytes"));
        }
        let file = self.app_node(user, app_id, id, true).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        let now = Utc::now();
        let expires_at = now + Duration::seconds(i64::from(seconds.max(1))).min(LOCK_MAX);
        let lock = file_lock::ActiveModel {
            node_id: Set(file.id),
            lock_id: Set(lock_id.to_owned()),
            app_id: Set(app_id.to_owned()),
            user_id: Set(user.id),
            expires_at: Set(expires_at),
            created_at: Set(now),
        };
        match self.live_lock(file.id).await? {
            Some(held) if held.app_id != app_id || held.lock_id != lock_id => return Ok(Locking::Taken(held.into())),
            Some(_) => {
                let held = file_lock::ActiveModel { expires_at: Set(expires_at), ..lock };
                return Ok(Locking::Held(held.update(&self.db).await?.into()));
            }
            None => {}
        }
        // An old lock that ran out gives way.
        file_lock::Entity::delete_by_id(file.id).exec(&self.db).await?;
        let held = lock
            .insert(&self.db)
            .await
            .map_err(|_| Error::conflict("FILE_LOCKED", "Someone else locked the file at the same moment"))?;
        audit!("file.lock", name = file.name, app = app_id, "file locked by an app");
        Ok(Locking::Held(held.into()))
    }

    /// Ends the app's lock `lock_id` on a granted file. `Some` with the lock
    /// someone else holds instead; nothing to end is no error.
    pub async fn app_unlock(&self, user: &User, app_id: &str, id: Uuid, lock_id: &str) -> Result<Option<FileLock>> {
        let file = self.app_node(user, app_id, id, true).await?;
        match self.live_lock(file.id).await? {
            Some(held) if held.app_id != app_id || held.lock_id != lock_id => Ok(Some(held.into())),
            Some(_) => {
                file_lock::Entity::delete_by_id(file.id).exec(&self.db).await?;
                audit!("file.unlock", name = file.name, app = app_id, "file unlocked by an app");
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// The lock held on a granted file, if any.
    pub async fn app_get_lock(&self, user: &User, app_id: &str, id: Uuid) -> Result<Option<FileLock>> {
        let file = self.app_node(user, app_id, id, false).await?;
        Ok(self.live_lock(file.id).await?.map(FileLock::from))
    }

    /// The owner breaks the lock on a file, e.g. when the editor that took
    /// it is gone.
    pub async fn unlock(&self, user: &User, id: Uuid) -> Result<()> {
        let file = self.accessible(user, id).await?;
        if let Some(held) = self.live_lock(file.id).await? {
            file_lock::Entity::delete_by_id(file.id).exec(&self.db).await?;
            audit!("file.unlock", name = file.name, app = held.app_id, "lock broken by the owner");
        }
        Ok(())
    }

    /// Fails with `FILE_LOCKED` if anyone but `app` (the panel: `None`) holds
    /// a lock on one of `ids`.
    pub(crate) async fn refuse_locked(&self, ids: &[Uuid], app: Option<&str>) -> Result<()> {
        let locks = self.live_locks(ids).await?;
        match locks.into_iter().find(|lock| Some(lock.app_id.as_str()) != app) {
            Some(lock) => Err(Error::conflict(
                "FILE_LOCKED",
                format!("The file is being edited in the app \"{}\" and can't be changed now", lock.app_id),
            )),
            None => Ok(()),
        }
    }

    /// Locks on these files that have not run out.
    pub(crate) async fn live_locks(&self, ids: &[Uuid]) -> Result<Vec<file_lock::Model>> {
        let mut locks = Vec::new();
        for ids in ids.chunks(IDS_AT_ONCE) {
            let found = file_lock::Entity::find()
                .filter(file_lock::Column::NodeId.is_in(ids.to_vec()))
                .filter(file_lock::Column::ExpiresAt.gt(Utc::now()))
                .all(&self.db)
                .await?;
            locks.extend(found);
        }
        Ok(locks)
    }

    async fn live_lock(&self, id: Uuid) -> Result<Option<file_lock::Model>> {
        Ok(self.live_locks(&[id]).await?.pop())
    }

    /// Removes the locks that ran out.
    pub async fn purge_locks(&self) -> Result<u64> {
        let gone = file_lock::Entity::delete_many()
            .filter(file_lock::Column::ExpiresAt.lte(Utc::now()))
            .exec(&self.db)
            .await?;
        Ok(gone.rows_affected)
    }
}
