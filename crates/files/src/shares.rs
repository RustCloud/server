//! Public links: read-only access to a file or folder for anyone with the link.

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, ByteRange, Error, Result};
use rustcloud_db::{node, share};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::Serialize;
use uuid::Uuid;

use crate::service::{Download, Files, Node};

/// A public link as seen by its owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Share {
    pub id: Uuid,
    /// The secret part of the link: `/s/<token>`.
    pub token: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// What the link points to.
    pub node: Node,
}

/// What a visitor of a public link sees.
#[derive(Debug, Clone, Serialize)]
pub struct PublicView {
    pub expires_at: Option<DateTime<Utc>>,
    /// The shared file or folder itself.
    pub shared: Node,
    /// The folder being viewed (the shared one or a subfolder of it), or the shared file.
    pub current: Node,
    /// Folders between `shared` and `current`, `shared` first; empty at the top.
    pub path: Vec<Node>,
    /// Contents of `current` when it is a folder.
    pub children: Vec<Node>,
}

impl Files {
    /// Creates a public link to a node. `expires_at` must be in the future.
    pub async fn create_share(&self, user: &User, node_id: Uuid, expires_at: Option<DateTime<Utc>>) -> Result<Share> {
        let node = self.get(user, node_id).await?;
        if node.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be shared"));
        }
        if expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(Error::bad_request("INVALID_EXPIRY", "Expiry time must be in the future"));
        }

        let row = share::ActiveModel {
            id: Set(Uuid::new_v4()),
            token: Set(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 24]>())),
            node_id: Set(node.id),
            owner_id: Set(user.id),
            expires_at: Set(expires_at),
            created_at: Set(Utc::now()),
        }
        .insert(&self.db)
        .await?;
        let expires = expires_at.map(|at| at.to_rfc3339());
        audit!("share.create", name = node.name, expires = expires.as_deref(), "public link created");
        Ok(to_share(row, node))
    }

    /// The user's public links, newest first.
    pub async fn shares(&self, user: &User) -> Result<Vec<Share>> {
        let rows = share::Entity::find()
            .filter(share::Column::OwnerId.eq(user.id))
            .order_by_desc(share::Column::CreatedAt)
            .all(&self.db)
            .await?;
        let nodes: HashMap<Uuid, node::Model> = node::Entity::find()
            .filter(node::Column::Id.is_in(rows.iter().map(|s| s.node_id)))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|n| (n.id, n))
            .collect();
        Ok(rows
            .into_iter()
            .filter_map(|share| {
                let node = nodes.get(&share.node_id)?.clone();
                Some(to_share(share, node.into()))
            })
            .collect())
    }

    pub async fn delete_share(&self, user: &User, share_id: Uuid) -> Result<()> {
        let deleted = share::Entity::delete_many()
            .filter(share::Column::Id.eq(share_id))
            .filter(share::Column::OwnerId.eq(user.id))
            .exec(&self.db)
            .await?;
        if deleted.rows_affected == 0 {
            return Err(Error::not_found("SHARE_NOT_FOUND", "Link not found"));
        }
        audit!("share.delete", "public link deleted");
        Ok(())
    }

    /// Opens a public link. `node_id` selects a node inside a shared folder;
    /// `None` means the shared node itself.
    pub async fn public_view(&self, token: &str, node_id: Option<Uuid>) -> Result<PublicView> {
        let (share, shared) = self.open_share(token).await?;
        let current = self.node_in_share(&shared, node_id).await?;

        let path = if current.id == shared.id {
            Vec::new()
        } else {
            let ancestors = self.ancestor_rows(&current).await?;
            let start = ancestors.iter().position(|a| a.id == shared.id).unwrap_or(0);
            ancestors[start..].iter().cloned().map(Node::from).collect()
        };
        let mut children: Vec<Node> = if current.is_dir {
            self.child_rows(current.id).await?.into_iter().map(Node::from).collect()
        } else {
            Vec::new()
        };
        children.sort_by_cached_key(|n| (!n.is_dir, n.name.to_lowercase()));

        Ok(PublicView { expires_at: share.expires_at, shared: shared.into(), current: current.into(), path, children })
    }

    /// Metadata of a file reachable through a public link.
    pub async fn public_file(&self, token: &str, node_id: Option<Uuid>) -> Result<Node> {
        let (_, shared) = self.open_share(token).await?;
        Ok(self.node_in_share(&shared, node_id).await?.into())
    }

    /// A file or folder reached through a public link.
    pub(crate) async fn public_node(&self, token: &str, node_id: Option<Uuid>) -> Result<node::Model> {
        let (_, shared) = self.open_share(token).await?;
        self.node_in_share(&shared, node_id).await
    }

    /// Reads a file reachable through a public link.
    pub async fn public_download(
        &self,
        token: &str,
        node_id: Option<Uuid>,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        let (_, shared) = self.open_share(token).await?;
        let file = self.node_in_share(&shared, node_id).await?;
        if crate::service::starts_reading(range) {
            audit!("share.download", name = file.name, size = file.size, "downloaded through a public link");
        }
        self.open(file, range).await
    }

    /// Finds a valid (existing, not expired) link and the node it points to.
    async fn open_share(&self, token: &str) -> Result<(share::Model, node::Model)> {
        let gone = || Error::not_found("SHARE_NOT_FOUND", "This link does not exist or has expired");
        let share =
            share::Entity::find().filter(share::Column::Token.eq(token)).one(&self.db).await?.ok_or_else(gone)?;
        if share.expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(gone());
        }
        let node = self.row(share.node_id).await?;
        if node.trashed_at.is_some() {
            return Err(gone());
        }
        // A disabled account shares nothing any more.
        let owner = rustcloud_db::user::Entity::find_by_id(node.owner_id).one(&self.db).await?;
        if owner.is_none_or(|owner| owner.disabled) {
            return Err(gone());
        }
        Ok((share, node))
    }

    /// `node_id` if it lies inside `shared` (or `shared` itself when `None`).
    async fn node_in_share(&self, shared: &node::Model, node_id: Option<Uuid>) -> Result<node::Model> {
        let Some(id) = node_id.filter(|id| *id != shared.id) else {
            return Ok(shared.clone());
        };
        let node = self.row(id).await?;
        if node.trashed_at.is_some() || !self.is_inside(&node, shared.id).await? {
            return Err(Error::not_found("FILE_NOT_FOUND", "File or folder not found"));
        }
        Ok(node)
    }
}

fn to_share(row: share::Model, node: Node) -> Share {
    Share { id: row.id, token: row.token, expires_at: row.expires_at, created_at: row.created_at, node }
}
