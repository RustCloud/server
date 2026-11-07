//! [`Files`]: the file tree of every user.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, SubsecRound, Utc};
use futures_util::StreamExt;
use rustcloud_auth::User;
use rustcloud_core::{audit, validate, BlobStore, ByteRange, ByteStream, Error, ErrorKind, Live, Result};
use rustcloud_db::node;

use crate::events::{FileEvent, FileEventKind, EVENT_BACKLOG};
use sea_orm::sea_query::{Expr, Func, LikeExpr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, ExprTrait, QueryFilter, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::locks::FileLock;
use crate::settings::FilesSettings;
use crate::storage::SpeedTestResult;
use crate::versions::Versioning;

/// Maximum number of search results.
const SEARCH_LIMIT: u64 = 200;
/// Folders can't be nested deeper than this; protects the ancestor walks.
const MAX_DEPTH: usize = 256;
/// Free space uploads always leave on the storage's disk, so the database
/// and the journal can still be written when users have filled it.
const DISK_RESERVE: u64 = 1 << 30;

/// A file or folder as returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    pub id: Uuid,
    /// `None` only for the root folder.
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub mime_type: Option<String>,
    /// Changes whenever the content changes (hex SHA-256); `None` for folders.
    pub etag: Option<String>,
    pub created_at: DateTime<Utc>,
    /// When the server last changed it: content, name or place.
    pub updated_at: DateTime<Utc>,
    /// When its content was last changed, as the client that wrote it said
    /// (the mtime of the file on that device); `updated_at` if it didn't say.
    pub modified_at: DateTime<Utc>,
    /// When it went to the trash; only for what is in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<DateTime<Utc>>,
    /// The lock an app holds on it while editing it; filled in where the
    /// panel shows files (a folder's contents, a file's details).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<FileLock>,
}

impl From<node::Model> for Node {
    fn from(row: node::Model) -> Self {
        Self {
            id: row.id,
            parent_id: row.parent_id,
            name: row.name,
            is_dir: row.is_dir,
            size: row.size,
            mime_type: row.mime_type,
            etag: row.sha256,
            created_at: row.created_at,
            updated_at: row.updated_at,
            modified_at: row.modified_at.unwrap_or(row.updated_at),
            trashed_at: row.trashed_at,
            lock: None,
        }
    }
}

/// Rename, move and/or set the mtime. Missing fields stay as they are.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeUpdate {
    pub name: Option<String>,
    /// New parent folder.
    pub parent_id: Option<Uuid>,
    /// New mtime of a file (see [`Node::modified_at`]).
    pub modified_at: Option<DateTime<Utc>>,
}

/// Storage used by a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Files, the trash and versions together.
    pub used_bytes: i64,
    /// The part of `used_bytes` taken by versions, which give way when room is needed.
    pub versions_bytes: i64,
    pub quota_bytes: Option<i64>,
}

/// Who changes a file, and what becomes of the content it replaces.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Writer<'a> {
    /// The app that writes; `None`: the user, in the panel or through the API.
    pub app: Option<&'a str>,
    pub versioning: Versioning,
    /// The ETag the file must still have; `FILE_CHANGED` otherwise.
    pub if_match: Option<&'a str>,
    /// The mtime of the new content (checked with [`check_mtime`]); `None`: now.
    pub mtime: Option<DateTime<Utc>>,
}

impl<'a> Writer<'a> {
    pub fn user() -> Self {
        Self { app: None, versioning: Versioning::Keep, if_match: None, mtime: None }
    }

    pub fn app(app_id: &'a str) -> Self {
        Self { app: Some(app_id), ..Self::user() }
    }
}

/// File content ready to be sent: the bytes of `range` (or the whole file).
pub struct Download {
    pub node: Node,
    pub range: Option<ByteRange>,
    pub body: ByteStream,
}

/// The file tree: folders, files and their content.
///
/// Every method takes the acting [`User`] and checks access with
/// [`Files::check_access`] before doing anything.
pub struct Files {
    pub(crate) db: DatabaseConnection,
    pub(crate) blobs: Arc<dyn BlobStore>,
    pub(crate) settings: Arc<Live<FilesSettings>>,
    /// Result of the latest storage speed test (see `storage.rs`).
    pub(crate) last_speed_test: Mutex<Option<SpeedTestResult>>,
    /// Held while a speed test runs, so tests never overlap.
    pub(crate) speed_test_running: tokio::sync::Mutex<()>,
    /// Files created and changed, for whoever watches (see `events.rs`).
    pub(crate) events: tokio::sync::broadcast::Sender<FileEvent>,
    /// Held while a node changes its folder (move, trash, restore), so the
    /// check "not into itself" and the move happen as one: two moves at once
    /// could otherwise put two folders into each other.
    pub(crate) moving: tokio::sync::Mutex<()>,
    /// Uploads under way, per user: what each user is writing at the moment
    /// counts against the quota of the user's other uploads.
    writing: Arc<Mutex<HashMap<Uuid, Writing>>>,
}

/// The uploads of one user under way.
#[derive(Debug, Default)]
struct Writing {
    /// How many there are.
    uploads: usize,
    /// Bytes they have received so far.
    bytes: i64,
    /// Bytes of uploads that have finished since (only ever grows): an
    /// upload that began before they finished doesn't see them in its quota.
    finished: i64,
}

impl Files {
    pub fn new(db: DatabaseConnection, blobs: Arc<dyn BlobStore>, settings: Arc<Live<FilesSettings>>) -> Self {
        Self {
            db,
            blobs,
            settings,
            last_speed_test: Mutex::new(None),
            speed_test_running: tokio::sync::Mutex::new(()),
            events: tokio::sync::broadcast::channel(EVENT_BACKLOG).0,
            moving: tokio::sync::Mutex::new(()),
            writing: Arc::default(),
        }
    }

    /// The largest upload the server takes, in bytes; `None`: no limit.
    pub fn max_upload_bytes(&self) -> Option<u64> {
        self.settings.read(FilesSettings::max_upload_bytes).map(|max| u64::try_from(max).unwrap_or(0))
    }

    /// The most `user` may upload at once now: the upload limit, what is left
    /// of the quota, and the free space on the disk past its reserve.
    /// `None`: nothing limits it.
    pub async fn upload_room(&self, user: &User) -> Result<Option<u64>> {
        let quota = self.remaining_quota(user).await?.map(|left| u64::try_from(left).unwrap_or(0));
        let limits = [self.max_upload_bytes(), quota, self.disk_room().await?];
        Ok(limits.into_iter().flatten().min())
    }

    /// Free space on the storage's disk past [`DISK_RESERVE`]; `None` for a
    /// storage without a fixed size.
    async fn disk_room(&self) -> Result<Option<u64>> {
        Ok(self.blobs.capacity().await?.map(|disk| disk.free_bytes.saturating_sub(DISK_RESERVE)))
    }

    /// The user's root folder, created on first use.
    pub async fn root(&self, user: &User) -> Result<Node> {
        if let Some(root) = self.find_root(user.id).await? {
            return Ok(root.into());
        }
        let created = new_node(user.id, None, "", true).insert(&self.db).await;
        match created {
            Ok(root) => Ok(root.into()),
            // Another request created it at the same moment: use that one.
            Err(_) => self
                .find_root(user.id)
                .await?
                .map(Node::from)
                .ok_or_else(|| Error::internal("root folder could not be created")),
        }
    }

    pub async fn get(&self, user: &User, id: Uuid) -> Result<Node> {
        let mut node = vec![Node::from(self.accessible(user, id).await?)];
        self.add_locks(&mut node).await?;
        Ok(node.remove(0))
    }

    /// The folders above a node, root first (the node itself is not included).
    pub async fn ancestors(&self, user: &User, id: Uuid) -> Result<Vec<Node>> {
        let node = self.accessible(user, id).await?;
        Ok(self.ancestor_rows(&node).await?.into_iter().map(Node::from).collect())
    }

    /// Contents of a folder: folders first, then files, each sorted by name.
    pub async fn children(&self, user: &User, folder_id: Uuid) -> Result<Vec<Node>> {
        let folder = self.accessible_folder(user, folder_id).await?;
        let mut children: Vec<Node> = self.child_rows(folder.id).await?.into_iter().map(Node::from).collect();
        children.sort_by_cached_key(|n| (!n.is_dir, n.name.to_lowercase()));
        self.add_locks(&mut children).await?;
        Ok(children)
    }

    pub async fn create_folder(&self, user: &User, parent_id: Uuid, name: &str) -> Result<Node> {
        let name = validate::file_name(name)?;
        let parent = self.writable_folder(user, parent_id).await?;
        let folder = self.insert_folder(&parent, name).await?;
        audit!("folder.create", name, "folder created");
        Ok(folder.into())
    }

    /// Creates folder `name` in `parent` (already checked), without an audit event.
    pub(crate) async fn insert_folder(&self, parent: &node::Model, name: &str) -> Result<node::Model> {
        new_node(parent.owner_id, Some(parent.id), name, true).insert(&self.db).await.map_err(name_taken)
    }

    /// Stores a new file `name` in a folder. An existing file with that name is
    /// replaced only when `overwrite` is set; a folder is never replaced.
    pub async fn upload(
        &self,
        user: &User,
        parent_id: Uuid,
        name: &str,
        body: ByteStream,
        overwrite: bool,
        mtime: Option<DateTime<Utc>>,
    ) -> Result<Node> {
        let name = validate::file_name(name)?;
        let mtime = mtime.map(check_mtime).transpose()?;
        let parent = self.writable_folder(user, parent_id).await?;
        let writer = Writer { mtime, ..Writer::user() };
        let (file, replaced) = self.put_file(user, &parent, name, body, overwrite, writer).await?;
        let kind = if replaced { FileEventKind::Changed } else { FileEventKind::Created };
        self.emit_for(kind, file.id, None).await;
        if replaced {
            audit!("file.upload", name, size = file.size, replaced = true, "file uploaded");
        } else {
            audit!("file.upload", name, size = file.size, "file uploaded");
        }
        Ok(file)
    }

    /// Stores file `name` in `parent` (already checked), without an audit
    /// event. Returns the file and whether it replaced an existing one.
    pub(crate) async fn put_file(
        &self,
        user: &User,
        parent: &node::Model,
        name: &str,
        body: ByteStream,
        overwrite: bool,
        writer: Writer<'_>,
    ) -> Result<(Node, bool)> {
        if let Some(existing) = self.child_by_name(parent.id, name).await? {
            if existing.is_dir || !overwrite {
                return Err(Error::conflict("NAME_TAKEN", format!("'{name}' already exists in this folder")));
            }
            return Ok((self.write_content(user, existing, body, writer).await?, true));
        }

        let stored = self.store(user, body, 0).await?;
        let mut file = new_node(parent.owner_id, Some(parent.id), name, false);
        file.size = Set(stored.size);
        file.mime_type = Set(Some(guess_mime(name)));
        file.content_key = Set(Some(stored.key.clone()));
        file.sha256 = Set(Some(stored.sha256));
        file.modified_by = Set(Some(user.id));
        file.modified_at = Set(writer.mtime);

        match file.insert(&self.db).await {
            Ok(file) => {
                self.fit_quota(user).await;
                Ok((file.into(), false))
            }
            Err(err) => {
                self.discard_blob(&stored.key).await;
                Err(name_taken(err))
            }
        }
    }

    /// Replaces the content of an existing file; with `if_match`, only if it
    /// still has that ETag.
    pub async fn replace_content(
        &self,
        user: &User,
        id: Uuid,
        body: ByteStream,
        if_match: Option<&str>,
        mtime: Option<DateTime<Utc>>,
    ) -> Result<Node> {
        let mtime = mtime.map(check_mtime).transpose()?;
        let file = self.accessible(user, id).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        self.refuse_trash(&file).await?;
        let name = file.name.clone();
        let updated = self.write_content(user, file, body, Writer { if_match, mtime, ..Writer::user() }).await?;
        self.emit_for(FileEventKind::Changed, updated.id, None).await;
        audit!("file.upload", name, size = updated.size, replaced = true, "file uploaded");
        Ok(updated)
    }

    /// Opens a file for reading. `range` must already fit inside the file
    /// (see [`crate::content::parse_range`]).
    pub async fn download(&self, user: &User, id: Uuid, range: Option<ByteRange>) -> Result<Download> {
        let file = self.accessible(user, id).await?;
        if starts_reading(range) {
            audit!("file.download", name = file.name, size = file.size, "file downloaded");
        }
        self.open(file, range).await
    }

    /// Renames and/or moves a node.
    pub async fn update(&self, user: &User, id: Uuid, update: NodeUpdate) -> Result<Node> {
        let _moving = match update.parent_id {
            Some(_) => Some(self.moving.lock().await),
            None => None,
        };
        let node = self.accessible(user, id).await?;
        if node.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be renamed or moved"));
        }
        if self.is_trash_folder(&node).await? {
            return Err(trash_folder());
        }

        let mut row: node::ActiveModel = node.clone().into();
        if let Some(mtime) = update.modified_at {
            if node.is_dir {
                return Err(Error::bad_request("NOT_A_FILE", "Only files have a modification time"));
            }
            row.modified_at = Set(Some(check_mtime(mtime)?));
        }
        if let Some(name) = &update.name {
            row.name = Set(validate::file_name(name)?.to_owned());
        }
        if let Some(parent_id) = update.parent_id {
            let target = self.writable_folder(user, parent_id).await?;
            if target.owner_id != node.owner_id {
                return Err(Error::bad_request("CROSS_OWNER_MOVE", "Items can only be moved within the same storage"));
            }
            if node.is_dir && self.is_inside(&target, node.id).await? {
                return Err(Error::bad_request("MOVE_INTO_ITSELF", "A folder cannot be moved into itself"));
            }
            row.parent_id = Set(Some(target.id));
        }
        row.updated_at = Set(Utc::now());
        // Who watched it where it was: there, it is gone.
        let before = self.watched_or_none(&node).await;
        let updated = row.update(&self.db).await.map_err(name_taken)?;
        // Moved out of the trash: restored there.
        let updated = match (node.trashed_at, updated.parent_id) {
            (Some(_), Some(parent)) if updated.parent_id != node.parent_id => {
                self.take_out(&updated, parent, &updated.name).await?
            }
            _ => updated,
        };
        if updated.name != node.name || updated.parent_id != node.parent_id {
            let mut before = before;
            if let Some(file) = before.iter_mut().find(|file| file.node.id == updated.id) {
                file.node = updated.clone().into();
            }
            let files = crate::events::together(self.watched_or_none(&updated).await, before);
            self.emit(FileEventKind::Moved, updated.owner_id, files, None);
        }
        if updated.name != node.name {
            audit!("file.rename", name = node.name, new_name = updated.name, "renamed");
        }
        if updated.parent_id != node.parent_id {
            audit!("file.move", name = updated.name, "moved");
        }
        Ok(updated.into())
    }

    /// Copies a file or a whole folder into `target_id`, optionally under a new name.
    pub async fn copy(&self, user: &User, id: Uuid, target_id: Uuid, name: Option<&str>) -> Result<Node> {
        let source = self.accessible(user, id).await?;
        if source.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be copied"));
        }
        let target = self.writable_folder(user, target_id).await?;
        let name = validate::file_name(name.unwrap_or(&source.name))?.to_owned();
        if source.is_dir && self.is_inside(&target, source.id).await? {
            return Err(Error::bad_request("COPY_INTO_ITSELF", "A folder cannot be copied into itself"));
        }

        let total: i64 = self.subtree(&source).await?.iter().map(|n| n.size).sum();
        if let Some(remaining) = self.remaining_quota(user).await? {
            if total > remaining {
                return Err(quota_exceeded());
            }
        }
        let mut created = HashSet::new();
        let copy = self.copy_recursive(&source, &target, &name, &mut created).await?;
        self.fit_quota(user).await;
        let files = self.watched_or_none(&copy).await;
        self.emit(FileEventKind::Created, copy.owner_id, files, None);
        audit!("file.copy", name = source.name, new_name = copy.name, "copied");
        Ok(copy.into())
    }

    /// Moves a file or a folder with everything inside it to the trash; in
    /// the trash, deletes it for good.
    pub async fn delete(&self, user: &User, id: Uuid) -> Result<()> {
        let _moving = self.moving.lock().await;
        let node = self.accessible(user, id).await?;
        if node.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be deleted"));
        }
        if self.is_trash_folder(&node).await? {
            return Err(trash_folder());
        }
        if node.trashed_at.is_some() {
            self.remove(&node).await?;
            audit!("file.delete", name = node.name, folder = node.is_dir, "deleted for good");
            return Ok(());
        }
        let ids: Vec<Uuid> = self.subtree(&node).await?.iter().map(|n| n.id).collect();
        self.refuse_locked(&ids, None).await?;
        let files = self.watched_or_none(&node).await;
        self.move_to_trash(&node).await?;
        self.emit(FileEventKind::Deleted, node.owner_id, files, None);
        audit!("file.trash", name = node.name, folder = node.is_dir, "moved to the trash");
        Ok(())
    }

    /// Deletes `node` (already checked) with everything inside it, without an
    /// audit event.
    pub(crate) async fn remove(&self, node: &node::Model) -> Result<()> {
        let subtree = self.subtree(node).await?;
        let ids: Vec<Uuid> = subtree.iter().map(|n| n.id).collect();
        let mut blob_keys: Vec<String> = subtree.into_iter().filter_map(|n| n.content_key).collect();
        blob_keys.extend(self.version_keys(&ids).await?);

        // Rows of the whole subtree (and shares and versions of it) go with the
        // node thanks to ON DELETE CASCADE.
        node::Entity::delete_by_id(node.id).exec(&self.db).await?;
        for key in blob_keys {
            self.discard_blob(&key).await;
        }
        Ok(())
    }

    /// Files and folders of the user whose name contains `query` (case-insensitive).
    pub async fn search(&self, user: &User, query: &str) -> Result<Vec<Node>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = query.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let pattern = LikeExpr::new(format!("%{escaped}%")).escape('\\');

        let rows = node::Entity::find()
            .filter(node::Column::OwnerId.eq(user.id))
            .filter(node::Column::ParentId.is_not_null())
            .filter(node::Column::TrashedAt.is_null())
            .filter(Expr::expr(Func::lower(Expr::col(node::Column::Name))).like(pattern))
            .limit(SEARCH_LIMIT)
            .all(&self.db)
            .await?;
        let mut found: Vec<Node> = rows.into_iter().map(Node::from).collect();
        found.sort_by_cached_key(|n| (!n.is_dir, n.name.to_lowercase()));
        Ok(found)
    }

    pub async fn usage(&self, user: &User) -> Result<Usage> {
        let versions_bytes = self.versions_bytes(user.id).await?;
        let used_bytes = self.files_bytes(user.id).await? + versions_bytes;
        Ok(Usage { used_bytes, versions_bytes, quota_bytes: user.quota_bytes })
    }

    /// Fills in the locks apps hold on these files.
    async fn add_locks(&self, nodes: &mut [Node]) -> Result<()> {
        let ids: Vec<Uuid> = nodes.iter().filter(|n| !n.is_dir).map(|n| n.id).collect();
        for lock in self.live_locks(&ids).await? {
            if let Some(node) = nodes.iter_mut().find(|n| n.id == lock.node_id) {
                node.lock = Some(lock.into());
            }
        }
        Ok(())
    }

    // ----- access -----

    /// **The single place that decides who may access a node.**
    ///
    /// Today only the owner may. Sharing with other users or groups will add
    /// its rules here. Nodes the user may not access are reported as missing,
    /// so their existence is not revealed.
    fn check_access(user: &User, node: &node::Model) -> Result<()> {
        if node.owner_id == user.id {
            Ok(())
        } else {
            Err(not_found())
        }
    }

    /// Loads a node the user may access.
    pub(crate) async fn accessible(&self, user: &User, id: Uuid) -> Result<node::Model> {
        let node = self.row(id).await?;
        Self::check_access(user, &node)?;
        Ok(node)
    }

    /// Loads a folder the user may access.
    async fn accessible_folder(&self, user: &User, id: Uuid) -> Result<node::Model> {
        let node = self.accessible(user, id).await?;
        if !node.is_dir {
            return Err(Error::bad_request("NOT_A_FOLDER", "This is not a folder"));
        }
        Ok(node)
    }

    /// A folder the user may put things in: not in the trash.
    async fn writable_folder(&self, user: &User, id: Uuid) -> Result<node::Model> {
        let folder = self.accessible_folder(user, id).await?;
        self.refuse_trash(&folder).await?;
        Ok(folder)
    }

    // ----- tree helpers (no access checks) -----

    pub(crate) async fn row(&self, id: Uuid) -> Result<node::Model> {
        node::Entity::find_by_id(id).one(&self.db).await?.ok_or_else(not_found)
    }

    pub(crate) async fn find_root(&self, owner_id: Uuid) -> Result<Option<node::Model>> {
        Ok(node::Entity::find()
            .filter(node::Column::OwnerId.eq(owner_id))
            .filter(node::Column::ParentId.is_null())
            .one(&self.db)
            .await?)
    }

    pub(crate) async fn child_rows(&self, folder_id: Uuid) -> Result<Vec<node::Model>> {
        Ok(node::Entity::find().filter(node::Column::ParentId.eq(folder_id)).all(&self.db).await?)
    }

    pub(crate) async fn child_by_name(&self, folder_id: Uuid, name: &str) -> Result<Option<node::Model>> {
        Ok(node::Entity::find()
            .filter(node::Column::ParentId.eq(folder_id))
            .filter(node::Column::Name.eq(name))
            .one(&self.db)
            .await?)
    }

    /// The folders above `node`, root first.
    pub(crate) async fn ancestor_rows(&self, node: &node::Model) -> Result<Vec<node::Model>> {
        let mut chain: Vec<node::Model> = Vec::new();
        let mut parent_id = node.parent_id;
        while let Some(id) = parent_id {
            if chain.len() >= MAX_DEPTH || id == node.id || chain.iter().any(|folder| folder.id == id) {
                return Err(Error::internal(format!("folder chain of {} is too deep", node.id)));
            }
            let parent = self.row(id).await?;
            parent_id = parent.parent_id;
            chain.push(parent);
        }
        chain.reverse();
        Ok(chain)
    }

    /// Whether `node` is `ancestor_id` itself or lies somewhere below it.
    pub(crate) async fn is_inside(&self, node: &node::Model, ancestor_id: Uuid) -> Result<bool> {
        if node.id == ancestor_id {
            return Ok(true);
        }
        Ok(self.ancestor_rows(node).await?.iter().any(|a| a.id == ancestor_id))
    }

    /// `node` and everything below it, loaded one folder level per query.
    /// Fails, instead of going round forever, if the tree has a loop.
    pub(crate) async fn subtree(&self, node: &node::Model) -> Result<Vec<node::Model>> {
        let mut seen = HashSet::from([node.id]);
        let mut all = vec![node.clone()];
        let mut level: Vec<Uuid> = if node.is_dir { vec![node.id] } else { Vec::new() };
        let mut depth = 0;
        while !level.is_empty() {
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(Error::internal(format!("folder tree below {} is too deep", node.id)));
            }
            let children = node::Entity::find().filter(node::Column::ParentId.is_in(level)).all(&self.db).await?;
            if children.iter().any(|child| !seen.insert(child.id)) {
                return Err(Error::internal(format!("folder tree below {} has a loop", node.id)));
            }
            level = children.iter().filter(|c| c.is_dir).map(|c| c.id).collect();
            all.extend(children);
        }
        Ok(all)
    }

    /// Copies `source` into `target`; `created` collects the new ids, so a
    /// copy that lands inside its own source is not copied again.
    async fn copy_recursive(
        &self,
        source: &node::Model,
        target: &node::Model,
        name: &str,
        created: &mut HashSet<Uuid>,
    ) -> Result<node::Model> {
        let mut copy = new_node(target.owner_id, Some(target.id), name, source.is_dir);
        let mut new_key = None;
        if let Some(key) = &source.content_key {
            let key_copy = Uuid::new_v4().to_string();
            self.blobs.copy(key, &key_copy).await?;
            copy.size = Set(source.size);
            copy.mime_type = Set(source.mime_type.clone());
            copy.sha256 = Set(source.sha256.clone());
            copy.modified_by = Set(source.modified_by);
            copy.modified_at = Set(Some(source.modified_at.unwrap_or(source.updated_at)));
            copy.content_key = Set(Some(key_copy.clone()));
            new_key = Some(key_copy);
        }
        let copy = match copy.insert(&self.db).await {
            Ok(copy) => copy,
            Err(err) => {
                if let Some(key) = new_key {
                    self.discard_blob(&key).await;
                }
                return Err(name_taken(err));
            }
        };

        created.insert(copy.id);
        if source.is_dir {
            for child in self.child_rows(source.id).await? {
                if !created.contains(&child.id) {
                    Box::pin(self.copy_recursive(&child, &copy, &child.name, created)).await?;
                }
            }
        }
        Ok(copy)
    }

    // ----- content -----

    pub(crate) async fn open(&self, file: node::Model, range: Option<ByteRange>) -> Result<Download> {
        let key =
            file.content_key.as_deref().ok_or_else(|| Error::bad_request("NOT_A_FILE", "Folders have no content"))?;
        let body = self.blobs.get(key, range).await?;
        Ok(Download { node: file.into(), range, body })
    }

    /// Replaces the content of `file`; the old content becomes a version or
    /// is removed, as the writer and the settings say.
    pub(crate) async fn write_content(
        &self,
        user: &User,
        file: node::Model,
        body: ByteStream,
        writer: Writer<'_>,
    ) -> Result<Node> {
        self.refuse_locked(&[file.id], writer.app).await?;
        if writer.if_match.is_some_and(|etag| file.sha256.as_deref() != Some(etag)) {
            return Err(file_changed());
        }
        let stored = self.store(user, body, file.size).await?;

        let (old, updated) = match self.put_in_place(user, file, &stored, writer).await {
            Ok(done) => done,
            Err(err) => {
                self.discard_blob(&stored.key).await;
                return Err(err);
            }
        };
        if let Some(old_key) = &old.content_key {
            // The same bytes again are no new version.
            let changed = old.sha256.as_deref() != Some(stored.sha256.as_str());
            let kept = changed
                && self.keep_version(user, &old, writer.versioning).await.unwrap_or_else(|err| {
                    tracing::warn!(file = %old.id, %err, "could not keep the old content as a version");
                    false
                });
            if !kept {
                self.discard_blob(old_key).await;
            }
        }
        self.fit_quota(user).await;
        Ok(updated.into())
    }

    /// Puts `stored` into the row of `file`. The row changes only if it still
    /// holds the content read before, so two writes at once can't both replace
    /// (and keep, or remove) the same old content, and an ETag checked before
    /// holds up to the very write. If someone wrote in between, a writer with
    /// an ETag fails with `FILE_CHANGED`; otherwise this content goes over theirs.
    /// The row as it was and as it is now.
    async fn put_in_place(
        &self,
        user: &User,
        mut file: node::Model,
        stored: &StoredBlob,
        writer: Writer<'_>,
    ) -> Result<(node::Model, node::Model)> {
        loop {
            if let Some(updated) = self.swap_content(user, &file, stored, writer.mtime).await? {
                return Ok((file, updated));
            }
            if writer.if_match.is_some() {
                return Err(file_changed());
            }
            file = self.row(file.id).await?;
            if file.is_dir {
                return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
            }
        }
    }

    /// Puts `stored` into the row of `old` if the row still holds the content
    /// of `old`; `None` if someone replaced it in the meantime.
    async fn swap_content(
        &self,
        user: &User,
        old: &node::Model,
        stored: &StoredBlob,
        mtime: Option<DateTime<Utc>>,
    ) -> Result<Option<node::Model>> {
        let now = Utc::now();
        let still_old = match &old.content_key {
            Some(key) => node::Column::ContentKey.eq(key.clone()),
            None => node::Column::ContentKey.is_null(),
        };
        let done = node::Entity::update_many()
            .col_expr(node::Column::Size, Expr::value(stored.size))
            .col_expr(node::Column::ContentKey, Expr::value(stored.key.clone()))
            .col_expr(node::Column::Sha256, Expr::value(stored.sha256.clone()))
            .col_expr(node::Column::ModifiedBy, Expr::value(user.id))
            .col_expr(node::Column::UpdatedAt, Expr::value(now))
            .col_expr(node::Column::ModifiedAt, Expr::value(mtime))
            .filter(node::Column::Id.eq(old.id))
            .filter(still_old)
            .exec(&self.db)
            .await?;
        Ok((done.rows_affected == 1).then(|| node::Model {
            size: stored.size,
            content_key: Some(stored.key.clone()),
            sha256: Some(stored.sha256.clone()),
            modified_by: Some(user.id),
            updated_at: now,
            modified_at: mtime,
            ..old.clone()
        }))
    }

    /// Streams `body` into a new blob while counting bytes and hashing them.
    /// `freed` is the size of content this upload replaces (it does not count
    /// against the quota; if it becomes a version, versions give way). Stops with `QUOTA_EXCEEDED` or `FILE_TOO_LARGE` as
    /// soon as the quota or the upload size limit would be exceeded.
    ///
    /// Other uploads of the user at the same time count against the quota
    /// too: what they have received so far, and what they stored once done.
    /// Uploads never take the last [`DISK_RESERVE`] of the disk (`STORAGE_FULL`).
    async fn store(&self, user: &User, body: ByteStream, freed: i64) -> Result<StoredBlob> {
        let quota_limit = self.remaining_quota(user).await?.map(|remaining| remaining + freed);
        let size_limit = self.settings.read(FilesSettings::max_upload_bytes);
        let disk_limit = self.disk_room().await?.map(|room| i64::try_from(room).unwrap_or(i64::MAX));
        let meter = Arc::new(Mutex::new(Meter::default()));
        let (mut guard, finished_before) = WritingGuard::start(&self.writing, user.id, meter.clone());

        let metered = {
            let meter = meter.clone();
            let book = WritingBook { book: self.writing.clone(), user: user.id };
            body.map(move |chunk| {
                let chunk = chunk?;
                let len = chunk.len() as i64;
                let mut meter = meter.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                meter.bytes += len;
                let all_uploads = book.add(len, finished_before);
                if size_limit.is_some_and(|limit| meter.bytes > limit) {
                    return Err(io::Error::other(file_too_large()));
                }
                if quota_limit.is_some_and(|limit| all_uploads > limit) {
                    return Err(io::Error::other(quota_exceeded()));
                }
                if disk_limit.is_some_and(|limit| meter.bytes > limit) {
                    return Err(io::Error::other(storage_full()));
                }
                meter.hasher.update(&chunk);
                Ok(chunk)
            })
        };

        let key = Uuid::new_v4().to_string();
        let size = self.blobs.put(&key, Box::pin(metered)).await?;
        guard.stored = true;
        drop(guard);
        let meter = std::mem::take(&mut *meter.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        Ok(StoredBlob {
            key,
            size: i64::try_from(size).map_err(Error::internal)?,
            sha256: format!("{:x}", meter.hasher.finalize()),
        })
    }

    /// Bytes the user may still store; `None` when there is no quota.
    /// Versions don't count here: they make room for files ([`Files::fit_quota`]).
    async fn remaining_quota(&self, user: &User) -> Result<Option<i64>> {
        match user.quota_bytes {
            Some(quota) => Ok(Some(std::cmp::max(quota - self.files_bytes(user.id).await?, 0))),
            None => Ok(None),
        }
    }

    /// Size of the owner's files, the trash included.
    pub(crate) async fn files_bytes(&self, owner_id: Uuid) -> Result<i64> {
        // SUM() of a BIGINT is NUMERIC in PostgreSQL; cast it back.
        let total = Func::cast_as(
            Func::coalesce([Expr::from(Func::sum(Expr::col(node::Column::Size))), Expr::val(0)]),
            "BIGINT",
        );
        let used: Option<i64> = node::Entity::find()
            .select_only()
            .column_as(Expr::from(total), "used")
            .filter(node::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(used.unwrap_or(0))
    }

    /// Removes a blob that is no longer referenced. Failures are only logged:
    /// the database is already consistent, and a leftover blob is harmless.
    pub(crate) async fn discard_blob(&self, key: &str) {
        if let Err(err) = self.blobs.delete(key).await {
            tracing::warn!(key, %err, "could not remove an unused blob");
        }
    }
}

/// Counts the bytes of one upload into the user's uploads under way.
struct WritingBook {
    book: Arc<Mutex<HashMap<Uuid, Writing>>>,
    user: Uuid,
}

impl WritingBook {
    /// Adds `received` bytes of this upload; returns what all uploads of the
    /// user add to the quota for it: their bytes under way (its own too), and
    /// the bytes of those that finished since it began.
    fn add(&self, received: i64, finished_before: i64) -> i64 {
        let mut book = self.book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let writing = book.entry(self.user).or_default();
        writing.bytes += received;
        writing.finished - finished_before + writing.bytes
    }
}

/// Takes an upload out of the user's uploads under way when it ends, also
/// when the request goes away halfway.
struct WritingGuard<'a> {
    book: &'a Mutex<HashMap<Uuid, Writing>>,
    user: Uuid,
    meter: Arc<Mutex<Meter>>,
    stored: bool,
}

impl<'a> WritingGuard<'a> {
    /// Counts a new upload of `user`; also returns the bytes of the uploads
    /// that finished before it.
    fn start(book: &'a Mutex<HashMap<Uuid, Writing>>, user: Uuid, meter: Arc<Mutex<Meter>>) -> (Self, i64) {
        let mut uploads = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let writing = uploads.entry(user).or_default();
        writing.uploads += 1;
        let finished = writing.finished;
        drop(uploads);
        (Self { book, user, meter, stored: false }, finished)
    }
}

impl Drop for WritingGuard<'_> {
    fn drop(&mut self) {
        let own = self.meter.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).bytes;
        let mut book = self.book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(writing) = book.get_mut(&self.user) {
            writing.uploads -= 1;
            writing.bytes -= own;
            if self.stored {
                writing.finished += own;
            }
            if writing.uploads == 0 {
                book.remove(&self.user);
            }
        }
    }
}

#[derive(Default)]
struct Meter {
    bytes: i64,
    hasher: Sha256,
}

struct StoredBlob {
    key: String,
    size: i64,
    sha256: String,
}

/// A new node row with fresh id and timestamps; content fields are empty.
fn new_node(owner_id: Uuid, parent_id: Option<Uuid>, name: &str, is_dir: bool) -> node::ActiveModel {
    let now = Utc::now();
    node::ActiveModel {
        id: Set(Uuid::new_v4()),
        owner_id: Set(owner_id),
        parent_id: Set(parent_id),
        name: Set(name.to_owned()),
        is_dir: Set(is_dir),
        size: Set(0),
        mime_type: Set(None),
        content_key: Set(None),
        sha256: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        trashed_at: Set(None),
        trashed_from: Set(None),
        modified_by: Set(None),
        modified_at: Set(None),
    }
}

/// How far ahead of the server's clock an mtime may be: clocks of devices
/// are seldom exact, but a file from next year is a mistake.
const MTIME_AHEAD: chrono::Duration = chrono::Duration::days(1);

/// An mtime a client sent, if it makes sense: not before 1970 and not in
/// the future (beyond [`MTIME_AHEAD`]).
pub(crate) fn check_mtime(mtime: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if mtime < DateTime::UNIX_EPOCH || mtime > Utc::now() + MTIME_AHEAD {
        return Err(Error::bad_request("INVALID_MTIME", "The modification time is out of range"));
    }
    // Databases keep microseconds.
    Ok(mtime.trunc_subsecs(6))
}

/// MIME type from the file extension; `application/octet-stream` if unknown.
pub fn guess_mime(name: &str) -> String {
    mime_guess::from_path(name).first_or_octet_stream().essence_str().to_owned()
}

fn not_found() -> Error {
    Error::not_found("FILE_NOT_FOUND", "File or folder not found")
}

/// Whether a download reads the file from its start. Players fetch a video in
/// many range requests; only the first one is worth an activity log entry.
pub(crate) fn starts_reading(range: Option<ByteRange>) -> bool {
    range.is_none_or(|range| range.start == 0)
}

fn trash_folder() -> Error {
    Error::bad_request("TRASH_FOLDER", "The trash folder itself can't be renamed, moved or deleted")
}

fn file_too_large() -> Error {
    Error::new(ErrorKind::PayloadTooLarge, "FILE_TOO_LARGE", "The file is larger than the upload limit")
}

fn file_changed() -> Error {
    Error::conflict("FILE_CHANGED", "The file has changed since it was read")
}

fn quota_exceeded() -> Error {
    Error::new(ErrorKind::PayloadTooLarge, "QUOTA_EXCEEDED", "Not enough storage quota")
}

fn storage_full() -> Error {
    Error::new(ErrorKind::PayloadTooLarge, "STORAGE_FULL", "The server's storage is full")
}

/// A duplicate name in the target folder becomes a clear `NAME_TAKEN` error.
pub(crate) fn name_taken(err: sea_orm::DbErr) -> Error {
    match Error::from(err) {
        e if e.kind == ErrorKind::Conflict => {
            Error::conflict("NAME_TAKEN", "An item with this name already exists in the folder")
        }
        e => e,
    }
}
