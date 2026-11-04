//! The RustCloud server: puts the modules together into one HTTP application.
//!
//! Each module (auth, files, and future apps) brings its own routes and its
//! settings section; this crate only creates the shared state, merges the
//! routers, keeps the settings file, and adds what is common to every
//! response (security headers, logging, the web UI).

mod app_bodies;
mod app_links;
mod app_passwords;
mod app_routes;
mod app_shares;
mod app_tokens;
pub mod apps;
pub mod config;
mod grants;
pub mod journal;
pub mod network;
mod plugin_jobs;
mod plugin_storage;
mod plugins;
mod service_proxy;
pub mod settings_file;
mod system;
mod use_ffmpeg;
mod use_onnx;
mod web;

use std::sync::Arc;

use axum::extract::{FromRef, Path, Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::routing::{any, get, put};
use axum::{middleware, Json, Router};
use rustcloud_auth::{AccountSettings, AdminUser, Auth, CurrentUser};
use rustcloud_core::{Error, Live, Result, SettingsSection};
use rustcloud_db::DatabaseConnection;
use rustcloud_files::{Files, FilesSettings};
use rustcloud_storage_local::LocalBlobStore;
use rustcloud_wasm_host::WasmHost;
use serde::Serialize;
use serde_json::{json, Value};
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub use config::Config;
use journal::{EntryFilter, EntryPage, Journal, JournalQueue, JournalSettings};
use network::ServerSettings;
use plugin_jobs::Jobs;
use plugin_storage::PluginStorage;
use plugins::{PluginSettings, Plugins};
use rustcloud_media::{Launcher, Media, MediaSettings};
use rustcloud_ml::{Onnx, OnnxSettings};
use settings_file::{SectionView, SettingsFile};

/// Everything request handlers need. Modules take the part they use through
/// `FromRef` (e.g. `State<Arc<Files>>`).
#[derive(Clone, FromRef)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub auth: Arc<Auth>,
    pub files: Arc<Files>,
    pub server: Arc<Live<ServerSettings>>,
    pub settings: Arc<SettingsFile>,
    pub journal: Arc<Journal>,
    pub plugins: Arc<Plugins>,
    pub jobs: Arc<Jobs>,
    pub media: Arc<Media>,
    pub onnx: Arc<Onnx>,
    pub links: Arc<app_links::AppLinks>,
    pub proxy: Arc<service_proxy::ServiceProxy>,
}

impl AppState {
    /// Connects to the database (applying migrations), opens the file storage
    /// and starts writing the activity log from `journal`.
    pub async fn open(config: &Config, journal: JournalQueue) -> StartupResult<Self> {
        let db = rustcloud_db::connect(&config.startup.database_url).await?;
        let blobs = LocalBlobStore::open(config.startup.data_dir.join("files")).await?;
        Self::new(db, Arc::new(blobs), config, journal).await
    }

    /// Creates the modules, each with its settings section from `config`,
    /// and loads the plugins. Starts the journal writer.
    pub async fn new(
        db: DatabaseConnection,
        blobs: Arc<dyn rustcloud_core::BlobStore>,
        config: &Config,
        journal: JournalQueue,
    ) -> StartupResult<Self> {
        let server = Arc::new(Live::new(config.section::<ServerSettings>()?));
        let accounts = Arc::new(Live::new(config.section::<AccountSettings>()?));
        let file_settings = Arc::new(Live::new(config.section::<FilesSettings>()?));
        let journal_settings = Arc::new(Live::new(config.section::<JournalSettings>()?));
        let plugin_settings = Arc::new(Live::new(config.section::<PluginSettings>()?));
        let media_settings = Arc::new(Live::new(config.section::<MediaSettings>()?));
        let onnx_settings = Arc::new(Live::new(config.section::<OnnxSettings>()?));

        // Tools for plugins run as `rustcloud sandbox`, models as `rustcloud
        // onnx-worker`; tests have no such binary and no tools, and get
        // engines that say so.
        let launcher = Launcher::this_program()?;
        let media =
            Arc::new(Media::start(launcher, config.startup.media_tools_dir.as_deref(), media_settings.clone()).await);
        let onnx = Onnx::start(rustcloud_ml::Launcher::this_program()?, onnx_settings.clone()).await;

        let storage = Arc::new(PluginStorage::new(db.clone(), blobs.clone()));
        let onnx_tools = use_onnx::OnnxTools { onnx: onnx.clone(), media: media.clone(), storage: storage.clone() };
        let host = Arc::new(
            WasmHost::new()?
                .with_storage(storage.clone())
                .with_ffmpeg(Arc::new(use_ffmpeg::Ffmpeg(media.clone())))
                .with_onnx(Arc::new(onnx_tools)),
        );
        let dirs = config.startup.apps_dirs();
        let plugins = Plugins::load(
            db.clone(),
            host,
            storage.clone(),
            plugin_settings.clone(),
            &dirs,
            config.startup.installed_apps_dir(),
        )
        .await?;
        let plugins = Arc::new(plugins);
        plugins.start_all_downloads();
        let auth = Arc::new(Auth::new(db.clone(), accounts.clone()));
        let files = Arc::new(Files::new(db.clone(), blobs, file_settings.clone()));
        files.start_trash_cleanup();
        std::fs::create_dir_all(&config.startup.data_dir).map_err(Error::internal)?;
        let tokens = app_tokens::AppTokens::open(&config.startup.data_dir)?;
        // Bodies of requests to apps too large for them; left over only by a crash.
        let bodies = config.startup.data_dir.join("tmp").join("request-bodies");
        let _ = std::fs::remove_dir_all(&bodies);
        let links = Arc::new(app_links::AppLinks::new(files.clone(), storage, media.clone(), tokens, bodies));
        let jobs = Arc::new(Jobs::new(db.clone(), files.clone(), auth.clone(), plugins.clone(), links.clone()));
        let proxy =
            Arc::new(service_proxy::ServiceProxy::new(plugin_settings.clone(), config.startup.listen, Arc::default())?);
        jobs.start(plugin_settings.read(|s| s.workers()));

        // The order of sections on the admin page.
        let sections: Vec<Arc<dyn SettingsSection>> = vec![
            server.clone(),
            accounts,
            file_settings,
            plugin_settings,
            media_settings,
            onnx_settings,
            journal_settings.clone(),
        ];
        Ok(Self {
            journal: Arc::new(Journal::start(db.clone(), journal, journal_settings)),
            auth,
            files,
            jobs,
            media,
            onnx,
            links,
            proxy,
            settings: Arc::new(SettingsFile::new(config, sections)?),
            plugins,
            server,
            db,
        })
    }
}

/// The complete HTTP application.
pub fn app(state: AppState) -> Router {
    let api = Router::new()
        .merge(rustcloud_auth::router())
        .merge(rustcloud_files::router())
        .merge(grants::router())
        .merge(app_passwords::router())
        .merge(app_shares::router())
        .merge(plugins::router())
        .merge(use_ffmpeg::router())
        .merge(use_onnx::router())
        .merge(system::router())
        .merge(app_routes::router())
        .merge(app_links::router())
        .route("/api/v1/info", get(info))
        .route("/api/v1/apps", get(list_apps))
        .route("/.well-known/{name}", any(well_known))
        .route("/api/v1/admin/settings", get(list_settings))
        .route("/api/v1/admin/settings/{section}", put(save_settings))
        .route("/api/v1/admin/journal", get(journal_entries))
        .route("/health", get(health));

    // Only the web UI is compressed: file downloads must keep exact byte
    // ranges, and API responses are small.
    let web = Router::new().fallback(web::serve).layer(CompressionLayer::new());

    let guard = network::GuardState { settings: state.server.clone(), inner_hosts: state.proxy.inner_hosts() };
    api.merge(web)
        // Paths of plugins' services go to them, before the routes and the web UI.
        .layer(middleware::from_fn_with_state(state.clone(), service_proxy::route))
        .layer(network::cors(state.server.clone()))
        .layer(middleware::from_fn_with_state(guard, network::guard))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("same-origin")))
        // The path only: queries carry tokens (links, services' access tokens).
        .layer(TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<axum::body::Body>| {
            tracing::debug_span!("request", method = %request.method(), path = request.uri().path())
        }))
        .with_state(state)
}

/// `GET /health`: 200 when the server and its database work.
async fn health(State(db): State<DatabaseConnection>) -> Result<Json<Value>> {
    db.ping().await.map_err(|err| Error::internal(format!("database ping failed: {err}")))?;
    Ok(Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })))
}

/// `GET /api/v1/info`: what the web UI needs to know about the server.
async fn info(State(server): State<Arc<Live<ServerSettings>>>) -> Json<Value> {
    let public_url = server.read(|s| s.public_url.trim_end_matches('/').to_owned());
    Json(json!({ "version": env!("CARGO_PKG_VERSION"), "public_url": public_url }))
}

/// The apps of this server and what each may take at once, for the panel.
#[derive(Serialize)]
struct AppsView {
    #[serde(flatten)]
    apps: Arc<apps::Apps>,
    /// By app id: the most a request of its page may carry.
    limits: std::collections::BTreeMap<String, AppLimits>,
    /// By app id: the services the panel may show for its page.
    frames: std::collections::BTreeMap<String, Vec<String>>,
    /// By app id: paths of the server that go to its service (`[[proxy]]`);
    /// the panel may show them for its page too.
    proxies: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct AppLimits {
    data_bytes: u64,
}

/// `GET /api/v1/apps`: the apps of this server, for the clients' navigation.
async fn list_apps(State(plugins): State<Arc<Plugins>>, _user: CurrentUser) -> Json<AppsView> {
    let apps = plugins.running();
    let limits =
        apps.apps.iter().map(|app| (app.id.clone(), AppLimits { data_bytes: plugins.limits(&app.id).data })).collect();
    let frames = apps
        .apps
        .iter()
        .map(|app| (app.id.clone(), plugins.frame_origins(&app.id)))
        .filter(|(_, origins)| !origins.is_empty())
        .collect();
    let mut proxies = std::collections::BTreeMap::<String, Vec<String>>::new();
    for route in plugins.proxies() {
        proxies.entry(route.app_id).or_default().extend(route.paths);
    }
    Json(AppsView { apps, limits, frames, proxies })
}

/// `/.well-known/<name>` (any method): discovery of a protocol an app serves,
/// e.g. CalDAV, redirected to the app's path for it. 301, as RFC 6764 shows:
/// DAV clients repeat their request at the new address.
async fn well_known(
    State(plugins): State<Arc<Plugins>>,
    Path(name): Path<String>,
) -> Result<(StatusCode, [(HeaderName, String); 1])> {
    let apps = plugins.running();
    let (id, path) =
        apps.well_known(&name).ok_or_else(|| Error::not_found("NOT_FOUND", "No app serves this address"))?;
    Ok((StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, format!("/app/{id}{path}"))]))
}

/// `GET /api/v1/admin/settings`: every settings section with its current values.
async fn list_settings(State(settings): State<Arc<SettingsFile>>, _admin: AdminUser) -> Json<Vec<SectionView>> {
    Json(settings.sections())
}

/// `PUT /api/v1/admin/settings/{section}`: saves one section to the config file and applies it.
async fn save_settings(
    State(settings): State<Arc<SettingsFile>>,
    _admin: AdminUser,
    Path(section): Path<String>,
    Json(values): Json<Value>,
) -> Result<Json<SectionView>> {
    Ok(Json(settings.save(&section, values).await?))
}

/// `GET /api/v1/admin/journal?problems=&q=&before=`: activity log entries, newest first.
async fn journal_entries(
    State(journal): State<Arc<Journal>>,
    _admin: AdminUser,
    Query(filter): Query<EntryFilter>,
) -> Result<Json<EntryPage>> {
    Ok(Json(journal.entries(filter).await?))
}

/// Result of startup code, where any failure just stops the program with a message.
pub type StartupResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
