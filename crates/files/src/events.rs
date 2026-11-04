//! What happens to users' files, for whoever watches: background jobs of
//! plugins start from here (`file.created`, `file.changed`, `file.deleted`,
//! `file.moved`). Only changes through the API, WebDAV and apps count;
//! AppData has none.
//!
//! An event names the apps that watch each of its files — those the owner
//! gave a folder above it "for good" — worked out when it happens: once a
//! file is deleted, nobody can tell any more where it was.

use std::collections::HashMap;

use rustcloud_auth::User;
use rustcloud_core::Result;
use rustcloud_db::{app_grant, node};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{Files, GrantKind, Node};

/// Events that wait for a slow watcher before the oldest are lost.
pub(crate) const EVENT_BACKLOG: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEventKind {
    Created,
    Changed,
    Deleted,
    /// Moved to another folder or renamed.
    Moved,
}

impl FileEventKind {
    /// The name plugins use in their manifest: `file.created`, `file.changed`…
    pub fn name(self) -> &'static str {
        match self {
            Self::Created => "file.created",
            Self::Changed => "file.changed",
            Self::Deleted => "file.deleted",
            Self::Moved => "file.moved",
        }
    }
}

/// Files were created, changed, deleted or moved: one event for one
/// operation, so a deleted folder is one event with all its files.
#[derive(Debug, Clone)]
pub struct FileEvent {
    pub kind: FileEventKind,
    /// Whose files they are.
    pub owner_id: Uuid,
    /// The files, not folders, that some app watches. A deleted file is as
    /// it was; a moved one as it is now.
    pub files: Vec<WatchedFile>,
    /// The app that did it, if one did.
    pub app_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WatchedFile {
    pub node: Node,
    /// Apps that watch the file: for a moved one, where it was or where it is.
    pub apps: Vec<String>,
}

impl Files {
    /// Events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<FileEvent> {
        self.events.subscribe()
    }

    /// Tells watchers about `files`, if any app watches them.
    pub(crate) fn emit(&self, kind: FileEventKind, owner_id: Uuid, files: Vec<WatchedFile>, app_id: Option<&str>) {
        if files.is_empty() {
            return;
        }
        // Nobody listening is not an error.
        let _ = self.events.send(FileEvent { kind, owner_id, files, app_id: app_id.map(str::to_owned) });
    }

    /// Works out who watches node `id` and what is inside it, and tells them.
    /// The change is already made; a failure here doesn't undo it.
    pub(crate) async fn emit_for(&self, kind: FileEventKind, id: Uuid, app_id: Option<&str>) {
        match self.row(id).await {
            Ok(node) => {
                let files = self.watched_or_none(&node).await;
                self.emit(kind, node.owner_id, files, app_id);
            }
            Err(err) => tracing::warn!("file events: {err}"),
        }
    }

    /// [`Self::watched`], or nothing with a warning: an event lost is better
    /// than a failed upload or deletion.
    pub(crate) async fn watched_or_none(&self, node: &node::Model) -> Vec<WatchedFile> {
        self.watched(node).await.unwrap_or_else(|err| {
            tracing::warn!("file events: {err}");
            Vec::new()
        })
    }

    /// The files of `top` (itself, or everything in it but hidden folders)
    /// that apps watch, each with those apps: the owner gave them `top`, a
    /// folder above it, or one on the way down to the file, until revoked.
    pub(crate) async fn watched(&self, top: &node::Model) -> Result<Vec<WatchedFile>> {
        let grants = app_grant::Entity::find()
            .filter(app_grant::Column::UserId.eq(top.owner_id))
            .filter(app_grant::Column::Kind.eq(GrantKind::Standing.as_str()))
            .all(&self.db)
            .await?;
        if grants.is_empty() {
            return Ok(Vec::new());
        }
        let given = |node_id: Uuid| grants.iter().filter(move |g| g.node_id == node_id).map(|g| g.app_id.clone());

        let ancestors = self.ancestor_rows(top).await?;
        if top.name.starts_with('.') || ancestors.iter().any(|folder| folder.name.starts_with('.')) {
            return Ok(Vec::new());
        }
        let mut above = Vec::new();
        for folder in ancestors {
            above.extend(given(folder.id));
        }
        let mut files = Vec::new();
        let mut level = vec![(top.clone(), above)];
        while !level.is_empty() {
            let mut folders: HashMap<Uuid, Vec<String>> = HashMap::new();
            for (node, mut apps) in level {
                apps.extend(given(node.id));
                if node.is_dir {
                    folders.insert(node.id, apps);
                } else if !apps.is_empty() {
                    apps.sort();
                    apps.dedup();
                    files.push(WatchedFile { node: node.into(), apps });
                }
            }
            if folders.is_empty() {
                break;
            }
            let children = node::Entity::find()
                .filter(node::Column::ParentId.is_in(folders.keys().copied()))
                .all(&self.db)
                .await?;
            level = children
                .into_iter()
                .filter(|child| !child.name.starts_with('.'))
                .map(|child| {
                    let apps = child.parent_id.and_then(|id| folders.get(&id)).cloned().unwrap_or_default();
                    (child, apps)
                })
                .collect();
        }
        Ok(files)
    }

    /// The files a user's grant gives its app, without hidden folders (AppData).
    pub async fn granted_files(&self, user: &User, grant_id: Uuid) -> Result<Vec<Node>> {
        let grant = app_grant::Entity::find_by_id(grant_id)
            .filter(app_grant::Column::UserId.eq(user.id))
            .one(&self.db)
            .await?
            .ok_or_else(|| rustcloud_core::Error::not_found("GRANT_NOT_FOUND", "No such grant"))?;
        let top = self.accessible(user, grant.node_id).await?;
        if !top.is_dir {
            return Ok(vec![top.into()]);
        }
        let mut files = Vec::new();
        let mut level = vec![top.id];
        while !level.is_empty() {
            let children = node::Entity::find().filter(node::Column::ParentId.is_in(level)).all(&self.db).await?;
            level = Vec::new();
            for child in children.into_iter().filter(|child| !child.name.starts_with('.')) {
                match child.is_dir {
                    true => level.push(child.id),
                    false => files.push(child.into()),
                }
            }
        }
        Ok(files)
    }
}

/// The files of a move: those watched where they are `now`, and those
/// watched where they were `before`, each with the apps of both.
pub(crate) fn together(now: Vec<WatchedFile>, before: Vec<WatchedFile>) -> Vec<WatchedFile> {
    let mut files: HashMap<Uuid, WatchedFile> = before.into_iter().map(|file| (file.node.id, file)).collect();
    for file in now {
        match files.get_mut(&file.node.id) {
            Some(old) => {
                old.node = file.node;
                old.apps.extend(file.apps);
                old.apps.sort();
                old.apps.dedup();
            }
            None => {
                files.insert(file.node.id, file);
            }
        }
    }
    files.into_values().collect()
}
