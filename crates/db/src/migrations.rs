//! Schema migrations. Migrations are append-only: never edit one that has been
//! released; add a new one instead.

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(InitialSchema),
            Box::new(ActivityLog),
            Box::new(AppGrants),
            Box::new(AppPasswords),
            Box::new(AppShares),
            Box::new(PluginConfigs),
            Box::new(PluginFiles),
            Box::new(PluginJobs),
            Box::new(PluginLimits),
            Box::new(Trash),
            Box::new(FileVersions),
            Box::new(FileLocks),
            Box::new(OidcProviders),
        ]
    }
}

/// Users, login identities, sessions, the file tree and public links.
struct InitialSchema;

impl MigrationName for InitialSchema {
    /// Stored in `seaql_migrations`; must never change once released.
    fn name(&self) -> &str {
        "m0001_initial_schema"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for InitialSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("users")
                    .col(pk_uuid("id"))
                    .col(string_uniq("username"))
                    .col(string("display_name"))
                    .col(string_null("email"))
                    .col(boolean("is_admin"))
                    .col(boolean("disabled"))
                    .col(big_integer_null("quota_bytes"))
                    .col(timestamp_with_time_zone("created_at"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table("identities")
                    .col(pk_uuid("id"))
                    .col(uuid("user_id"))
                    .col(string("provider"))
                    .col(string("subject"))
                    .col(string_null("secret_hash"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("identities", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "identities", &["provider", "subject"], true).await?;
        create_index(manager, "identities", &["user_id"], false).await?;

        manager
            .create_table(
                Table::create()
                    .table("sessions")
                    .col(pk_uuid("id"))
                    .col(string_uniq("token_hash"))
                    .col(uuid("user_id"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone("expires_at"))
                    .foreign_key(&mut cascade_fk("sessions", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "sessions", &["user_id"], false).await?;

        manager
            .create_table(
                Table::create()
                    .table("nodes")
                    .col(pk_uuid("id"))
                    .col(uuid("owner_id"))
                    .col(uuid_null("parent_id"))
                    .col(string("name"))
                    .col(boolean("is_dir"))
                    .col(big_integer("size"))
                    .col(string_null("mime_type"))
                    .col(string_null("content_key"))
                    .col(string_null("sha256"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone("updated_at"))
                    .foreign_key(&mut cascade_fk("nodes", "owner_id", "users"))
                    .foreign_key(&mut cascade_fk("nodes", "parent_id", "nodes"))
                    .to_owned(),
            )
            .await?;
        // Names are unique inside a folder…
        create_index(manager, "nodes", &["parent_id", "name"], true).await?;
        create_index(manager, "nodes", &["owner_id"], false).await?;
        // …and every user has exactly one root folder (the only node without a parent).
        // A partial index is written as plain SQL: the syntax is the same in
        // PostgreSQL and SQLite.
        manager
            .get_connection()
            .execute_unprepared("CREATE UNIQUE INDEX idx_nodes_one_root ON nodes (owner_id) WHERE parent_id IS NULL")
            .await?;

        manager
            .create_table(
                Table::create()
                    .table("shares")
                    .col(pk_uuid("id"))
                    .col(string_uniq("token"))
                    .col(uuid("node_id"))
                    .col(uuid("owner_id"))
                    .col(timestamp_with_time_zone_null("expires_at"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("shares", "node_id", "nodes"))
                    .foreign_key(&mut cascade_fk("shares", "owner_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "shares", &["node_id"], false).await?;
        create_index(manager, "shares", &["owner_id"], false).await?;

        Ok(())
    }
}

/// The activity log (`log_entries`).
struct ActivityLog;

impl MigrationName for ActivityLog {
    fn name(&self) -> &str {
        "m0002_activity_log"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ActivityLog {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("log_entries")
                    .col(big_pk_auto("id"))
                    .col(timestamp_with_time_zone("time"))
                    .col(string("level"))
                    .col(string_null("action"))
                    .col(text("message"))
                    .col(string_null("username"))
                    .col(string_null("ip"))
                    .col(text("details"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "log_entries", &["time"], false).await
    }
}

/// Access users gave apps to their files (`app_grants`).
struct AppGrants;

impl MigrationName for AppGrants {
    fn name(&self) -> &str {
        "m0003_app_grants"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AppGrants {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("app_grants")
                    .col(pk_uuid("id"))
                    .col(uuid("user_id"))
                    .col(string("app_id"))
                    .col(uuid("node_id"))
                    .col(boolean("write"))
                    .col(string("kind"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone("last_seen_at"))
                    .foreign_key(&mut cascade_fk("app_grants", "user_id", "users"))
                    .foreign_key(&mut cascade_fk("app_grants", "node_id", "nodes"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "app_grants", &["user_id", "app_id"], false).await
    }
}

/// Passwords for programs that log in with Basic authentication (CalDAV…).
struct AppPasswords;

impl MigrationName for AppPasswords {
    fn name(&self) -> &str {
        "m0004_app_passwords"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AppPasswords {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("app_passwords")
                    .col(pk_uuid("id"))
                    .col(uuid("user_id"))
                    .col(string("name"))
                    .col(string_uniq("token_hash"))
                    .col(string("scopes"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone_null("last_used_at"))
                    .foreign_key(&mut cascade_fk("app_passwords", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "app_passwords", &["user_id"], false).await
    }
}

/// Folders of their AppData users share with other users (`app_shares`).
struct AppShares;

impl MigrationName for AppShares {
    fn name(&self) -> &str {
        "m0005_app_shares"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AppShares {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("app_shares")
                    .col(pk_uuid("id"))
                    .col(string("app_id"))
                    .col(uuid("owner_id"))
                    .col(string("path"))
                    .col(string("label"))
                    .col(uuid("user_id"))
                    .col(boolean("write"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("app_shares", "owner_id", "users"))
                    .foreign_key(&mut cascade_fk("app_shares", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "app_shares", &["owner_id", "app_id", "path", "user_id"], true).await?;
        create_index(manager, "app_shares", &["user_id", "app_id"], false).await
    }
}

/// What the administrator set for each plugin (`plugin_configs`).
struct PluginConfigs;

impl MigrationName for PluginConfigs {
    fn name(&self) -> &str {
        "m0006_plugin_configs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PluginConfigs {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("plugin_configs")
                    .col(string("app_id").primary_key())
                    .col(boolean("enabled"))
                    .col(integer_null("memory_mib"))
                    .col(integer_null("time_seconds"))
                    .col(text("settings"))
                    .col(timestamp_with_time_zone("updated_at"))
                    .to_owned(),
            )
            .await
    }
}

/// The plugins' own storage on the server (`plugin_files`).
struct PluginFiles;

impl MigrationName for PluginFiles {
    fn name(&self) -> &str {
        "m0007_plugin_files"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PluginFiles {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("plugin_files")
                    .col(pk_uuid("id"))
                    .col(string("app_id"))
                    .col(text("path"))
                    .col(string("blob_key"))
                    .col(big_integer("size"))
                    .col(string("sha256"))
                    .col(timestamp_with_time_zone("updated_at"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "plugin_files", &["app_id", "path"], true).await
    }
}

/// Background jobs of plugins (`plugin_jobs`) and what the administrator
/// sets for each kind (`plugin_configs.jobs`).
struct PluginJobs;

impl MigrationName for PluginJobs {
    fn name(&self) -> &str {
        "m0008_plugin_jobs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PluginJobs {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("plugin_jobs")
                    .col(pk_uuid("id"))
                    .col(string("app_id"))
                    .col(uuid("user_id"))
                    .col(string("kind"))
                    .col(text("key"))
                    .col(text("data"))
                    .col(string("state"))
                    .col(integer("attempts"))
                    .col(boolean("alone"))
                    .col(text_null("error"))
                    .col(timestamp_with_time_zone("run_after"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone("updated_at"))
                    .foreign_key(&mut cascade_fk("plugin_jobs", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "plugin_jobs", &["app_id", "user_id", "kind", "key"], true).await?;
        create_index(manager, "plugin_jobs", &["state", "run_after"], false).await?;
        manager
            .alter_table(Table::alter().table("plugin_configs").add_column(text("jobs").default("{}")).to_owned())
            .await
    }
}

/// The limits of a plugin become three multiples of a base
/// (`plugin_configs.limits`, JSON); the administrator's memory and time per
/// plugin and per kind of jobs go.
struct PluginLimits;

impl MigrationName for PluginLimits {
    fn name(&self) -> &str {
        "m0009_plugin_limits"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PluginLimits {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.alter_table(Table::alter().table("plugin_configs").add_column(text_null("limits")).to_owned()).await?;
        // What was set in MiB and seconds, as the next step up of 64 MiB and 10 s.
        let step = |column: &str, base: u32| {
            let steps: String = [1, 2, 4, 8, 16, 32]
                .iter()
                .map(|step| format!("WHEN {column} <= {} THEN '{step}' ", step * base))
                .collect();
            format!("(CASE {steps}ELSE '64' END)")
        };
        let sql = format!(
            "UPDATE plugin_configs SET limits = '{{\"memory\":' || {} || ',\"time\":' || {} || ',\"workers\":1}}' \
             WHERE memory_mib IS NOT NULL OR time_seconds IS NOT NULL",
            step("COALESCE(memory_mib, 64)", 64),
            step("COALESCE(time_seconds, 10)", 10),
        );
        manager.get_connection().execute_unprepared(&sql).await?;
        for column in ["memory_mib", "time_seconds", "jobs"] {
            manager.alter_table(Table::alter().table("plugin_configs").drop_column(column).to_owned()).await?;
        }
        Ok(())
    }
}

/// The trash: when a file or folder was deleted (on it and everything inside)
/// and the folder it was deleted from (on it only).
struct Trash;

impl MigrationName for Trash {
    fn name(&self) -> &str {
        "m0010_trash"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Trash {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One column per statement: SQLite adds only one at a time.
        let columns = [timestamp_with_time_zone_null("trashed_at"), uuid_null("trashed_from")];
        for column in columns {
            manager.alter_table(Table::alter().table("nodes").add_column(column).to_owned()).await?;
        }
        create_index(manager, "nodes", &["trashed_at"], false).await
    }
}

/// Earlier contents of files (`file_versions`), and who wrote the current
/// content of a file (`nodes.modified_by`).
struct FileVersions;

impl MigrationName for FileVersions {
    fn name(&self) -> &str {
        "m0011_file_versions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for FileVersions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.alter_table(Table::alter().table("nodes").add_column(uuid_null("modified_by")).to_owned()).await?;
        manager
            .create_table(
                Table::create()
                    .table("file_versions")
                    .col(pk_uuid("id"))
                    .col(uuid("node_id"))
                    .col(uuid("owner_id"))
                    .col(big_integer("size"))
                    .col(string("content_key"))
                    .col(string("sha256"))
                    .col(uuid_null("modified_by"))
                    .col(timestamp_with_time_zone("modified_at"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("file_versions", "node_id", "nodes"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "file_versions", &["node_id", "created_at"], false).await?;
        create_index(manager, "file_versions", &["owner_id", "created_at"], false).await
    }
}

/// Locks apps hold on files while they edit them (`file_locks`).
struct FileLocks;

impl MigrationName for FileLocks {
    fn name(&self) -> &str {
        "m0012_file_locks"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for FileLocks {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("file_locks")
                    .col(pk_uuid("node_id"))
                    .col(string("lock_id"))
                    .col(string("app_id"))
                    .col(uuid("user_id"))
                    .col(timestamp_with_time_zone("expires_at"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("file_locks", "node_id", "nodes"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "file_locks", &["expires_at"], false).await
    }
}

/// External login services (OpenID Connect) the administrator set up
/// (`oidc_providers`).
struct OidcProviders;

impl MigrationName for OidcProviders {
    fn name(&self) -> &str {
        "m0013_oidc_providers"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for OidcProviders {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("oidc_providers")
                    .col(string("id").primary_key())
                    .col(string("name"))
                    .col(string("issuer"))
                    .col(string("client_id"))
                    .col(text("client_secret"))
                    .col(string("scopes"))
                    .col(boolean("enabled"))
                    .col(boolean("auto_create"))
                    .col(boolean("link_by_email"))
                    .col(string("groups_claim"))
                    .col(text("user_groups"))
                    .col(text("admin_groups"))
                    .col(timestamp_with_time_zone("created_at"))
                    .to_owned(),
            )
            .await
    }
}

/// Foreign key `table.column -> target.id` that deletes the row together with
/// the row it points to.
fn cascade_fk(table: &'static str, column: &'static str, target: &'static str) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(format!("fk_{table}_{column}"))
        .from(table, column)
        .to(target, "id")
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

/// Creates the index `idx_<table>_<columns…>`.
async fn create_index(
    manager: &SchemaManager<'_>,
    table: &'static str,
    columns: &[&'static str],
    unique: bool,
) -> Result<(), DbErr> {
    let mut index = Index::create();
    index.name(format!("idx_{table}_{}", columns.join("_"))).table(table);
    for column in columns {
        index.col(*column);
    }
    if unique {
        index.unique();
    }
    manager.create_index(index.to_owned()).await
}
