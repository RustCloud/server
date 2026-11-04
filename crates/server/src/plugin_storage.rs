//! Each plugin's own storage on the server: models, reference data, caches —
//! one storage per plugin, for all its users, counted against nobody's
//! quota. Rows in `plugin_files`, bytes in the blob store like users' files.
//!
//! The plugin uses it through its `storage` import; files of its manifest's
//! `[[downloads]]` are put here by [`Plugins`](crate::plugins::Plugins). The
//! administrator sees the size and can clear it. There is no size limit
//! per plugin yet; it never takes the disk's last [`DISK_RESERVE`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::Utc;
use futures_util::{stream, TryStreamExt};
use rustcloud_core::{BlobStore, ByteRange, ByteStream, Error, Result};
use rustcloud_db::{plugin_file, DatabaseConnection};
use rustcloud_plugin_api::valid_storage_path;
use rustcloud_wasm_host::{AppStorage, StoredFile};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Free space a plugin's storage always leaves on the disk.
const DISK_RESERVE: u64 = 1 << 30;

/// How much a plugin keeps in its storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
}

pub struct PluginStorage {
    db: DatabaseConnection,
    blobs: Arc<dyn BlobStore>,
}

impl PluginStorage {
    pub fn new(db: DatabaseConnection, blobs: Arc<dyn BlobStore>) -> Self {
        Self { db, blobs }
    }

    /// The usage of every plugin that keeps something, by plugin id.
    pub async fn usage(&self) -> Result<HashMap<String, Usage>> {
        let mut usage: HashMap<String, Usage> = HashMap::new();
        for row in plugin_file::Entity::find().all(&self.db).await? {
            let entry = usage.entry(row.app_id).or_default();
            entry.files += 1;
            entry.bytes += row.size as u64;
        }
        Ok(usage)
    }

    /// Stores `data` as the file `path`, replacing it if there is one.
    /// `sha256`, if given, must match the content, or nothing is stored.
    /// `progress` learns how many bytes have come so far.
    pub async fn save(
        &self,
        app_id: &str,
        path: &str,
        data: ByteStream,
        sha256: Option<&str>,
        progress: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<StoredFile> {
        check_path(path)?;
        let room = self.blobs.capacity().await?.map(|disk| disk.free_bytes.saturating_sub(DISK_RESERVE));
        let hasher = Arc::new(Mutex::new(Sha256::new()));
        let received = Arc::new(AtomicU64::new(0));
        let (counting, hashing) = (received.clone(), hasher.clone());
        let data = data.and_then(move |chunk| {
            hashing.lock().unwrap_or_else(|e| e.into_inner()).update(&chunk);
            let so_far = counting.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
            progress(so_far);
            let full = room.is_some_and(|room| so_far > room);
            futures_util::future::ready(match full {
                true => Err(std::io::Error::other(Error::new(
                    rustcloud_core::ErrorKind::PayloadTooLarge,
                    "STORAGE_FULL",
                    "The server's storage is full",
                ))),
                false => Ok(chunk),
            })
        });
        let key = Uuid::new_v4().to_string();
        let size = self.blobs.put(&key, Box::pin(data)).await?;
        let digest = hasher.lock().unwrap_or_else(|e| e.into_inner()).clone().finalize();
        let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if let Some(expected) = sha256.filter(|expected| !expected.eq_ignore_ascii_case(&digest)) {
            self.blobs.delete(&key).await?;
            return Err(Error::bad_request(
                "CHECKSUM_MISMATCH",
                format!("{path}: the SHA-256 is {digest}, not {expected}"),
            ));
        }

        let old = self.row(app_id, path).await?;
        let row = plugin_file::ActiveModel {
            id: Set(old.as_ref().map_or_else(Uuid::now_v7, |old| old.id)),
            app_id: Set(app_id.to_owned()),
            path: Set(path.to_owned()),
            blob_key: Set(key.clone()),
            size: Set(size as i64),
            sha256: Set(digest),
            updated_at: Set(Utc::now()),
        };
        let saved = match &old {
            Some(_) => plugin_file::Entity::update(row).exec(&self.db).await,
            None => plugin_file::Entity::insert(row).exec_with_returning(&self.db).await,
        };
        let saved = match saved {
            Ok(saved) => saved,
            Err(err) => {
                self.blobs.delete(&key).await?;
                return Err(err.into());
            }
        };
        if let Some(old) = old {
            self.blobs.delete(&old.blob_key).await?;
        }
        Ok(stored(saved))
    }

    /// Deletes everything a plugin keeps.
    pub async fn clear(&self, app_id: &str) -> Result<()> {
        let rows = self.rows(app_id).await?;
        self.remove(rows).await
    }

    async fn row(&self, app_id: &str, path: &str) -> Result<Option<plugin_file::Model>> {
        Ok(plugin_file::Entity::find()
            .filter(plugin_file::Column::AppId.eq(app_id))
            .filter(plugin_file::Column::Path.eq(path))
            .one(&self.db)
            .await?)
    }

    /// The plugin's files, by path.
    async fn rows(&self, app_id: &str) -> Result<Vec<plugin_file::Model>> {
        Ok(plugin_file::Entity::find()
            .filter(plugin_file::Column::AppId.eq(app_id))
            .order_by_asc(plugin_file::Column::Path)
            .all(&self.db)
            .await?)
    }

    /// The plugin's files in the folder `dir` and below (`""`: all).
    async fn rows_in(&self, app_id: &str, dir: &str) -> Result<Vec<plugin_file::Model>> {
        let inside = format!("{dir}/");
        let rows = self.rows(app_id).await?;
        Ok(rows.into_iter().filter(|row| dir.is_empty() || row.path == dir || row.path.starts_with(&inside)).collect())
    }

    async fn remove(&self, rows: Vec<plugin_file::Model>) -> Result<()> {
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        plugin_file::Entity::delete_many().filter(plugin_file::Column::Id.is_in(ids)).exec(&self.db).await?;
        for row in rows {
            self.blobs.delete(&row.blob_key).await?;
        }
        Ok(())
    }
}

impl PluginStorage {
    /// A file of an app's storage as the server sends it: its name, size,
    /// type and SHA-256 as the ETag.
    pub async fn file(&self, app_id: &str, path: &str) -> Result<rustcloud_files::Node> {
        check_path(path)?;
        Ok(node(self.row(app_id, path).await?.ok_or_else(not_found)?))
    }

    /// `range` of a file of an app's storage, or all, for the server to send.
    pub async fn download(
        &self,
        app_id: &str,
        path: &str,
        range: Option<ByteRange>,
    ) -> Result<rustcloud_files::Download> {
        check_path(path)?;
        let row = self.row(app_id, path).await?.ok_or_else(not_found)?;
        let body = self.blobs.get(&row.blob_key, range).await?;
        Ok(rustcloud_files::Download { node: node(row), range, body })
    }
}

/// A file of the storage in the shape of a user's file, for `content::serve`.
fn node(row: plugin_file::Model) -> rustcloud_files::Node {
    let name = row.path.rsplit('/').next().unwrap_or(&row.path).to_owned();
    rustcloud_files::Node {
        id: Uuid::nil(),
        parent_id: None,
        mime_type: Some(rustcloud_files::guess_mime(&name)),
        name,
        is_dir: false,
        size: row.size,
        etag: Some(row.sha256),
        created_at: row.updated_at,
        updated_at: row.updated_at,
        trashed_at: None,
        lock: None,
    }
}

#[async_trait::async_trait]
impl AppStorage for PluginStorage {
    async fn list(&self, app_id: &str, dir: &str) -> Result<Vec<StoredFile>> {
        if !dir.is_empty() {
            check_path(dir)?;
        }
        Ok(self.rows_in(app_id, dir).await?.into_iter().map(stored).collect())
    }

    async fn stat(&self, app_id: &str, path: &str) -> Result<StoredFile> {
        check_path(path)?;
        self.row(app_id, path).await?.map(stored).ok_or_else(not_found)
    }

    async fn read(&self, app_id: &str, path: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        check_path(path)?;
        let row = self.row(app_id, path).await?.ok_or_else(not_found)?;
        let end = (row.size as u64).min(offset.saturating_add(length));
        if offset >= end {
            return Ok(Vec::new());
        }
        let range = ByteRange { start: offset, end: end - 1 };
        let chunks: Vec<Bytes> = self.blobs.get(&row.blob_key, Some(range)).await?.try_collect().await?;
        Ok(chunks.concat())
    }

    async fn write(&self, app_id: &str, path: &str, data: Vec<u8>) -> Result<StoredFile> {
        let data = Box::pin(stream::once(async move { Ok(Bytes::from(data)) }));
        self.save(app_id, path, data, None, |_| ()).await
    }

    async fn delete(&self, app_id: &str, path: &str) -> Result<()> {
        check_path(path)?;
        let rows = self.rows_in(app_id, path).await?;
        if rows.is_empty() {
            return Err(not_found());
        }
        self.remove(rows).await
    }

    async fn open(&self, app_id: &str, path: &str) -> Result<std::fs::File> {
        check_path(path)?;
        let row = self.row(app_id, path).await?.ok_or_else(not_found)?;
        rustcloud_core::open_as_file(self.blobs.as_ref(), &row.blob_key).await
    }
}

fn stored(row: plugin_file::Model) -> StoredFile {
    StoredFile { path: row.path, size: row.size as u64, sha256: row.sha256, updated_at: row.updated_at.to_rfc3339() }
}

fn check_path(path: &str) -> Result<()> {
    if valid_storage_path(path) {
        Ok(())
    } else {
        Err(Error::bad_request("INVALID_NAME", format!("\"{path}\" is not a valid path")))
    }
}

fn not_found() -> Error {
    Error::not_found("FILE_NOT_FOUND", "There is no such file")
}

#[cfg(test)]
mod tests {
    use rustcloud_db::testing::test_db;
    use rustcloud_storage_local::LocalBlobStore;

    use super::*;

    #[tokio::test]
    async fn plugins_keep_files_of_their_own() {
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let blobs = Arc::new(LocalBlobStore::open(dir.path().to_owned()).await.unwrap());
        let storage = PluginStorage::new(db.conn.clone(), blobs);

        let saved = storage.write("faces", "models/small.bin", b"0123456789".to_vec()).await.unwrap();
        assert_eq!((saved.size, saved.sha256.len()), (10, 64));
        storage.write("faces", "models/large.bin", vec![7; 100]).await.unwrap();
        storage.write("faces", "index.json", b"{}".to_vec()).await.unwrap();
        storage.write("other", "models/small.bin", b"x".to_vec()).await.unwrap();

        assert_eq!(storage.read("faces", "models/small.bin", 3, 4).await.unwrap(), b"3456");
        assert_eq!(storage.read("faces", "models/small.bin", 8, 100).await.unwrap(), b"89", "fewer at the end");
        assert!(storage.read("faces", "models/small.bin", 10, 1).await.unwrap().is_empty(), "none past it");
        let models: Vec<String> = storage.list("faces", "models").await.unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(models, ["models/large.bin", "models/small.bin"]);
        assert_eq!(storage.usage().await.unwrap()["faces"], Usage { files: 3, bytes: 112 });

        let replaced = storage.write("faces", "models/small.bin", b"new".to_vec()).await.unwrap();
        assert_ne!(replaced.sha256, saved.sha256);
        assert_eq!(storage.read("faces", "models/small.bin", 0, 100).await.unwrap(), b"new");

        storage.delete("faces", "models").await.unwrap();
        assert_eq!(storage.list("faces", "").await.unwrap().len(), 1, "the folder with what was in it");
        assert_eq!(storage.stat("faces", "models/small.bin").await.unwrap_err().code, "FILE_NOT_FOUND");
        assert_eq!(storage.stat("faces", "../x").await.unwrap_err().code, "INVALID_NAME");
        assert_eq!(storage.read("other", "models/small.bin", 0, 10).await.unwrap(), b"x", "each plugin its own");

        storage.clear("faces").await.unwrap();
        assert!(!storage.usage().await.unwrap().contains_key("faces"));
    }

    #[tokio::test]
    async fn content_that_does_not_match_its_checksum_is_thrown_away() {
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let blobs = Arc::new(LocalBlobStore::open(dir.path().to_owned()).await.unwrap());
        let storage = PluginStorage::new(db.conn.clone(), blobs);

        let data = || -> ByteStream {
            Box::pin(stream::iter([Ok(Bytes::from_static(b"mod")), Ok(Bytes::from_static(b"el"))]))
        };
        let seen = Arc::new(AtomicU64::new(0));
        let progress = seen.clone();
        let saved = storage
            .save("faces", "model.bin", data(), None, move |bytes| progress.store(bytes, Ordering::Relaxed))
            .await
            .unwrap();
        assert_eq!(seen.load(Ordering::Relaxed), 5);

        let err = storage.save("faces", "other.bin", data(), Some(&"0".repeat(64)), |_| ()).await.unwrap_err();
        assert_eq!(err.code, "CHECKSUM_MISMATCH");
        assert!(storage.stat("faces", "other.bin").await.is_err());
        let again = storage.save("faces", "model.bin", data(), Some(&saved.sha256.to_uppercase()), |_| ()).await;
        assert!(again.is_ok(), "the checksum in any case");
        let shards = std::fs::read_dir(dir.path().join("blobs")).unwrap();
        let blobs: usize = shards.map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count()).sum();
        assert_eq!(blobs, 1, "no blobs left behind");
    }
}
