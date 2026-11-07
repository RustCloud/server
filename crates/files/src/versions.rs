//! Versions: earlier contents of files.
//!
//! Replacing a file keeps the old content as a version (`file_versions`)
//! instead of removing its blob. A version stays while it is one of the latest
//! `[files] versions_keep` of its file **or** is younger than `versions_days`;
//! only what is neither goes — right after a new version and every hour.
//!
//! Saves of one person close together make one version: an editor saving
//! every few seconds would otherwise fill the list ([`SAVES_TOGETHER`]).
//! Versions count against the owner's quota but give way to files: when a
//! write needs room, the owner's oldest versions go first. AppData keeps no
//! versions.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, ByteRange, Error, Result};
use rustcloud_db::{file_version, node};
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serde::Serialize;
use uuid::Uuid;

use crate::events::FileEventKind;
use crate::service::{Download, Files, Node, Writer};
use crate::settings::FilesSettings;

/// Saves by the same person within this time of the latest version (or of
/// creating the file) add no version of their own.
pub const SAVES_TOGETHER: Duration = Duration::minutes(5);
/// Ids per `IN (…)` list: well under the database's limit of parameters.
const IDS_AT_ONCE: usize = 1000;

/// What happens to the old content when a file is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Versioning {
    /// Becomes a version, unless the same person saved just before.
    Keep,
    /// Becomes a version in any case (restoring a version).
    Always,
    /// Is removed (AppData).
    Off,
}

/// An earlier content of a file.
#[derive(Debug, Clone, Serialize)]
pub struct Version {
    pub id: Uuid,
    pub size: i64,
    /// Hex SHA-256 of the content, like a file's ETag.
    pub etag: String,
    /// Who wrote this content, if known.
    pub modified_by: Option<Uuid>,
    /// When this content was written.
    pub modified_at: DateTime<Utc>,
    /// When it was replaced.
    pub created_at: DateTime<Utc>,
}

impl From<file_version::Model> for Version {
    fn from(v: file_version::Model) -> Self {
        Self {
            id: v.id,
            size: v.size,
            etag: v.sha256,
            modified_by: v.modified_by,
            modified_at: v.modified_at,
            created_at: v.created_at,
        }
    }
}

/// The versions of a file, newest first, and how long they are kept.
#[derive(Debug, Clone, Serialize)]
pub struct Versions {
    pub versions: Vec<Version>,
    /// `[files] versions_keep`.
    pub keep: u32,
    /// `[files] versions_days`.
    pub days: u32,
}

impl Files {
    pub async fn versions(&self, user: &User, id: Uuid) -> Result<Versions> {
        let file = self.accessible_file(user, id).await?;
        let versions = self.version_rows(file.id).await?.into_iter().map(Version::from).collect();
        let (keep, days) = self.settings.read(|s: &FilesSettings| (s.versions_keep, s.versions_days));
        Ok(Versions { versions, keep, days })
    }

    /// The file as it was in a version: its size, ETag and time.
    pub async fn version_node(&self, user: &User, id: Uuid, version_id: Uuid) -> Result<Node> {
        let file = self.accessible_file(user, id).await?;
        let version = self.version_row(file.id, version_id).await?;
        Ok(as_of(file, &version).into())
    }

    /// Opens a version for reading, like [`Files::download`]; the node is
    /// [`Files::version_node`].
    pub async fn download_version(
        &self,
        user: &User,
        id: Uuid,
        version_id: Uuid,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        let file = self.accessible_file(user, id).await?;
        let version = self.version_row(file.id, version_id).await?;
        if crate::service::starts_reading(range) {
            audit!("file.version.download", name = file.name, size = version.size, "version downloaded");
        }
        let body = self.blobs.get(&version.content_key, range).await?;
        Ok(Download { node: as_of(file, &version).into(), range, body })
    }

    /// Makes a version the current content; what was current becomes a version.
    pub async fn restore_version(&self, user: &User, id: Uuid, version_id: Uuid) -> Result<Node> {
        let file = self.accessible_file(user, id).await?;
        self.refuse_trash(&file).await?;
        let version = self.version_row(file.id, version_id).await?;
        let body = self.blobs.get(&version.content_key, None).await?;
        let name = file.name.clone();
        // The content comes back with the time it had.
        let mtime = Some(version.modified_at);
        let writer = Writer { versioning: Versioning::Always, mtime, ..Writer::user() };
        let restored = self.write_content(user, file, body, writer).await?;
        self.emit_for(FileEventKind::Changed, restored.id, None).await;
        audit!("file.version.restore", name, size = restored.size, "version restored");
        Ok(restored)
    }

    /// Versions of a file the user gave the app, newest first.
    pub async fn app_versions(&self, user: &User, app_id: &str, id: Uuid) -> Result<Vec<Version>> {
        let file = self.app_node(user, app_id, id, false).await?;
        Ok(self.version_rows(file.id).await?.into_iter().map(Version::from).collect())
    }

    /// Up to `length` bytes of a version of a granted file from `offset`.
    pub async fn app_read_version(
        &self,
        user: &User,
        app_id: &str,
        id: Uuid,
        version_id: Uuid,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        let file = self.app_node(user, app_id, id, false).await?;
        let version = self.version_row(file.id, version_id).await?;
        let content_key = Some(version.content_key.clone());
        self.read_range(node::Model { content_key, ..as_of(file, &version) }, offset, length).await
    }

    /// Keeps the content `file` had before a write by `user` as a version.
    /// Returns whether it was kept; if not, its blob is to be removed.
    pub(crate) async fn keep_version(&self, user: &User, file: &node::Model, versioning: Versioning) -> Result<bool> {
        let (Some(key), Some(sha256)) = (&file.content_key, &file.sha256) else {
            return Ok(false);
        };
        let off = self.settings.read(|s: &FilesSettings| s.versions_keep == 0 && s.versions_days == 0);
        if versioning == Versioning::Off || off {
            return Ok(false);
        }
        if versioning == Versioning::Keep && file.modified_by == Some(user.id) {
            let latest = self.version_rows(file.id).await?.first().map(|v| v.created_at);
            if latest.unwrap_or(file.created_at) > Utc::now() - SAVES_TOGETHER {
                return Ok(false);
            }
        }
        file_version::ActiveModel {
            id: Set(Uuid::new_v4()),
            node_id: Set(file.id),
            owner_id: Set(file.owner_id),
            size: Set(file.size),
            content_key: Set(key.clone()),
            sha256: Set(sha256.clone()),
            modified_by: Set(file.modified_by),
            modified_at: Set(file.modified_at.unwrap_or(file.updated_at)),
            created_at: Set(Utc::now()),
        }
        .insert(&self.db)
        .await?;
        self.trim_versions(file.id).await?;
        Ok(true)
    }

    /// Removes the versions of one file the settings no longer keep.
    async fn trim_versions(&self, node_id: Uuid) -> Result<usize> {
        let (keep, days) = self.settings.read(|s: &FilesSettings| (s.versions_keep, s.versions_days));
        let cutoff = Utc::now() - Duration::days(i64::from(days));
        let old: Vec<file_version::Model> = self
            .version_rows(node_id)
            .await?
            .into_iter()
            .skip(keep as usize)
            .filter(|v| v.created_at < cutoff)
            .collect();
        self.remove_versions(&old).await?;
        Ok(old.len())
    }

    /// Removes the versions the settings no longer keep, of all files.
    pub async fn purge_versions(&self) -> Result<usize> {
        let days = self.settings.read(|s: &FilesSettings| s.versions_days);
        let cutoff = Utc::now() - Duration::days(i64::from(days));
        let files: BTreeSet<Uuid> = file_version::Entity::find()
            .select_only()
            .column(file_version::Column::NodeId)
            .filter(file_version::Column::CreatedAt.lt(cutoff))
            .into_tuple()
            .all(&self.db)
            .await?
            .into_iter()
            .collect();
        let mut removed = 0;
        for node_id in files {
            removed += self.trim_versions(node_id).await?;
        }
        Ok(removed)
    }

    /// Removes the owner's oldest versions until files and versions together
    /// fit into the quota again. Failures are only logged: the write that
    /// called it has succeeded.
    pub(crate) async fn fit_quota(&self, user: &User) {
        let Some(quota) = user.quota_bytes else {
            return;
        };
        if let Err(err) = self.make_room(user.id, quota).await {
            tracing::warn!(user = %user.id, %err, "could not remove old versions to fit the quota");
        }
    }

    async fn make_room(&self, owner_id: Uuid, quota: i64) -> Result<()> {
        let mut over = self.files_bytes(owner_id).await? + self.versions_bytes(owner_id).await? - quota;
        if over <= 0 {
            return Ok(());
        }
        let oldest = file_version::Entity::find()
            .filter(file_version::Column::OwnerId.eq(owner_id))
            .order_by_asc(file_version::Column::CreatedAt)
            .all(&self.db)
            .await?;
        let mut gone = Vec::new();
        for version in oldest {
            if over <= 0 {
                break;
            }
            over -= version.size;
            gone.push(version);
        }
        self.remove_versions(&gone).await
    }

    pub(crate) async fn versions_bytes(&self, owner_id: Uuid) -> Result<i64> {
        // SUM() of a BIGINT is NUMERIC in PostgreSQL; cast it back.
        let total = Func::cast_as(
            Func::coalesce([Expr::from(Func::sum(Expr::col(file_version::Column::Size))), Expr::val(0)]),
            "BIGINT",
        );
        let used: Option<i64> = file_version::Entity::find()
            .select_only()
            .column_as(Expr::from(total), "used")
            .filter(file_version::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(used.unwrap_or(0))
    }

    /// Blob keys of the versions of these files, to remove together with them.
    pub(crate) async fn version_keys(&self, node_ids: &[Uuid]) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        for ids in node_ids.chunks(IDS_AT_ONCE) {
            let found: Vec<String> = file_version::Entity::find()
                .select_only()
                .column(file_version::Column::ContentKey)
                .filter(file_version::Column::NodeId.is_in(ids.to_vec()))
                .into_tuple()
                .all(&self.db)
                .await?;
            keys.extend(found);
        }
        Ok(keys)
    }

    async fn remove_versions(&self, versions: &[file_version::Model]) -> Result<()> {
        for ids in versions.chunks(IDS_AT_ONCE) {
            let ids: Vec<Uuid> = ids.iter().map(|v| v.id).collect();
            file_version::Entity::delete_many().filter(file_version::Column::Id.is_in(ids)).exec(&self.db).await?;
        }
        for version in versions {
            self.discard_blob(&version.content_key).await;
        }
        Ok(())
    }

    /// Versions of a file, newest first.
    async fn version_rows(&self, node_id: Uuid) -> Result<Vec<file_version::Model>> {
        Ok(file_version::Entity::find()
            .filter(file_version::Column::NodeId.eq(node_id))
            .order_by_desc(file_version::Column::CreatedAt)
            .all(&self.db)
            .await?)
    }

    async fn version_row(&self, node_id: Uuid, version_id: Uuid) -> Result<file_version::Model> {
        file_version::Entity::find_by_id(version_id)
            .filter(file_version::Column::NodeId.eq(node_id))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("VERSION_NOT_FOUND", "This version no longer exists"))
    }

    /// Loads a file (not a folder) the user may access.
    async fn accessible_file(&self, user: &User, id: Uuid) -> Result<node::Model> {
        let file = self.accessible(user, id).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        Ok(file)
    }
}

/// `file` with the size, ETag and time of `version`.
fn as_of(file: node::Model, version: &file_version::Model) -> node::Model {
    node::Model {
        size: version.size,
        sha256: Some(version.sha256.clone()),
        updated_at: version.modified_at,
        modified_at: Some(version.modified_at),
        ..file
    }
}
