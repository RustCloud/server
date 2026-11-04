//! SeaORM entities, one module per table. The schema itself is created by the
//! migrations in `migrations.rs`; keep both in sync when adding columns.

/// A user account.
pub mod user {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        /// Lower-case login name, unique.
        #[sea_orm(unique)]
        pub username: String,
        pub display_name: String,
        pub email: Option<String>,
        pub is_admin: bool,
        /// Disabled users cannot log in; their sessions stop working.
        pub disabled: bool,
        /// Storage limit in bytes; `None` means unlimited.
        pub quota_bytes: Option<i64>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// One way to log in to a user account.
///
/// `provider` names the login method and `subject` is the account's id within
/// it. For passwords: `("password", "<username>")` with the Argon2 hash in
/// `secret_hash`. For an OIDC provider it would be `("oidc:<name>", "<sub claim>")`
/// without a secret. One user can have several identities.
pub mod identity {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "identities")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_id: Uuid,
        pub provider: String,
        pub subject: String,
        pub secret_hash: Option<String>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A login session. The client holds a random token; only its SHA-256 is stored,
/// so a leaked database does not leak usable tokens.
pub mod session {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "sessions")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(unique)]
        pub token_hash: String,
        pub user_id: Uuid,
        pub created_at: DateTimeUtc,
        pub expires_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A file or folder.
///
/// Every user has exactly one root folder (`parent_id = None`, empty name).
/// Names are unique within a folder. File bytes live in the blob store under
/// `content_key`; a new key is used for every upload, so replacing a file never
/// overwrites bytes that a running download may still be reading.
pub mod node {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "nodes")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner_id: Uuid,
        pub parent_id: Option<Uuid>,
        pub name: String,
        pub is_dir: bool,
        /// File size in bytes; always 0 for folders.
        pub size: i64,
        pub mime_type: Option<String>,
        /// Blob store key of the current content; `None` for folders.
        pub content_key: Option<String>,
        /// Hex SHA-256 of the content; also used as the HTTP ETag.
        pub sha256: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        /// When it went to the trash, on it and on everything inside it.
        pub trashed_at: Option<DateTimeUtc>,
        /// The folder it was deleted from; only on what was deleted itself,
        /// not on what is inside it.
        pub trashed_from: Option<Uuid>,
        /// Who wrote the current content; `None` for folders and for files
        /// written before versions existed.
        pub modified_by: Option<Uuid>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// An earlier content of a file. Replacing a file keeps the old blob here
/// instead of removing it; `[files] versions_keep` and `versions_days` decide
/// how long it stays.
pub mod file_version {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "file_versions")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub node_id: Uuid,
        /// Owner of the file: versions count against their quota.
        pub owner_id: Uuid,
        pub size: i64,
        pub content_key: String,
        pub sha256: String,
        /// Who wrote this content, if known.
        pub modified_by: Option<Uuid>,
        /// When this content was written.
        pub modified_at: DateTimeUtc,
        /// When it was replaced and became a version.
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A lock an app holds on a file while it edits it: until `expires_at`, only
/// that app changes or deletes the file.
pub mod file_lock {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "file_locks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub node_id: Uuid,
        /// What the app calls it (a WOPI lock id).
        pub lock_id: String,
        pub app_id: String,
        /// The user it was taken for.
        pub user_id: Uuid,
        pub expires_at: DateTimeUtc,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A public link to a file or folder. Anyone with the token can read the node
/// (and everything inside it, for a folder) until the link expires or is deleted.
pub mod share {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "shares")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(unique)]
        pub token: String,
        pub node_id: Uuid,
        pub owner_id: Uuid,
        pub expires_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// One entry of the activity log: who did what, plus warnings and errors.
pub mod log_entry {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "log_entries")]
    pub struct Model {
        /// Increasing, so it also orders entries and pages through them.
        #[sea_orm(primary_key)]
        pub id: i64,
        pub time: DateTimeUtc,
        /// `ERROR`, `WARN` or `INFO`.
        pub level: String,
        /// What happened, e.g. `file.download`; `None` for plain warnings and errors.
        pub action: Option<String>,
        pub message: String,
        /// Who did it, if known.
        pub username: Option<String>,
        /// From where, if known.
        pub ip: Option<String>,
        /// Other fields of the event as a JSON object, e.g. `{"name": "a.txt"}`.
        pub details: String,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// Access a user gave an app to some of their files: a file while it is open
/// in the app (`session`), or a folder until revoked (`standing`).
pub mod app_grant {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_grants")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_id: Uuid,
        /// Id of the app from its manifest, e.g. `notes`.
        pub app_id: String,
        /// The file or folder; access to a folder covers everything inside it.
        pub node_id: Uuid,
        /// Whether the app may change the files, not only read them.
        pub write: bool,
        /// `session` or `standing`.
        pub kind: String,
        pub created_at: DateTimeUtc,
        /// Last sign of life from the client that keeps a session open.
        pub last_seen_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod app_password {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_passwords")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_id: Uuid,
        /// What the user called it, e.g. "Phone calendar".
        pub name: String,
        /// SHA-256 of the password; the password itself is shown once.
        #[sea_orm(unique)]
        pub token_hash: String,
        /// What the password opens: `calendar:write notes:read`.
        pub scopes: String,
        pub created_at: DateTimeUtc,
        pub last_used_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A folder of a user's AppData shared with another user: the app opens it
/// for that user, read-only or for changes.
pub mod app_share {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_shares")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        /// Id of the app from its manifest, e.g. `calendar`.
        pub app_id: String,
        /// Whose AppData it is.
        pub owner_id: Uuid,
        /// The folder, relative to the owner's AppData of the app: `calendars/work`.
        pub path: String,
        /// What the app called the folder when it was shared, e.g. the calendar's name.
        pub label: String,
        /// Who it is shared with.
        pub user_id: Uuid,
        /// Whether that user may change it, not only read it.
        pub write: bool,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// What the administrator set for a plugin. A plugin without a row runs
/// with the defaults; one installed from the admin page starts disabled.
pub mod plugin_config {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "plugin_configs")]
    pub struct Model {
        /// Id of the app from its manifest.
        #[sea_orm(primary_key, auto_increment = false)]
        pub app_id: String,
        pub enabled: bool,
        /// The multiples of the base the administrator chose, JSON
        /// (`{"memory":4,"time":1,"workers":2}`); `None`: as the manifest says.
        pub limits: Option<String>,
        /// Values of the plugin's server settings, a JSON object.
        pub settings: String,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A file in a plugin's own storage on the server (models, caches); the
/// bytes are a blob, as for users' files.
pub mod plugin_file {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "plugin_files")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        /// Id of the app from its manifest.
        pub app_id: String,
        /// Names separated by `/`, unique per app.
        pub path: String,
        pub blob_key: String,
        pub size: i64,
        /// SHA-256 of the content, hex.
        pub sha256: String,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// A background job of a plugin for a user, waiting, running or failed;
/// done jobs are deleted.
pub mod plugin_job {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "plugin_jobs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub app_id: String,
        /// The user it runs for.
        pub user_id: Uuid,
        /// One of the `[[jobs]]` of the plugin's manifest.
        pub kind: String,
        /// What it works on, unique per plugin, user and kind.
        pub key: String,
        /// What the plugin gave with it.
        pub data: String,
        /// `waiting`, `running` or `failed`.
        pub state: String,
        /// Tries so far.
        pub attempts: i32,
        /// Its batch failed as a whole: it runs alone from now on.
        pub alone: bool,
        /// Why it failed.
        pub error: Option<String>,
        /// Not before this (a retry waits).
        pub run_after: DateTimeUtc,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}
