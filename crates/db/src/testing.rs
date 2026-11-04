//! A fresh, migrated database for every test.
//!
//! By default tests use a SQLite file in a temporary directory. When the
//! environment variable `RUSTCLOUD_TEST_POSTGRES` is set to a PostgreSQL URL
//! (`postgres://user@host/postgres`), every test gets its own new PostgreSQL
//! database on that server instead. CI runs the suite both ways.

use sea_orm::{ConnectionTrait, DatabaseConnection};
use tempfile::TempDir;

/// A test database. Keep it alive for the whole test: dropping it removes the
/// SQLite file.
pub struct TestDb {
    pub conn: DatabaseConnection,
    _dir: Option<TempDir>,
}

/// Creates an empty database with the current schema.
pub async fn test_db() -> TestDb {
    match std::env::var("RUSTCLOUD_TEST_POSTGRES") {
        Ok(server_url) if !server_url.is_empty() => postgres_db(&server_url).await,
        _ => sqlite_db().await,
    }
}

async fn sqlite_db() -> TestDb {
    let dir = tempfile::tempdir().expect("create temp dir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let conn = crate::connect(&url).await.expect("open SQLite test database");
    TestDb { conn, _dir: Some(dir) }
}

async fn postgres_db(server_url: &str) -> TestDb {
    let name = format!("rc_test_{}", uuid::Uuid::new_v4().simple());
    let admin = sea_orm::Database::connect(server_url).await.expect("connect to PostgreSQL");
    admin.execute_unprepared(&format!("CREATE DATABASE {name}")).await.expect("create test database");

    let (server, _default_db) = server_url.rsplit_once('/').expect("URL ends with /<database>");
    let conn = crate::connect(&format!("{server}/{name}")).await.expect("open PostgreSQL test database");
    TestDb { conn, _dir: None }
}
