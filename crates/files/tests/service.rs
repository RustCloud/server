//! Behaviour of the files service against a real database and a real blob store.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use rustcloud_auth::{Auth, NewAccount, User};
use rustcloud_core::{ByteRange, ByteStream, Live};
use rustcloud_db::testing::{test_db, TestDb};
use rustcloud_db::{file_change, file_version, node, share, upload_session};
use rustcloud_files::{
    ChangeKind, Download, FileEventKind, Files, FilesSettings, GrantKind, Locking, NewAccess, NewGrant, NewUpload,
    NodeUpdate, ShareOptions,
};
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
    s.files.upload(&s.alice, root.id, "b.txt", body("b"), false, None).await.unwrap();
    s.files.create_folder(&s.alice, root.id, "Zebra").await.unwrap();
    s.files.upload(&s.alice, root.id, "A.txt", body("a"), false, None).await.unwrap();
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
    let err = s.files.upload(&s.alice, root.id, "Docs", body("x"), true, None).await.unwrap_err();
    assert_eq!(err.code, "NAME_TAKEN", "a folder is never overwritten by a file");
}

#[tokio::test]
async fn uploaded_content_can_be_downloaded() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "hello.txt", body("hello world"), false, None).await.unwrap();

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
    let first = s.files.upload(&s.alice, root.id, "notes.txt", body("v1"), false, None).await.unwrap();

    let err = s.files.upload(&s.alice, root.id, "notes.txt", body("v2"), false, None).await.unwrap_err();
    assert_eq!(err.code, "NAME_TAKEN");

    let second = s.files.upload(&s.alice, root.id, "notes.txt", body("v2!"), true, None).await.unwrap();
    assert_eq!(second.id, first.id, "the same file is updated");
    assert_ne!(second.etag, first.etag);
    assert_eq!(read(s.files.download(&s.alice, first.id, None).await.unwrap()).await, "v2!");
    assert_eq!(blob_count(s.blob_dir.path()), 1, "the old content is removed");
}

#[tokio::test]
async fn other_users_cannot_see_or_touch_files() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "secret.txt", body("secret"), false, None).await.unwrap();

    assert_eq!(s.files.get(&s.bob, file.id).await.unwrap_err().code, "FILE_NOT_FOUND");
    assert!(s.files.download(&s.bob, file.id, None).await.is_err());
    assert!(s.files.children(&s.bob, root.id).await.is_err());
    assert!(s.files.delete(&s.bob, file.id).await.is_err());
    assert!(s.files.upload(&s.bob, root.id, "x.txt", body("x"), false, None).await.is_err());
    let bob_root = s.files.root(&s.bob).await.unwrap();
    assert!(s.files.copy(&s.bob, file.id, bob_root.id, None).await.is_err());
}

#[tokio::test]
async fn rename_and_move() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false, None).await.unwrap();

    let update = NodeUpdate { name: Some("b.txt".into()), parent_id: Some(docs.id), ..Default::default() };
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
        let update = NodeUpdate { name: None, parent_id: Some(target), ..Default::default() };
        let err = s.files.update(&s.alice, a.id, update).await.unwrap_err();
        assert_eq!(err.code, "MOVE_INTO_ITSELF");
    }
}

#[tokio::test]
async fn two_folders_moved_into_each_other_at_once_make_no_loop() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    for round in 0..10 {
        let a = s.files.create_folder(&s.alice, root.id, &format!("A{round}")).await.unwrap();
        let b = s.files.create_folder(&s.alice, root.id, &format!("B{round}")).await.unwrap();
        let (into_b, into_a) = tokio::join!(
            s.files.update(&s.alice, a.id, NodeUpdate { name: None, parent_id: Some(b.id), ..Default::default() }),
            s.files.update(&s.alice, b.id, NodeUpdate { name: None, parent_id: Some(a.id), ..Default::default() }),
        );
        assert!(into_b.is_ok() != into_a.is_ok(), "exactly one of the moves wins");
        let lost = into_b.err().or(into_a.err()).unwrap();
        assert_eq!(lost.code, "MOVE_INTO_ITSELF");
    }
}

#[tokio::test]
async fn a_loop_already_in_the_tree_fails_instead_of_hanging() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let a = s.files.create_folder(&s.alice, root.id, "A").await.unwrap();
    let b = s.files.create_folder(&s.alice, a.id, "B").await.unwrap();
    // Made by hand, as a race in an older version could.
    let mut row: node::ActiveModel = node::Entity::find_by_id(a.id).one(&s.db.conn).await.unwrap().unwrap().into();
    row.parent_id = Set(Some(b.id));
    row.update(&s.db.conn).await.unwrap();

    let delete = tokio::time::timeout(std::time::Duration::from_secs(10), s.files.delete(&s.alice, a.id));
    assert!(delete.await.expect("no endless walk").is_err());
    let copy = tokio::time::timeout(std::time::Duration::from_secs(10), s.files.copy(&s.alice, b.id, root.id, None));
    assert!(copy.await.expect("no endless walk").is_err());
}

#[tokio::test]
async fn root_cannot_be_renamed_deleted_or_shared() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let rename = NodeUpdate { name: Some("x".into()), parent_id: None, ..Default::default() };
    assert_eq!(s.files.update(&s.alice, root.id, rename).await.unwrap_err().code, "ROOT_FOLDER");
    assert_eq!(s.files.delete(&s.alice, root.id).await.unwrap_err().code, "ROOT_FOLDER");
    assert_eq!(s.files.create_share(&s.alice, root.id, ShareOptions::default()).await.unwrap_err().code, "ROOT_FOLDER");
}

#[tokio::test]
async fn copying_a_folder_copies_everything_inside() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let sub = s.files.create_folder(&s.alice, docs.id, "Sub").await.unwrap();
    s.files.upload(&s.alice, sub.id, "deep.txt", body("deep"), false, None).await.unwrap();

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
    let file = s.files.upload(&s.alice, sub.id, "a.txt", body("a"), false, None).await.unwrap();
    s.files.upload(&s.alice, docs.id, "b.txt", body("b"), false, None).await.unwrap();

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
    let report = s.files.upload(&s.alice, docs.id, "report.pdf", body("1"), false, None).await.unwrap();
    let share = s.files.create_share(&s.alice, report.id, ShareOptions::default()).await.unwrap();

    s.files.delete(&s.alice, report.id).await.unwrap();
    assert!(s.files.search(&s.alice, "report").await.unwrap().is_empty(), "search leaves the trash out");
    assert!(s.files.public_view(&share.token, None, None).await.is_err(), "links stop working");
    // Another file of the same name deleted: both stay.
    let again = s.files.upload(&s.alice, docs.id, "report.pdf", body("2"), false, None).await.unwrap();
    s.files.delete(&s.alice, again.id).await.unwrap();
    let trash = s.files.trash(&s.alice).await.unwrap();
    let mut names: Vec<String> =
        s.files.children(&s.alice, trash.id).await.unwrap().into_iter().map(|n| n.name).collect();
    names.sort();
    assert_eq!(names, ["report (2).pdf", "report.pdf"]);

    // Nothing gets in but by deleting, and the trash itself stays.
    let put = s.files.upload(&s.alice, trash.id, "x.txt", body("x"), false, None).await.unwrap_err();
    assert_eq!(put.code, "IN_TRASH");
    let moved = s
        .files
        .update(&s.alice, docs.id, NodeUpdate { name: None, parent_id: Some(trash.id), ..Default::default() })
        .await;
    assert_eq!(moved.unwrap_err().code, "IN_TRASH");
    assert_eq!(s.files.delete(&s.alice, trash.id).await.unwrap_err().code, "TRASH_FOLDER");

    // Restored where it was; the name taken there gets a number.
    let restored = s.files.restore(&s.alice, report.id).await.unwrap();
    assert_eq!((restored.parent_id, restored.name.as_str()), (Some(docs.id), "report.pdf"));
    assert!(restored.trashed_at.is_none());
    assert!(s.files.public_view(&share.token, None, None).await.is_ok(), "and its links work again");
    s.files.delete(&s.alice, report.id).await.unwrap();
    s.files.upload(&s.alice, docs.id, "report.pdf", body("3"), false, None).await.unwrap();
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
    let file = s.files.upload(&s.alice, root.id, "old.txt", body("x"), false, None).await.unwrap();
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

    let file = s.files.upload(&s.alice, root.id, "a.txt", body("123456"), false, None).await.unwrap();
    let err = s.files.upload(&s.alice, root.id, "b.txt", body("123456"), false, None).await.unwrap_err();
    assert_eq!(err.code, "QUOTA_EXCEEDED");
    assert_eq!(s.files.children(&s.alice, root.id).await.unwrap().len(), 1, "nothing half-uploaded is kept");
    assert_eq!(blob_count(s.blob_dir.path()), 1);

    // Replacing a file only needs room for the difference.
    s.files.replace_content(&s.alice, file.id, body("1234567890"), None, None).await.unwrap();

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

    s.files.upload(&s.alice, root.id, "big.bin", mib(1024 * 1024 + 1), false, None).await.unwrap();

    s.settings.set(FilesSettings { max_upload_mib: Some(1), ..Default::default() });
    let err = s.files.upload(&s.alice, root.id, "too-big.bin", mib(1024 * 1024 + 1), false, None).await.unwrap_err();
    assert_eq!(err.code, "FILE_TOO_LARGE");
    s.files.upload(&s.alice, root.id, "fits.bin", mib(1024 * 1024), false, None).await.unwrap();
    assert_eq!(blob_count(s.blob_dir.path()), 2, "the rejected upload left nothing behind");
}

#[tokio::test]
async fn search_is_case_insensitive_and_treats_wildcards_literally() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    for name in ["Report 2024.pdf", "report_final.doc", "100% done.txt", "other.txt"] {
        s.files.upload(&s.alice, root.id, name, body("x"), false, None).await.unwrap();
    }
    let bob_root = s.files.root(&s.bob).await.unwrap();
    s.files.upload(&s.bob, bob_root.id, "report-bob.txt", body("x"), false, None).await.unwrap();

    let names = |found: Vec<rustcloud_files::Node>| found.into_iter().map(|n| n.name).collect::<Vec<_>>();
    assert_eq!(names(s.files.search(&s.alice, "REPORT").await.unwrap()), ["Report 2024.pdf", "report_final.doc"]);
    assert_eq!(names(s.files.search(&s.alice, "%").await.unwrap()), ["100% done.txt"]);
    assert_eq!(names(s.files.search(&s.alice, "t_f").await.unwrap()), ["report_final.doc"]);
    assert!(s.files.search(&s.alice, "  ").await.unwrap().is_empty());
    assert!(s.files.search(&s.alice, "re\0port").await.unwrap().is_empty(), "no error, PostgreSQL included");
}

#[tokio::test]
async fn public_link_gives_read_access_to_a_folder_and_nothing_else() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let shared = s.files.create_folder(&s.alice, root.id, "Shared").await.unwrap();
    let sub = s.files.create_folder(&s.alice, shared.id, "Sub").await.unwrap();
    let inside = s.files.upload(&s.alice, sub.id, "in.txt", body("inside"), false, None).await.unwrap();
    let outside = s.files.upload(&s.alice, root.id, "out.txt", body("outside"), false, None).await.unwrap();

    let link = s.files.create_share(&s.alice, shared.id, ShareOptions::default()).await.unwrap();
    assert_eq!(s.files.shares(&s.alice).await.unwrap()[0].node.id, shared.id);

    let top = s.files.public_view(&link.token, None, None).await.unwrap();
    assert_eq!(top.current.id, shared.id);
    assert_eq!(top.children[0].id, sub.id);

    let deeper = s.files.public_view(&link.token, None, Some(sub.id)).await.unwrap();
    assert_eq!(deeper.path.iter().map(|n| n.id).collect::<Vec<_>>(), [shared.id]);
    assert_eq!(read(s.files.public_download(&link.token, None, Some(inside.id), None).await.unwrap()).await, "inside");

    assert!(s.files.public_download(&link.token, None, Some(outside.id), None).await.is_err());
    assert!(s.files.public_view(&link.token, None, Some(root.id)).await.is_err());

    s.files.delete_share(&s.alice, link.id).await.unwrap();
    assert_eq!(s.files.public_view(&link.token, None, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn only_the_owner_makes_public_links_and_only_the_owners_open() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    s.files.upload(&s.alice, docs.id, "secret.txt", body("secret"), false, None).await.unwrap();
    for write in [false, true] {
        let access = NewAccess { username: Some("bob".into()), group_id: None, write };
        s.files.grant_access(&s.alice, docs.id, access).await.unwrap();
        let options = ShareOptions { allow_upload: Some(true), ..Default::default() };
        let refused = s.files.create_share(&s.bob, docs.id, options).await;
        assert_eq!(code_of(refused), "FILE_NOT_FOUND", "bob passes nothing of alice's on (write: {write})");
    }

    // A link someone else made before (older servers allowed it) opens nothing.
    let token = "made-by-bob";
    share::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        token: Set(token.into()),
        node_id: Set(docs.id),
        owner_id: Set(s.bob.id),
        expires_at: Set(None),
        created_at: Set(chrono::Utc::now()),
        password_hash: Set(None),
        allow_upload: Set(true),
    }
    .insert(&s.db.conn)
    .await
    .unwrap();
    assert_eq!(s.files.public_view(token, None, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
    let upload = s.files.public_upload(token, None, None, "x.txt", body("x")).await;
    assert_eq!(code_of(upload), "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn files_tell_whether_someone_else_wrote_them() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let own = s.files.upload(&s.alice, root.id, "own.pdf", body("%PDF"), false, None).await.unwrap();
    assert!(!s.files.from_someone_else(&s.alice, own.id).await.unwrap());

    let shared = s.files.create_folder(&s.alice, root.id, "Shared").await.unwrap();
    let access = NewAccess { username: Some("bob".into()), group_id: None, write: true };
    s.files.grant_access(&s.alice, shared.id, access).await.unwrap();
    let alices = s.files.upload(&s.alice, shared.id, "alices.pdf", body("%PDF"), false, None).await.unwrap();
    assert!(s.files.from_someone_else(&s.bob, alices.id).await.unwrap(), "shared with bob: alice's");
    let bobs = s.files.upload(&s.bob, shared.id, "bobs.pdf", body("%PDF"), false, None).await.unwrap();
    assert!(s.files.from_someone_else(&s.alice, bobs.id).await.unwrap(), "in alice's folder, but bob's");

    let options = ShareOptions { allow_upload: Some(true), ..Default::default() };
    let link = s.files.create_share(&s.alice, shared.id, options).await.unwrap();
    let dropped = s.files.public_upload(&link.token, None, None, "dropped.pdf", body("%PDF")).await.unwrap();
    assert!(s.files.from_someone_else(&s.alice, dropped.id).await.unwrap(), "a visitor's");

    // Visitors don't get the media engine to take apart what they uploaded.
    let refused = s.files.public_open_for_media(&link.token, None, Some(dropped.id)).await;
    assert_eq!(refused.err().map(|err| err.code), Some("NO_THUMBNAIL"));
    assert!(s.files.public_open_for_media(&link.token, None, Some(alices.id)).await.is_ok());
    assert!(s.files.open_for_media(&s.alice, dropped.id).await.is_ok(), "the owner still sees it");
}

#[tokio::test]
async fn links_of_a_disabled_account_stop_working() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false, None).await.unwrap();
    let link = s.files.create_share(&s.alice, file.id, ShareOptions::default()).await.unwrap();
    assert!(s.files.public_view(&link.token, None, None).await.is_ok());

    let mut alice: rustcloud_db::user::ActiveModel =
        rustcloud_db::user::Entity::find_by_id(s.alice.id).one(&s.db.conn).await.unwrap().unwrap().into();
    alice.disabled = Set(true);
    alice.update(&s.db.conn).await.unwrap();
    assert_eq!(s.files.public_view(&link.token, None, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn expired_links_stop_working() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false, None).await.unwrap();

    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    assert_eq!(
        s.files
            .create_share(&s.alice, file.id, ShareOptions { expires_at: Some(Some(past)), ..Default::default() })
            .await
            .unwrap_err()
            .code,
        "INVALID_EXPIRY"
    );

    let tomorrow = chrono::Utc::now() + chrono::Duration::days(1);
    let link = s
        .files
        .create_share(&s.alice, file.id, ShareOptions { expires_at: Some(Some(tomorrow)), ..Default::default() })
        .await
        .unwrap();
    assert!(s.files.public_view(&link.token, None, None).await.is_ok());

    // Let the link expire by moving its expiry into the past.
    let mut row: share::ActiveModel = share::Entity::find_by_id(link.id).one(&s.db.conn).await.unwrap().unwrap().into();
    row.expires_at = Set(Some(past));
    row.update(&s.db.conn).await.unwrap();
    assert_eq!(s.files.public_view(&link.token, None, None).await.unwrap_err().code, "SHARE_NOT_FOUND");
}

#[tokio::test]
async fn only_the_owner_can_delete_a_link() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false, None).await.unwrap();
    let link = s.files.create_share(&s.alice, file.id, ShareOptions::default()).await.unwrap();

    assert_eq!(s.files.delete_share(&s.bob, link.id).await.unwrap_err().code, "SHARE_NOT_FOUND");
    assert!(s.files.shares(&s.bob).await.unwrap().is_empty());
}

#[tokio::test]
async fn storage_report_counts_all_files_and_keeps_the_last_speed_test() {
    let s = setup().await;
    let alice_root = s.files.root(&s.alice).await.unwrap();
    let bob_root = s.files.root(&s.bob).await.unwrap();
    s.files.upload(&s.alice, alice_root.id, "a.txt", body("12345"), false, None).await.unwrap();
    s.files.upload(&s.bob, bob_root.id, "b.txt", body("123"), false, None).await.unwrap();
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
    s.files.replace_content(&s.alice, id, body(text), None, None).await.unwrap();
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
    let file = s.files.upload(&s.alice, root.id, "plan.txt", body("v1"), false, None).await.unwrap();
    s.files.replace_content(&s.alice, file.id, body("v2"), None, None).await.unwrap();
    assert!(version_texts(&s, file.id).await.is_empty(), "saved right after creating it: one change");

    save_later(&s, file.id, "v3").await;
    s.files.replace_content(&s.alice, file.id, body("v4"), None, None).await.unwrap();
    s.files.replace_content(&s.alice, file.id, body("v4"), None, None).await.unwrap();
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
async fn files_keep_the_mtime_their_client_gave() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let at = |text: &str| chrono::DateTime::parse_from_rfc3339(text).unwrap().to_utc();
    let (t1, t2) = (at("2024-01-02T03:04:05.123456Z"), at("2025-06-07T08:09:10Z"));

    let file = s.files.upload(&s.alice, root.id, "a.txt", body("v1"), false, Some(t1)).await.unwrap();
    assert_eq!(file.modified_at, t1);
    let renamed = NodeUpdate { name: Some("b.txt".into()), ..Default::default() };
    assert_eq!(s.files.update(&s.alice, file.id, renamed).await.unwrap().modified_at, t1, "renaming keeps it");
    let copy = s.files.copy(&s.alice, file.id, root.id, Some("c.txt")).await.unwrap();
    assert_eq!(copy.modified_at, t1, "so does copying");

    // Without one, the time of the upload.
    age(&s, file.id, 1).await;
    let v2 = s.files.replace_content(&s.alice, file.id, body("v2"), None, None).await.unwrap();
    assert_eq!(v2.modified_at, v2.updated_at);
    let touched = NodeUpdate { modified_at: Some(t2), ..Default::default() };
    assert_eq!(s.files.update(&s.alice, file.id, touched).await.unwrap().modified_at, t2);

    // A restored version comes back with its time.
    let versions = s.files.versions(&s.alice, file.id).await.unwrap().versions;
    assert_eq!(versions[0].modified_at, t1);
    let restored = s.files.restore_version(&s.alice, file.id, versions[0].id).await.unwrap();
    assert_eq!(restored.modified_at, t1);
    assert_eq!(s.files.versions(&s.alice, file.id).await.unwrap().versions[0].modified_at, t2);

    let future = chrono::Utc::now() + chrono::Duration::days(2);
    let refused = s.files.upload(&s.alice, root.id, "d.txt", body("x"), false, Some(future)).await;
    assert_eq!(code_of(refused), "INVALID_MTIME");
    let before_1970 = at("1969-12-31T23:59:59Z");
    assert_eq!(
        code_of(s.files.replace_content(&s.alice, file.id, body("x"), None, Some(before_1970)).await),
        "INVALID_MTIME"
    );
    let folder = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let touched = NodeUpdate { modified_at: Some(t1), ..Default::default() };
    assert_eq!(code_of(s.files.update(&s.alice, folder.id, touched).await), "NOT_A_FILE");
}

#[tokio::test]
async fn a_version_stays_while_it_is_one_of_the_latest_or_recent_enough() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("1"), false, None).await.unwrap();
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
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("123456"), false, None).await.unwrap();
    save_later(&s, file.id, "12345678").await;
    let usage = s.files.usage(&s.alice).await.unwrap();
    assert_eq!((usage.used_bytes, usage.versions_bytes), (14, 6), "versions count in the quota");

    // 8 + 10 fit into 20 once the version goes.
    s.files.upload(&s.alice, root.id, "b.txt", body("1234567890"), false, None).await.unwrap();
    let usage = s.files.usage(&s.alice).await.unwrap();
    assert_eq!((usage.used_bytes, usage.versions_bytes), (18, 0));
    assert!(version_texts(&s, file.id).await.is_empty());

    // Files themselves still can't go over it.
    let err = s.files.upload(&s.alice, root.id, "c.txt", body("123"), false, None).await.unwrap_err();
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
    let cat = s.files.upload(&s.alice, photos.id, "cat.jpg", body("cat"), false, None).await.unwrap();
    let sea = s.files.upload(&s.alice, trip.id, "sea.jpg", body("sea"), false, None).await.unwrap();
    s.files.upload(&s.alice, hidden.id, "cat.webp", body("small"), false, None).await.unwrap();
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
    let file = s.files.upload(&s.alice, root.id, "cat.jpg", body("cat"), false, None).await.unwrap();
    let mut events = s.files.subscribe();
    let rename = NodeUpdate { name: Some("kitten.jpg".into()), parent_id: None, ..Default::default() };
    s.files.update(&s.alice, file.id, rename).await.unwrap();
    assert!(events.try_recv().is_err(), "no app watches it");

    let folders = permissions(FileScope::Folder, false);
    s.files.create_grant(&s.alice, new_grant(photos.id, GrantKind::Standing, false), &folders).await.unwrap();
    let into = NodeUpdate { name: None, parent_id: Some(photos.id), ..Default::default() };
    s.files.update(&s.alice, file.id, into).await.unwrap();
    let event = events.try_recv().unwrap();
    assert_eq!(event.kind, FileEventKind::Moved);
    assert_eq!(event.files.len(), 1);
    assert_eq!(event.files[0].node.parent_id, Some(photos.id), "as it is now");
    assert_eq!(event.files[0].apps, ["notes"]);

    let out = NodeUpdate { name: None, parent_id: Some(root.id), ..Default::default() };
    s.files.update(&s.alice, file.id, out).await.unwrap();
    let event = events.try_recv().unwrap();
    assert_eq!((event.files[0].node.parent_id, event.files[0].apps.clone()), (Some(root.id), vec!["notes".into()]));
}

#[tokio::test]
async fn an_app_given_all_files_still_cannot_reach_other_apps_data() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let calendar = s.files.appdata_write(&s.alice, "calendar", "events.json", b"{}".to_vec()).await.unwrap();
    let notes = s.files.appdata_write(&s.alice, "notes", "theme", b"light".to_vec()).await.unwrap();
    let file = s.files.upload(&s.alice, root.id, "a.txt", body("a"), false, None).await.unwrap();

    let all = permissions(FileScope::All, true);
    s.files.create_grant(&s.alice, new_grant(root.id, GrantKind::Standing, true), &all).await.unwrap();
    assert!(s.files.app_may_access(&s.alice, "notes", file.id, true).await.unwrap());
    assert!(!s.files.app_may_access(&s.alice, "notes", calendar.id, false).await.unwrap(), "another app's data");
    assert!(!s.files.app_may_access(&s.alice, "notes", notes.id, false).await.unwrap(), "its own only through AppData");
    let names: Vec<String> =
        s.files.app_children(&s.alice, "notes", root.id).await.unwrap().into_iter().map(|n| n.name).collect();
    assert_eq!(names, ["a.txt"], "no .appdata and no trash in the listing");
    assert_eq!(s.files.appdata_read(&s.alice, "notes", "theme", 100).await.unwrap(), b"light");
}

#[tokio::test]
async fn a_folder_grant_covers_what_is_inside_and_nothing_else() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let notes = s.files.create_folder(&s.alice, root.id, "Notes").await.unwrap();
    let inner = s.files.create_folder(&s.alice, notes.id, "Work").await.unwrap();
    let note = s.files.upload(&s.alice, inner.id, "todo.md", body("- [ ] x"), false, None).await.unwrap();
    let other = s.files.upload(&s.alice, root.id, "secret.txt", body("x"), false, None).await.unwrap();

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
    let file = s.files.upload(&s.alice, folder.id, "a.md", body("a"), false, None).await.unwrap();
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
    let file = s.files.upload(&s.alice, root.id, "doc.md", body("x"), false, None).await.unwrap();
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
    let todo = s.files.upload(&s.alice, notes.id, "todo.md", body("- milk"), false, None).await.unwrap();
    let secret = s.files.upload(&s.alice, root.id, "secret.txt", body("x"), false, None).await.unwrap();
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
    let photo = s.files.upload(&s.alice, photos.id, "cat.jpg", body("jpg"), false, None).await.unwrap();
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
    let doc = s.files.upload(&s.alice, root.id, "doc.odt", body("v1"), false, None).await.unwrap();
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
    assert_eq!(locked(s.files.replace_content(&s.alice, doc.id, body("panel"), None, None).await), "FILE_LOCKED");
    assert_eq!(locked(s.files.upload(&s.alice, root.id, "doc.odt", body("x"), true, None).await), "FILE_LOCKED");
    assert_eq!(locked(s.files.app_write(&s.alice, "notes", doc.id, b"x".to_vec(), None).await), "FILE_LOCKED");
    assert_eq!(code_of(s.files.delete(&s.alice, doc.id).await), "FILE_LOCKED");
    s.files.app_write(&s.alice, "office", doc.id, b"v2".to_vec(), None).await.unwrap();
    let renamed = NodeUpdate { name: Some("report.odt".into()), parent_id: None, ..Default::default() };
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
    s.files.replace_content(&s.alice, doc.id, body("panel"), None, None).await.unwrap();
    assert_eq!(code_of(s.files.unlock(&s.bob, doc.id).await), "FILE_NOT_FOUND");
}

#[tokio::test]
async fn a_conditional_write_fails_once_the_file_changed() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("v1"), false, None).await.unwrap();
    let etag = doc.etag.clone().unwrap();
    let changed = s.files.replace_content(&s.alice, doc.id, body("v2"), Some(&etag), None).await.unwrap();
    assert_eq!(code_of(s.files.replace_content(&s.alice, doc.id, body("v3"), Some(&etag), None).await), "FILE_CHANGED");
    s.files.replace_content(&s.alice, doc.id, body("v3"), changed.etag.as_deref(), None).await.unwrap();

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
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("v1"), false, None).await.unwrap();
    let etag = doc.etag.clone().unwrap();

    // A conditional write that checked the ETag, then waits for its body
    // while another write gets in first: it fails at the very write.
    let (go, wait) = tokio::sync::oneshot::channel();
    let slow = tokio::spawn({
        let s = s.clone();
        let etag = etag.clone();
        async move { s.files.replace_content(&s.alice, doc.id, held_body("slow", wait), Some(&etag), None).await }
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    s.files.replace_content(&s.alice, doc.id, body("fast"), None, None).await.unwrap();
    go.send(()).unwrap();
    assert_eq!(code_of(slow.await.unwrap()), "FILE_CHANGED");
    assert_eq!(read(s.files.download(&s.alice, doc.id, None).await.unwrap()).await, "fast");

    // Without a condition the later one wins, and the content it replaced
    // is the one written in between, not the one it read.
    let (go, wait) = tokio::sync::oneshot::channel();
    let slow = tokio::spawn({
        let s = s.clone();
        async move { s.files.replace_content(&s.alice, doc.id, held_body("slow", wait), None, None).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    s.files.replace_content(&s.alice, doc.id, body("fast 2"), None, None).await.unwrap();
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
    let doc = s.files.upload(&s.alice, root.id, "doc.md", body("old text"), false, None).await.unwrap();
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

/// The change feed of alice after `cursor`: the entries and the next cursor.
async fn feed(s: &Setup, cursor: &str) -> (Vec<(uuid::Uuid, ChangeKind)>, String) {
    let page = s.files.changes(&s.alice, Some(cursor)).await.unwrap();
    assert!(!page.more);
    (page.changes.into_iter().map(|change| (change.id, change.kind)).collect(), page.cursor)
}

#[tokio::test]
async fn sync_clients_follow_the_change_feed() {
    use ChangeKind::{Changed, Created, Deleted, Moved};
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let start = s.files.changes(&s.alice, None).await.unwrap();
    assert!(start.changes.is_empty(), "without a cursor: only where to start");
    let mut watch = s.files.watch_changes(&s.alice).unwrap();

    let docs = s.files.create_folder(&s.alice, root.id, "Docs").await.unwrap();
    let file = s.files.upload(&s.alice, docs.id, "a.txt", body("v1"), false, None).await.unwrap();
    s.files.replace_content(&s.alice, file.id, body("v2"), None, None).await.unwrap();
    let rename = NodeUpdate { name: Some("b.txt".into()), ..Default::default() };
    s.files.update(&s.alice, file.id, rename).await.unwrap();
    let touch = NodeUpdate { modified_at: Some(chrono::Utc::now()), ..Default::default() };
    s.files.update(&s.alice, file.id, touch).await.unwrap();
    let copy = s.files.copy(&s.alice, docs.id, root.id, Some("Docs 2")).await.unwrap();
    s.files.delete(&s.alice, file.id).await.unwrap();
    s.files.restore(&s.alice, file.id).await.unwrap();

    // Hidden folders and what is in them are not in the feed: moving in is
    // deleting, moving out is creating.
    let hidden = s.files.create_folder(&s.alice, root.id, ".hidden").await.unwrap();
    s.files.appdata_write(&s.alice, "notes", "list.json", b"[]".to_vec()).await.unwrap();
    let hide = NodeUpdate { parent_id: Some(hidden.id), ..Default::default() };
    s.files.update(&s.alice, file.id, hide).await.unwrap();
    s.files.upload(&s.alice, hidden.id, "secret.txt", body("x"), false, None).await.unwrap();
    let show = NodeUpdate { parent_id: Some(docs.id), ..Default::default() };
    s.files.update(&s.alice, file.id, show).await.unwrap();
    // Deleting for good what is in the trash already was deleted for clients.
    s.files.delete(&s.alice, copy.id).await.unwrap();
    s.files.delete(&s.alice, copy.id).await.unwrap();
    // Bob's files are in his feed only.
    let bobs = s.files.root(&s.bob).await.unwrap();
    s.files.create_folder(&s.bob, bobs.id, "Bob's").await.unwrap();

    let (changes, cursor) = feed(&s, &start.cursor).await;
    let expected = [
        (docs.id, Created),
        (file.id, Created),
        (file.id, Changed),
        (file.id, Moved),
        (file.id, Changed),
        (copy.id, Created),
        (file.id, Deleted),
        (file.id, Created),
        (file.id, Deleted),
        (file.id, Created),
        (copy.id, Deleted),
    ];
    assert_eq!(changes, expected);
    assert_eq!(feed(&s, &cursor).await, (Vec::new(), cursor.clone()), "nothing new");
    assert_eq!(watch.hints.try_recv().unwrap(), s.alice.id, "open connections hear of it");

    assert_eq!(code_of(s.files.changes(&s.alice, Some("soon")).await), "INVALID_CURSOR");
    let ahead = (cursor.parse::<i64>().unwrap() + 100).to_string();
    assert_eq!(code_of(s.files.changes(&s.alice, Some(&ahead)).await), "CURSOR_EXPIRED");

    // Old entries go; a cursor from before them can't be followed any more.
    let long_ago = chrono::Utc::now() - chrono::Duration::days(rustcloud_files::KEEP_DAYS + 1);
    file_change::Entity::update_many()
        .col_expr(file_change::Column::Time, sea_orm::sea_query::Expr::value(long_ago))
        .exec(&s.db.conn)
        .await
        .unwrap();
    assert!(s.files.purge_changes().await.unwrap() > 0);
    assert_eq!(code_of(s.files.changes(&s.alice, Some(&start.cursor)).await), "CURSOR_EXPIRED");
    assert_eq!(feed(&s, &cursor).await.0, Vec::new(), "the latest entry stays, and with it the latest cursor");
}

#[tokio::test]
async fn a_user_holds_only_a_few_connections_for_changes() {
    let s = setup().await;
    let open: Vec<_> = (0..rustcloud_files::MAX_WATCHERS).map(|_| s.files.watch_changes(&s.alice).unwrap()).collect();
    assert_eq!(code_of(s.files.watch_changes(&s.alice)), "TOO_MANY_WATCHERS");
    assert!(s.files.watch_changes(&s.bob).is_ok(), "others are not counted against her");
    drop(open);
    assert!(s.files.watch_changes(&s.alice).is_ok());
}

fn sha256_of(text: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(text))
}

fn new_upload(name: &str, size: i64) -> NewUpload {
    NewUpload { name: name.into(), size, overwrite: false, mtime: None }
}

#[tokio::test]
async fn large_files_come_in_parts() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let at = chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z").unwrap().to_utc();
    let upload = NewUpload { mtime: Some(at), ..new_upload("big.txt", 11) };
    let session = s.files.start_upload(&s.alice, root.id, upload).await.unwrap();
    assert_eq!(session.received, 0);

    let session = s.files.upload_part(&s.alice, session.id, 0, body("hello ")).await.unwrap();
    assert_eq!(session.received, 6);
    // A part sent again, or one from too far, is refused: parts come in order.
    assert_eq!(code_of(s.files.upload_part(&s.alice, session.id, 0, body("hello ")).await), "WRONG_OFFSET");
    assert_eq!(code_of(s.files.upload_part(&s.alice, session.id, 8, body("rld")).await), "WRONG_OFFSET");
    // More than the size given: the part is dropped whole.
    assert_eq!(code_of(s.files.upload_part(&s.alice, session.id, 6, body("world!")).await), "PART_TOO_LARGE");
    assert_eq!(s.files.upload_info(&s.alice, session.id).await.unwrap().received, 6);
    assert_eq!(
        code_of(s.files.finish_upload(&s.alice, session.id, &sha256_of("hello world")).await),
        "UPLOAD_INCOMPLETE"
    );
    s.files.upload_part(&s.alice, session.id, 6, body("world")).await.unwrap();

    // Nobody else sees the session.
    assert_eq!(code_of(s.files.upload_info(&s.bob, session.id).await), "UPLOAD_NOT_FOUND");
    assert_eq!(code_of(s.files.finish_upload(&s.alice, session.id, "abc").await), "INVALID_CHECKSUM");

    let file = s.files.finish_upload(&s.alice, session.id, &sha256_of("hello world")).await.unwrap();
    assert_eq!((file.name.as_str(), file.size, file.modified_at), ("big.txt", 11, at));
    assert_eq!(file.etag, Some(sha256_of("hello world")));
    assert_eq!(read(s.files.download(&s.alice, file.id, None).await.unwrap()).await, "hello world");
    assert_eq!(code_of(s.files.upload_info(&s.alice, session.id).await), "UPLOAD_NOT_FOUND", "the session is done");
    assert_eq!(blob_count(s.blob_dir.path()), 1, "the parts are gone");

    // The name is checked at the start and again at the end.
    assert_eq!(code_of(s.files.start_upload(&s.alice, root.id, new_upload("big.txt", 1)).await), "NAME_TAKEN");
    let late = s.files.start_upload(&s.alice, root.id, new_upload("late.txt", 1)).await.unwrap();
    s.files.upload_part(&s.alice, late.id, 0, body("x")).await.unwrap();
    s.files.upload(&s.alice, root.id, "late.txt", body("y"), false, None).await.unwrap();
    assert_eq!(code_of(s.files.finish_upload(&s.alice, late.id, &sha256_of("x")).await), "NAME_TAKEN");
    assert_eq!(s.files.upload_info(&s.alice, late.id).await.unwrap().received, 1, "the session stays");

    // A wrong checksum ends the session.
    let replace = NewUpload { overwrite: true, ..new_upload("late.txt", 1) };
    let bad = s.files.start_upload(&s.alice, root.id, replace).await.unwrap();
    s.files.upload_part(&s.alice, bad.id, 0, body("z")).await.unwrap();
    assert_eq!(code_of(s.files.finish_upload(&s.alice, bad.id, &sha256_of("x")).await), "CHECKSUM_MISMATCH");
    assert_eq!(code_of(s.files.upload_info(&s.alice, bad.id).await), "UPLOAD_NOT_FOUND");

    s.files.cancel_upload(&s.alice, late.id).await.unwrap();
    assert!(s.files.uploads(&s.alice).await.unwrap().is_empty());
    assert_eq!(blob_count(s.blob_dir.path()), 2, "big.txt and late.txt, no parts");
}

#[tokio::test]
async fn uploads_in_parts_take_their_size_of_the_quota_at_once() {
    let s = setup().await;
    let alice = User { quota_bytes: Some(20), ..s.alice.clone() };
    let root = s.files.root(&alice).await.unwrap();
    assert_eq!(code_of(s.files.start_upload(&alice, root.id, new_upload("a", 21)).await), "QUOTA_EXCEEDED");
    let session = s.files.start_upload(&alice, root.id, new_upload("a", 15)).await.unwrap();
    assert_eq!(code_of(s.files.start_upload(&alice, root.id, new_upload("b", 6)).await), "QUOTA_EXCEEDED");
    let refused = s.files.upload(&alice, root.id, "c", body("123456"), false, None).await;
    assert_eq!(code_of(refused), "QUOTA_EXCEEDED", "other uploads see the space taken");

    s.files.upload_part(&alice, session.id, 0, body("123456789012345")).await.unwrap();
    s.files.finish_upload(&alice, session.id, &sha256_of("123456789012345")).await.unwrap();
    assert_eq!(s.files.usage(&alice).await.unwrap().used_bytes, 15);
}

#[tokio::test]
async fn uploads_in_parts_into_a_shared_folder_fit_the_owners_quota() {
    let s = setup().await;
    let mut alice: rustcloud_db::user::ActiveModel =
        rustcloud_db::user::Entity::find_by_id(s.alice.id).one(&s.db.conn).await.unwrap().unwrap().into();
    alice.quota_bytes = Set(Some(10));
    let alice = User::from(alice.update(&s.db.conn).await.unwrap());
    let root = s.files.root(&alice).await.unwrap();
    let shared = s.files.create_folder(&alice, root.id, "Shared").await.unwrap();
    let access = NewAccess { username: Some("bob".into()), group_id: None, write: true };
    s.files.grant_access(&alice, shared.id, access).await.unwrap();

    let too_large = s.files.start_upload(&s.bob, shared.id, new_upload("big", 11)).await;
    assert_eq!(code_of(too_large), "QUOTA_EXCEEDED", "bob's own quota is unlimited, alice's is not");

    let session = s.files.start_upload(&s.bob, shared.id, new_upload("eight", 8)).await.unwrap();
    s.files.upload_part(&s.bob, session.id, 0, body("12345678")).await.unwrap();
    s.files.upload(&alice, root.id, "mine", body("12345"), false, None).await.unwrap();
    let finished = s.files.finish_upload(&s.bob, session.id, &sha256_of("12345678")).await;
    assert_eq!(code_of(finished), "QUOTA_EXCEEDED", "5 + 8 bytes don't fit into alice's 10");
    assert_eq!(s.files.usage(&alice).await.unwrap().used_bytes, 5);
}

#[tokio::test]
async fn abandoned_uploads_in_parts_go() {
    let s = setup().await;
    let root = s.files.root(&s.alice).await.unwrap();
    let mut sessions = Vec::new();
    for n in 0..rustcloud_files::MAX_UPLOADS {
        sessions.push(s.files.start_upload(&s.alice, root.id, new_upload(&format!("{n}"), 1)).await.unwrap());
    }
    assert_eq!(code_of(s.files.start_upload(&s.alice, root.id, new_upload("one more", 1)).await), "TOO_MANY_UPLOADS");
    s.files.upload_part(&s.alice, sessions[0].id, 0, body("x")).await.unwrap();

    assert_eq!(s.files.purge_uploads().await.unwrap(), 0, "nothing is old yet");
    let long_ago = chrono::Utc::now() - rustcloud_files::ABANDONED_AFTER - chrono::Duration::minutes(1);
    upload_session::Entity::update_many()
        .col_expr(upload_session::Column::UpdatedAt, sea_orm::sea_query::Expr::value(long_ago))
        .filter(upload_session::Column::Id.eq(sessions[0].id))
        .exec(&s.db.conn)
        .await
        .unwrap();
    assert_eq!(s.files.purge_uploads().await.unwrap(), 1);
    assert_eq!(code_of(s.files.upload_info(&s.alice, sessions[0].id).await), "UPLOAD_NOT_FOUND");
    assert_eq!(blob_count(s.blob_dir.path()), 0, "its part went with it");
}

#[tokio::test]
async fn only_a_few_folders_are_zipped_at_once() {
    let s = setup().await;
    let files = Arc::new(s.files);
    let root = files.root(&s.alice).await.unwrap();
    let folder = files.create_folder(&s.alice, root.id, "Big").await.unwrap();
    // Larger than the pipe to the client: an archive nobody reads keeps its place.
    let big: ByteStream = Box::pin(stream::once(async { Ok(Bytes::from(vec![7u8; 1 << 20])) }));
    files.upload(&s.alice, folder.id, "big.bin", big, false, None).await.unwrap();

    let mut unread = Vec::new();
    for _ in 0..rustcloud_files::ZIP_AT_ONCE {
        unread.push(files.download_zip(&s.alice, folder.id).await.unwrap());
    }
    let busy = files.download_zip(&s.alice, folder.id).await;
    assert_eq!(busy.err().map(|err| err.code), Some("SERVER_BUSY"));

    // A client that reads to the end gives its place back.
    let done: Vec<Bytes> = unread.pop().unwrap().body.map(|chunk| chunk.unwrap()).collect().await;
    assert!(done.iter().map(Bytes::len).sum::<usize>() > 1 << 20);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(files.download_zip(&s.alice, folder.id).await.is_ok());
}
