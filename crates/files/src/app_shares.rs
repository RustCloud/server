//! Folders of AppData users share with other users: a calendar shared with
//! the family, read-only or for changes.
//!
//! The owner shares from the panel, never the app on its own. The app then
//! reaches the folder for the other user, by paths relative to it, and only
//! as far as the share allows. Changes count against the owner's quota. The
//! owner revokes a share, or the other user gives it up; deleting the folder
//! or one of the two accounts ends it too. Sharing and revoking are in the
//! activity log.
//!
//! Sharing with a username nobody has answers as if it had worked: the
//! share is kept (in `app_shares_unmatched`) and listed among those given,
//! but opens nothing — so sharing doesn't tell which usernames exist. For
//! the same reason the owner sees the people they shared with by username,
//! not by display name.

use rustcloud_auth::User;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::{app_share, app_share_unmatched, user};
use sea_orm::sea_query::ExprTrait;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::appdata::segments;
use crate::service::{Files, Node};

/// Longest label of a shared folder.
const MAX_LABEL: usize = 100;

/// A user as other users see them in shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// A folder of someone's AppData shared with someone else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppShare {
    pub id: Uuid,
    pub app_id: String,
    /// The folder, relative to the owner's AppData of the app.
    pub path: String,
    /// What the app called the folder when it was shared.
    pub label: String,
    pub owner: Person,
    pub user: Person,
    pub write: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// What an owner shares, and with whom.
#[derive(Debug, Clone, Deserialize)]
pub struct NewAppShare {
    pub app_id: String,
    pub path: String,
    #[serde(default)]
    pub label: String,
    pub username: String,
    #[serde(default)]
    pub write: bool,
}

/// A user's shares: given to others and received from them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AppShares {
    pub given: Vec<AppShare>,
    pub received: Vec<AppShare>,
}

impl Files {
    /// Shares a folder of the owner's AppData with another user. Sharing the
    /// same folder with the same user again changes the access.
    pub async fn share_appdata(&self, owner: &User, share: NewAppShare) -> Result<AppShare> {
        let path = segments(&share.path)?.join("/");
        if path.is_empty() {
            return Err(Error::bad_request("INVALID_NAME", "Only a folder inside the app's data can be shared"));
        }
        if self.appdata_folder(owner, &share.app_id, &path).await?.is_none() {
            return Err(Error::not_found("FILE_NOT_FOUND", "No such folder in the app's data"));
        }
        let label = match share.label.trim() {
            "" => path.rsplit('/').next().unwrap_or_default().to_owned(),
            label if label.chars().count() > MAX_LABEL => {
                return Err(Error::bad_request("INVALID_NAME", "The label is longer than 100 characters"))
            }
            label => label.to_owned(),
        };
        let username = share.username.trim().to_lowercase();
        if username == owner.username {
            return Err(Error::bad_request("SHARE_WITH_SELF", "This is your own account"));
        }
        let other = user::Entity::find().filter(user::Column::Username.eq(&username)).one(&self.db).await?;
        audit!(
            "app_share.create",
            app = share.app_id,
            name = label,
            with = username,
            write = share.write,
            "app data shared"
        );
        let owner_person = person(owner.id, &owner.username, &owner.display_name);
        let Some(other) = other else {
            let row = self.share_with_nobody(owner, share, path, label, username).await?;
            return Ok(unmatched_share(row, owner_person));
        };

        let existing = app_share::Entity::find()
            .filter(app_share::Column::OwnerId.eq(owner.id))
            .filter(app_share::Column::AppId.eq(&share.app_id))
            .filter(app_share::Column::Path.eq(&path))
            .filter(app_share::Column::UserId.eq(other.id))
            .one(&self.db)
            .await?;
        let row = match existing {
            Some(row) => {
                let mut row: app_share::ActiveModel = row.into();
                row.label = Set(label);
                row.write = Set(share.write);
                row.update(&self.db).await?
            }
            None => {
                app_share::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    app_id: Set(share.app_id),
                    owner_id: Set(owner.id),
                    path: Set(path),
                    label: Set(label),
                    user_id: Set(other.id),
                    write: Set(share.write),
                    created_at: Set(chrono::Utc::now()),
                }
                .insert(&self.db)
                .await?
            }
        };
        Ok(to_share(row, owner_person, person(other.id, &other.username, &other.username)))
    }

    /// Keeps a share with a username nobody has, as a real one would be kept.
    async fn share_with_nobody(
        &self,
        owner: &User,
        share: NewAppShare,
        path: String,
        label: String,
        username: String,
    ) -> Result<app_share_unmatched::Model> {
        let existing = app_share_unmatched::Entity::find()
            .filter(app_share_unmatched::Column::OwnerId.eq(owner.id))
            .filter(app_share_unmatched::Column::AppId.eq(&share.app_id))
            .filter(app_share_unmatched::Column::Path.eq(&path))
            .filter(app_share_unmatched::Column::Username.eq(&username))
            .one(&self.db)
            .await?;
        Ok(match existing {
            Some(row) => {
                let mut row: app_share_unmatched::ActiveModel = row.into();
                row.label = Set(label);
                row.write = Set(share.write);
                row.update(&self.db).await?
            }
            None => {
                app_share_unmatched::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    app_id: Set(share.app_id),
                    owner_id: Set(owner.id),
                    path: Set(path),
                    label: Set(label),
                    username: Set(username),
                    person_id: Set(Uuid::new_v4()),
                    write: Set(share.write),
                    created_at: Set(chrono::Utc::now()),
                }
                .insert(&self.db)
                .await?
            }
        })
    }

    /// The user's shares in all apps, newest first. Shares from disabled
    /// users are left out: nobody reaches their data.
    pub async fn appdata_shares(&self, user: &User) -> Result<AppShares> {
        let rows = app_share::Entity::find()
            .filter(app_share::Column::OwnerId.eq(user.id).or(app_share::Column::UserId.eq(user.id)))
            .order_by_desc(app_share::Column::CreatedAt)
            .all(&self.db)
            .await?;
        let people = self.people(rows.iter().flat_map(|row| [row.owner_id, row.user_id])).await?;
        let mut shares = AppShares::default();
        for row in rows {
            let (Some(owner), Some(other)) = (people.get(&row.owner_id), people.get(&row.user_id)) else {
                continue;
            };
            let owner_person = person(owner.id, &owner.username, &owner.display_name);
            if row.owner_id == user.id {
                shares.given.push(to_share(row, owner_person, person(other.id, &other.username, &other.username)));
            } else if !owner.disabled {
                shares.received.push(to_share(
                    row,
                    owner_person,
                    person(other.id, &other.username, &other.display_name),
                ));
            }
        }
        let unmatched = app_share_unmatched::Entity::find()
            .filter(app_share_unmatched::Column::OwnerId.eq(user.id))
            .all(&self.db)
            .await?;
        let me = person(user.id, &user.username, &user.display_name);
        shares.given.extend(unmatched.into_iter().map(|row| unmatched_share(row, me.clone())));
        shares.given.sort_by_key(|share| std::cmp::Reverse(share.created_at));
        Ok(shares)
    }

    /// What other users shared with the user in one app.
    pub async fn received_appdata_shares(&self, user: &User, app_id: &str) -> Result<Vec<AppShare>> {
        let mut received = self.appdata_shares(user).await?.received;
        received.retain(|share| share.app_id == app_id);
        Ok(received)
    }

    /// Ends a share: the owner revokes it, or the other user gives it up.
    pub async fn revoke_appdata_share(&self, user: &User, share_id: Uuid) -> Result<()> {
        let unmatched = app_share_unmatched::Entity::find_by_id(share_id)
            .filter(app_share_unmatched::Column::OwnerId.eq(user.id))
            .one(&self.db)
            .await?;
        if let Some(row) = unmatched {
            app_share_unmatched::Entity::delete_by_id(row.id).exec(&self.db).await?;
            audit!("app_share.revoke", app = row.app_id, name = row.label, by = "owner", "app data no longer shared");
            return Ok(());
        }
        let row = app_share::Entity::find_by_id(share_id)
            .filter(app_share::Column::OwnerId.eq(user.id).or(app_share::Column::UserId.eq(user.id)))
            .one(&self.db)
            .await?
            .ok_or_else(share_not_found)?;
        app_share::Entity::delete_by_id(row.id).exec(&self.db).await?;
        let by = if row.owner_id == user.id { "owner" } else { "recipient" };
        audit!("app_share.revoke", app = row.app_id, name = row.label, by, "app data no longer shared");
        Ok(())
    }

    /// Reads a file of a folder shared with the user, if it is at most `limit` bytes.
    pub async fn shared_read(
        &self,
        user: &User,
        app_id: &str,
        share_id: Uuid,
        path: &str,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let (owner, path) = self.through_share(user, app_id, share_id, path, false).await?;
        self.appdata_read(&owner, app_id, &path, limit).await
    }

    /// Writes a file of a folder shared with the user for changes.
    pub async fn shared_write(
        &self,
        user: &User,
        app_id: &str,
        share_id: Uuid,
        path: &str,
        data: Vec<u8>,
    ) -> Result<Node> {
        let (owner, path) = self.through_share(user, app_id, share_id, path, true).await?;
        self.appdata_write(&owner, app_id, &path, data).await
    }

    /// Lists a folder shared with the user (`""`: the shared folder itself).
    pub async fn shared_list(&self, user: &User, app_id: &str, share_id: Uuid, dir: &str) -> Result<Vec<Node>> {
        let row = self.received_row(user, app_id, share_id).await?;
        let owner = self.share_owner(&row).await?;
        let dir = format!("{}/{}", row.path, segments(dir)?.join("/"));
        self.appdata_list(&owner, app_id, &dir).await
    }

    /// Deletes a file or folder inside a folder shared with the user for changes.
    pub async fn shared_delete(&self, user: &User, app_id: &str, share_id: Uuid, path: &str) -> Result<()> {
        let (owner, path) = self.through_share(user, app_id, share_id, path, true).await?;
        self.appdata_delete(&owner, app_id, &path).await
    }

    /// Forgets the shares of folders that `path` (in the owner's AppData of
    /// the app) was or contained; it has just been deleted.
    pub(crate) async fn forget_appdata_shares(&self, owner: &User, app_id: &str, path: &str) -> Result<()> {
        let rows = app_share::Entity::find()
            .filter(app_share::Column::OwnerId.eq(owner.id))
            .filter(app_share::Column::AppId.eq(app_id))
            .all(&self.db)
            .await?;
        let inside = |shared: &str| shared == path || shared.starts_with(&format!("{path}/"));
        let gone: Vec<Uuid> = rows.iter().filter(|row| inside(&row.path)).map(|row| row.id).collect();
        if !gone.is_empty() {
            app_share::Entity::delete_many().filter(app_share::Column::Id.is_in(gone)).exec(&self.db).await?;
        }
        let rows = app_share_unmatched::Entity::find()
            .filter(app_share_unmatched::Column::OwnerId.eq(owner.id))
            .filter(app_share_unmatched::Column::AppId.eq(app_id))
            .all(&self.db)
            .await?;
        let gone: Vec<Uuid> = rows.iter().filter(|row| inside(&row.path)).map(|row| row.id).collect();
        if !gone.is_empty() {
            app_share_unmatched::Entity::delete_many()
                .filter(app_share_unmatched::Column::Id.is_in(gone))
                .exec(&self.db)
                .await?;
        }
        Ok(())
    }

    /// The owner, and the full path in the owner's AppData of a non-empty
    /// `path` inside a share the user received. A change needs a share for changes.
    async fn through_share(
        &self,
        user: &User,
        app_id: &str,
        share_id: Uuid,
        path: &str,
        write: bool,
    ) -> Result<(User, String)> {
        let row = self.received_row(user, app_id, share_id).await?;
        if write && !row.write {
            return Err(Error::forbidden("This folder is shared read-only").with_code("READ_ONLY"));
        }
        let names = segments(path)?;
        if names.is_empty() {
            return Err(Error::bad_request("INVALID_NAME", "The path is empty"));
        }
        let owner = self.share_owner(&row).await?;
        Ok((owner, format!("{}/{}", row.path, names.join("/"))))
    }

    async fn received_row(&self, user: &User, app_id: &str, share_id: Uuid) -> Result<app_share::Model> {
        app_share::Entity::find_by_id(share_id)
            .filter(app_share::Column::UserId.eq(user.id))
            .filter(app_share::Column::AppId.eq(app_id))
            .one(&self.db)
            .await?
            .ok_or_else(share_not_found)
    }

    /// The owner of a share, if the account is still active.
    async fn share_owner(&self, row: &app_share::Model) -> Result<User> {
        let owner = user::Entity::find_by_id(row.owner_id).one(&self.db).await?;
        owner.filter(|owner| !owner.disabled).map(User::from).ok_or_else(share_not_found)
    }

    async fn people(&self, ids: impl Iterator<Item = Uuid>) -> Result<std::collections::HashMap<Uuid, user::Model>> {
        let ids: Vec<Uuid> = ids.collect();
        let rows = user::Entity::find().filter(user::Column::Id.is_in(ids)).all(&self.db).await?;
        Ok(rows.into_iter().map(|row| (row.id, row)).collect())
    }
}

fn person(id: Uuid, username: &str, display_name: &str) -> Person {
    Person { id, username: username.to_owned(), display_name: display_name.to_owned() }
}

/// A share with nobody, as the owner sees it: like any other.
fn unmatched_share(row: app_share_unmatched::Model, owner: Person) -> AppShare {
    AppShare {
        id: row.id,
        app_id: row.app_id,
        path: row.path,
        label: row.label,
        owner,
        user: person(row.person_id, &row.username, &row.username),
        write: row.write,
        created_at: row.created_at,
    }
}

fn to_share(row: app_share::Model, owner: Person, user: Person) -> AppShare {
    AppShare {
        id: row.id,
        app_id: row.app_id,
        path: row.path,
        label: row.label,
        owner,
        user,
        write: row.write,
        created_at: row.created_at,
    }
}

fn share_not_found() -> Error {
    Error::not_found("SHARE_NOT_FOUND", "This share does not exist or has ended")
}
