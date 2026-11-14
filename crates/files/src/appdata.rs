//! AppData: each app's own folder in each user's files, `.appdata/<app id>/`.
//!
//! An app keeps there whatever it needs for that user (calendar files,
//! templates, indexes, settings). It is an ordinary hidden folder: it counts
//! against the user's quota, the user can show hidden files and open or
//! download it, and it stays when the app is removed. The app reaches only its
//! own folder, by paths relative to it (and folders other users shared with
//! the user, see `app_shares.rs`); writes and deletes there are the app's
//! housekeeping and are not activity log events.

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use rustcloud_auth::User;
use rustcloud_core::{validate, ByteRange, ByteStream, Error, ErrorKind, Result};
use rustcloud_db::node;
use uuid::Uuid;

use crate::service::{Download, Files, Node, Writer};
use crate::versions::Versioning;

/// Name of the hidden folder that holds every app's data.
pub const APPDATA_FOLDER: &str = ".appdata";

impl Files {
    /// Reads a file from the app's AppData, if it is at most `limit` bytes.
    pub async fn appdata_read(&self, user: &User, app_id: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
        let (parent, name) = self.appdata_parent(user, app_id, path, false).await?;
        let file = self.child_by_name(parent.id, name).await?.filter(|n| !n.is_dir).ok_or_else(not_found)?;
        self.read_whole(file, limit).await
    }

    /// A file of the app's AppData.
    pub async fn appdata_file(&self, user: &User, app_id: &str, path: &str) -> Result<Node> {
        Ok(self.appdata_row(user, app_id, path).await?.into())
    }

    /// Up to `length` bytes of a file of the app's AppData from `offset`.
    pub async fn appdata_read_range(
        &self,
        user: &User,
        app_id: &str,
        path: &str,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        let file = self.appdata_row(user, app_id, path).await?;
        self.read_range(file, offset, length).await
    }

    /// A file of the app's AppData for the server to send for the app.
    pub async fn appdata_download(
        &self,
        user: &User,
        app_id: &str,
        path: &str,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        let file = self.appdata_row(user, app_id, path).await?;
        self.open(file, range).await
    }

    async fn appdata_row(&self, user: &User, app_id: &str, path: &str) -> Result<node::Model> {
        let (parent, name) = self.appdata_parent(user, app_id, path, false).await?;
        self.child_by_name(parent.id, name).await?.filter(|n| !n.is_dir).ok_or_else(not_found)
    }

    /// Up to `length` bytes of `file` from `offset`: fewer at the end, none past it.
    pub(crate) async fn read_range(&self, file: node::Model, offset: u64, length: u64) -> Result<Vec<u8>> {
        let end = (file.size.max(0) as u64).min(offset.saturating_add(length));
        if offset >= end {
            return Ok(Vec::new());
        }
        let mut body = self.open(file, Some(ByteRange { start: offset, end: end - 1 })).await?.body;
        let mut data = Vec::new();
        while let Some(chunk) = body.next().await {
            data.extend_from_slice(&chunk?);
        }
        Ok(data)
    }

    /// A file of the app's AppData opened for reading, for a tool the server
    /// runs for the app (ffmpeg).
    pub async fn appdata_open(&self, user: &User, app_id: &str, path: &str) -> Result<std::fs::File> {
        let (parent, name) = self.appdata_parent(user, app_id, path, false).await?;
        let file = self.child_by_name(parent.id, name).await?.filter(|n| !n.is_dir).ok_or_else(not_found)?;
        self.open_local(&file).await
    }

    /// A file of the user opened for reading, for the media engine
    /// (thumbnails), with what it is.
    pub async fn open_for_media(&self, user: &User, id: Uuid) -> Result<(Node, std::fs::File)> {
        let file = self.readable(user, id).await?;
        self.media_file(file).await
    }

    /// A file reached through a public link opened for the media engine.
    /// Not one a visitor uploaded through a link: anyone could have the
    /// engine (ffmpeg, written in C) take apart a file made to attack it,
    /// as often as they like. The owner's own view still shows it.
    pub async fn public_open_for_media(
        &self,
        token: &str,
        key: Option<&str>,
        node_id: Option<Uuid>,
    ) -> Result<(Node, std::fs::File)> {
        let file = self.public_node(token, key, node_id).await?;
        if file.modified_by == Some(crate::shares::LINK_VISITOR) {
            return Err(Error::not_found("NO_THUMBNAIL", "There is no thumbnail of this file"));
        }
        self.media_file(file).await
    }

    async fn media_file(&self, file: node::Model) -> Result<(Node, std::fs::File)> {
        if file.is_dir || file.trashed_at.is_some() {
            return Err(not_found());
        }
        let local = self.open_local(&file).await?;
        Ok((file.into(), local))
    }

    /// A file's content as a file opened for reading.
    pub(crate) async fn open_local(&self, file: &node::Model) -> Result<std::fs::File> {
        let key = file.content_key.as_deref().ok_or_else(not_found)?;
        rustcloud_core::open_as_file(self.blobs.as_ref(), key).await
    }

    /// The whole content of a file, for apps: at most `limit` bytes, what
    /// the app may take at once.
    pub(crate) async fn read_whole(&self, file: node::Model, limit: u64) -> Result<Vec<u8>> {
        if file.size as u64 > limit {
            return Err(Error::new(
                ErrorKind::PayloadTooLarge,
                "TOO_LARGE_TO_READ",
                "The file is too large to read at once",
            ));
        }
        let mut body = self.open(file, None).await?.body;
        let mut data = Vec::new();
        while let Some(chunk) = body.next().await {
            data.extend_from_slice(&chunk?);
        }
        Ok(data)
    }

    /// Writes (creates or replaces) a file in the app's AppData, creating
    /// folders on the way.
    pub async fn appdata_write(&self, user: &User, app_id: &str, path: &str, data: Vec<u8>) -> Result<Node> {
        let (parent, name) = self.appdata_parent(user, app_id, path, true).await?;
        let body: ByteStream = Box::pin(stream::once(async move { Ok(Bytes::from(data)) }));
        let writer = Writer { versioning: Versioning::Off, ..Writer::app(app_id) };
        Ok(self.put_file(user, &parent, name, body, true, writer).await?.0)
    }

    /// Contents of a folder in the app's AppData; `""` is the AppData folder
    /// itself. A missing AppData folder is empty.
    pub async fn appdata_list(&self, user: &User, app_id: &str, dir: &str) -> Result<Vec<Node>> {
        let Some(mut folder) = self.appdata_root(user, app_id, false).await? else {
            return Ok(Vec::new());
        };
        for name in segments(dir)? {
            folder = self.child_by_name(folder.id, name).await?.filter(|n| n.is_dir).ok_or_else(not_found)?;
        }
        let mut children: Vec<Node> = self.child_rows(folder.id).await?.into_iter().map(Node::from).collect();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(children)
    }

    /// Deletes a file or folder in the app's AppData.
    pub async fn appdata_delete(&self, user: &User, app_id: &str, path: &str) -> Result<()> {
        let (parent, name) = self.appdata_parent(user, app_id, path, false).await?;
        let node = self.child_by_name(parent.id, name).await?.ok_or_else(not_found)?;
        self.remove(&node).await?;
        if node.is_dir {
            self.forget_appdata_shares(user, app_id, &segments(path)?.join("/")).await?;
        }
        Ok(())
    }

    /// The folder at `path` in the app's AppData, if there is one.
    pub(crate) async fn appdata_folder(&self, user: &User, app_id: &str, path: &str) -> Result<Option<node::Model>> {
        let Some(mut folder) = self.appdata_root(user, app_id, false).await? else {
            return Ok(None);
        };
        for name in segments(path)? {
            match self.child_by_name(folder.id, name).await?.filter(|n| n.is_dir) {
                Some(child) => folder = child,
                None => return Ok(None),
            }
        }
        Ok(Some(folder))
    }

    /// The folder that holds the last segment of `path`, and that segment.
    async fn appdata_parent<'p>(
        &self,
        user: &User,
        app_id: &str,
        path: &'p str,
        create: bool,
    ) -> Result<(node::Model, &'p str)> {
        let mut names = segments(path)?;
        let last = names.pop().ok_or_else(|| Error::bad_request("INVALID_NAME", "The path is empty"))?;
        let mut folder = self.appdata_root(user, app_id, create).await?.ok_or_else(not_found)?;
        for name in names {
            folder = self.subfolder(&folder, name, create).await?;
        }
        Ok((folder, last))
    }

    /// `.appdata/<app id>` in the user's root folder, created if `create`.
    async fn appdata_root(&self, user: &User, app_id: &str, create: bool) -> Result<Option<node::Model>> {
        self.root(user).await?;
        let root = self.find_root(user.id).await?.ok_or_else(|| Error::internal("root folder missing"))?;
        let Some(all) = self.find_subfolder(&root, APPDATA_FOLDER, create).await? else {
            return Ok(None);
        };
        self.find_subfolder(&all, app_id, create).await
    }

    async fn subfolder(&self, parent: &node::Model, name: &str, create: bool) -> Result<node::Model> {
        self.find_subfolder(parent, name, create).await?.ok_or_else(not_found)
    }

    /// Folder `name` in `parent`; created if missing and `create` is set. A
    /// file with that name is an error.
    async fn find_subfolder(&self, parent: &node::Model, name: &str, create: bool) -> Result<Option<node::Model>> {
        match self.child_by_name(parent.id, name).await? {
            Some(existing) if existing.is_dir => Ok(Some(existing)),
            Some(_) => Err(Error::conflict("NAME_TAKEN", format!("'{name}' is a file, not a folder"))),
            None if !create => Ok(None),
            None => match self.insert_folder(parent, name).await {
                Ok(folder) => Ok(Some(folder)),
                // Created by a parallel request in the meantime: use that one.
                Err(_) => Ok(self.child_by_name(parent.id, name).await?.filter(|n| n.is_dir)),
            },
        }
    }
}

/// The names in a `/`-separated relative path, each a valid file name.
pub(crate) fn segments(path: &str) -> Result<Vec<&str>> {
    path.split('/').filter(|s| !s.is_empty()).map(validate::file_name).collect()
}

fn not_found() -> Error {
    Error::not_found("FILE_NOT_FOUND", "No such file in the app's data")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_relative_names_only() {
        assert_eq!(segments("notes/2026/todo.md").unwrap(), ["notes", "2026", "todo.md"]);
        assert_eq!(segments("/a//b/").unwrap(), ["a", "b"]);
        assert!(segments("").unwrap().is_empty());
        assert_eq!(segments("../secret").unwrap_err().code, "INVALID_NAME");
        assert_eq!(segments("a/./b").unwrap_err().code, "INVALID_NAME");
    }
}
