//! Files and folders shared with other users and with groups, for reading
//! or for changes.
//!
//! A share opens the node and everything below it. Who received one finds
//! it under "Shared with me" and reaches it by id: reading (contents,
//! downloads, archives, thumbnails) with any share, changes (uploads,
//! folders, renames and moves inside it, deleting into the owner's trash)
//! with one for changes. The shared node itself can't be renamed, moved or
//! deleted but by its owner, and the owner's folders above it stay unseen.
//! What is written counts against the owner's quota. Versions, the trash,
//! public links and apps stay the owner's.
//!
//! The owner shares with a username or with a group they are in. A
//! username nobody has is kept and listed like any other but opens nothing,
//! so sharing doesn't tell which names exist; for the same reason the owner
//! sees people by username.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::{file_share, group, group_member, node, user};
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_shares::Person;
use crate::appdata::APPDATA_FOLDER;
use crate::service::{Files, Node};

/// A group as shares name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupRef {
    pub id: Uuid,
    pub name: String,
}

/// A share as its owner sees it: with whom, and what they may do.
#[derive(Debug, Clone, Serialize)]
pub struct Access {
    pub id: Uuid,
    pub node_id: Uuid,
    /// A person (by username), or…
    pub user: Option<Person>,
    /// …a group.
    pub group: Option<GroupRef>,
    pub write: bool,
    pub created_at: DateTime<Utc>,
}

/// Whom the owner shares with: a username or one of their groups.
#[derive(Debug, Clone, Deserialize)]
pub struct NewAccess {
    pub username: Option<String>,
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub write: bool,
}

/// A share as its recipient sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Received {
    pub id: Uuid,
    pub node: Node,
    pub owner: Person,
    pub write: bool,
    /// The group it came through; `None`: shared with the user.
    pub group: Option<GroupRef>,
}

/// How a user reaches a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    Own,
    /// Through a share; `top` is the highest shared node above (or the node).
    Shared {
        write: bool,
        top: Uuid,
    },
}

impl Files {
    /// Who the owner shared a node with.
    pub async fn node_access(&self, owner: &User, node_id: Uuid) -> Result<Vec<Access>> {
        let node = self.accessible(owner, node_id).await?;
        let rows = file_share::Entity::find().filter(file_share::Column::NodeId.eq(node.id)).all(&self.db).await?;
        let mut access = self.to_access(rows).await?;
        access.sort_by_key(|access| access.created_at);
        Ok(access)
    }

    /// Shares a node with a user or a group; sharing it with them again
    /// changes what they may do.
    pub async fn grant_access(&self, owner: &User, node_id: Uuid, new: NewAccess) -> Result<Access> {
        let node = self.accessible(owner, node_id).await?;
        self.refuse_unshareable(&node).await?;
        let mut row = file_share::ActiveModel {
            id: Set(Uuid::new_v4()),
            node_id: Set(node.id),
            owner_id: Set(owner.id),
            user_id: Set(None),
            group_id: Set(None),
            username: Set(None),
            write: Set(new.write),
            created_at: Set(Utc::now()),
        };
        let existing = file_share::Entity::find().filter(file_share::Column::NodeId.eq(node.id));
        let (existing, with) = match (new.username.as_deref().map(str::trim), new.group_id) {
            (Some(username), None) if !username.is_empty() => {
                let username = username.to_lowercase();
                if username == owner.username {
                    return Err(Error::bad_request("SHARE_WITH_SELF", "This is your own account"));
                }
                let other = user::Entity::find().filter(user::Column::Username.eq(&username)).one(&self.db).await?;
                match other {
                    Some(other) => {
                        row.user_id = Set(Some(other.id));
                        (existing.filter(file_share::Column::UserId.eq(other.id)), username)
                    }
                    None => {
                        row.username = Set(Some(username.clone()));
                        (existing.filter(file_share::Column::Username.eq(&username)), username)
                    }
                }
            }
            (None, Some(group_id)) => {
                if !self.group_ids(owner.id).await?.contains(&group_id) {
                    return Err(Error::not_found("GROUP_NOT_FOUND", "You are not in this group"));
                }
                row.group_id = Set(Some(group_id));
                let name = group::Entity::find_by_id(group_id).one(&self.db).await?.map(|g| g.name).unwrap_or_default();
                (existing.filter(file_share::Column::GroupId.eq(group_id)), format!("group {name}"))
            }
            _ => return Err(Error::bad_request("INVALID_SHARE", "Give either a username or a group")),
        };
        let row = match existing.one(&self.db).await? {
            Some(found) => {
                let mut found: file_share::ActiveModel = found.into();
                found.write = Set(new.write);
                found.update(&self.db).await?
            }
            None => row.insert(&self.db).await?,
        };
        audit!("access.grant", name = node.name, with, write = new.write, "shared");
        Ok(self.to_access(vec![row]).await?.remove(0))
    }

    /// Lets the people of a share change things, or only read them.
    pub async fn change_access(&self, owner: &User, id: Uuid, write: bool) -> Result<Access> {
        let row = file_share::Entity::find_by_id(id)
            .filter(file_share::Column::OwnerId.eq(owner.id))
            .one(&self.db)
            .await?
            .ok_or_else(share_not_found)?;
        let mut row: file_share::ActiveModel = row.into();
        row.write = Set(write);
        let row = row.update(&self.db).await?;
        Ok(self.to_access(vec![row]).await?.remove(0))
    }

    /// Ends a share: the owner takes it back, or its recipient gives it up
    /// (one shared with them, not with a group).
    pub async fn remove_access(&self, user: &User, id: Uuid) -> Result<()> {
        let mine =
            Condition::any().add(file_share::Column::OwnerId.eq(user.id)).add(file_share::Column::UserId.eq(user.id));
        let row = file_share::Entity::find_by_id(id).filter(mine).one(&self.db).await?.ok_or_else(share_not_found)?;
        file_share::Entity::delete_by_id(row.id).exec(&self.db).await?;
        let by = if row.owner_id == user.id { "owner" } else { "recipient" };
        audit!("access.revoke", by, "no longer shared");
        Ok(())
    }

    /// What others shared with the user, directly or through groups.
    pub async fn shared_with_me(&self, user: &User) -> Result<Vec<Received>> {
        let groups = self.group_ids(user.id).await?;
        let rows = file_share::Entity::find().filter(recipient(user.id, &groups)).all(&self.db).await?;
        let nodes: HashMap<Uuid, node::Model> = node::Entity::find()
            .filter(node::Column::Id.is_in(rows.iter().map(|row| row.node_id)))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|node| (node.id, node))
            .collect();
        let owners: HashMap<Uuid, user::Model> = user::Entity::find()
            .filter(user::Column::Id.is_in(rows.iter().map(|row| row.owner_id)))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|owner| (owner.id, owner))
            .collect();
        let group_names = self.group_names(rows.iter().filter_map(|row| row.group_id)).await?;

        let mut received: Vec<Received> = Vec::new();
        for row in rows {
            let (Some(node), Some(owner)) = (nodes.get(&row.node_id), owners.get(&row.owner_id)) else { continue };
            if node.trashed_at.is_some() || owner.disabled || owner.id == user.id {
                continue;
            }
            // Shared both with the user and a group: once, with the most it allows.
            if let Some(same) = received.iter_mut().find(|item| item.node.id == node.id) {
                same.write |= row.write;
                if row.user_id.is_some() {
                    same.id = row.id;
                    same.group = None;
                }
                continue;
            }
            let group =
                row.group_id.and_then(|id| group_names.get(&id).map(|name| GroupRef { id, name: name.clone() }));
            received.push(Received {
                id: row.id,
                node: node.clone().into(),
                owner: Person {
                    id: owner.id,
                    username: owner.username.clone(),
                    display_name: owner.display_name.clone(),
                },
                write: row.write,
                group,
            });
        }
        received.sort_by_cached_key(|item| item.node.name.to_lowercase());
        Ok(received)
    }

    /// How the user reaches a node: their own, or through a share. Neither
    /// (or in the trash, or the owner disabled) is `FILE_NOT_FOUND`.
    pub(crate) async fn reach(&self, user: &User, id: Uuid) -> Result<(node::Model, Reach)> {
        let node = self.row(id).await?;
        if node.owner_id == user.id {
            return Ok((node, Reach::Own));
        }
        if node.trashed_at.is_some() {
            return Err(not_found());
        }
        let mut chain = self.ancestor_rows(&node).await?;
        chain.push(node.clone());
        let ids: Vec<Uuid> = chain.iter().map(|n| n.id).collect();
        let groups = self.group_ids(user.id).await?;
        let shares = file_share::Entity::find()
            .filter(file_share::Column::NodeId.is_in(ids.clone()))
            .filter(recipient(user.id, &groups))
            .all(&self.db)
            .await?;
        let Some(top) = ids.iter().copied().find(|id| shares.iter().any(|share| share.node_id == *id)) else {
            return Err(not_found());
        };
        let owner = user::Entity::find_by_id(node.owner_id).one(&self.db).await?;
        if owner.is_none_or(|owner| owner.disabled) {
            return Err(not_found());
        }
        let write = shares.iter().any(|share| share.write);
        Ok((node, Reach::Shared { write, top }))
    }

    /// A node the user may read: their own or shared with them.
    pub(crate) async fn readable(&self, user: &User, id: Uuid) -> Result<node::Model> {
        Ok(self.reach(user, id).await?.0)
    }

    /// A node the user may change: their own or shared with them for changes.
    pub(crate) async fn writable(&self, user: &User, id: Uuid) -> Result<(node::Model, Reach)> {
        let (node, reach) = self.reach(user, id).await?;
        if let Reach::Shared { write: false, .. } = reach {
            return Err(Error::forbidden("This is shared with you for reading only").with_code("READ_ONLY_SHARE"));
        }
        Ok((node, reach))
    }

    /// Fails for the node a share opens: only its owner renames, moves or deletes it.
    pub(crate) fn refuse_shared_top(node: &node::Model, reach: Reach) -> Result<()> {
        match reach {
            Reach::Shared { top, .. } if top == node.id => {
                Err(Error::forbidden("Only its owner can do this with a shared folder").with_code("SHARED_ROOT"))
            }
            _ => Ok(()),
        }
    }

    /// Whose quota pays for what `actor` writes into `owner_id`'s files.
    pub(crate) async fn payer(&self, actor: &User, owner_id: Uuid) -> Result<User> {
        if actor.id == owner_id {
            return Ok(actor.clone());
        }
        user::Entity::find_by_id(owner_id).one(&self.db).await?.map(User::from).ok_or_else(not_found)
    }

    /// Ids of the groups the user is in.
    async fn group_ids(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = group_member::Entity::find().filter(group_member::Column::UserId.eq(user_id)).all(&self.db).await?;
        Ok(rows.into_iter().map(|row| row.group_id).collect())
    }

    async fn group_names(&self, ids: impl Iterator<Item = Uuid>) -> Result<HashMap<Uuid, String>> {
        let rows = group::Entity::find().filter(group::Column::Id.is_in(ids.collect::<Vec<_>>())).all(&self.db).await?;
        Ok(rows.into_iter().map(|row| (row.id, row.name)).collect())
    }

    /// The root, the trash and AppData (apps' own data) are not shared.
    async fn refuse_unshareable(&self, node: &node::Model) -> Result<()> {
        if node.parent_id.is_none() {
            return Err(Error::bad_request("ROOT_FOLDER", "The root folder cannot be shared"));
        }
        if self.in_trash(node).await? {
            return Err(Error::bad_request("IN_TRASH", "Restore it to share it"));
        }
        let chain = self.ancestor_rows(node).await?;
        let below_root = chain.get(1).unwrap_or(node);
        if below_root.is_dir && below_root.name == APPDATA_FOLDER {
            return Err(Error::bad_request("CANNOT_SHARE", "Apps' data is shared from the apps"));
        }
        Ok(())
    }

    async fn to_access(&self, rows: Vec<file_share::Model>) -> Result<Vec<Access>> {
        let users: HashMap<Uuid, user::Model> = user::Entity::find()
            .filter(user::Column::Id.is_in(rows.iter().filter_map(|row| row.user_id)))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|user| (user.id, user))
            .collect();
        let groups = self.group_names(rows.iter().filter_map(|row| row.group_id)).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let user = match (row.user_id.and_then(|id| users.get(&id)), &row.username) {
                    (Some(user), _) => Some(by_username(user.id, &user.username)),
                    // Nobody has the name: an id of its own, as a person would have.
                    (None, Some(username)) => Some(by_username(row.id, username)),
                    (None, None) => None,
                };
                let group = row.group_id.map(|id| GroupRef { id, name: groups.get(&id).cloned().unwrap_or_default() });
                Access { id: row.id, node_id: row.node_id, user, group, write: row.write, created_at: row.created_at }
            })
            .collect())
    }
}

/// Shares that reach `user_id`: with them, or with one of `groups`.
fn recipient(user_id: Uuid, groups: &[Uuid]) -> Condition {
    let mut condition = Condition::any().add(file_share::Column::UserId.eq(user_id));
    if !groups.is_empty() {
        condition = condition.add(file_share::Column::GroupId.is_in(groups.to_vec()));
    }
    condition
}

/// A person as owners see them: by username, so names that don't exist
/// look the same.
fn by_username(id: Uuid, username: &str) -> Person {
    Person { id, username: username.to_owned(), display_name: username.to_owned() }
}

fn not_found() -> Error {
    Error::not_found("FILE_NOT_FOUND", "File or folder not found")
}

fn share_not_found() -> Error {
    Error::not_found("SHARE_NOT_FOUND", "This share does not exist or has ended")
}
