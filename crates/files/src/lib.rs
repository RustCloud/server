//! Files module: every user's file tree, uploads and downloads, quotas and
//! public links.
//!
//! - Metadata (names, folders, owners) lives in the database (`nodes` table).
//! - Content lives in a [`BlobStore`](rustcloud_core::BlobStore) chosen at startup.
//! - All access decisions go through one function, `Files::check_access`, so
//!   sharing with users and groups can be added in one place later.
//!
//! HTTP API (all under `/api/v1`):
//!
//! | method | path                       | what it does                               |
//! |--------|----------------------------|--------------------------------------------|
//! | GET    | `/files/root`              | the user's root folder                     |
//! | GET    | `/files/{id}`              | a node and the folders above it            |
//! | GET    | `/files/{id}/children`     | folder contents                            |
//! | POST   | `/files/{id}/folders`      | create a subfolder `{name}`                |
//! | POST   | `/files/{id}/upload?name=` | upload a file (raw body), `&overwrite=true`|
//! | POST   | `/files/{id}/uploads`      | start an upload in parts `{name, size}`    |
//! | GET    | `/files/uploads`           | the user's uploads in parts under way      |
//! | GET/DELETE | `/files/uploads/{u}`   | an upload in parts / cancel it             |
//! | PUT    | `/files/uploads/{u}?offset=` | the next part (raw body)                 |
//! | POST   | `/files/uploads/{u}/finish`| make the parts the file `{sha256}`         |
//! | GET    | `/files/{id}/content`      | download (supports `Range`, `If-None-Match`)|
//! | PUT    | `/files/{id}/content`      | replace file content (raw body)            |
//! | PATCH  | `/files/{id}`              | rename / move `{name?, parent_id?}`        |
//! | POST   | `/files/{id}/copy`         | copy `{parent_id, name?}`                  |
//! | DELETE | `/files/{id}`              | to the trash; in the trash, for good       |
//! | POST   | `/files/{id}/restore`      | restore from the trash                     |
//! | DELETE | `/files/{id}/lock`         | break an app's lock on the file            |
//! | GET    | `/files/{id}/versions`     | earlier versions and how long they stay    |
//! | GET    | `/files/{id}/versions/{v}/content` | download a version                 |
//! | POST   | `/files/{id}/versions/{v}/restore` | make a version current             |
//! | GET    | `/files/trash`             | the user's trash folder (`.trash`)         |
//! | DELETE | `/files/trash`             | empty the trash                            |
//! | GET    | `/files/search?q=`         | search by name                             |
//! | GET    | `/files/changes?cursor=`   | change feed of sync clients                |
//! | GET    | `/files/changes/watch`     | SSE: a hint whenever the feed has new ones |
//! | GET    | `/usage`                   | used bytes and quota                       |
//! | GET/POST | `/shares`                | list / create public links                 |
//! | DELETE | `/shares/{id}`             | delete a public link                       |
//! | GET    | `/public/{token}`          | public link: metadata and folder contents  |
//! | GET    | `/public/{token}/content`  | public link: download                      |
//! | GET    | `/admin/storage`           | storage size, use and speed (admins)       |
//! | POST   | `/admin/storage/speed-test`| run a storage speed test (admins)          |

mod app;
mod app_files;
mod app_shares;
mod appdata;
mod changes;
pub mod content;
mod events;
mod grants;
mod http;
mod locks;
mod service;
mod settings;
mod shares;
mod storage;
mod trash;
mod uploads;
mod versions;

pub use app::{manifest, FILES_INTERFACE_VERSION};
pub use app_shares::{AppShare, AppShares, NewAppShare, Person};
pub use appdata::APPDATA_FOLDER;
pub use changes::{
    Change, ChangeKind, ChangePage, ChangeWatch, CHANGES_AT_ONCE, HINT_EVERY, KEEP_DAYS, MAX_WATCHERS, WATCH_TIME,
};
pub use events::{FileEvent, FileEventKind, WatchedFile};
pub use grants::{Grant, GrantKind, NewGrant, CLOSE_GRACE, KEEP_ALIVE_EVERY, SESSION_IDLE, SESSION_MAX};
pub use http::router;
pub use locks::{FileLock, Locking, LOCK_MAX};
pub use service::{guess_mime, Download, Files, Node, NodeUpdate, Usage};
pub use settings::FilesSettings;
pub use shares::{PublicView, Share};
pub use storage::{SpeedTestResult, StorageReport, NEARLY_FULL_PERCENT, SPEED_TEST_BYTES};
pub use trash::TRASH_FOLDER;
pub use uploads::{NewUpload, Upload, ABANDONED_AFTER, MAX_PARTS, MAX_UPLOADS};
pub use versions::{Version, Versions, SAVES_TOGETHER};
