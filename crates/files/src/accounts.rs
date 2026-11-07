//! What becomes of a user's files when the administrator deletes the
//! account. The rows go with the user (`ON DELETE CASCADE`); the content
//! is in the blob store, so its keys are gathered first and the blobs
//! removed after. Nobody opens the files on the way.

use rustcloud_core::Result;
use rustcloud_db::{node, upload_part, upload_session};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use uuid::Uuid;

use crate::service::Files;

/// The blobs of one account: its files, their earlier versions and the
/// parts of its unfinished uploads.
pub struct AccountBlobs(Vec<String>);

impl AccountBlobs {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Files {
    /// The keys of every blob of `owner_id`; call before deleting the account.
    pub async fn account_blobs(&self, owner_id: Uuid) -> Result<AccountBlobs> {
        let nodes: Vec<(Uuid, Option<String>)> = node::Entity::find()
            .select_only()
            .column(node::Column::Id)
            .column(node::Column::ContentKey)
            .filter(node::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .all(&self.db)
            .await?;
        let ids: Vec<Uuid> = nodes.iter().map(|(id, _)| *id).collect();
        let mut keys: Vec<String> = nodes.into_iter().filter_map(|(_, key)| key).collect();
        keys.extend(self.version_keys(&ids).await?);

        let sessions: Vec<Uuid> = upload_session::Entity::find()
            .select_only()
            .column(upload_session::Column::Id)
            .filter(upload_session::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .all(&self.db)
            .await?;
        if !sessions.is_empty() {
            let parts: Vec<String> = upload_part::Entity::find()
                .select_only()
                .column(upload_part::Column::ContentKey)
                .filter(upload_part::Column::SessionId.is_in(sessions))
                .into_tuple()
                .all(&self.db)
                .await?;
            keys.extend(parts);
        }
        Ok(AccountBlobs(keys))
    }

    /// Removes the blobs of an account that is gone.
    pub async fn remove_account_blobs(&self, blobs: AccountBlobs) {
        for key in blobs.0 {
            self.discard_blob(&key).await;
        }
    }
}
