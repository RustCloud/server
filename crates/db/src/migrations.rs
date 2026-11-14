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
            Box::new(FileMtime),
            Box::new(DeviceSessions),
            Box::new(FileChanges),
            Box::new(UploadSessions),
            Box::new(UnmatchedAppShares),
            Box::new(ShareOptions),
            Box::new(Groups),
            Box::new(EmailVerified),
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

/// When the content of a file was last changed as its client says
/// (`nodes.modified_at`, the mtime); `NULL` means the same as `updated_at`.
struct FileMtime;

impl MigrationName for FileMtime {
    fn name(&self) -> &str {
        "m0014_file_mtime"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for FileMtime {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let column = timestamp_with_time_zone_null("modified_at");
        manager.alter_table(Table::alter().table("nodes").add_column(column).to_owned()).await
    }
}

/// Sessions of client apps (devices) next to those of browsers: their name,
/// login method, last use, time limit and the refresh token they replaced;
/// and the time limit of logins through each login service.
struct DeviceSessions;

impl MigrationName for DeviceSessions {
    fn name(&self) -> &str {
        "m0015_device_sessions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for DeviceSessions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One column per statement: SQLite adds only one at a time.
        let columns = [
            string_null("kind"),
            string_null("name"),
            string_null("method"),
            string_null("ip"),
            timestamp_with_time_zone_null("last_used_at"),
            timestamp_with_time_zone_null("not_after"),
            string_null("previous_hash"),
            timestamp_with_time_zone_null("rotated_at"),
        ];
        for column in columns {
            manager.alter_table(Table::alter().table("sessions").add_column(column).to_owned()).await?;
        }
        create_index(manager, "sessions", &["previous_hash"], false).await?;
        let limit = integer("session_days").default(30).to_owned();
        manager.alter_table(Table::alter().table("oidc_providers").add_column(limit).to_owned()).await
    }
}

/// The change feed of sync clients (`file_changes`): what changed in whose
/// files, in order. `node_id` has no foreign key: deleted files stay in it.
struct FileChanges;

impl MigrationName for FileChanges {
    fn name(&self) -> &str {
        "m0016_file_changes"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for FileChanges {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("file_changes")
                    .col(big_pk_auto("id"))
                    .col(uuid("owner_id"))
                    .col(uuid("node_id"))
                    .col(string("kind"))
                    .col(timestamp_with_time_zone("time"))
                    .foreign_key(&mut cascade_fk("file_changes", "owner_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "file_changes", &["owner_id", "id"], false).await?;
        create_index(manager, "file_changes", &["time"], false).await
    }
}

/// Uploads in parts (`upload_sessions`) and the parts received so far
/// (`upload_parts`, each one blob).
struct UploadSessions;

impl MigrationName for UploadSessions {
    fn name(&self) -> &str {
        "m0017_upload_sessions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for UploadSessions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("upload_sessions")
                    .col(pk_uuid("id"))
                    .col(uuid("owner_id"))
                    .col(uuid("folder_id"))
                    .col(string("name"))
                    .col(big_integer("size"))
                    .col(big_integer("received"))
                    .col(integer("parts"))
                    .col(boolean("overwrite"))
                    .col(timestamp_with_time_zone_null("mtime"))
                    .col(timestamp_with_time_zone("created_at"))
                    .col(timestamp_with_time_zone("updated_at"))
                    .col(timestamp_with_time_zone_null("finishing_at"))
                    .foreign_key(&mut cascade_fk("upload_sessions", "owner_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "upload_sessions", &["owner_id"], false).await?;
        create_index(manager, "upload_sessions", &["updated_at"], false).await?;
        manager
            .create_table(
                Table::create()
                    .table("upload_parts")
                    .col(pk_uuid("id"))
                    .col(uuid("session_id"))
                    .col(big_integer("start"))
                    .col(big_integer("size"))
                    .col(string("content_key"))
                    .foreign_key(&mut cascade_fk("upload_parts", "session_id", "upload_sessions"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "upload_parts", &["session_id", "start"], true).await
    }
}

/// Shares of AppData with usernames nobody has (`app_shares_unmatched`):
/// kept and listed like real ones, so sharing doesn't tell which names exist.
struct UnmatchedAppShares;

impl MigrationName for UnmatchedAppShares {
    fn name(&self) -> &str {
        "m0018_unmatched_app_shares"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for UnmatchedAppShares {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("app_shares_unmatched")
                    .col(pk_uuid("id"))
                    .col(string("app_id"))
                    .col(uuid("owner_id"))
                    .col(string("path"))
                    .col(string("label"))
                    .col(string("username"))
                    .col(uuid("person_id"))
                    .col(boolean("write"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("app_shares_unmatched", "owner_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "app_shares_unmatched", &["owner_id", "app_id", "path", "username"], true).await
    }
}

/// Public links get a password (`shares.password_hash`) and may let
/// visitors upload into the folder (`shares.allow_upload`).
struct ShareOptions;

impl MigrationName for ShareOptions {
    fn name(&self) -> &str {
        "m0019_share_options"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ShareOptions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.alter_table(Table::alter().table("shares").add_column(string_null("password_hash")).to_owned()).await?;
        let allow_upload = boolean("allow_upload").default(false).to_owned();
        manager.alter_table(Table::alter().table("shares").add_column(allow_upload).to_owned()).await
    }
}

/// Groups of users the administrator keeps (`groups`, `group_members`),
/// and files and folders shared with users or groups (`file_shares`).
struct Groups;

impl MigrationName for Groups {
    fn name(&self) -> &str {
        "m0020_groups_and_file_shares"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Groups {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table("groups")
                    .col(pk_uuid("id"))
                    .col(string_uniq("name"))
                    .col(timestamp_with_time_zone("created_at"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table("group_members")
                    .col(uuid("group_id"))
                    .col(uuid("user_id"))
                    .primary_key(Index::create().col("group_id").col("user_id"))
                    .foreign_key(&mut cascade_fk("group_members", "group_id", "groups"))
                    .foreign_key(&mut cascade_fk("group_members", "user_id", "users"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "group_members", &["user_id"], false).await?;
        manager
            .create_table(
                Table::create()
                    .table("file_shares")
                    .col(pk_uuid("id"))
                    .col(uuid("node_id"))
                    .col(uuid("owner_id"))
                    .col(uuid_null("user_id"))
                    .col(uuid_null("group_id"))
                    .col(string_null("username"))
                    .col(boolean("write"))
                    .col(timestamp_with_time_zone("created_at"))
                    .foreign_key(&mut cascade_fk("file_shares", "node_id", "nodes"))
                    .foreign_key(&mut cascade_fk("file_shares", "owner_id", "users"))
                    .foreign_key(&mut cascade_fk("file_shares", "user_id", "users"))
                    .foreign_key(&mut cascade_fk("file_shares", "group_id", "groups"))
                    .to_owned(),
            )
            .await?;
        create_index(manager, "file_shares", &["node_id"], false).await?;
        create_index(manager, "file_shares", &["user_id"], false).await?;
        create_index(manager, "file_shares", &["group_id"], false).await
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

/// Whether an account's email is known to be its owner's
/// (`users.email_verified`): set by an administrator or confirmed by a
/// login service. Only such addresses let a login service take the account
/// by email; one users typed in themselves could be anyone's.
struct EmailVerified;

impl MigrationName for EmailVerified {
    fn name(&self) -> &str {
        "m0021_email_verified"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for EmailVerified {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nobody knows who typed the addresses there already: none counts.
        let column = boolean("email_verified").default(false).to_owned();
        manager.alter_table(Table::alter().table("users").add_column(column).to_owned()).await
    }
}
