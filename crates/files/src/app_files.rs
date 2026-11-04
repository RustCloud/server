//! The user's files as an app (plugin) sees them: only what the user gave the
//! app access to (see [`grants`](crate::grants)).
//!
//! Anything outside the granted files and folders is reported as missing, so
//! an app learns nothing about the rest. Changes need write access and are
//! activity log events naming the app, unlike the app's own AppData.

use bytes::Bytes;
use futures_util::stream;
use rustcloud_auth::User;
use rustcloud_core::{audit, validate, ByteRange, ByteStream, Error, Result};
use rustcloud_db::node;
use uuid::Uuid;

use crate::appdata::APPDATA_FOLDER;
use crate::events::FileEventKind;
use crate::grants::Reach;
use crate::service::{Download, Files, Node, Writer};

impl Files {
    /// The files and folders the user gave the app access to.
    pub async fn app_granted(&self, user: &User, app_id: &str) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        for grant in self.active_grant_rows(user, Some(app_id)).await? {
            if let Ok(node) = self.accessible(user, grant.node_id).await {
                if node.trashed_at.is_none() && !nodes.iter().any(|n: &Node| n.id == node.id) {
                    nodes.push(node.into());
                }
            }
        }
        Ok(nodes)
    }

    pub async fn app_get(&self, user: &User, app_id: &str, id: Uuid) -> Result<Node> {
        Ok(self.app_node(user, app_id, id, false).await?.into())
    }

    /// Contents of a granted folder: folders first, then files, by name.
    pub async fn app_children(&self, user: &User, app_id: &str, folder_id: Uuid) -> Result<Vec<Node>> {
        let folder = self.app_node(user, app_id, folder_id, false).await?;
        if !folder.is_dir {
            return Err(Error::bad_request("NOT_A_FOLDER", "This is a file"));
        }
        let mut children = Vec::new();
        for child in self.child_rows(folder.id).await? {
            // The trash and the apps' data are the user's own.
            let appdata = folder.parent_id.is_none() && child.is_dir && child.name == APPDATA_FOLDER;
            if !appdata && !self.is_trash_folder(&child).await? {
                children.push(Node::from(child));
            }
        }
        children.sort_by_cached_key(|n| (!n.is_dir, n.name.to_lowercase()));
        Ok(children)
    }

    /// The whole content of a granted file, if it is at most `limit` bytes,
    /// and the file. The caller writes the reads of a request or a batch to
    /// the journal at once: an app may read thousands.
    pub async fn app_read(&self, user: &User, app_id: &str, id: Uuid, limit: u64) -> Result<(Vec<u8>, Node)> {
        let file = self.app_node(user, app_id, id, false).await?;
        let node = Node::from(file.clone());
        Ok((self.read_whole(file, limit).await?, node))
    }

    /// Up to `length` bytes of a granted file from `offset`, and the file.
    pub async fn app_read_range(
        &self,
        user: &User,
        app_id: &str,
        id: Uuid,
        offset: u64,
        length: u64,
    ) -> Result<(Vec<u8>, Node)> {
        let file = self.app_file(user, app_id, id).await?;
        let node = Node::from(file.clone());
        Ok((self.read_range(file, offset, length).await?, node))
    }

    /// A granted file for the server to send for the app (a link of its
    /// page, a file it answers with): `range` of it, or all.
    pub async fn app_download(
        &self,
        user: &User,
        app_id: &str,
        id: Uuid,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        let file = self.app_file(user, app_id, id).await?;
        self.open(file, range).await
    }

    /// A granted file the app may read, not a folder.
    async fn app_file(&self, user: &User, app_id: &str, id: Uuid) -> Result<node::Model> {
        let file = self.app_node(user, app_id, id, false).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        Ok(file)
    }

    /// A granted file opened for reading, for a tool the server runs for the
    /// app (ffmpeg), and the file. As [`Self::app_read`], not in the journal.
    pub async fn app_open(&self, user: &User, app_id: &str, id: Uuid) -> Result<(std::fs::File, Node)> {
        let file = self.app_node(user, app_id, id, false).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        let opened = self.open_local(&file).await?;
        Ok((opened, file.into()))
    }

    /// Replaces the content of a granted file; with `if_match`, only if it
    /// still has that ETag (`FILE_CHANGED` otherwise).
    pub async fn app_write(
        &self,
        user: &User,
        app_id: &str,
        id: Uuid,
        data: Vec<u8>,
        if_match: Option<&str>,
    ) -> Result<Node> {
        let file = self.app_node(user, app_id, id, true).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        let parent = self.row(file.parent_id.ok_or_else(|| Error::bad_request("ROOT_FOLDER", "Not a file"))?).await?;
        let name = file.name.clone();
        let (node, _) =
            self.put_file(user, &parent, &name, body(data), true, Writer { if_match, ..Writer::app(app_id) }).await?;
        self.emit_for(FileEventKind::Changed, node.id, Some(app_id)).await;
        audit!("file.upload", name, size = node.size, replaced = true, app = app_id, "file changed by an app");
        Ok(node)
    }

    /// Creates a file in a granted folder; the name must be free.
    pub async fn app_create_file(
        &self,
        user: &User,
        app_id: &str,
        folder_id: Uuid,
        name: &str,
        data: Vec<u8>,
    ) -> Result<Node> {
        let name = validate::file_name(name)?;
        let folder = self.app_folder(user, app_id, folder_id).await?;
        let (node, _) = self.put_file(user, &folder, name, body(data), false, Writer::app(app_id)).await?;
        self.emit_for(FileEventKind::Created, node.id, Some(app_id)).await;
        audit!("file.upload", name, size = node.size, app = app_id, "file created by an app");
        Ok(node)
    }

    /// Fails unless the app's page may upload `name` into a granted folder:
    /// the app may change the folder, and the name is valid and, unless
    /// `overwrite`, free. Checked when the page gets a link to upload to.
    pub async fn app_upload_check(
        &self,
        user: &User,
        app_id: &str,
        folder_id: Uuid,
        name: &str,
        overwrite: bool,
    ) -> Result<()> {
        let name = validate::file_name(name)?;
        let folder = self.app_folder(user, app_id, folder_id).await?;
        match self.child_by_name(folder.id, name).await? {
            Some(existing) if existing.is_dir || !overwrite => {
                Err(Error::conflict("NAME_TAKEN", format!("'{name}' already exists in this folder")))
            }
            _ => Ok(()),
        }
    }

    /// Stores a file the app's page uploads into a granted folder, as a
    /// stream that never passes through the app. Unlike what the app writes
    /// itself, its jobs hear of it: the file comes from the user.
    pub async fn app_upload(
        &self,
        user: &User,
        app_id: &str,
        folder_id: Uuid,
        name: &str,
        body: ByteStream,
        overwrite: bool,
    ) -> Result<Node> {
        let name = validate::file_name(name)?;
        let folder = self.app_folder(user, app_id, folder_id).await?;
        let (node, replaced) = self.put_file(user, &folder, name, body, overwrite, Writer::app(app_id)).await?;
        let kind = if replaced { FileEventKind::Changed } else { FileEventKind::Created };
        self.emit_for(kind, node.id, None).await;
        audit!("file.upload", name, size = node.size, replaced, app = app_id, "file uploaded through an app's page");
        Ok(node)
    }

    /// Fails unless the app's page may replace a granted file. Checked when
    /// the page gets a link to upload it to.
    pub async fn app_replace_check(&self, user: &User, app_id: &str, id: Uuid) -> Result<()> {
        let file = self.app_node(user, app_id, id, true).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        Ok(())
    }

    /// Replaces a granted file with what the app's page uploads, as a stream
    /// that never passes through the app; with `if_match`, only if the file
    /// still has that ETag. Its jobs hear of it, as of other uploads.
    pub async fn app_replace(
        &self,
        user: &User,
        app_id: &str,
        id: Uuid,
        body: ByteStream,
        if_match: Option<&str>,
    ) -> Result<Node> {
        let file = self.app_node(user, app_id, id, true).await?;
        if file.is_dir {
            return Err(Error::bad_request("NOT_A_FILE", "This is a folder"));
        }
        let name = file.name.clone();
        let node = self.write_content(user, file, body, Writer { if_match, ..Writer::app(app_id) }).await?;
        self.emit_for(FileEventKind::Changed, node.id, None).await;
        audit!(
            "file.upload",
            name,
            size = node.size,
            replaced = true,
            app = app_id,
            "file uploaded through an app's page"
        );
        Ok(node)
    }

    /// Creates a folder in a granted folder.
    pub async fn app_create_folder(&self, user: &User, app_id: &str, parent_id: Uuid, name: &str) -> Result<Node> {
        let name = validate::file_name(name)?;
        let parent = self.app_folder(user, app_id, parent_id).await?;
        let folder = self.insert_folder(&parent, name).await?;
        audit!("folder.create", name, app = app_id, "folder created by an app");
        Ok(folder.into())
    }

    /// Moves a file or folder inside a granted folder to the trash. The
    /// granted folder itself can't be deleted by the app.
    pub async fn app_delete(&self, user: &User, app_id: &str, id: Uuid) -> Result<()> {
        let _moving = self.moving.lock().await;
        let node = self.app_node(user, app_id, id, true).await?;
        if self.app_granted(user, app_id).await?.iter().any(|granted| granted.id == node.id) {
            return Err(Error::forbidden("An app can't delete what it was given access to").with_code("READ_ONLY"));
        }
        let ids: Vec<Uuid> = self.subtree(&node).await?.iter().map(|n| n.id).collect();
        self.refuse_locked(&ids, Some(app_id)).await?;
        let files = self.watched_or_none(&node).await;
        self.move_to_trash(&node).await?;
        self.emit(FileEventKind::Deleted, node.owner_id, files, Some(app_id));
        audit!("file.trash", name = node.name, folder = node.is_dir, app = app_id, "moved to the trash by an app");
        Ok(())
    }

    /// A granted folder the app may change.
    async fn app_folder(&self, user: &User, app_id: &str, id: Uuid) -> Result<node::Model> {
        let folder = self.app_node(user, app_id, id, true).await?;
        if !folder.is_dir {
            return Err(Error::bad_request("NOT_A_FOLDER", "This is a file"));
        }
        Ok(folder)
    }

    /// A node the app may read (and with `write`, change). Nodes outside the
    /// app's access are missing; read-only access to a change is `READ_ONLY`.
    pub(crate) async fn app_node(&self, user: &User, app_id: &str, id: Uuid, write: bool) -> Result<node::Model> {
        match self.app_reach(user, app_id, id).await? {
            Reach::None => return Err(Error::not_found("FILE_NOT_FOUND", "File not found")),
            Reach::Read if write => {
                return Err(Error::forbidden("The app may only read this").with_code("READ_ONLY"));
            }
            Reach::Read | Reach::Write => {}
        }
        self.accessible(user, id).await
    }
}

fn body(data: Vec<u8>) -> ByteStream {
    Box::pin(stream::once(async move { Ok(Bytes::from(data)) }))
}
