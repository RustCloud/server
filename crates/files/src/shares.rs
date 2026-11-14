//! Public links: access to a file or folder for anyone with the link.
//!
//! A link may have a password. A visitor gives it once
//! ([`Files::unlock_share`]) and gets a key, kept in a cookie for the
//! link's path only; the key is an HMAC of the link's id by its password
//! hash, so a new password (or none) makes old keys useless. Wrong
//! passwords slow the next tries down, per link.
//!
//! A link to a folder may let visitors upload into it ([`Files::public_upload`]):
//! new files only, never over existing ones (a taken name gets a number);
//! they count against the owner's quota.

use std::collections::HashMap;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rustcloud_auth::{hash_password, hmac_sha256, same, verify_password, User};
use rustcloud_core::{audit, validate, ByteRange, ByteStream, Error, ErrorKind, Result};
use rustcloud_db::{node, share};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::FileEventKind;
use crate::service::{Download, Files, Node, Writer};

/// Wrong passwords of a link are counted over this long.
pub(crate) const ATTEMPTS_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Wrong passwords of a link before the next tries have to wait.
const FREE_ATTEMPTS: u32 = 10;
/// Shortest password of a link.
const MIN_PASSWORD: usize = 4;
/// Name of the cookie that holds the key of an unlocked link.
pub const LINK_COOKIE: &str = "rustcloud_link";

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
    /// Whether visitors must give a password.
    pub has_password: bool,
    /// Whether visitors may upload into the folder.
    pub allow_upload: bool,
}

/// A new public link, or changes to one (`None`: as it is).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShareOptions {
    /// `Some(None)`: never expires.
    #[serde(default, with = "double_option")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
    /// `Some("")`: no password.
    pub password: Option<String>,
    pub allow_upload: Option<bool>,
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
    /// Whether the visitor may upload into `current`.
    pub can_upload: bool,
}

impl Files {
    /// Creates a public link to a node of the user's own: what others shared
    /// with them they don't pass on. `expires_at` must be in the future.
    pub async fn create_share(&self, user: &User, node_id: Uuid, options: ShareOptions) -> Result<Share> {
        let node: Node = self.accessible(user, node_id).await?.into();
        if node.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be shared"));
        }
        let mut row = share::ActiveModel {
            id: Set(Uuid::new_v4()),
            token: Set(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 24]>())),
            node_id: Set(node.id),
            owner_id: Set(user.id),
            expires_at: Set(None),
            created_at: Set(Utc::now()),
            password_hash: Set(None),
            allow_upload: Set(false),
        };
        apply_options(&mut row, &node, &options).await?;
        let row = row.insert(&self.db).await?;
        let expires = row.expires_at.map(|at| at.to_rfc3339());
        audit!(
            "share.create",
            name = node.name,
            expires = expires.as_deref(),
            password = row.password_hash.is_some(),
            upload = row.allow_upload,
            "public link created"
        );
        Ok(to_share(row, node))
    }

    /// Changes the expiry, password or uploads of one of the user's links.
    pub async fn update_share(&self, user: &User, share_id: Uuid, options: ShareOptions) -> Result<Share> {
        let row = share::Entity::find_by_id(share_id)
            .filter(share::Column::OwnerId.eq(user.id))
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("SHARE_NOT_FOUND", "Link not found"))?;
        let node: Node = self.row(row.node_id).await?.into();
        let mut row: share::ActiveModel = row.into();
        apply_options(&mut row, &node, &options).await?;
        let row = row.update(&self.db).await?;
        audit!(
            "share.update",
            name = node.name,
            password = row.password_hash.is_some(),
            upload = row.allow_upload,
            "public link changed"
        );
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
    pub async fn public_view(&self, token: &str, key: Option<&str>, node_id: Option<Uuid>) -> Result<PublicView> {
        let (share, shared) = self.open_share(token, key).await?;
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

        let can_upload = share.allow_upload && current.is_dir;
        Ok(PublicView {
            expires_at: share.expires_at,
            shared: shared.into(),
            current: current.into(),
            path,
            children,
            can_upload,
        })
    }

    /// Metadata of a file reachable through a public link.
    pub async fn public_file(&self, token: &str, key: Option<&str>, node_id: Option<Uuid>) -> Result<Node> {
        let (_, shared) = self.open_share(token, key).await?;
        Ok(self.node_in_share(&shared, node_id).await?.into())
    }

    /// A file or folder reached through a public link.
    pub(crate) async fn public_node(
        &self,
        token: &str,
        key: Option<&str>,
        node_id: Option<Uuid>,
    ) -> Result<node::Model> {
        let (_, shared) = self.open_share(token, key).await?;
        self.node_in_share(&shared, node_id).await
    }

    /// Reads a file reachable through a public link.
    pub async fn public_download(
        &self,
        token: &str,
        key: Option<&str>,
        node_id: Option<Uuid>,
        range: Option<ByteRange>,
    ) -> Result<Download> {
        let (_, shared) = self.open_share(token, key).await?;
        let file = self.node_in_share(&shared, node_id).await?;
        if crate::service::starts_reading(range) {
            audit!("share.download", name = file.name, size = file.size, "downloaded through a public link");
        }
        self.open(file, range).await
    }

    /// Checks the password of a link; the key it gives opens the link.
    pub async fn unlock_share(&self, token: &str, password: &str) -> Result<String> {
        let (share, _) = self.find_share(token).await?;
        let Some(hash) = share.password_hash.as_deref() else {
            return Ok(String::new());
        };
        let attempts = format!("link:{}", share.id);
        if let Err(wait) = self.link_attempts.try_start_delayed(&attempts, FREE_ATTEMPTS) {
            let message = format!("Too many wrong passwords, try again in {} s", wait.as_secs().max(1));
            return Err(Error::new(ErrorKind::TooManyRequests, "TOO_MANY_ATTEMPTS", message));
        }
        if !verify_password(password, Some(hash)).await? {
            audit!(warn, "share.wrong_password", "wrong password of a public link");
            return Err(Error::forbidden("Wrong password").with_code("WRONG_LINK_PASSWORD"));
        }
        self.link_attempts.record_success(&attempts);
        Ok(link_key(&share, hash))
    }

    /// Uploads a file through a link that allows it, into the shared folder
    /// or one inside it. A taken name gets a number: nothing is overwritten.
    pub async fn public_upload(
        &self,
        token: &str,
        key: Option<&str>,
        folder_id: Option<Uuid>,
        name: &str,
        body: ByteStream,
    ) -> Result<Node> {
        let (share, shared) = self.open_share(token, key).await?;
        let folder = self.node_in_share(&shared, folder_id).await?;
        if !share.allow_upload || !folder.is_dir {
            return Err(Error::forbidden("This link does not take uploads").with_code("UPLOAD_NOT_ALLOWED"));
        }
        let name = self.free_name(folder.id, validate::file_name(name)?, false).await?;
        let owner = rustcloud_db::user::Entity::find_by_id(share.owner_id)
            .one(&self.db)
            .await?
            .map(User::from)
            .ok_or_else(|| Error::not_found("SHARE_NOT_FOUND", "This link does not exist or has expired"))?;
        let (file, _) = self.put_file(&owner, &folder, &name, body, false, Writer::user()).await?;
        self.emit_for(FileEventKind::Created, file.id, None).await;
        audit!("share.upload", name = file.name, size = file.size, "uploaded through a public link");
        Ok(file)
    }

    /// Finds a valid link and the node it points to; one with a password
    /// needs the key [`Self::unlock_share`] gave (`PASSWORD_REQUIRED`).
    async fn open_share(&self, token: &str, key: Option<&str>) -> Result<(share::Model, node::Model)> {
        let (share, node) = self.find_share(token).await?;
        if let Some(hash) = share.password_hash.as_deref() {
            let expected = link_key(&share, hash);
            if !key.is_some_and(|key| same(key.as_bytes(), expected.as_bytes())) {
                return Err(Error::forbidden("This link needs a password").with_code("PASSWORD_REQUIRED"));
            }
        }
        Ok((share, node))
    }

    /// Finds a valid (existing, not expired) link and the node it points to.
    async fn find_share(&self, token: &str) -> Result<(share::Model, node::Model)> {
        let gone = || Error::not_found("SHARE_NOT_FOUND", "This link does not exist or has expired");
        let share =
            share::Entity::find().filter(share::Column::Token.eq(token)).one(&self.db).await?.ok_or_else(gone)?;
        if share.expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(gone());
        }
        let node = self.row(share.node_id).await?;
        // Only the owner's own links open anything: one made by someone the
        // node was shared with (servers before 2026-11-14 allowed that) would
        // outlive the share.
        if node.trashed_at.is_some() || share.owner_id != node.owner_id {
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
    Share {
        id: row.id,
        token: row.token,
        expires_at: row.expires_at,
        created_at: row.created_at,
        node,
        has_password: row.password_hash.is_some(),
        allow_upload: row.allow_upload,
    }
}

/// Sets what `options` give on a link to `node`.
async fn apply_options(row: &mut share::ActiveModel, node: &Node, options: &ShareOptions) -> Result<()> {
    if let Some(expires_at) = options.expires_at {
        if expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(Error::bad_request("INVALID_EXPIRY", "Expiry time must be in the future"));
        }
        row.expires_at = Set(expires_at);
    }
    match options.password.as_deref() {
        None => {}
        Some("") => row.password_hash = Set(None),
        Some(password) if password.chars().count() < MIN_PASSWORD => {
            return Err(Error::bad_request("WEAK_LINK_PASSWORD", "The password of a link has at least 4 characters"))
        }
        Some(password) => row.password_hash = Set(Some(hash_password(password).await?)),
    }
    if let Some(allow_upload) = options.allow_upload {
        if allow_upload && !node.is_dir {
            return Err(Error::bad_request("NOT_A_FOLDER", "Uploads go into folders only"));
        }
        row.allow_upload = Set(allow_upload);
    }
    Ok(())
}

/// The key that opens a link with a password: changes with the password.
fn link_key(share: &share::Model, password_hash: &str) -> String {
    let mac = hmac_sha256(password_hash.as_bytes(), share.id.as_bytes());
    URL_SAFE_NO_PAD.encode(mac)
}

/// `"expires_at": null` (never) apart from no `expires_at` at all (as it is).
mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}
