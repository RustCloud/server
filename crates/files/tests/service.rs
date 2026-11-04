//! Behaviour of the files service against a real database and a real blob store.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use rustcloud_auth::{Auth, NewAccount, User};
use rustcloud_core::{ByteRange, ByteStream, Live};
use rustcloud_db::testing::{test_db, TestDb};
use rustcloud_db::{file_version, node, share};
use rustcloud_files::{Download, FileEventKind, Files, FilesSettings, GrantKind, Locking, NewGrant, NodeUpdate};
use rustcloud_plugin_api::{FileScope, Permissions};
use rustcloud_storage_local::LocalBlobStore;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

struct Setup {
    files: Files,
    settings: Arc<Live<FilesSettings>>,
    alice: User,
    bob: User,
    blob_dir: tempfile::TempDir,
    db: TestDb,
}

async fn setup() -> Setup {
    let db = test_db().await;
    let auth = Auth::new(db.conn.clone(), Arc::default());
    let account = |name: &str| NewAccount {
        username: name.into(),
        password: "password123".into(),
        display_name: None,
        email: None,
    };
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let bob = auth.create_user(account("bob"), false).await.unwrap();

    let blob_dir = tempfile::tempdir().unwrap();
    let blobs = LocalBlobStore::open(blob_dir.path()).await.unwrap();
    let settings = Arc::new(Live::new(FilesSettings::default()));
    let files = Files::new(db.conn.clone(), Arc::new(blobs), settings.clone());
    Setup { files, settings, alice, bob, blob_dir, db }
}

fn code_of<T: std::fmt::Debug>(result: rustcloud_core::Result<T>) -> String {
    result.unwrap_err().code.to_string()
}

/// What a plugin at ×1 reads at once.
const LIMIT: u64 = 16 << 20;

fn body(text: &'static str) -> ByteStream {
    Box::pin(stream::once(async move { Ok(Bytes::from_static(text.as_bytes())) }))
}

async fn read(download: Download) -> String {
    let chunks: Vec<Bytes> = download.body.map(|chunk| chunk.unwrap()).collect().await;
    String::from_utf8(chunks.concat()).unwrap()
}

/// Number of stored blobs (files under `blobs/`).
fn blob_count(dir: &Path) -> usize {
    let blobs = dir.join("blobs");
    std::fs::read_dir(blobs).unwrap().map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count()).sum()
}

#[tokio::test]
async fn root_folder_is_created_once() {
    let s = setup().await;
    let first = s.files.root(&s.alice).await.unwrap();
    let second = s.files.root(&s.alice).await.unwrap();
    assert_eq!(first.id, second.id);
    assert!(first.is_dir);
    assert_ne!(s.files.root(&s.bob).await.unwrap().id, first.id, "each user has their own root");
}

#[tokio::test]
async fn folders_are_listed_before_files_and_sorted_by_name() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    s.files.upload(&s.alice, root.id, "b.txt", body("b"), false).await.unwrap();
    s.files.create_folder(&s.alice, root.id, "Zebra").await.unwrap();
    s.files.upload(&s.alice, root.id, "A.txt", body("a"), false).await.unwrap();
    s.files.create_folder(&s.alice, root.id, "apple").await.unwrap();

    let names: Vec<String> = s.files.children(&s.alice, root.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(names, ["apple", "Zebra", "A.txt", "b.txt"]);
}

#[tokio::test]
async fn duplicate_names_are_rejected() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let err = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap_err();
    assert_eq!(err.code, "NAME_TAKEN");
    let err = s.files.upload(&s.alice, root.id, "Docs", body("x"), true).await.unwrap_err();
    assert_eq!(err.code, "NAME_TAKEN", "a folder is never overwritten by a file");
}

#[tokio::test]
async fn uploaded_content_can_be_downloaded() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "hello.txt", body("hello world"), false).await.unwrap();

    assert_eq!(file.size, 11);
    assert_eq!(file.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(
        file.etag.as_deref(),
        Some("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"),
        "etag is the SHA-256 of the content"
    );
    assert_eq!(read(s.files.download(&s.alice, file.id, None).await.unwrap()).await, "hello world");

    let range = ByteRange { start: 6, end: 10 };
    assert_eq!(read(s.files.download(&s.alice, file.id, Some(range)).await.unwrap()).await, "world");
}

#[tokio::test]
async fn overwriting_needs_permission_and_replaces_the_blob() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let first = s.files.upload(&s.alice, root.id, "notes.txt", body("v1"), false).await.unwrap();

    let err = s.files.upload(&s.alice, root.id, "notes.txt", body("v2"), false).await.unwrap_err();
    assert_eq!(err.code, "NAME_TAKEN");

    let second = s.files.upload(&s.alice, root.id, "notes.txt", body("v2!"), true).await.unwrap();
    assert_eq!(second.id, first.id, "the same file is updated");
    assert_ne!(second.etag, first.etag);
    assert_eq!(read(s.files.download(&s.alice, first.id, None).await.unwrap()).await, "v2!");
    assert_eq!(blob_count(s.blob_dir.path()), 1, "the old content is removed");
}

#[tokio::test]
async fn other_users_cannot_see_or_touch_files() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "secret.txt", body("secret"), false).await.unwrap();

    assert_eq!(s.files.get(&s.bob, file.id).await.unwrap_err().code, "FILE_NOT_FOUND");
    assert!(s.files.download(&s.bob, file.id, None).await.is_err());
    assert!(s.files.children(&s.bob, root.id).await.is_err());
    assert!(s.files.delete(&s.bob, file.id).await.is_err());
    assert!(s.files.upload(&s.bob, root.id, "x.txt", body("x"), false).await.is_err());
    let bob_root = s.files.root(&s.bob).await.unwrap();
    assert!(s.files.copy(&s.bob, file.id, bob_root.id, None).await.is_err());
}

#[tokio::test]
async fn rename_and_move() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false).await.unwrap();

    let update = NodeUpdate { name: Some("b.txt".into()), parent_id: Some(docs.id) };
    let moved = s.files.update(&s.alice, file.id, update).await.unwrap();
    assert_eq!(moved.name, "b.txt");
    assert_eq!(moved.parent_id, Some(docs.id));

    let path: Vec<String> = s.files.ancestors(&s.alice, file.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(path, ["", "Docs"], "root first");
}

#[tokio::test]
async fn folders_cannot_be_moved_into_themselves() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let a = s.files.create_folder(&s.alice, root.id, "A").await.unwrap();
    let b = s.files.create_folder(&s.alice, a.id, "B").await.unwrap();

    for target in [a.id, b.id] {
        let update = NodeUpdate { name: None, parent_id: Some(target) };
        let err = s.files.update(&s.alice, a.id, update).await.unwrap_err();
        assert_eq!(err.code, "MOVE_INTO_ITSELF");
    }
}

#[tokio::test]
async fn root_cannot_be_renamed_deleted_or_shared() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let rename = NodeUpdate { name: Some("x".into()), parent_id: None };
    assert_eq!(s.files.update(&s.alice, root.id, rename).await.unwrap_err().code, "ROOT_FOLDER");
    assert_eq!(s.files.delete(&s.alice, root.id).await.unwrap_err().code, "ROOT_FOLDER");
    assert_eq!(s.files.create_share(&s.alice, root.id, None).await.unwrap_err().code, "ROOT_FOLDER");
}

#[tokio::test]
async fn copying_a_folder_copies_everything_inside() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let sub = s.files.create_folder(&s.alice, docs.id, "Sub").await.unwrap();
    s.files.upload(&s.alice, sub.id, "deep.txt", body("deep"), false).await.unwrap();

    let copy = s.files.copy(&s.alice, docs.id, root.id, Some("Docs copy")).await.unwrap();
    let copied_sub = &s.files.children(&s.alice, copy.id).await.unwrap()[0];
    let copied_file = &s.files.children(&s.alice, copied_sub.id).await.unwrap()[0];
    assert_eq!(copied_file.name, "deep.txt");
    assert_eq!(read(s.files.download(&s.alice, copied_file.id, None).await.unwrap()).await, "deep");
    assert_eq!(blob_count(s.blob_dir.path()), 2, "content is copied, not shared");

    let err = s.files.copy(&s.alice, docs.id, sub.id, None).await.unwrap_err();
    assert_eq!(err.code, "COPY_INTO_ITSELF");
}

#[tokio::test]
async fn deleting_a_folder_removes_all_content() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let sub = s.files.create_folder(&s.alice, docs.id, "Sub").await.unwrap();
    let file = s.files.upload(&s.alice, sub.id, "a.txt", body("a"), false).await.unwrap();
    s.files.upload(&s.alice, docs.id, "b.txt", body("b"), false).await.unwrap();

    s.files.delete(&s.alice, docs.id).await.unwrap();
    let trash = s.files.trash(&s.alice).await.unwrap();
    let left: Vec<String> = s.files.children(&s.alice, root.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(left, [".trash"], "deleted things go to the trash");
    assert!(s.files.get(&s.alice, file.id).await.unwrap().trashed_at.is_some(), "with everything inside");

    s.files.empty_trash(&s.alice).await.unwrap();
    assert!(s.files.get(&s.alice, file.id).await.is_err());
    assert!(s.files.children(&s.alice, trash.id).await.unwrap().is_empty());
    assert_eq!(blob_count(s.blob_dir.path()), 0);
}

#[tokio::test]
async fn the_trash_keeps_what_was_deleted_until_restored_or_deleted_again() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let report = s.files.upload(&s.alice, docs.id, "report.pdf", body("1"), false).await.unwrap();
    let share = s.files.create_share(&s.alice, report.id, None).await.unwrap();

    s.files.delete(&s.alice, report.id).await.unwrap();
    assert!(s.files.search(&s.alice, "report").await.unwrap().is_empty(), "search leaves the trash out");
    assert!(s.files.public_view(&share.token, None).await.is_err(), "links stop working");
    // Another file of the same name deleted: both stay.
    let again = s.files.upload(&s.alice, docs.id, "report.pdf", body("2"), false).await.unwrap();
    s.files.delete(&s.alice, again.id).await.unwrap();
    let trash = s.files.trash(&s.alice).await.unwrap();
    let mut names: Vec<String> =
        s.files.children(&s.alice, trash.id).await.unwrap().into_iter().map(|n| n.name).collect();
    names.sort();
    assert_eq!(names, ["report (2).pdf", "report.pdf"]);

    // Nothing gets in but by deleting, and the trash itself stays.
    let put = s.files.upload(&s.alice, trash.id, "x.txt", body("x"), false).await.unwrap_err();
    assert_eq!(put.code, "IN_TRASH");
    let moved = s.files.update(&s.alice, docs.id, NodeUpdate { name: None, parent_id: Some(trash.id) }).await;
    assert_eq!(moved.unwrap_err().code, "IN_TRASH");
    assert_eq!(s.files.delete(&s.alice, trash.id).await.unwrap_err().code, "TRASH_FOLDER");

    // Restored where it was; the name taken there gets a number.
    let restored = s.files.restore(&s.alice, report.id).await.unwrap();
    assert_eq!((restored.parent_id, restored.name.as_str()), (Some(docs.id), "report.pdf"));
    assert!(restored.trashed_at.is_none());
    assert!(s.files.public_view(&share.token, None).await.is_ok(), "and its links work again");
    s.files.delete(&s.alice, report.id).await.unwrap();
    s.files.upload(&s.alice, docs.id, "report.pdf", body("3"), false).await.unwrap();
    assert_eq!(s.files.restore(&s.alice, report.id).await.unwrap().name, "report (2).pdf");
    assert_eq!(s.files.restore(&s.alice, report.id).await.unwrap_err().code, "NOT_IN_TRASH");

    // The folder it came from deleted too: to the root.
    s.files.delete(&s.alice, docs.id).await.unwrap();
    let restored = s.files.restore(&s.alice, again.id).await.unwrap();
    assert_eq!(restored.parent_id, Some(root.id));

    // Deleted in the trash: gone for good.
    s.files.delete(&s.alice, docs.id).await.unwrap();
    assert!(s.files.get(&s.alice, docs.id).await.is_err());
    assert!(s.files.get(&s.alice, report.id).await.is_err(), "with what was inside");
}

#[tokio::test]
async fn old_things_leave_the_trash() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "old.txt", body("x"), false).await.unwrap();
    s.files.delete(&s.alice, file.id).await.unwrap();

    let before = chrono::Utc::now() - chrono::Duration::days(1);
    assert_eq!(s.files.purge_trash(before).await.unwrap(), 0, "deleted just now");
    let later = chrono::Utc::now() + chrono::Duration::seconds(1);
    assert_eq!(s.files.purge_trash(later).await.unwrap(), 1);
    assert!(s.files.get(&s.alice, file.id).await.is_err());
    assert_eq!(blob_count(s.blob_dir.path()), 0);
}

#[tokio::test]
async fn quota_is_enforced_while_uploading() {
    let mut s = setup().await;
    s.alice.quota_bytes = Some(10);
    let root = s.files.root(&s.alice).await.unwrap();

    let file = s.files.upload(&s.alice, root.id, "a.txt", body("123456"), false).await.unwrap();
    let err = s.files.upload(&s.alice, root.id, "b.txt", body("123456"), false).await.unwrap_err();
    assert_eq!(err.code, "QUOTA_EXCEEDED");
    assert_eq!(s.files.children(&s.alice, root.id).await.unwrap().len(), 1, "nothing half-uploaded is kept");
    assert_eq!(blob_count(s.blob_dir.path()), 1);

    // Replacing a file only needs room for the difference.
    s.files.replace_content(&s.alice, file.id, body("1234567890"), None).await.unwrap();

    let usage = s.files.usage(&s.alice).await.unwrap();
    assert_eq!(usage.used_bytes, 10);
    assert_eq!(usage.quota_bytes, Some(10));

    let err = s.files.copy(&s.alice, file.id, root.id, Some("copy.txt")).await.unwrap_err();
    assert_eq!(err.code, "QUOTA_EXCEEDED");
}

#[tokio::test]
async fn upload_size_limit_applies_as_soon_as_it_is_changed() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let mib = |n: usize| -> ByteStream { Box::pin(stream::once(async move { Ok(Bytes::from(vec![0u8; n])) })) };

    s.files.upload(&s.alice, root.id, "big.bin", mib(1024 * 1024 + 1), false).await.unwrap();

    s.settings.set(FilesSettings { max_upload_mib: Some(1), ..Default::default() });
    let err = s.files.upload(&s.alice, root.id, "too-big.bin", mib(1024 * 1024 + 1), false).await.unwrap_err();
    assert_eq!(err.code, "FILE_TOO_LARGE");
    s.files.upload(&s.alice, root.id, "fits.bin", mib(1024 * 1024), false).await.unwrap();
    assert_eq!(blob_count(s.blob_dir.path()), 2, "the rejected upload left nothing behind");
}

#[tokio::test]
async fn search_is_case_insensitive_and_treats_wildcards_literally() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    for name in ["Report 2024.pdf", "report_final.doc", "100% done.txt", "other.txt"] {
        s.files.upload(&s.alice, root.id, name, body("x"), false).await.unwrap();
    }
    let bob_root = s.files.root(&s.bob).await.unwrap();
    s.files.upload(&s.bob, bob_root.id, "report-bob.txt", body("x"), false).await.unwrap();

    let names = |found: Vec<rustcloud_files::Node>| found.into_iter().map(|n| n.name).collect::<Vec<_>>();
    assert_eq!(names(s.files.search(&s.alice, "REPORT").await.unwrap()), ["Report 2024.pdf", "report_final.doc"]);
    assert_eq!(names(s.files.search(&s.alice, "%").await.unwrap()), ["100% done.txt"]);
    assert_eq!(names(s.files.search(&s.alice, "t_f").await.unwrap()), ["report_final.doc"]);
    assert!(s.files.search(&s.alice, "  ").await.unwrap().is_empty());
}

#[tokio::test]
async fn public_link_gives_read_access_to_a_folder_and_nothing_else() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let shared = s.files.create_folder(&s.alice, root.id, "Shared").await.unwrap();
    let sub = s.files.create_folder(&s.alice, shared.id, "Sub").await.unwrap();
    let inside = s.files.upload(&s.alice, sub.id, "in.txt", body("inside"), false).await.unwrap();
    let outside = s.files.upload(&s.alice, root.id, "out.txt", body("outside"), false).await.unwrap();

    let link = s.files.create_share(&s.alice, shared.id, None).await.unwrap();
    assert_eq!(s.files.shares(&s.alice).await.unwrap()[0].node.id, shared.id);

    let top = s.files.public_view(&link.token, None).await.unwrap();
    assert_eq!(top.current.id, shared.id);
    assert_eq!(top.children[0].id, sub.id);

    let deeper = s.files.public_view(&link.token, Some(sub.id)).await.unwrap();
    assert_eq!(deeper.path.iter().map(|n| n.id).collect::<Vec<_>>(), [shared.id]);
    assert_eq!(read(s.files.public_download(&link.token, Some(inside.id), None).await.unwrap()).await, "inside");

    assert!(s.files.public_download(&link.token, Some(outside.id), None).await.is_err());
    assert!(s.files.public_view(&link.token, Some(root.id)).await.is_err());

    s.files.delete_share(&s.alice, link.id).await.unwrap();
    assert_eq!(s.files.public_view(&link.token, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn expired_links_stop_working() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false).await.unwrap();

    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    assert_eq!(s.files.create_share(&s.alice, file.id, Some(past)).await.unwrap_err().code, "INVALID_EXPIRY");

    let tomorrow = chrono::Utc::now() + chrono::Duration::days(1);
    let link = s.files.create_share(&s.alice, file.id, Some(tomorrow)).await.unwrap();
    assert!(s.files.public_view(&link.token, None).await.is_ok());

    // Let the link expire by moving its expiry into the past.
    let mut row: share::ActiveModel = share::Entity::find_by_id(link.id).one(&s.db.conn).await.unwrap().unwrap().into();
    row.expires_at = Set(Some(past));
    row.update(&s.db.conn).await.unwrap();
    assert_eq!(s.files.public_view(&link.token, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn only_the_owner_can_delete_a_link() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false).await.unwrap();
    let link = s.files.create_share(&s.alice, file.id, None).await.unwrap();

    assert_eq!(s.files.delete_share(&s.bob, link.id).await.unwrap_err().code, "SHARE_NOT_FOUND");
    assert!(s.files.shares(&s.bob).await.unwrap().is_empty());
}

#[tokio::test]
async fn storage_report_counts_all_files_and_keeps_the_last_speed_test() {
    let s = setup().await;
    let alice_root = s.files.root(&s.alice).await.unwrap();
    let bob_root = s.files.root(&s.bob).await.unwrap();
    s.files.upload(&s.alice, alice_root.id, "a.txt", body("12345"), false).await.unwrap();
    s.files.upload(&s.bob, bob_root.id, "b.txt", body("123"), false).await.unwrap();
    s.files.create_folder(&s.bob, bob_root.id, "folder").await.unwrap();

    let report = s.files.storage_report().await.unwrap();
    assert_eq!((report.stored_bytes, report.file_count), (8, 2), "folders are not counted");
    assert_eq!(report.kind, "local");
    assert!(report.capacity.is_some() && report.used_percent.is_some());
    assert!(report.speed_test.is_none());

    let result = s.files.speed_test().await.unwrap();
    assert!(result.write_speed > 0.0 && result.read_speed > 0.0);
    let report = s.files.storage_report().await.unwrap();
    assert_eq!(report.speed_test.map(|t| t.tested_at), Some(result.tested_at));
    assert_eq!(blob_count(s.blob_dir.path()), 2, "the test data is removed");
}

/// Moves the creation of a file and of all its versions `hours` back, as if
/// the latest save were long ago; their order stays.
async fn age(s: &Setup, id: uuid::Uuid, hours: i64) {
    let back = chrono::Duration::hours(hours);
    let file = node::Entity::find_by_id(id).one(&s.db.conn).await.unwrap().unwrap();
    let created_at = file.created_at - back;
    node::ActiveModel { created_at: Set(created_at), ..file.into() }.update(&s.db.conn).await.unwrap();
    let versions = file_version::Entity::find().filter(file_version::Column::NodeId.eq(id)).all(&s.db.conn).await;
    for version in versions.unwrap() {
        let created_at = version.created_at - back;
        file_version::ActiveModel { created_at: Set(created_at), ..version.into() }.update(&s.db.conn).await.unwrap();
    }
}

/// Replaces the content a while after the latest save, so it makes a version.
async fn save_later(s: &Setup, id: uuid::Uuid, text: &'static str) {
    age(s, id, 1).await;
    s.files.replace_content(&s.alice, id, body(text), None).await.unwrap();
}

async fn version_texts(s: &Setup, id: uuid::Uuid) -> Vec<String> {
    let mut texts = Vec::new();
    for version in s.files.versions(&s.alice, id).await.unwrap().versions {
        texts.push(read(s.files.download_version(&s.alice, id, version.id, None).await.unwrap()).await);
    }
    texts
}

#[tokio::test]
async fn replacing_a_file_keeps_the_old_content_as_a_version() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "plan.txt", body("v1"), false).await.unwrap();
    s.files.replace_content(&s.alice, file.id, body("v2"), None).await.unwrap();
    assert!(version_texts(&s, file.id).await.is_empty(), "saved right after creating it: one change");

    save_later(&s, file.id, "v3").await;
    s.files.replace_content(&s.alice, file.id, body("v4"), None).await.unwrap();
    s.files.replace_content(&s.alice, file.id, body("v4"), None).await.unwrap();
    assert_eq!(version_texts(&s, file.id).await, ["v2"], "saves close together and the same bytes add none");

    save_later(&s, file.id, "v5").await;
    assert_eq!(version_texts(&s, file.id).await, ["v4", "v2"], "newest first");
    let versions = s.files.versions(&s.alice, file.id).await.unwrap();
    assert_eq!((versions.keep, versions.days), (10, 30), "how long they stay, for the list to tell");
    assert_eq!(versions.versions[0].modified_by, Some(s.alice.id));

    // Restoring keeps what was current, even right after saving it.
    let v2 = versions.versions[1].id;
    let restored = s.files.restore_version(&s.alice, file.id, v2).await.unwrap();
    assert_eq!(restored.etag, Some(versions.versions[1].etag.clone()));
    assert_eq!(read(s.files.download(&s.alice, file.id, None).await.unwrap()).await, "v2");
    assert_eq!(version_texts(&s, file.id).await, ["v5", "v4", "v2"]);

    assert_eq!(code_of(s.files.versions(&s.bob, file.id).await), "FILE_NOT_FOUND");
    assert_eq!(code_of(s.files.restore_version(&s.bob, file.id, v2).await), "FILE_NOT_FOUND");
    assert_eq!(code_of(s.files.versions(&s.alice, root.id).await), "NOT_A_FILE");

    // Versions go together with the file.
    s.files.delete(&s.alice, file.id).await.unwrap();
    s.files.empty_trash(&s.alice).await.unwrap();
    assert_eq!(blob_count(s.blob_dir.path()), 0);
}

#[tokio::test]
async fn a_version_stays_while_it_is_one_of_the_latest_or_recent_enough() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("1"), false).await.unwrap();
    for text in ["2", "3", "4", "5"] {
        save_later(&s, file.id, text).await;
    }
    assert_eq!(version_texts(&s, file.id).await, ["4", "3", "2", "1"]);

    // Two to keep, but all four from the last ten days: all stay.
    s.settings.set(FilesSettings { versions_keep: 2, versions_days: 10, ..Default::default() });
    age(&s, file.id, 24 * 5).await;
    assert_eq!(s.files.purge_versions().await.unwrap(), 0);
    assert_eq!(version_texts(&s, file.id).await.len(), 4);

    // Ten to keep, though older than thirty days: all stay.
    s.settings.set(FilesSettings { versions_keep: 10, versions_days: 30, ..Default::default() });
    age(&s, file.id, 24 * 60).await;
    assert_eq!(s.files.purge_versions().await.unwrap(), 0);

    // Neither: only the latest two stay.
    s.settings.set(FilesSettings { versions_keep: 2, versions_days: 30, ..Default::default() });
    assert_eq!(s.files.purge_versions().await.unwrap(), 2);
    assert_eq!(version_texts(&s, file.id).await, ["4", "3"]);
    assert_eq!(blob_count(s.blob_dir.path()), 3, "the content and two versions");

    // Both 0: no versions at all.
    s.settings.set(FilesSettings { versions_keep: 0, versions_days: 0, ..Default::default() });
    save_later(&s, file.id, "6").await;
    assert_eq!(s.files.purge_versions().await.unwrap(), 2);
    assert!(version_texts(&s, file.id).await.is_empty());
}

#[tokio::test]
async fn versions_give_way_when_files_need_the_room() {
    let mut s = setup().await;
    s.alice.quota_bytes = Some(20);
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("123456"), false).await.unwrap();
    save_later(&s, file.id, "12345678").await;
    let usage = s.files.usage(&s.alice).await.unwrap();
    assert_eq!((usage.used_bytes, usage.versions_bytes), (14, 6), "versions count in the quota");

    // 8 + 10 fit into 20 once the version goes.
    s.files.upload(&s.alice, root.id, "b.txt", body("1234567890"), false).await.unwrap();
    let usage = s.files.usage(&s.alice).await.unwrap();
    assert_eq!((usage.used_bytes, usage.versions_bytes), (18, 0));
    assert!(version_texts(&s, file.id).await.is_empty());

    // Files themselves still can't go over it.
    let err = s.files.upload(&s.alice, root.id, "c.txt", body("123"), false).await.unwrap_err();
    assert_eq!(err.code, "QUOTA_EXCEEDED");
}

#[tokio::test]
async fn app_data_keeps_no_versions() {
    let s = setup().await;
    let node = s.files.appdata_write(&s.alice, "notes", "theme", b"dark".to_vec()).await.unwrap();
    age(&s, node.id, 1).await;
    s.files.appdata_write(&s.alice, "notes", "theme", b"light".to_vec()).await.unwrap();
    assert!(s.files.versions(&s.alice, node.id).await.unwrap().versions.is_empty());
    assert_eq!(blob_count(s.blob_dir.path()), 1);
}

fn permissions(files: FileScope, files_write: bool) -> Permissions {
    Permissions { files, files_write, ..Default::default() }
}

fn new_grant(node_id: uuid::Uuid, kind: GrantKind, write: bool) -> NewGrant {
    NewGrant { app_id: "notes".into(), node_id, write, kind }
}

#[tokio::test]
async fn events_name_the_apps_that_watched_the_files_before_they_were_deleted() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let photos = s.files.create_folder(&s.alice, root.id, "Photos").await.unwrap();
    let trip = s.files.create_folder(&s.alice, photos.id, "Trip").await.unwrap();
    let hidden = s.files.create_folder(&s.alice, photos.id, ".thumbs").await.unwrap();
    let cat = s.files.upload(&s.alice, photos.id, "cat.jpg", body("cat"), false).await.unwrap();
    let sea = s.files.upload(&s.alice, trip.id, "sea.jpg", body("sea"), false).await.unwrap();
    s.files.upload(&s.alice, hidden.id, "cat.webp", body("small"), false).await.unwrap();
    let grant = |app: &str, node_id| NewGrant { app_id: app.into(), node_id, write: false, kind: GrantKind::Standing };
    let folders = permissions(FileScope::Folder, false);
    s.files.create_grant(&s.alice, grant("photos", photos.id), &folders).await.unwrap();
    s.files.create_grant(&s.alice, grant("maps", trip.id), &folders).await.unwrap();
    let opened = permissions(FileScope::Opened, false);
    s.files
        .create_grant(&s.alice, NewGrant { kind: GrantKind::Session, ..grant("notes", cat.id) }, &opened)
        .await
        .unwrap();

    let mut events = s.files.subscribe();
    s.files.delete(&s.alice, photos.id).await.unwrap();
    let event = events.try_recv().expect("one event for the whole folder");
    assert_eq!(event.kind, FileEventKind::Deleted);
    assert_eq!(event.owner_id, s.alice.id);
    let mut watched: Vec<(String, Vec<String>)> =
        event.files.iter().map(|file| (file.node.name.clone(), file.apps.clone())).collect();
    watched.sort();
    assert_eq!(
        watched,
        [
            ("cat.jpg".to_owned(), vec!["photos".to_owned()]),
            ("sea.jpg".to_owned(), vec!["maps".into(), "photos".into()])
        ],
        "files only, not hidden ones, with the apps given a folder above them for good"
    );
    assert_eq!(event.files.iter().find(|file| file.node.id == sea.id).unwrap().node.parent_id, Some(trip.id));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn moving_tells_the_apps_of_both_places_and_nobody_watching_means_no_event() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let photos = s.files.create_folder(&s.alice, root.id, "Photos").await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "cat.jpg", body("cat"), false).await.unwrap();
    let mut events = s.files.subscribe();
    let rename = NodeUpdate { name: Some("kitten.jpg".into()), parent_id: None };
    s.files.update(&s.alice, file.id, rename).await.unwrap();
    assert!(events.try_recv().is_err(), "no app watches it");

    let folders = permissions(FileScope::Folder, false);
    s.files.create_grant(&s.alice, new_grant(photos.id, GrantKind::Standing, false), &folders).await.unwrap();
    let into = NodeUpdate { name: None, parent_id: Some(photos.id) };
    s.files.update(&s.alice, file.id, into).await.unwrap();
    let event = events.try_recv().unwrap();
    assert_eq!(event.kind, FileEventKind::Moved);
    assert_eq!(event.files.len(), 1);
    assert_eq!(event.files[0].node.parent_id, Some(photos.id), "as it is now");
    assert_eq!(event.files[0].apps, ["notes"]);

    let out = NodeUpdate { name: None, parent_id: Some(root.id) };
    s.files.update(&s.alice, file.id, out).await.unwrap();
    let event = events.try_recv().unwrap();
    assert_eq!((event.files[0].node.parent_id, event.files[0].apps.clone()), (Some(root.id), vec!["notes".into()]));
}

#[tokio::test]
async fn a_folder_grant_covers_what_is_inside_and_nothing_else() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let notes = s.files.create_folder(&s.alice, root.id, "Notes").await.unwrap();
    let inner = s.files.create_folder(&s.alice, notes.id, "Work").await.unwrap();
    let note = s.files.upload(&s.alice, inner.id, "todo.md", body("- [ ] x"), false).await.unwrap();
    let other = s.files.upload(&s.alice, root.id, "secret.txt", body("x"), false).await.unwrap();

    let allowed = permissions(FileScope::Folder, false);
    s.files.create_grant(&s.alice, new_grant(notes.id, GrantKind::Standing, false), &allowed).await.unwrap();

    assert!(s.files.app_may_access(&s.alice, "notes", note.id, false).await.unwrap());
    assert!(!s.files.app_may_access(&s.alice, "notes", note.id, true).await.unwrap(), "read only");
    assert!(!s.files.app_may_access(&s.alice, "notes", other.id, false).await.unwrap(), "outside the folder");
    assert!(!s.files.app_may_access(&s.alice, "calendar", note.id, false).await.unwrap(), "another app");
    assert!(!s.files.app_may_access(&s.bob, "notes", note.id, false).await.unwrap(), "another user");
}

#[tokio::test]
async fn apps_get_no_more_than_their_manifest_declares() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let folder = s.files.create_folder(&s.alice, root.id, "Notes").await.unwrap();
    let file = s.files.upload(&s.alice, folder.id, "a.md", body("a"), false).await.unwrap();
    let denied = |result: rustcloud_core::Result<_>| result.unwrap_err().code;

    let opened = permissions(FileScope::Opened, true);
    s.files.create_grant(&s.alice, new_grant(file.id, GrantKind::Session, true), &opened).await.unwrap();
    let err = s.files.create_grant(&s.alice, new_grant(folder.id, GrantKind::Standing, false), &opened).await;
    assert_eq!(denied(err), "PERMISSION_NOT_DECLARED", "opened files only");

    let folders = permissions(FileScope::Folder, false);
    let err = s.files.create_grant(&s.alice, new_grant(folder.id, GrantKind::Standing, true), &folders).await;
    assert_eq!(denied(err), "PERMISSION_NOT_DECLARED", "no write access declared");
    let err = s.files.create_grant(&s.alice, new_grant(root.id, GrantKind::Standing, false), &folders).await;
    assert_eq!(denied(err), "PERMISSION_NOT_DECLARED", "the root folder is all files");
    let all = permissions(FileScope::All, false);
    s.files.create_grant(&s.alice, new_grant(root.id, GrantKind::Standing, false), &all).await.unwrap();

    let err = s.files.create_grant(&s.bob, new_grant(file.id, GrantKind::Session, false), &opened).await;
    assert_eq!(denied(err), "FILE_NOT_FOUND", "only the owner gives access");
}

#[tokio::test]
async fn closing_or_deleting_ends_access() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "doc.md", body("x"), false).await.unwrap();
    let opened = permissions(FileScope::Opened, true);

    let session = s.files.create_grant(&s.alice, new_grant(file.id, GrantKind::Session, true), &opened).await.unwrap();
    s.files.keep_grant_alive(&s.alice, session.id).await.unwrap();
    assert!(s.files.app_may_access(&s.alice, "notes", file.id, true).await.unwrap());
    assert_eq!(s.files.grants(&s.alice).await.unwrap().len(), 1);
    assert!(s.files.grants(&s.bob).await.unwrap().is_empty(), "users see only their own grants");

    assert_eq!(s.files.revoke_grant(&s.bob, session.id).await.unwrap_err().code, "GRANT_NOT_FOUND");
    s.files.revoke_grant(&s.alice, session.id).await.unwrap();
    assert!(!s.files.app_may_access(&s.alice, "notes", file.id, false).await.unwrap(), "the app was closed");
    assert_eq!(s.files.keep_grant_alive(&s.alice, session.id).await.unwrap_err().code, "GRANT_NOT_FOUND");

    s.files.create_grant(&s.alice, new_grant(file.id, GrantKind::Session, false), &opened).await.unwrap();
    s.files.delete(&s.alice, file.id).await.unwrap();
    assert!(s.files.grants(&s.alice).await.unwrap().is_empty(), "the file is in the trash, the access ended");
    assert!(!s.files.app_may_access(&s.alice, "notes", file.id, false).await.unwrap());
}

#[tokio::test]
async fn apps_keep_their_data_in_a_hidden_folder_of_each_user() {
    let s = setup().await;
    s.files.appdata_write(&s.alice, "notes", "templates/daily.md", b"# Day".to_vec()).await.unwrap();
    s.files.appdata_write(&s.alice, "notes", "theme", b"dark".to_vec()).await.unwrap();

    assert_eq!(s.files.appdata_read(&s.alice, "notes", "templates/daily.md", LIMIT).await.unwrap(), b"# Day");
    let names: Vec<String> =
        s.files.appdata_list(&s.alice, "notes", "").await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(names, ["templates", "theme"]);

    s.files.appdata_write(&s.alice, "notes", "theme", b"light".to_vec()).await.unwrap();
    assert_eq!(s.files.appdata_read(&s.alice, "notes", "theme", LIMIT).await.unwrap(), b"light", "replaced");

    // Ordinary files of the user: visible as .appdata, counted in the quota.
    let root = s.files.root(&s.alice).await.unwrap();
    let top: Vec<String> = s.files.children(&s.alice, root.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(top, [".appdata"]);
    assert_eq!(s.files.usage(&s.alice).await.unwrap().used_bytes, 5 + 5);

    let missing = |result: rustcloud_core::Result<Vec<u8>>| result.unwrap_err().code;
    assert_eq!(
        missing(s.files.appdata_read(&s.alice, "calendar", "theme", LIMIT).await),
        "FILE_NOT_FOUND",
        "other app"
    );
    assert_eq!(missing(s.files.appdata_read(&s.bob, "notes", "theme", LIMIT).await), "FILE_NOT_FOUND", "other user");
    assert_eq!(missing(s.files.appdata_read(&s.alice, "notes", "../notes/theme", LIMIT).await), "INVALID_NAME");
    assert!(s.files.appdata_list(&s.bob, "notes", "").await.unwrap().is_empty());

    s.files.appdata_delete(&s.alice, "notes", "templates").await.unwrap();
    assert_eq!(missing(s.files.appdata_read(&s.alice, "notes", "templates/daily.md", LIMIT).await), "FILE_NOT_FOUND");
}

#[tokio::test]
async fn app_data_respects_the_quota() {
    let s = setup().await;
    let auth = Auth::new(s.db.conn.clone(), Arc::default());
    let update = rustcloud_auth::AccountUpdate { quota_bytes: Some(Some(4)), ..Default::default() };
    let admin = User { is_admin: true, ..s.bob.clone() };
    let alice = auth.update_account(&admin, s.alice.id, update).await.unwrap();

    let err = s.files.appdata_write(&alice, "notes", "big", b"too big".to_vec()).await.unwrap_err();
    assert_eq!(err.code, "QUOTA_EXCEEDED");
}

#[tokio::test]
async fn apps_work_only_inside_what_they_were_given() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let notes = s.files.create_folder(&s.alice, root.id, "Notes").await.unwrap();
    let todo = s.files.upload(&s.alice, notes.id, "todo.md", body("- milk"), false).await.unwrap();
    let secret = s.files.upload(&s.alice, root.id, "secret.txt", body("x"), false).await.unwrap();
    let write = permissions(FileScope::Folder, true);
    s.files.create_grant(&s.alice, new_grant(notes.id, GrantKind::Standing, true), &write).await.unwrap();

    let granted = s.files.app_granted(&s.alice, "notes").await.unwrap();
    assert_eq!(granted.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["Notes"]);
    assert_eq!(s.files.app_read(&s.alice, "notes", todo.id, LIMIT).await.unwrap().0, b"- milk");
    let too_large = s.files.app_read(&s.alice, "notes", todo.id, 3).await.unwrap_err();
    assert_eq!(too_large.code, "TOO_LARGE_TO_READ", "more than the app may take at once");

    let day = s.files.app_create_folder(&s.alice, "notes", notes.id, "2026").await.unwrap();
    let new = s.files.app_create_file(&s.alice, "notes", day.id, "a.md", b"# A".to_vec()).await.unwrap();
    s.files.app_write(&s.alice, "notes", new.id, b"# B".to_vec(), None).await.unwrap();
    assert_eq!(s.files.app_read(&s.alice, "notes", new.id, LIMIT).await.unwrap().0, b"# B");
    let names: Vec<String> =
        s.files.app_children(&s.alice, "notes", notes.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(names, ["2026", "todo.md"]);
    s.files.app_delete(&s.alice, "notes", day.id).await.unwrap();
    assert!(s.files.get(&s.alice, day.id).await.unwrap().trashed_at.is_some(), "to the user's trash");
    assert_eq!(code_of(s.files.app_get(&s.alice, "notes", day.id).await), "FILE_NOT_FOUND", "out of the app's reach");

    let code = |err: rustcloud_core::Error| err.code;
    assert_eq!(code(s.files.app_read(&s.alice, "notes", secret.id, LIMIT).await.unwrap_err()), "FILE_NOT_FOUND");
    assert_eq!(code(s.files.app_get(&s.alice, "notes", root.id).await.unwrap_err()), "FILE_NOT_FOUND");
    assert_eq!(code(s.files.app_read(&s.alice, "calendar", todo.id, LIMIT).await.unwrap_err()), "FILE_NOT_FOUND");
    assert_eq!(code(s.files.app_read(&s.bob, "notes", todo.id, LIMIT).await.unwrap_err()), "FILE_NOT_FOUND");
    assert_eq!(code(s.files.app_delete(&s.alice, "notes", notes.id).await.unwrap_err()), "READ_ONLY");
}

#[tokio::test]
async fn read_only_access_reads_but_never_changes() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let photos = s.files.create_folder(&s.alice, root.id, "Photos").await.unwrap();
    let photo = s.files.upload(&s.alice, photos.id, "cat.jpg", body("jpg"), false).await.unwrap();
    let read = permissions(FileScope::Folder, false);
    s.files.create_grant(&s.alice, new_grant(photos.id, GrantKind::Standing, false), &read).await.unwrap();

    assert_eq!(s.files.app_read(&s.alice, "notes", photo.id, LIMIT).await.unwrap().0, b"jpg");
    let code = |err: rustcloud_core::Error| err.code;
    assert_eq!(
        code(s.files.app_write(&s.alice, "notes", photo.id, b"x".to_vec(), None).await.unwrap_err()),
        "READ_ONLY"
    );
    assert_eq!(code(s.files.app_delete(&s.alice, "notes", photo.id).await.unwrap_err()), "READ_ONLY");
    let err = s.files.app_create_file(&s.alice, "notes", photos.id, "x", Vec::new()).await.unwrap_err();
    assert_eq!(code(err), "READ_ONLY");
}

#[tokio::test]
async fn while_an_app_holds_a_lock_only_it_changes_the_file() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let doc = s.files.upload(&s.alice, root.id, "doc.odt", body("v1"), false).await.unwrap();
    let edit = permissions(FileScope::Opened, true);
    for app in ["office", "notes"] {
        let grant = NewGrant { app_id: app.into(), node_id: doc.id, write: true, kind: GrantKind::Session };
        s.files.create_grant(&s.alice, grant, &edit).await.unwrap();
    }

    let Locking::Held(lock) = s.files.app_lock(&s.alice, "office", doc.id, "wopi-1", 1800).await.unwrap() else {
        panic!("the file was free");
    };
    assert_eq!((lock.lock_id.as_str(), lock.app_id.as_str(), lock.user_id), ("wopi-1", "office", s.alice.id));
    let renewed = s.files.app_lock(&s.alice, "office", doc.id, "wopi-1", 7200).await.unwrap();
    assert!(matches!(renewed, Locking::Held(l) if l.expires_at <= chrono::Utc::now() + chrono::Duration::hours(1)));
    assert!(matches!(
        s.files.app_lock(&s.alice, "office", doc.id, "wopi-2", 60).await.unwrap(),
        Locking::Taken(l) if l.lock_id == "wopi-1"
    ));
    assert!(matches!(s.files.app_lock(&s.alice, "notes", doc.id, "x", 60).await.unwrap(), Locking::Taken(_)));

    // Others can't change or delete it; the app that holds the lock can.
    let locked = |result: rustcloud_core::Result<_>| code_of(result);
    assert_eq!(locked(s.files.replace_content(&s.alice, doc.id, body("panel"), None).await), "FILE_LOCKED");
    assert_eq!(locked(s.files.upload(&s.alice, root.id, "doc.odt", body("x"), true).await), "FILE_LOCKED");
    assert_eq!(locked(s.files.app_write(&s.alice, "notes", doc.id, b"x".to_vec(), None).await), "FILE_LOCKED");
    assert_eq!(code_of(s.files.delete(&s.alice, doc.id).await), "FILE_LOCKED");
    s.files.app_write(&s.alice, "office", doc.id, b"v2".to_vec(), None).await.unwrap();
    let renamed = NodeUpdate { name: Some("report.odt".into()), parent_id: None };
    s.files.update(&s.alice, doc.id, renamed).await.unwrap();

    // The panel sees it.
    let listed = s.files.children(&s.alice, root.id).await.unwrap();
    assert_eq!(listed[0].lock.as_ref().map(|l| l.app_id.as_str()), Some("office"));
    assert_eq!(
        s.files.app_get_lock(&s.alice, "notes", doc.id).await.unwrap().map(|l| l.lock_id),
        Some("wopi-1".into())
    );

    // Only the right lock id ends it; the owner may break it.
    assert!(s.files.app_unlock(&s.alice, "office", doc.id, "wopi-2").await.unwrap().is_some());
    assert!(s.files.app_unlock(&s.alice, "office", doc.id, "wopi-1").await.unwrap().is_none());
    assert!(s.files.children(&s.alice, root.id).await.unwrap()[0].lock.is_none());
    s.files.app_lock(&s.alice, "office", doc.id, "wopi-3", 60).await.unwrap();
    s.files.unlock(&s.alice, doc.id).await.unwrap();
    s.files.replace_content(&s.alice, doc.id, body("panel"), None).await.unwrap();
    assert_eq!(code_of(s.files.unlock(&s.bob, doc.id).await), "FILE_NOT_FOUND");
}

#[tokio::test]
async fn a_conditional_write_fails_once_the_file_changed() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("v1"), false).await.unwrap();
    let etag = doc.etag.clone().unwrap();
    let changed = s.files.replace_content(&s.alice, doc.id, body("v2"), Some(&etag)).await.unwrap();
    assert_eq!(code_of(s.files.replace_content(&s.alice, doc.id, body("v3"), Some(&etag)).await), "FILE_CHANGED");
    s.files.replace_content(&s.alice, doc.id, body("v3"), changed.etag.as_deref()).await.unwrap();

    let edit = permissions(FileScope::Opened, true);
    let grant = NewGrant { app_id: "office".into(), node_id: doc.id, write: true, kind: GrantKind::Session };
    s.files.create_grant(&s.alice, grant, &edit).await.unwrap();
    let err = s.files.app_write(&s.alice, "office", doc.id, b"x".to_vec(), Some(&etag)).await;
    assert_eq!(code_of(err), "FILE_CHANGED");
    let stream = s.files.app_replace(&s.alice, "office", doc.id, body("v4"), None).await.unwrap();
    assert_eq!(read(s.files.download(&s.alice, stream.id, None).await.unwrap()).await, "v4");
}

/// A body that sends `text` only once `go` is told to.
fn held_body(text: &'static str, go: tokio::sync::oneshot::Receiver<()>) -> ByteStream {
    Box::pin(stream::once(async move {
        go.await.ok();
        Ok(Bytes::from_static(text.as_bytes()))
    }))
}

#[tokio::test]
async fn a_write_that_was_overtaken_does_not_lose_or_share_content() {
    let s = Arc::new(setup().await);
    let root = s.files.root(&s.alice).await.unwrap();
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("v1"), false).await.unwrap();
    let etag = doc.etag.clone().unwrap();

    // A conditional write that checked the ETag, then waits for its body
    // while another write gets in first: it fails at the very write.
    let (go, wait) = tokio::sync::oneshot::channel();
    let slow = tokio::spawn({
        let s = s.clone();
        let etag = etag.clone();
        async move { s.files.replace_content(&s.alice, doc.id, held_body("slow", wait), Some(&etag)).await }
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    s.files.replace_content(&s.alice, doc.id, body("fast"), None).await.unwrap();
    go.send(()).unwrap();
    assert_eq!(code_of(slow.await.unwrap()), "FILE_CHANGED");
    assert_eq!(read(s.files.download(&s.alice, doc.id, None).await.unwrap()).await, "fast");

    // Without a condition the later one wins, and the content it replaced
    // is the one written in between, not the one it read.
    let (go, wait) = tokio::sync::oneshot::channel();
    let slow = tokio::spawn({
        let s = s.clone();
        async move { s.files.replace_content(&s.alice, doc.id, held_body("slow", wait), None).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    s.files.replace_content(&s.alice, doc.id, body("fast 2"), None).await.unwrap();
    go.send(()).unwrap();
    slow.await.unwrap().unwrap();
    assert_eq!(read(s.files.download(&s.alice, doc.id, None).await.unwrap()).await, "slow");
    for version in s.files.versions(&s.alice, doc.id).await.unwrap().versions {
        // Every version still has its content.
        s.files.download_version(&s.alice, doc.id, version.id, None).await.unwrap();
    }
}

#[tokio::test]
async fn apps_read_the_versions_of_the_files_they_were_given() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("old text"), false).await.unwrap();
    save_later(&s, doc.id, "new").await;
    assert_eq!(code_of(s.files.app_versions(&s.alice, "office", doc.id).await), "FILE_NOT_FOUND", "no access yet");

    let read_only = permissions(FileScope::Opened, false);
    let grant = NewGrant { app_id: "office".into(), node_id: doc.id, write: false, kind: GrantKind::Session };
    s.files.create_grant(&s.alice, grant, &read_only).await.unwrap();
    let versions = s.files.app_versions(&s.alice, "office", doc.id).await.unwrap();
    assert_eq!(versions.len(), 1);
    let part = s.files.app_read_version(&s.alice, "office", doc.id, versions[0].id, 4, 100).await.unwrap();
    assert_eq!(part, b"text");
}
