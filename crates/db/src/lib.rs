//! Database schema of RustCloud.
//!
//! The same code works with **PostgreSQL** and **SQLite**; the backend is
//! picked by the connection URL:
//!
//! - `postgres://user:password@host/dbname`
//! - `sqlite://path/to/rustcloud.db?mode=rwc` (`mode=rwc` creates the file)
//!
//! [`connect`] opens the database and applies pending migrations, so the schema
//! is always up to date after startup.
//!
//! Tables:
//!
//! | table        | what it holds                                               |
//! |--------------|-------------------------------------------------------------|
//! | `users`      | accounts: profile, admin flag, quota                        |
//! | `identities` | ways to log in to an account (password, OIDC, later LDAP)  |
//! | `sessions`   | login sessions (only a hash of the token is stored)         |
//! | `nodes`      | the file tree: folders and files of every user              |
//! | `shares`     | public links to files and folders                           |
//! | `log_entries`| activity log: logins, file actions, warnings and errors     |
//! | `app_grants` | access users gave apps to their files                       |
//! | `app_passwords` | passwords for CalDAV and other clients, with their scopes |
//! | `app_shares` | AppData folders users shared with other users               |
//! | `plugin_configs` | what the administrator set for each plugin              |
//! | `plugin_files` | each plugin's own storage on the server (models, caches)  |
//! | `plugin_jobs` | background jobs of plugins, waiting or failed             |
//! | `oidc_providers` | external login services (OpenID Connect)              |
//! | `upload_sessions`, `upload_parts` | uploads in parts and the parts received |
//! | `file_changes` | the change feed of sync clients (ids only, kept 30 days) |

pub mod entities;
mod migrations;
#[cfg(feature = "testing")]
pub mod testing;

pub use entities::{
    app_grant, app_password, app_share, file_change, file_lock, file_version, identity, log_entry, node, oidc_provider,
    plugin_config, plugin_file, plugin_job, session, share, upload_part, upload_session, user,
};
pub use sea_orm::DatabaseConnection;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, DbErr};
use sea_orm_migration::MigratorTrait;

/// Opens the database at `url` and brings its schema up to date.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DbErr> {
    create_sqlite_dir(url)?;
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    let db = Database::connect(options).await?;

    if db.get_database_backend() == DbBackend::Sqlite {
        // WAL lets readers work while a write is in progress. The setting is
        // stored in the database file, so running it on one connection is enough.
        db.execute_unprepared("PRAGMA journal_mode = WAL").await?;
    }

    migrations::Migrator::up(&db, None).await?;
    tracing::info!(backend = ?db.get_database_backend(), "database ready");
    Ok(db)
}

/// SQLite creates the database file (`mode=rwc`) but not missing folders, so a
/// fresh install would fail on `sqlite://data/rustcloud.db`. Create the folder first.
fn create_sqlite_dir(url: &str) -> Result<(), DbErr> {
    let Some(rest) = url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    let path = std::path::Path::new(rest.split('?').next().unwrap_or_default());
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() && rest != ":memory:" => std::fs::create_dir_all(dir)
            .map_err(|err| DbErr::Custom(format!("cannot create database folder {}: {err}", dir.display()))),
        _ => Ok(()),
    }
}
