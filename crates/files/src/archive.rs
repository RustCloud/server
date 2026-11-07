//! A folder downloaded as one zip archive, written while it is sent: the
//! server never holds the archive, nor more than a little of one file.
//!
//! Files are stored as they are (no compression): most large files —
//! photos, videos, archives — are compressed already, and the archive's
//! size stays the sum of the files'. Files of 4 GiB and more use ZIP64.
//! The trash is left out of a download of the root folder.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Timelike, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, ByteStream, Error, Result};
use rustcloud_db::node;
use tokio::runtime::Handle;
use tokio_util::io::{ReaderStream, StreamReader, SyncIoBridge};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::service::Files;
use crate::trash::TRASH_FOLDER;

/// Bytes on their way between the writer of the archive and the client.
const PIPE: usize = 256 << 10;
/// Most files and folders in one archive.
pub const ZIP_MAX_ENTRIES: usize = 100_000;

/// A zip archive being written.
pub struct ZipDownload {
    /// `<folder>.zip`
    pub name: String,
    pub body: ByteStream,
}

impl Files {
    /// A folder of the user as a zip archive.
    pub async fn download_zip(self: &Arc<Self>, user: &User, id: Uuid) -> Result<ZipDownload> {
        let folder = self.readable(user, id).await?;
        if self.in_trash(&folder).await? {
            return Err(Error::bad_request("IN_TRASH", "Restore it to download it"));
        }
        self.zip(folder).await
    }

    /// A folder reached through a public link as a zip archive.
    pub async fn public_zip(
        self: &Arc<Self>,
        token: &str,
        key: Option<&str>,
        node_id: Option<Uuid>,
    ) -> Result<ZipDownload> {
        let folder = self.public_node(token, key, node_id).await?;
        self.zip(folder).await
    }

    async fn zip(self: &Arc<Self>, folder: node::Model) -> Result<ZipDownload> {
        if !folder.is_dir {
            return Err(Error::bad_request("NOT_A_FOLDER", "Only folders are downloaded as archives"));
        }
        let entries = archive_entries(&folder, self.subtree(&folder).await?)?;
        let name = if folder.parent_id.is_none() { "files.zip".to_owned() } else { format!("{}.zip", folder.name) };
        let files = entries.iter().filter(|(_, node)| !node.is_dir).count();
        let size: i64 = entries.iter().map(|(_, node)| node.size).sum();
        audit!("file.download", name = folder.name, files, size, folder = true, "folder downloaded as zip");

        let (reader, writer) = tokio::io::duplex(PIPE);
        let this = self.clone();
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            // The client went away, or a file could not be read: the archive
            // just ends, and the client sees it broken.
            if let Err(err) = write_archive(&this, &handle, entries, SyncIoBridge::new(writer)) {
                tracing::debug!("zip archive ended early: {err}");
            }
        });
        Ok(ZipDownload { name, body: Box::pin(ReaderStream::new(reader)) })
    }
}

/// The paths in the archive of everything below `folder`, parents before
/// what is in them. The trash and what is in it are left out.
fn archive_entries(folder: &node::Model, subtree: Vec<node::Model>) -> Result<Vec<(String, node::Model)>> {
    if subtree.len() > ZIP_MAX_ENTRIES + 1 {
        return Err(Error::bad_request("TOO_MANY_FILES", "The folder has too many files for one archive"));
    }
    let is_trash = |node: &node::Model| {
        node.trashed_at.is_some()
            || (folder.parent_id.is_none() && node.parent_id == Some(folder.id) && node.name == TRASH_FOLDER)
    };
    let by_id: HashMap<Uuid, &node::Model> = subtree.iter().map(|node| (node.id, node)).collect();
    let mut entries = Vec::new();
    for node in subtree.iter().filter(|node| node.id != folder.id && !is_trash(node)) {
        let mut names = vec![node.name.as_str()];
        let mut parent = node.parent_id;
        while let Some(id) = parent.filter(|id| *id != folder.id) {
            let Some(above) = by_id.get(&id) else { break };
            names.push(&above.name);
            parent = above.parent_id;
        }
        names.reverse();
        let mut path = names.join("/");
        if node.is_dir {
            path.push('/');
        }
        entries.push((path, node.clone()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

fn write_archive(
    files: &Files,
    handle: &Handle,
    entries: Vec<(String, node::Model)>,
    out: impl Write,
) -> io::Result<()> {
    let mut zip = ZipWriter::new_stream(out);
    for (path, node) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(zip_time(node.modified_at.unwrap_or(node.updated_at)))
            .large_file(node.size >= i64::from(u32::MAX));
        if node.is_dir {
            zip.add_directory(path, options)?;
            continue;
        }
        zip.start_file(path, options)?;
        let content = handle.block_on(files.open(node, None)).map_err(io::Error::other)?;
        let mut reader = SyncIoBridge::new_with_handle(StreamReader::new(content.body), handle.clone());
        io::copy(&mut reader, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

/// A time as zip keeps it (local time of 1980–2107, two seconds apart).
fn zip_time(time: DateTime<Utc>) -> zip::DateTime {
    let year = u16::try_from(time.year()).unwrap_or(1980);
    let at = |value: u32| u8::try_from(value).unwrap_or(0);
    zip::DateTime::from_date_and_time(
        year,
        at(time.month()),
        at(time.day()),
        at(time.hour()),
        at(time.minute()),
        at(time.second()),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u128, parent: Option<u128>, name: &str, is_dir: bool) -> node::Model {
        let now = Utc::now();
        node::Model {
            id: Uuid::from_u128(id),
            parent_id: parent.map(Uuid::from_u128),
            owner_id: Uuid::nil(),
            name: name.to_owned(),
            is_dir,
            size: 0,
            mime_type: None,
            content_key: None,
            sha256: None,
            created_at: now,
            updated_at: now,
            modified_at: None,
            modified_by: None,
            trashed_at: None,
            trashed_from: None,
        }
    }

    #[test]
    fn entries_are_paths_below_the_folder_without_the_trash() {
        let root = node(1, None, "", true);
        let subtree = vec![
            root.clone(),
            node(2, Some(1), "Docs", true),
            node(3, Some(2), "a.txt", false),
            node(4, Some(1), TRASH_FOLDER, true),
            node::Model { trashed_at: Some(Utc::now()), ..node(5, Some(4), "old.txt", false) },
            node(6, Some(1), "b.txt", false),
        ];
        let paths: Vec<String> = archive_entries(&root, subtree.clone()).unwrap().into_iter().map(|e| e.0).collect();
        assert_eq!(paths, ["Docs/", "Docs/a.txt", "b.txt"]);
        let docs = subtree[1].clone();
        let paths: Vec<String> =
            archive_entries(&docs, subtree[1..3].to_vec()).unwrap().into_iter().map(|e| e.0).collect();
        assert_eq!(paths, ["a.txt"]);
    }

    #[test]
    fn times_fit_zip() {
        let time = DateTime::parse_from_rfc3339("2024-11-07T13:20:01Z").unwrap().with_timezone(&Utc);
        let zipped = zip_time(time);
        assert_eq!((zipped.year(), zipped.month(), zipped.day(), zipped.hour()), (2024, 11, 7, 13));
        let ancient = DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(zip_time(ancient), zip::DateTime::default());
    }
}
