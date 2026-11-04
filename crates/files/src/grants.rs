//! Access users give apps (plugins) to their files.
//!
//! Only the owner of the files gives access, never an administrator. It comes
//! in two kinds:
//!
//! - **session** — one file while it is open in the app. The client that shows
//!   the app confirms every [`KEEP_ALIVE_EVERY`]; the access ends when the app
//!   is closed (the client revokes it), after [`SESSION_IDLE`] without a
//!   confirmation, or after [`SESSION_MAX`] in any case;
//! - **standing** — a folder (or all files) until the user revokes it.
//!
//! An app never gets more than its manifest declares ([`Permissions`]) and
//! never more than the user may do: [`Files::app_may_access`] checks both.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustcloud_auth::User;
use rustcloud_core::{audit, Error, Result};
use rustcloud_db::{app_grant, node};
use rustcloud_plugin_api::{FileScope, Permissions};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::service::{Files, Node};

/// How often the client confirms that a session is still open.
pub const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(30);
/// A session without a confirmation for this long has ended.
pub const SESSION_IDLE: Duration = Duration::from_secs(120);
/// No session lasts longer than this.
pub const SESSION_MAX: Duration = Duration::from_secs(12 * 3600);
/// How long a session lasts after its page was closed, for a service the app
/// works with to finish (an editor saving the file on close).
pub const CLOSE_GRACE: Duration = Duration::from_secs(60);

/// How long access lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// While the file is open in the app.
    Session,
    /// Until the user revokes it.
    Standing,
}

impl GrantKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Standing => "standing",
        }
    }
}

/// Access an app has to some of the user's files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    pub id: Uuid,
    pub app_id: String,
    /// The file or folder; access to a folder covers everything inside it.
    pub node: Node,
    pub write: bool,
    pub kind: GrantKind,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// What a user grants: the app, the file or folder, and how.
#[derive(Debug, Clone, Deserialize)]
pub struct NewGrant {
    pub app_id: String,
    pub node_id: Uuid,
    #[serde(default)]
    pub write: bool,
    pub kind: GrantKind,
}

impl Files {
    /// Gives an app access to one of the user's files or folders, within what
    /// the app's manifest allows (`permissions`).
    pub async fn create_grant(&self, user: &User, grant: NewGrant, permissions: &Permissions) -> Result<Grant> {
        let node = self.accessible(user, grant.node_id).await?;
        let allowed = match grant.kind {
            GrantKind::Session => permissions.files >= FileScope::Opened && !node.is_dir,
            GrantKind::Standing if node.parent_id.is_none() => permissions.files >= FileScope::All,
            GrantKind::Standing => permissions.files >= FileScope::Folder && node.is_dir,
        };
        if !allowed || (grant.write && !permissions.files_write) {
            return Err(Error::forbidden(format!("The app \"{}\" did not declare this access to files", grant.app_id))
                .with_code("PERMISSION_NOT_DECLARED"));
        }

        let now = Utc::now();
        let row = app_grant::ActiveModel {
            id: Set(Uuid::new_v4()),
            user_id: Set(user.id),
            app_id: Set(grant.app_id),
            node_id: Set(node.id),
            write: Set(grant.write),
            kind: Set(grant.kind.as_str().to_owned()),
            created_at: Set(now),
            last_seen_at: Set(now),
        }
        .insert(&self.db)
        .await?;
        audit!(
            "grant.create",
            app = row.app_id,
            name = node.name,
            write = row.write,
            kind = row.kind,
            "access given to an app"
        );
        Ok(to_grant(row, node.into()))
    }

    /// The user's access grants that are still in force, newest first. Ended
    /// sessions are removed on the way.
    pub async fn grants(&self, user: &User) -> Result<Vec<Grant>> {
        let rows = self.active_grant_rows(user, None).await?;
        let nodes: HashMap<Uuid, node::Model> = node::Entity::find()
            .filter(node::Column::Id.is_in(rows.iter().map(|g| g.node_id)))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|n| (n.id, n))
            .collect();
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                // In the trash: no access until it is restored.
                let node = nodes.get(&row.node_id).filter(|node| node.trashed_at.is_none())?.clone();
                Some(to_grant(row, node.into()))
            })
            .collect())
    }

    /// Confirms that the app still has the file open, extending the session.
    pub async fn keep_grant_alive(&self, user: &User, grant_id: Uuid) -> Result<()> {
        let row = self
            .active_grant_rows(user, None)
            .await?
            .into_iter()
            .find(|g| g.id == grant_id && g.kind == GrantKind::Session.as_str())
            .ok_or_else(grant_not_found)?;
        let mut row: app_grant::ActiveModel = row.into();
        row.last_seen_at = Set(Utc::now());
        row.update(&self.db).await?;
        Ok(())
    }

    /// The app's page with the file was closed: the session ends after
    /// [`CLOSE_GRACE`], unless a token of the app keeps it alive meanwhile.
    pub async fn close_grant(&self, user: &User, grant_id: Uuid) -> Result<()> {
        let row = self
            .active_grant_rows(user, None)
            .await?
            .into_iter()
            .find(|g| g.id == grant_id && g.kind == GrantKind::Session.as_str())
            .ok_or_else(grant_not_found)?;
        let ends = Utc::now() - chrono::Duration::from_std(SESSION_IDLE - CLOSE_GRACE).map_err(Error::internal)?;
        let mut row: app_grant::ActiveModel = row.into();
        row.last_seen_at = Set(ends.min(Utc::now()));
        row.update(&self.db).await?;
        Ok(())
    }

    /// Whether the grant `grant_id` of `app_id` is still in force and covers
    /// the file: a call with a token the app was given for it. A session it
    /// keeps alive, as the page would.
    pub async fn grant_covers(&self, user: &User, app_id: &str, grant_id: Uuid, file_id: Uuid) -> Result<bool> {
        let Some(grant) = self.active_grant_rows(user, Some(app_id)).await?.into_iter().find(|g| g.id == grant_id)
        else {
            return Ok(false);
        };
        let file = match self.accessible(user, file_id).await {
            Ok(file) => file,
            Err(err) if err.code == "FILE_NOT_FOUND" => return Ok(false),
            Err(err) => return Err(err),
        };
        if !self.is_inside(&file, grant.node_id).await? {
            return Ok(false);
        }
        if grant.kind == GrantKind::Session.as_str() {
            let mut row: app_grant::ActiveModel = grant.into();
            row.last_seen_at = Set(Utc::now());
            row.update(&self.db).await?;
        }
        Ok(true)
    }

    /// The grant of `app_id` through which it reaches the file: a session for
    /// it, or a folder above it. For a token the app gives another server.
    pub async fn grant_for(&self, user: &User, app_id: &str, file_id: Uuid, write: bool) -> Result<Option<Uuid>> {
        let file = self.accessible(user, file_id).await?;
        for grant in self.active_grant_rows(user, Some(app_id)).await? {
            if (grant.write || !write) && self.is_inside(&file, grant.node_id).await? {
                return Ok(Some(grant.id));
            }
        }
        Ok(None)
    }

    /// Whether the user has a file open in `app_id` now (a session in force):
    /// only then does the user reach the service the app shows (`[[proxy]]`).
    pub async fn has_open_file(&self, user: &User, app_id: &str) -> Result<bool> {
        let rows = self.active_grant_rows(user, Some(app_id)).await?;
        Ok(rows.iter().any(|grant| grant.kind == GrantKind::Session.as_str()))
    }

    /// Ends an access grant: the user revokes it, or the app was closed.
    pub async fn revoke_grant(&self, user: &User, grant_id: Uuid) -> Result<()> {
        let row = app_grant::Entity::find_by_id(grant_id)
            .filter(app_grant::Column::UserId.eq(user.id))
            .one(&self.db)
            .await?
            .ok_or_else(grant_not_found)?;
        app_grant::Entity::delete_by_id(row.id).exec(&self.db).await?;
        audit!("grant.revoke", app = row.app_id, kind = row.kind, "access of an app ended");
        Ok(())
    }

    /// Whether `app_id`, acting for `user`, may read (or with `write`, change)
    /// the node: the user may, and the user gave the app access to the node or
    /// a folder above it.
    pub async fn app_may_access(&self, user: &User, app_id: &str, node_id: Uuid, write: bool) -> Result<bool> {
        let node = match self.accessible(user, node_id).await {
            Ok(node) => node,
            Err(err) if err.code == "FILE_NOT_FOUND" => return Ok(false),
            Err(err) => return Err(err),
        };
        // The trash is the user's own, even with all files given.
        if self.in_trash(&node).await? {
            return Ok(false);
        }
        for grant in self.active_grant_rows(user, Some(app_id)).await? {
            if (grant.write || !write) && self.is_inside(&node, grant.node_id).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Grants of the user (of one app, if given) that are in force; deletes
    /// the user's ended sessions.
    pub(crate) async fn active_grant_rows(&self, user: &User, app_id: Option<&str>) -> Result<Vec<app_grant::Model>> {
        let mut query = app_grant::Entity::find().filter(app_grant::Column::UserId.eq(user.id));
        if let Some(app_id) = app_id {
            query = query.filter(app_grant::Column::AppId.eq(app_id));
        }
        let rows = query.order_by_desc(app_grant::Column::CreatedAt).all(&self.db).await?;

        let now = Utc::now();
        let (active, ended): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| is_active(row, now));
        if !ended.is_empty() {
            app_grant::Entity::delete_many()
                .filter(app_grant::Column::Id.is_in(ended.iter().map(|g| g.id)))
                .exec(&self.db)
                .await?;
        }
        Ok(active)
    }
}

/// Standing grants last until revoked; sessions end when idle or too old.
fn is_active(row: &app_grant::Model, now: DateTime<Utc>) -> bool {
    if row.kind != GrantKind::Session.as_str() {
        return true;
    }
    let idle = now.signed_duration_since(row.last_seen_at).to_std().unwrap_or_default();
    let age = now.signed_duration_since(row.created_at).to_std().unwrap_or_default();
    idle < SESSION_IDLE && age < SESSION_MAX
}

fn to_grant(row: app_grant::Model, node: Node) -> Grant {
    let kind = if row.kind == GrantKind::Session.as_str() { GrantKind::Session } else { GrantKind::Standing };
    Grant {
        id: row.id,
        app_id: row.app_id,
        node,
        write: row.write,
        kind,
        created_at: row.created_at,
        last_seen_at: row.last_seen_at,
    }
}

fn grant_not_found() -> Error {
    Error::not_found("GRANT_NOT_FOUND", "This access does not exist or has ended")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: GrantKind, created_ago: u64, seen_ago: u64) -> app_grant::Model {
        let now = Utc::now();
        app_grant::Model {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            app_id: "notes".to_owned(),
            node_id: Uuid::new_v4(),
            write: false,
            kind: kind.as_str().to_owned(),
            created_at: now - Duration::from_secs(created_ago),
            last_seen_at: now - Duration::from_secs(seen_ago),
        }
    }

    #[test]
    fn sessions_end_when_idle_or_too_old_standing_grants_do_not() {
        let now = Utc::now();
        assert!(is_active(&row(GrantKind::Session, 60, 10), now));
        assert!(!is_active(&row(GrantKind::Session, 600, 300), now), "no confirmation for 5 minutes");
        assert!(!is_active(&row(GrantKind::Session, 13 * 3600, 5), now), "older than 12 hours");
        assert!(is_active(&row(GrantKind::Standing, 400 * 24 * 3600, 400 * 24 * 3600), now));
    }
}
