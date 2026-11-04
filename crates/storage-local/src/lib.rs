//! [`BlobStore`] that keeps blobs as files in a local directory.
//!
//! Layout inside the root directory:
//!
//! ```text
//! blobs/3f/3f2a…-…   finished blobs, sharded by the first two key characters
//! tmp/               uploads in progress
//! ```
//!
//! A blob is first written to `tmp/` and then renamed into `blobs/`. A rename
//! within one file system is atomic, so a blob is either complete or absent:
//! an interrupted upload never leaves half a file behind.

use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures_util::StreamExt;
use rustcloud_core::{BlobStore, ByteRange, ByteStream, Capacity, Error, Result, StorageInfo};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

/// Bytes read from disk at a time when sending a file. The default of the
/// stream adapter (8 KiB) makes large downloads about twice as slow.
const READ_CHUNK: usize = 64 * 1024;

/// Blob store backed by a directory on the local file system.
pub struct LocalBlobStore {
    root: PathBuf,
    blobs_dir: PathBuf,
    tmp_dir: PathBuf,
}

impl LocalBlobStore {
    /// Opens (and creates, if needed) a blob store in `root`.
    pub async fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        let store = Self { root: root.to_owned(), blobs_dir: root.join("blobs"), tmp_dir: root.join("tmp") };
        fs::create_dir_all(&store.blobs_dir).await?;
        fs::create_dir_all(&store.tmp_dir).await?;
        Ok(store)
    }

    /// Final location of a blob. Fails for keys that are not plain
    /// `[A-Za-z0-9-]` strings, so a key can never point outside the store.
    fn blob_path(&self, key: &str) -> Result<PathBuf> {
        let valid = (2..=64).contains(&key.len()) && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Err(Error::internal(format!("invalid blob key {key:?}")));
        }
        Ok(self.blobs_dir.join(&key[..2]).join(key))
    }

    /// Where a blob is written before it is complete.
    fn tmp_path(&self, key: &str) -> PathBuf {
        self.tmp_dir.join(format!("{key}.part"))
    }

    /// Moves a finished temporary file to its final place.
    async fn publish(&self, tmp: &Path, key: &str) -> Result<()> {
        let path = self.blob_path(key)?;
        if let Some(shard) = path.parent() {
            fs::create_dir_all(shard).await?;
        }
        fs::rename(tmp, &path).await?;
        Ok(())
    }
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn put(&self, key: &str, mut data: ByteStream) -> Result<u64> {
        self.blob_path(key)?; // validate the key before touching the disk
        let tmp = self.tmp_path(key);

        let written = async {
            let mut file = File::create(&tmp).await?;
            let mut written = 0u64;
            while let Some(chunk) = data.next().await {
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                written += chunk.len() as u64;
            }
            file.sync_all().await?;
            Ok::<_, io::Error>(written)
        }
        .await;

        match written {
            Ok(written) => {
                self.publish(&tmp, key).await?;
                Ok(written)
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp).await;
                Err(stream_error(err))
            }
        }
    }

    async fn get(&self, key: &str, range: Option<ByteRange>) -> Result<ByteStream> {
        let mut file = File::open(self.blob_path(key)?).await?;
        match range {
            None => Ok(Box::pin(ReaderStream::with_capacity(file, READ_CHUNK))),
            Some(range) => {
                file.seek(SeekFrom::Start(range.start)).await?;
                Ok(Box::pin(ReaderStream::with_capacity(file.take(range.byte_count()), READ_CHUNK)))
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        match fs::remove_file(self.blob_path(key)?).await {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        }
    }

    async fn open_file(&self, key: &str) -> Result<Option<std::fs::File>> {
        Ok(Some(File::open(self.blob_path(key)?).await?.into_std().await))
    }

    async fn copy(&self, from: &str, to: &str) -> Result<()> {
        let tmp = self.tmp_path(to);
        if let Err(err) = fs::copy(self.blob_path(from)?, &tmp).await {
            let _ = fs::remove_file(&tmp).await;
            return Err(err.into());
        }
        self.publish(&tmp, to).await
    }

    fn describe(&self) -> StorageInfo {
        StorageInfo { kind: "local", location: self.root.display().to_string() }
    }

    /// Size of the file system the store is on.
    async fn capacity(&self) -> Result<Option<Capacity>> {
        let root = self.root.clone();
        let stats = tokio::task::spawn_blocking(move || rustix::fs::statvfs(&root))
            .await
            .map_err(Error::internal)?
            .map_err(io::Error::from)?;
        Ok(Some(Capacity {
            total_bytes: stats.f_blocks.saturating_mul(stats.f_frsize),
            free_bytes: stats.f_bavail.saturating_mul(stats.f_frsize),
        }))
    }

    /// Drops the blob from the page cache (Linux), so a speed test reads the disk.
    async fn forget_cached(&self, key: &str) -> Result<()> {
        let file = std::fs::File::open(self.blob_path(key)?)?;
        tokio::task::spawn_blocking(move || rustix::fs::fadvise(&file, 0, None, rustix::fs::Advice::DontNeed))
            .await
            .map_err(Error::internal)?
            .map_err(io::Error::from)?;
        Ok(())
    }
}

/// Errors coming from the upload stream may carry a meaningful [`Error`]
/// (e.g. "quota exceeded" from the caller's size limit); keep it instead of
/// turning it into an internal error.
fn stream_error(err: io::Error) -> Error {
    match err.get_ref().and_then(|inner| inner.downcast_ref::<Error>()) {
        Some(inner) => inner.clone(),
        None => err.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;
    use rustcloud_core::ErrorKind;

    async fn store() -> (tempfile::TempDir, LocalBlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalBlobStore::open(dir.path()).await.unwrap();
        (dir, store)
    }

    fn chunks(parts: &'static [&'static str]) -> ByteStream {
        Box::pin(stream::iter(parts.iter().map(|p| Ok(Bytes::from_static(p.as_bytes())))))
    }

    async fn read_all(stream: ByteStream) -> String {
        let parts: Vec<Bytes> = stream.map(|chunk| chunk.unwrap()).collect().await;
        String::from_utf8(parts.concat()).unwrap()
    }

    #[tokio::test]
    async fn stores_and_reads_back_a_blob() {
        let (_dir, store) = store().await;
        let size = store.put("abc-1", chunks(&["hello ", "world"])).await.unwrap();
        assert_eq!(size, 11);
        assert_eq!(read_all(store.get("abc-1", None).await.unwrap()).await, "hello world");
    }

    #[tokio::test]
    async fn speed_test_measures_and_cleans_up() {
        let (dir, store) = store().await;
        let result = store.speed_test(3 * 1024 * 1024 + 5).await.unwrap();
        assert_eq!(result.bytes, 3 * 1024 * 1024 + 5);
        assert!(result.write > std::time::Duration::ZERO && result.read > std::time::Duration::ZERO);
        let left = std::fs::read_dir(dir.path().join("blobs"))
            .unwrap()
            .flat_map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap());
        assert_eq!(left.count(), 0, "the test blob is removed");

        let capacity = store.capacity().await.unwrap().expect("a disk has a size");
        assert!(capacity.total_bytes >= capacity.free_bytes && capacity.total_bytes > 0);
        assert_eq!(store.describe().kind, "local");
    }

    #[tokio::test]
    async fn reads_a_byte_range() {
        let (_dir, store) = store().await;
        store.put("abc-1", chunks(&["hello world"])).await.unwrap();
        let range = ByteRange { start: 6, end: 9 };
        assert_eq!(read_all(store.get("abc-1", Some(range)).await.unwrap()).await, "worl");
    }

    #[tokio::test]
    async fn failed_upload_leaves_nothing_behind() {
        let (dir, store) = store().await;
        let failing: ByteStream = Box::pin(stream::iter(vec![
            Ok(Bytes::from_static(b"partial")),
            Err(io::Error::other(Error::new(ErrorKind::PayloadTooLarge, "QUOTA_EXCEEDED", "Quota exceeded"))),
        ]));

        let err = store.put("abc-1", failing).await.unwrap_err();
        assert_eq!(err.code, "QUOTA_EXCEEDED", "the stream's own error is kept");
        assert!(store.get("abc-1", None).await.is_err());
        assert_eq!(std::fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn copies_a_blob() {
        let (_dir, store) = store().await;
        store.put("abc-1", chunks(&["data"])).await.unwrap();
        store.copy("abc-1", "abc-2").await.unwrap();
        store.delete("abc-1").await.unwrap();
        assert_eq!(read_all(store.get("abc-2", None).await.unwrap()).await, "data");
    }

    #[tokio::test]
    async fn deleting_a_missing_blob_is_fine() {
        let (_dir, store) = store().await;
        store.delete("never-stored").await.unwrap();
    }

    #[tokio::test]
    async fn rejects_keys_that_could_escape_the_store() {
        let (_dir, store) = store().await;
        for key in ["../etc/passwd", "a/b", "", "x", "a.b"] {
            assert!(store.put(key, chunks(&["x"])).await.is_err(), "{key:?} must be rejected");
            assert!(store.get(key, None).await.is_err(), "{key:?} must be rejected");
        }
    }
}
