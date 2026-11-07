//! Groups of users. The administrator makes them and puts users in them;
//! users share files with the groups they are in.

use std::collections::HashMap;

use chrono::Utc;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::{group, group_member, user};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, SqlErr};
use serde::Serialize;
use uuid::Uuid;

use crate::service::{Auth, User};

/// Longest name of a group.
const MAX_NAME: usize = 64;

/// A group as the administrator sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<Member>,
}

/// A user in a group.
#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// A group as its members see it: the name only.
#[derive(Debug, Clone, Serialize)]
pub struct GroupName {
    pub id: Uuid,
    pub name: String,
}

impl Auth {
    /// All groups with their members, by name.
    pub async fn groups(&self) -> Result<Vec<Group>> {
        let groups = group::Entity::find().order_by_asc(group::Column::Name).all(&self.db).await?;
        let members = group_member::Entity::find().all(&self.db).await?;
        let users: HashMap<Uuid, user::Model> =
            user::Entity::find().all(&self.db).await?.into_iter().map(|user| (user.id, user)).collect();
        Ok(groups
            .into_iter()
            .map(|group| {
                let mut members: Vec<Member> = members
                    .iter()
                    .filter(|member| member.group_id == group.id)
                    .filter_map(|member| users.get(&member.user_id))
                    .map(|user| Member {
                        id: user.id,
                        username: user.username.clone(),
                        display_name: user.display_name.clone(),
                    })
                    .collect();
                members.sort_by(|a, b| a.username.cmp(&b.username));
                Group { id: group.id, name: group.name, members }
            })
            .collect())
    }

    pub async fn create_group(&self, name: &str) -> Result<Group> {
        let name = group_name(name)?;
        let row = group::ActiveModel { id: Set(Uuid::new_v4()), name: Set(name), created_at: Set(Utc::now()) }
            .insert(&self.db)
            .await
            .map_err(group_exists)?;
        audit!("group.create", name = row.name, "group created");
        Ok(Group { id: row.id, name: row.name, members: Vec::new() })
    }

    pub async fn rename_group(&self, id: Uuid, name: &str) -> Result<()> {
        let row = self.group_row(id).await?;
        let old = row.name.clone();
        let mut row: group::ActiveModel = row.into();
        row.name = Set(group_name(name)?);
        let row = row.update(&self.db).await.map_err(group_exists)?;
        audit!("group.rename", name = old, new_name = row.name, "group renamed");
        Ok(())
    }

    /// Deletes a group; what was shared with it is no longer.
    pub async fn delete_group(&self, id: Uuid) -> Result<()> {
        let row = self.group_row(id).await?;
        group::Entity::delete_by_id(id).exec(&self.db).await?;
        audit!("group.delete", name = row.name, "group deleted");
        Ok(())
    }

    pub async fn add_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()> {
        let group = self.group_row(group_id).await?;
        let user = self.find_row(user_id).await?;
        let exists = group_member::Entity::find_by_id((group_id, user_id)).one(&self.db).await?.is_some();
        if !exists {
            group_member::ActiveModel { group_id: Set(group_id), user_id: Set(user_id) }.insert(&self.db).await?;
            audit!("group.add_member", name = group.name, account = user.username, "added to a group");
        }
        Ok(())
    }

    pub async fn remove_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()> {
        let group = self.group_row(group_id).await?;
        let deleted = group_member::Entity::delete_by_id((group_id, user_id)).exec(&self.db).await?;
        if deleted.rows_affected > 0 {
            let user = self.find_row(user_id).await?;
            audit!("group.remove_member", name = group.name, account = user.username, "removed from a group");
        }
        Ok(())
    }

    /// The groups `user` is in, by name.
    pub async fn my_groups(&self, user: &User) -> Result<Vec<GroupName>> {
        let ids: Vec<Uuid> = group_member::Entity::find()
            .filter(group_member::Column::UserId.eq(user.id))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|member| member.group_id)
            .collect();
        let groups = group::Entity::find()
            .filter(group::Column::Id.is_in(ids))
            .order_by_asc(group::Column::Name)
            .all(&self.db)
            .await?;
        Ok(groups.into_iter().map(|group| GroupName { id: group.id, name: group.name }).collect())
    }

    async fn group_row(&self, id: Uuid) -> Result<group::Model> {
        group::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| Error::not_found("GROUP_NOT_FOUND", "Group not found"))
    }
}

fn group_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(Error::bad_request("INVALID_NAME", "A group's name has 1 to 64 characters"));
    }
    Ok(name.to_owned())
}

fn group_exists(err: sea_orm::DbErr) -> Error {
    match err.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => Error::conflict("GROUP_EXISTS", "A group with this name exists"),
        _ => err.into(),
    }
}
