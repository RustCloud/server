//! Checks that the migrations produce a schema with the promised constraints,
//! on whichever backend the tests run against.

use chrono::Utc;
use rustcloud_db::testing::test_db;
use rustcloud_db::{node, user};
use sea_orm::{ActiveModelTrait, EntityTrait, PaginatorTrait, Set};
use uuid::Uuid;

async fn insert_user(db: &sea_orm::DatabaseConnection, username: &str) -> user::Model {
    user::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(username.to_string()),
        display_name: Set(username.to_string()),
        email: Set(None),
        is_admin: Set(false),
        disabled: Set(false),
        quota_bytes: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .unwrap()
}

fn folder(owner: Uuid, parent: Option<Uuid>, name: &str) -> node::ActiveModel {
    node::ActiveModel {
        id: Set(Uuid::new_v4()),
        owner_id: Set(owner),
        parent_id: Set(parent),
        name: Set(name.to_string()),
        is_dir: Set(true),
        size: Set(0),
        mime_type: Set(None),
        content_key: Set(None),
        sha256: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        trashed_at: Set(None),
        trashed_from: Set(None),
        modified_by: Set(None),
        modified_at: Set(None),
    }
}

#[tokio::test]
async fn usernames_are_unique() {
    let db = test_db().await;
    insert_user(&db.conn, "alice").await;
    let duplicate = user::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set("alice".into()),
        display_name: Set("Alice 2".into()),
        email: Set(None),
        is_admin: Set(false),
        disabled: Set(false),
        quota_bytes: Set(None),
        created_at: Set(Utc::now()),
    };
    assert!(duplicate.insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn a_user_has_only_one_root_folder() {
    let db = test_db().await;
    let alice = insert_user(&db.conn, "alice").await;
    folder(alice.id, None, "").insert(&db.conn).await.unwrap();
    assert!(folder(alice.id, None, "").insert(&db.conn).await.is_err());
}

#[tokio::test]
async fn names_are_unique_within_a_folder() {
    let db = test_db().await;
    let alice = insert_user(&db.conn, "alice").await;
    let root = folder(alice.id, None, "").insert(&db.conn).await.unwrap();
    folder(alice.id, Some(root.id), "Docs").insert(&db.conn).await.unwrap();
    assert!(folder(alice.id, Some(root.id), "Docs").insert(&db.conn).await.is_err());
    // The same name in another folder is fine.
    let other = folder(alice.id, Some(root.id), "Other").insert(&db.conn).await.unwrap();
    folder(alice.id, Some(other.id), "Docs").insert(&db.conn).await.unwrap();
}

#[tokio::test]
async fn deleting_a_folder_deletes_its_subtree() {
    let db = test_db().await;
    let alice = insert_user(&db.conn, "alice").await;
    let root = folder(alice.id, None, "").insert(&db.conn).await.unwrap();
    let docs = folder(alice.id, Some(root.id), "Docs").insert(&db.conn).await.unwrap();
    folder(alice.id, Some(docs.id), "2024").insert(&db.conn).await.unwrap();

    node::Entity::delete_by_id(docs.id).exec(&db.conn).await.unwrap();
    assert_eq!(node::Entity::find().count(&db.conn).await.unwrap(), 1, "only the root is left");
}

#[tokio::test]
async fn deleting_a_user_deletes_their_files() {
    let db = test_db().await;
    let alice = insert_user(&db.conn, "alice").await;
    folder(alice.id, None, "").insert(&db.conn).await.unwrap();

    user::Entity::delete_by_id(alice.id).exec(&db.conn).await.unwrap();
    assert_eq!(node::Entity::find().count(&db.conn).await.unwrap(), 0);
}

#[tokio::test]
async fn sqlite_folder_is_created_on_first_start() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("new/nested/rustcloud.db");
    rustcloud_db::connect(&format!("sqlite://{}?mode=rwc", db_path.display())).await.unwrap();
    assert!(db_path.exists());
}

#[tokio::test]
async fn log_entries_get_increasing_ids() {
    let db = test_db().await;
    let entry = |message: &str| rustcloud_db::log_entry::ActiveModel {
        time: Set(Utc::now()),
        level: Set("INFO".into()),
        action: Set(Some("test".into())),
        message: Set(message.into()),
        username: Set(None),
        ip: Set(None),
        details: Set("{}".into()),
        ..Default::default()
    };
    let first = entry("first").insert(&db.conn).await.unwrap();
    let second = entry("second").insert(&db.conn).await.unwrap();
    assert!(second.id > first.id);
}
