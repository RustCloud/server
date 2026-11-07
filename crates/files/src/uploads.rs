//! Uploads in parts: a large file comes in pieces, one after another, and a
//! piece lost with the connection is sent again from where the last one ended.
//!
//! 1. The client opens a session with the name, folder and size of the file.
//!    The size counts against the quota at once; the name and folder are
//!    checked now and again at the end.
//! 2. It sends the parts strictly in order: each starts at `received`, the
//!    bytes the server has. A part cut off halfway is dropped whole.
//! 3. It finishes with the SHA-256 of the whole file: the parts become the
//!    file, as an upload in one piece would.
//!
//! Each part is a blob of its own until the end, when they are read in
//! order into the file's blob (any storage can do that, S3 too). A session
//! with no new part for [`ABANDONED_AFTER`] is removed with its parts.

use std::io;

use chrono::{DateTime, Utc};
use futures_util::{StreamExt, TryStreamExt};
use rustcloud_auth::User;
use rustcloud_core::{audit, validate, ByteStream, Error, ErrorKind, Result};
use rustcloud_db::{upload_part, upload_session};
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, ExprTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::FileEventKind;
use crate::service::{check_mtime, Files, Node, Writer};

/// Most sessions a user may have open at once.
pub const MAX_UPLOADS: u64 = 20;
/// Most parts in one session: parts of a large file can't be tiny.
pub const MAX_PARTS: i32 = 10_000;
/// A session without a new part for this long is abandoned.
pub const ABANDONED_AFTER: chrono::Duration = chrono::Duration::hours(24);
/// A finishing that took longer than this has failed (the server stopped
/// halfway): the session takes parts and can be finished again.
const FINISHING_STALE: chrono::Duration = chrono::Duration::hours(1);

/// What the client says when it opens a session.
#[derive(Debug, Clone, Deserialize)]
pub struct NewUpload {
    pub name: String,
    /// Size of the whole file in bytes.
    pub size: i64,
    /// Replace a file of that name in the folder.
    #[serde(default)]
    pub overwrite: bool,
    /// The file's mtime (see [`Node::modified_at`]).
    #[serde(skip)]
    pub mtime: Option<DateTime<Utc>>,
}

/// A session as the client sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Upload {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub size: i64,
    /// Bytes the server has: where the next part starts.
    pub received: i64,
    pub overwrite: bool,
    pub mtime: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// When the session goes if no part comes before.
    pub expires_at: DateTime<Utc>,
}

impl From<upload_session::Model> for Upload {
    fn from(row: upload_session::Model) -> Self {
        Self {
            id: row.id,
            folder_id: row.folder_id,
            name: row.name,
            size: row.size,
            received: row.received,
            overwrite: row.overwrite,
            mtime: row.mtime,
            created_at: row.created_at,
            expires_at: row.updated_at + ABANDONED_AFTER,
        }
    }
}

impl Files {
    /// Opens a session for a file `upload.name` of `upload.size` bytes in a folder.
    pub async fn start_upload(&self, user: &User, folder_id: Uuid, upload: NewUpload) -> Result<Upload> {
        let name = validate::file_name(&upload.name)?.to_owned();
        let mtime = upload.mtime.map(check_mtime).transpose()?;
        if upload.size < 0 {
            return Err(Error::bad_request("INVALID_SIZE", "The size can't be negative"));
        }
        let folder = self.writable_folder(user, folder_id).await?;
        self.check_name_free(folder.id, &name, upload.overwrite).await?;
        let open =
            upload_session::Entity::find().filter(upload_session::Column::OwnerId.eq(user.id)).count(&self.db).await?;
        if open >= MAX_UPLOADS {
            return Err(Error::new(
                ErrorKind::TooManyRequests,
                "TOO_MANY_UPLOADS",
                format!("At most {MAX_UPLOADS} uploads in parts at once; finish or cancel one"),
            ));
        }
        if self.max_upload_bytes().is_some_and(|max| upload.size as u64 > max) {
            return Err(Error::new(
                ErrorKind::PayloadTooLarge,
                "FILE_TOO_LARGE",
                "The file is larger than the upload limit",
            ));
        }
        if self.remaining_quota(user).await?.is_some_and(|left| upload.size > left) {
            return Err(Error::new(ErrorKind::PayloadTooLarge, "QUOTA_EXCEEDED", "Not enough storage quota"));
        }
        if self.disk_room().await?.is_some_and(|room| upload.size as u64 > room) {
            return Err(Error::new(ErrorKind::PayloadTooLarge, "STORAGE_FULL", "The server's storage is full"));
        }

        let now = Utc::now();
        let session = upload_session::ActiveModel {
            id: Set(Uuid::new_v4()),
            owner_id: Set(user.id),
            folder_id: Set(folder.id),
            name: Set(name.clone()),
            size: Set(upload.size),
            received: Set(0),
            parts: Set(0),
            overwrite: Set(upload.overwrite),
            mtime: Set(mtime),
            created_at: Set(now),
            updated_at: Set(now),
            finishing_at: Set(None),
        }
        .insert(&self.db)
        .await?;
        audit!("file.upload_start", name, size = upload.size, "upload in parts started");
        Ok(session.into())
    }

    /// The user's open sessions, oldest first: what a client restarted
    /// halfway can go on with.
    pub async fn uploads(&self, user: &User) -> Result<Vec<Upload>> {
        let rows = upload_session::Entity::find()
            .filter(upload_session::Column::OwnerId.eq(user.id))
            .order_by_asc(upload_session::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(Upload::from).collect())
    }

    pub async fn upload_info(&self, user: &User, id: Uuid) -> Result<Upload> {
        Ok(self.upload_row(user, id).await?.into())
    }

    /// Takes the part that starts at `offset`, which must be where the last
    /// one ended (`WRONG_OFFSET` otherwise, with the right offset in the
    /// message: the client asks for the session and goes on from there).
    pub async fn upload_part(&self, user: &User, id: Uuid, offset: i64, body: ByteStream) -> Result<Upload> {
        let session = self.upload_row(user, id).await?;
        if finishing(&session) {
            return Err(Error::conflict("UPLOAD_FINISHING", "The upload is being finished"));
        }
        if offset != session.received {
            return Err(wrong_offset(session.received));
        }
        if session.parts >= MAX_PARTS {
            return Err(Error::conflict("TOO_MANY_PARTS", format!("At most {MAX_PARTS} parts; send larger ones")));
        }

        let room = session.size - offset;
        let disk = self.disk_room().await?.map(|room| i64::try_from(room).unwrap_or(i64::MAX));
        let key = Uuid::new_v4().to_string();
        let size = self.blobs.put(&key, limited(body, room, disk)).await?;
        let size = i64::try_from(size).map_err(Error::internal)?;
        if size == 0 {
            self.discard_blob(&key).await;
            return Ok(session.into());
        }
        match self.add_part(&session, offset, size, &key).await {
            Ok(Some(updated)) => Ok(updated.into()),
            // Another part came first.
            Ok(None) => {
                self.discard_blob(&key).await;
                Err(wrong_offset(self.upload_row(user, id).await?.received))
            }
            Err(err) => {
                self.discard_blob(&key).await;
                Err(err)
            }
        }
    }

    /// Makes the received parts the file. `sha256` is the hex SHA-256 of the
    /// whole file; a mismatch ends the session (`CHECKSUM_MISMATCH`). Other
    /// failures (the name taken meanwhile, the folder deleted) leave the
    /// session as it was, to finish again or cancel.
    pub async fn finish_upload(&self, user: &User, id: Uuid, sha256: &str) -> Result<Node> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::bad_request("INVALID_CHECKSUM", "sha256 must be 64 hex digits"));
        }
        let session = self.upload_row(user, id).await?;
        if session.received != session.size {
            return Err(Error::conflict(
                "UPLOAD_INCOMPLETE",
                format!("{} of {} bytes received", session.received, session.size),
            ));
        }
        if !self.claim_finishing(&session).await? {
            return Err(Error::conflict("UPLOAD_FINISHING", "The upload is being finished"));
        }

        match self.put_parts(user, &session, sha256).await {
            Ok((file, replaced)) => {
                self.drop_upload(&session).await?;
                let kind = if replaced { FileEventKind::Changed } else { FileEventKind::Created };
                self.emit_for(kind, file.id, None).await;
                if replaced {
                    audit!("file.upload", name = file.name, size = file.size, replaced = true, "file uploaded");
                } else {
                    audit!("file.upload", name = file.name, size = file.size, "file uploaded");
                }
                Ok(file)
            }
            Err(err) if err.code == "CHECKSUM_MISMATCH" => {
                self.drop_upload(&session).await?;
                Err(err)
            }
            Err(err) => {
                upload_session::Entity::update_many()
                    .col_expr(upload_session::Column::FinishingAt, Expr::value(Option::<DateTime<Utc>>::None))
                    .filter(upload_session::Column::Id.eq(session.id))
                    .exec(&self.db)
                    .await?;
                Err(err)
            }
        }
    }

    /// Cancels a session and removes what it received.
    pub async fn cancel_upload(&self, user: &User, id: Uuid) -> Result<()> {
        let session = self.upload_row(user, id).await?;
        if finishing(&session) {
            return Err(Error::conflict("UPLOAD_FINISHING", "The upload is being finished"));
        }
        self.drop_upload(&session).await
    }

    /// Removes sessions abandoned for [`ABANDONED_AFTER`] with their parts.
    pub async fn purge_uploads(&self) -> Result<usize> {
        let before = Utc::now() - ABANDONED_AFTER;
        let old = upload_session::Entity::find()
            .filter(upload_session::Column::UpdatedAt.lt(before))
            .filter(not_finishing(Utc::now()))
            .all(&self.db)
            .await?;
        for session in &old {
            self.drop_upload(session).await?;
        }
        Ok(old.len())
    }

    /// Bytes the user's open sessions set aside in the quota.
    pub(crate) async fn reserved_bytes(&self, owner_id: Uuid) -> Result<i64> {
        // SUM() of a BIGINT is NUMERIC in PostgreSQL; cast it back.
        let total = Func::cast_as(
            Func::coalesce([Expr::from(Func::sum(Expr::col(upload_session::Column::Size))), Expr::val(0)]),
            "BIGINT",
        );
        let reserved: Option<i64> = upload_session::Entity::find()
            .select_only()
            .column_as(Expr::from(total), "reserved")
            .filter(upload_session::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(reserved.unwrap_or(0))
    }

    async fn upload_row(&self, user: &User, id: Uuid) -> Result<upload_session::Model> {
        upload_session::Entity::find_by_id(id)
            .filter(upload_session::Column::OwnerId.eq(user.id))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("UPLOAD_NOT_FOUND", "No such upload; it may have expired"))
    }

    /// Fails if `name` is taken in the folder by a folder, or by a file
    /// unless `overwrite`.
    async fn check_name_free(&self, folder_id: Uuid, name: &str, overwrite: bool) -> Result<()> {
        match self.child_by_name(folder_id, name).await? {
            Some(existing) if existing.is_dir || !overwrite => {
                Err(Error::conflict("NAME_TAKEN", format!("'{name}' already exists in this folder")))
            }
            _ => Ok(()),
        }
    }

    /// Records a stored part, if the session still expects it at `offset`
    /// and isn't finishing; `None` otherwise.
    async fn add_part(
        &self,
        session: &upload_session::Model,
        offset: i64,
        size: i64,
        key: &str,
    ) -> Result<Option<upload_session::Model>> {
        let now = Utc::now();
        let db = self.db.begin().await?;
        let moved = upload_session::Entity::update_many()
            .col_expr(upload_session::Column::Received, Expr::value(offset + size))
            .col_expr(upload_session::Column::Parts, Expr::col(upload_session::Column::Parts).add(1))
            .col_expr(upload_session::Column::UpdatedAt, Expr::value(now))
            .filter(upload_session::Column::Id.eq(session.id))
            .filter(upload_session::Column::Received.eq(offset))
            .filter(not_finishing(now))
            .exec(&db)
            .await?;
        if moved.rows_affected != 1 {
            return Ok(None);
        }
        upload_part::ActiveModel {
            id: Set(Uuid::new_v4()),
            session_id: Set(session.id),
            start: Set(offset),
            size: Set(size),
            content_key: Set(key.to_owned()),
        }
        .insert(&db)
        .await?;
        db.commit().await?;
        Ok(Some(upload_session::Model {
            received: offset + size,
            parts: session.parts + 1,
            updated_at: now,
            ..session.clone()
        }))
    }

    /// Marks the session as finishing, unless another request does it.
    async fn claim_finishing(&self, session: &upload_session::Model) -> Result<bool> {
        let now = Utc::now();
        let claimed = upload_session::Entity::update_many()
            .col_expr(upload_session::Column::FinishingAt, Expr::value(now))
            .filter(upload_session::Column::Id.eq(session.id))
            .filter(not_finishing(now))
            .exec(&self.db)
            .await?;
        Ok(claimed.rows_affected == 1)
    }

    /// Writes the parts of `session`, in order, as its file.
    async fn put_parts(&self, user: &User, session: &upload_session::Model, sha256: &str) -> Result<(Node, bool)> {
        let folder = self.writable_folder(user, session.folder_id).await?;
        self.check_name_free(folder.id, &session.name, session.overwrite).await?;
        let keys: Vec<String> = upload_part::Entity::find()
            .filter(upload_part::Column::SessionId.eq(session.id))
            .order_by_asc(upload_part::Column::Start)
            .all(&self.db)
            .await?
            .into_iter()
            .map(|part| part.content_key)
            .collect();
        let blobs = self.blobs.clone();
        let body: ByteStream = Box::pin(
            futures_util::stream::iter(keys)
                .then(move |key| {
                    let blobs = blobs.clone();
                    async move { blobs.get(&key, None).await.map_err(io::Error::other) }
                })
                .try_flatten(),
        );
        let writer = Writer { mtime: session.mtime, reserved: session.size, sha256: Some(sha256), ..Writer::user() };
        self.put_file(user, &folder, &session.name, body, session.overwrite, writer).await
    }

    /// Removes a session and the blobs of its parts.
    async fn drop_upload(&self, session: &upload_session::Model) -> Result<()> {
        let parts =
            upload_part::Entity::find().filter(upload_part::Column::SessionId.eq(session.id)).all(&self.db).await?;
        // The parts' rows go with the session (ON DELETE CASCADE).
        upload_session::Entity::delete_by_id(session.id).exec(&self.db).await?;
        for part in parts {
            self.discard_blob(&part.content_key).await;
        }
        Ok(())
    }
}

/// Whether a finishing of the session is under way.
fn finishing(session: &upload_session::Model) -> bool {
    session.finishing_at.is_some_and(|at| Utc::now() - at < FINISHING_STALE)
}

/// Sessions no request is finishing at `now`.
fn not_finishing(now: DateTime<Utc>) -> Condition {
    Condition::any()
        .add(upload_session::Column::FinishingAt.is_null())
        .add(upload_session::Column::FinishingAt.lt(now - FINISHING_STALE))
}

fn wrong_offset(received: i64) -> Error {
    Error::conflict("WRONG_OFFSET", format!("The next part starts at {received}"))
}

/// `body`, failing once it is longer than `room` (`PART_TOO_LARGE`) or than
/// the free space on the disk `disk` (`STORAGE_FULL`).
fn limited(body: ByteStream, room: i64, disk: Option<i64>) -> ByteStream {
    let mut seen = 0i64;
    Box::pin(body.map(move |chunk| {
        let chunk = chunk?;
        seen += chunk.len() as i64;
        if seen > room {
            return Err(io::Error::other(Error::bad_request(
                "PART_TOO_LARGE",
                "The parts are larger than the size given for the file",
            )));
        }
        if disk.is_some_and(|disk| seen > disk) {
            return Err(io::Error::other(Error::new(
                ErrorKind::PayloadTooLarge,
                "STORAGE_FULL",
                "The server's storage is full",
            )));
        }
        Ok(chunk)
    }))
}
