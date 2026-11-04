//! Runs RustCloud apps (plugins) compiled to WebAssembly components.
//!
//! An app is a component of the `app` world in
//! [`plugin-api/wit/app.wit`](../../plugin-api/wit/app.wit). It is compiled
//! once when loaded; every request then gets a fresh instance in its own
//! store, so requests never share memory and a crash affects only one request.
//!
//! What an app may do is exactly what the WIT imports give it. WASI is linked
//! because Rust's standard library needs it, but with an empty context: no
//! files, no network, no environment variables. HTTP requests go through the
//! `http` import, only to the addresses of the app's manifest. Each instance is limited in
//! memory, data moved in one call, its own work and time in all ([`Limits`],
//! worked out from the multiples of [`rustcloud_plugin_api::limits`]).
//!
//! Apps are loaded and unloaded while the server runs (the administrator
//! installs, removes or updates them); requests already running finish with
//! the version they started with.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rustcloud_core::{Error, Result};
pub use rustcloud_plugin_api::Limits;
use rustcloud_plugin_api::{Manifest, NetworkRule};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

mod http;

mod bindings {
    wasmtime::component::bindgen!({
        path: "../plugin-api/wit",
        world: "app",
        imports: { default: async },
        exports: { default: async },
    });
}

pub use bindings::rustcloud::app::appdata::Entry as AppDataEntry;
pub use bindings::rustcloud::app::files::{FileLock, Node as FileNode, Version as FileVersion};
pub use bindings::rustcloud::app::jobs::{Job, KindProgress as JobProgress, NewJob, Outcome as JobOutcome};
pub use bindings::rustcloud::app::links::{Error as LinkError, StreamMode, Target as LinkTarget};
pub use bindings::rustcloud::app::shares::Share;
pub use bindings::rustcloud::app::storage::File as StoredFile;
pub use bindings::rustcloud::app::tokens::{Scope as TokenScope, Token as AppToken};

/// The header of an app's answer that asks the server to send a file
/// instead of the body: `file:<id>`, `appdata:<path>` or `storage:<path>`.
pub const SEND_HEADER: &str = "x-rustcloud-send";

/// The header of a request whose body the app doesn't get because it is
/// larger than what the app takes at once: the body's size in bytes. The app
/// saves it with `files.write-request-body`.
pub const BODY_SIZE_HEADER: &str = "x-rustcloud-body-size";

/// The types of the `use-ffmpeg` import.
pub mod ffmpeg {
    pub use crate::bindings::rustcloud::app::use_ffmpeg::{
        AudioStream, Capabilities, Fit, ImageFormat, Location, Metadata, PictureOptions, Source, SubtitleStream,
        VideoStream,
    };
}

/// The types of the `use-onnx` import.
pub mod onnx {
    pub use crate::bindings::rustcloud::app::use_onnx::{
        AudioOptions, Capabilities, Channels, Crop, ImageFit, ImageInput, ImageOptions, Layout, ModelInfo, NamedTensor,
        Source, Tensor, TensorInfo, TensorType,
    };
}
pub use bindings::rustcloud::app::types::{Request, Response, User};

use bindings::rustcloud::app::appdata::Error as AppDataError;
use bindings::rustcloud::app::files::{Error as FilesError, LockError};
use bindings::rustcloud::app::http::{Error as HttpError, Request as HttpRequest, Response as HttpResponse};
use bindings::rustcloud::app::jobs::Error as JobsError;
use bindings::rustcloud::app::shares::Error as SharesError;
use bindings::rustcloud::app::storage::Error as StorageError;
use bindings::rustcloud::app::tokens::Error as TokenError;
use bindings::rustcloud::app::use_ffmpeg::Error as FfmpegError;
use bindings::rustcloud::app::use_onnx::Error as OnnxError;

/// The user a request came from, and what the server does for the app on
/// that user's behalf.
#[derive(Clone)]
pub struct Caller {
    pub user: User,
    pub services: Arc<dyn UserServices>,
}

/// What the server does for an app on behalf of one user. The server
/// implements it for each request; the app reaches it through its WIT imports.
#[async_trait::async_trait]
pub trait UserServices: Send + Sync {
    /// Reads a file of the app's AppData, if it is at most `limit` bytes.
    async fn appdata_read(&self, path: &str, limit: u64) -> Result<Vec<u8>>;
    /// Up to `length` bytes of a file of the app's AppData from `offset`.
    async fn appdata_read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>>;
    /// Creates or replaces a file of the app's AppData.
    async fn appdata_write(&self, path: &str, data: Vec<u8>) -> Result<AppDataEntry>;
    /// Lists a folder of the app's AppData (`""`: the app's folder).
    async fn appdata_list(&self, dir: &str) -> Result<Vec<AppDataEntry>>;
    /// Deletes a file or folder of the app's AppData.
    async fn appdata_delete(&self, path: &str) -> Result<()>;

    /// The user's files and folders granted to the app.
    async fn files_granted(&self) -> Result<Vec<FileNode>>;
    async fn files_get(&self, id: &str) -> Result<FileNode>;
    async fn files_children(&self, folder_id: &str) -> Result<Vec<FileNode>>;
    /// A whole file, if it is at most `limit` bytes.
    async fn files_read(&self, id: &str, limit: u64) -> Result<Vec<u8>>;
    /// Up to `length` bytes of a file from `offset`.
    async fn files_read_range(&self, id: &str, offset: u64, length: u64) -> Result<Vec<u8>>;
    async fn files_write(&self, id: &str, data: Vec<u8>) -> Result<FileNode>;
    async fn files_create_file(&self, folder_id: &str, name: &str, data: Vec<u8>) -> Result<FileNode>;
    async fn files_create_folder(&self, parent_id: &str, name: &str) -> Result<FileNode>;
    async fn files_delete(&self, id: &str) -> Result<()>;
    /// Replaces a file only if it still has the ETag `if_match`.
    async fn files_write_if_match(&self, _id: &str, _data: Vec<u8>, _if_match: &str) -> Result<FileNode> {
        Err(unavailable())
    }
    /// Replaces a file with the body of the request being answered.
    async fn files_write_request_body(&self, _id: &str, _if_match: Option<&str>) -> Result<FileNode> {
        Err(unavailable())
    }
    /// Takes or renews the app's lock on a file: `Err` with the lock
    /// someone else holds instead.
    async fn files_lock(
        &self,
        _id: &str,
        _lock_id: &str,
        _seconds: u32,
    ) -> Result<std::result::Result<FileLock, FileLock>> {
        Err(unavailable())
    }
    /// Ends the app's lock on a file: `Some` with the lock someone else holds instead.
    async fn files_unlock(&self, _id: &str, _lock_id: &str) -> Result<Option<FileLock>> {
        Err(unavailable())
    }
    async fn files_get_lock(&self, _id: &str) -> Result<Option<FileLock>> {
        Err(unavailable())
    }
    /// Earlier versions of a file, newest first.
    async fn files_versions(&self, _id: &str) -> Result<Vec<FileVersion>> {
        Err(unavailable())
    }
    /// Up to `length` bytes of a version from `offset`.
    async fn files_read_version(&self, _id: &str, _version: &str, _offset: u64, _length: u64) -> Result<Vec<u8>> {
        Err(unavailable())
    }

    /// Folders of their AppData other users shared with the user in this app.
    async fn shares_received(&self) -> Result<Vec<Share>>;
    async fn shared_read(&self, share_id: &str, path: &str, limit: u64) -> Result<Vec<u8>>;
    async fn shared_write(&self, share_id: &str, path: &str, data: Vec<u8>) -> Result<AppDataEntry>;
    async fn shared_list(&self, share_id: &str, dir: &str) -> Result<Vec<AppDataEntry>>;
    async fn shared_delete(&self, share_id: &str, path: &str) -> Result<()>;
    /// The user gives a share up.
    async fn shares_leave(&self, share_id: &str) -> Result<()>;

    /// Adds background jobs of the app for the user.
    async fn jobs_add(&self, jobs: Vec<NewJob>) -> Result<()>;
    /// The user's jobs of the app, by kind.
    async fn jobs_progress(&self) -> Result<Vec<JobProgress>>;

    /// Links for the app's page to files, one per target (see the `links`
    /// import); `NO_SESSION` without a browser session.
    async fn links_create(&self, targets: Vec<LinkTarget>) -> Vec<Result<String>>;
    /// An HLS playlist of a video for the app's page (see the `links` import).
    async fn links_video_stream(&self, target: LinkTarget, mode: StreamMode) -> Result<String>;
    /// A link the app's page uploads one file to (see the `links` import).
    async fn links_upload(&self, folder: &str, name: &str, overwrite: bool) -> Result<String>;
    /// A link the app's page uploads a new content of a file to.
    async fn links_replace(&self, _file: &str, _if_match: Option<&str>) -> Result<String> {
        Err(unavailable())
    }

    /// A token another server calls the app with, for one file (see the
    /// `tokens` import).
    async fn tokens_issue(&self, _file: &str, _write: bool, _seconds: u32) -> Result<AppToken> {
        Err(unavailable())
    }
    /// What the token of this request allows; `None` without one.
    fn token_scope(&self) -> Option<TokenScope> {
        None
    }

    /// A granted file of the user, opened for reading, for a tool (ffmpeg).
    async fn files_open(&self, id: &str) -> Result<std::fs::File>;
    /// A file of the app's AppData, opened for reading, for a tool.
    async fn appdata_open(&self, path: &str) -> Result<std::fs::File>;
}

/// ffmpeg, which the server runs for apps that may `use` it. Errors carry the
/// codes of the media engine: `MEDIA_UNSUPPORTED`, `INVALID_OPTIONS`,
/// `MEDIA_BUSY`, `MEDIA_TIMEOUT`, `MEDIA_FAILED`.
#[async_trait::async_trait]
pub trait UseFfmpeg: Send + Sync {
    fn capabilities(&self) -> ffmpeg::Capabilities;
    /// `background`: for a batch of jobs, which gives way to requests.
    async fn metadata(&self, file: std::fs::File, background: bool) -> Result<ffmpeg::Metadata>;
    /// A picture of at most `limit` bytes.
    async fn thumbnail(
        &self,
        file: std::fs::File,
        options: ffmpeg::PictureOptions,
        limit: u64,
        background: bool,
    ) -> Result<Vec<u8>>;
}

/// Neural networks, which the server runs for apps that may `use` them.
/// Models are files of the app's storage, named by path. Errors carry the
/// codes of the engines: `ONNX_UNSUPPORTED`, `ONNX_INVALID_INPUT`,
/// `ONNX_BUSY`, `ONNX_TIMEOUT`, `ONNX_FAILED`, and the media engine's for
/// the helpers.
#[async_trait::async_trait]
pub trait UseOnnx: Send + Sync {
    fn capabilities(&self) -> onnx::Capabilities;
    /// Unloads the models of an app that is gone.
    fn forget(&self, app_id: &str);
    /// `background`: for a batch of jobs, which waits longer for its turn.
    async fn model_info(&self, app_id: &str, model: &str, background: bool) -> Result<onnx::ModelInfo>;
    /// The outputs, at most `limit` bytes together.
    async fn run(
        &self,
        app_id: &str,
        model: &str,
        inputs: Vec<onnx::NamedTensor>,
        limit: u64,
        background: bool,
    ) -> Result<Vec<onnx::NamedTensor>>;
    /// A tensor of at most `limit` bytes.
    async fn image_tensor(
        &self,
        file: std::fs::File,
        options: onnx::ImageOptions,
        limit: u64,
        background: bool,
    ) -> Result<onnx::ImageInput>;
    /// A tensor of at most `limit` bytes.
    async fn audio_tensor(
        &self,
        file: std::fs::File,
        options: onnx::AudioOptions,
        limit: u64,
        background: bool,
    ) -> Result<onnx::Tensor>;
}

/// The apps' own storage on the server ([`StoredFile`]s, one storage per
/// app, for all its users). The server implements it; an app reaches its own
/// through the `storage` import.
#[async_trait::async_trait]
pub trait AppStorage: Send + Sync {
    /// The files in the folder `dir` and below (`""`: all), by path.
    async fn list(&self, app_id: &str, dir: &str) -> Result<Vec<StoredFile>>;
    async fn stat(&self, app_id: &str, path: &str) -> Result<StoredFile>;
    /// Up to `length` bytes from `offset` (the caller keeps it within reason).
    async fn read(&self, app_id: &str, path: &str, offset: u64, length: u64) -> Result<Vec<u8>>;
    async fn write(&self, app_id: &str, path: &str, data: Vec<u8>) -> Result<StoredFile>;
    /// Deletes a file, or everything in the folder `path`.
    async fn delete(&self, app_id: &str, path: &str) -> Result<()>;
    /// A file opened for reading, for a tool (ffmpeg).
    async fn open(&self, app_id: &str, path: &str) -> Result<std::fs::File>;
}

#[cfg(feature = "testing")]
pub mod testing;

/// What the server gives one request of an app besides the request itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub limits: Limits,
    /// The app's server settings, a JSON object (the `settings` import).
    pub settings: String,
    /// Where the app may send HTTP requests (the `http` import): the
    /// manifest's `permissions.network`, once the administrator approved it.
    pub network: Vec<NetworkRule>,
    /// Native tools it may use (`use-ffmpeg`, `use-onnx`): the approved `permissions.use`.
    pub uses: Vec<String>,
    /// How the service of the app's `[[proxy]]` reaches this server (the
    /// `proxy` import).
    pub self_url: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            settings: "{}".to_owned(),
            network: Vec::new(),
            uses: Vec::new(),
            self_url: None,
        }
    }
}

/// Time spent in native tools (`use-ffmpeg`, `use-onnx`), which the wall limit doesn't
/// count: they have limits of their own, and a batch of thumbnails waits for
/// them most of its time.
#[derive(Debug, Default)]
struct ToolClock(std::sync::Mutex<(Duration, Option<Instant>)>);

impl ToolClock {
    fn start(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(Instant::now());
    }

    fn stop(&self) {
        let mut clock = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(started) = clock.1.take() {
            clock.0 += started.elapsed();
        }
    }

    /// All of it, the call going on now included.
    fn spent(&self) -> Duration {
        let clock = self.0.lock().unwrap_or_else(|e| e.into_inner());
        clock.0 + clock.1.map_or(Duration::ZERO, |started| started.elapsed())
    }
}

/// How often the clock that enforces [`Limits::time`] ticks.
const TICK: Duration = Duration::from_millis(100);

/// The compiled apps, ready to handle requests.
pub struct WasmHost {
    engine: Engine,
    linker: Linker<Instance>,
    http: http::Clients,
    storage: Option<Arc<dyn AppStorage>>,
    ffmpeg: Option<Arc<dyn UseFfmpeg>>,
    onnx: Option<Arc<dyn UseOnnx>>,
    apps: RwLock<HashMap<String, bindings::AppPre<Instance>>>,
    /// Requests that may run in apps at once; more wait a moment, then get 429.
    requests: Arc<tokio::sync::Semaphore>,
}

/// How long a request waits for a free place before it is turned away.
const QUEUE_WAIT: Duration = Duration::from_secs(5);

impl WasmHost {
    /// Creates the host and starts the clock that stops runaway apps.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(Error::internal)?;

        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(Error::internal)?;
        bindings::App::add_to_linker::<_, HasSelf<Instance>>(&mut linker, |state| state).map_err(Error::internal)?;

        // A thread of its own: an app busy in a loop holds its async worker,
        // so a Tokio task might never get to tick. The clock stops when the
        // engine is dropped (the weak reference dies).
        let weak = engine.weak();
        std::thread::Builder::new()
            .name("wasm-clock".into())
            .spawn(move || {
                while let Some(engine) = weak.upgrade() {
                    engine.increment_epoch();
                    drop(engine);
                    std::thread::sleep(TICK);
                }
            })
            .map_err(Error::internal)?;
        let http = http::Clients::new().map_err(Error::internal)?;
        // Instances take memory (up to their limit each): a few per core.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let requests = Arc::new(tokio::sync::Semaphore::new((cores * 4).max(8)));
        Ok(Self { engine, linker, http, storage: None, ffmpeg: None, onnx: None, apps: RwLock::default(), requests })
    }

    /// Gives the apps their storage; without it the `storage` import fails.
    pub fn with_storage(mut self, storage: Arc<dyn AppStorage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Gives the apps that may use it ffmpeg; without it `use-ffmpeg` answers
    /// `unsupported`.
    pub fn with_ffmpeg(mut self, ffmpeg: Arc<dyn UseFfmpeg>) -> Self {
        self.ffmpeg = Some(ffmpeg);
        self
    }

    /// Gives the apps that may use them neural networks; without it
    /// `use-onnx` answers `unsupported`.
    pub fn with_onnx(mut self, onnx: Arc<dyn UseOnnx>) -> Self {
        self.onnx = Some(onnx);
        self
    }

    /// Downloads `url` for an app whose network addresses are `rules`
    /// (redirects included): its size if known, and its content.
    pub async fn download(
        &self,
        rules: &[NetworkRule],
        url: &str,
    ) -> std::result::Result<(Option<u64>, rustcloud_core::ByteStream), String> {
        http::download(&self.http, rules, url).await
    }

    /// Compiles an app, replacing a loaded one with the same id. Fails if the
    /// component is invalid or needs imports the host doesn't offer.
    pub fn load(&self, manifest: &Manifest, wasm: &[u8]) -> std::result::Result<(), String> {
        let component =
            Component::from_binary(&self.engine, wasm).map_err(|err| format!("app \"{}\": {err}", manifest.id))?;
        let pre = self
            .linker
            .instantiate_pre(&component)
            .and_then(bindings::AppPre::new)
            .map_err(|err| format!("app \"{}\" can't run here: {err}", manifest.id))?;
        if !manifest.jobs.is_empty() && !exports_run_batch(&component) {
            return Err(format!("app \"{}\" declares [[jobs]] but has no run-batch export", manifest.id));
        }
        self.apps.write().unwrap_or_else(|e| e.into_inner()).insert(manifest.id.clone(), pre);
        Ok(())
    }

    /// Whether a loaded app does background jobs (exports `run-batch`).
    pub fn runs_jobs(&self, app_id: &str) -> bool {
        let apps = self.apps.read().unwrap_or_else(|e| e.into_inner());
        apps.get(app_id).is_some_and(|pre| exports_run_batch(pre.instance_pre().component()))
    }

    /// Forgets an app; its next requests find no app, and its models are
    /// unloaded.
    pub fn unload(&self, app_id: &str) {
        self.apps.write().unwrap_or_else(|e| e.into_inner()).remove(app_id);
        if let Some(onnx) = &self.onnx {
            onnx.forget(app_id);
        }
    }

    /// Whether an app with this id is loaded.
    pub fn has(&self, app_id: &str) -> bool {
        self.apps.read().unwrap_or_else(|e| e.into_inner()).contains_key(app_id)
    }

    /// Runs `request` in a fresh instance of the app, within the limits and
    /// with the settings of `config`. `caller` is none for requests without a
    /// logged-in user.
    pub async fn handle(
        &self,
        app_id: &str,
        caller: Option<Caller>,
        request: Request,
        config: &AppConfig,
    ) -> Result<Response> {
        let _place = match tokio::time::timeout(QUEUE_WAIT, self.requests.clone().acquire_owned()).await {
            Ok(Ok(place)) => place,
            _ => {
                return Err(Error::new(
                    rustcloud_core::ErrorKind::TooManyRequests,
                    "SERVER_BUSY",
                    "The server is busy, try again in a moment",
                ))
            }
        };
        let user = caller.as_ref().map(|c| c.user.clone());
        let (pre, mut store) = self.prepare(app_id, caller, config, false)?;
        let clock = (store.data().started, store.data().tools.clone());
        let run = async {
            let app = pre.instantiate_async(&mut store).await?;
            app.call_handle(&mut store, user.as_ref(), &request).await
        };
        finish(app_id, config.limits, clock, run).await
    }

    /// Runs a batch of background jobs of `caller` in a fresh instance of the
    /// app (its `run-batch` export). Unlike a request, the app gives way to
    /// other work every [`TICK`], so long jobs don't hold up requests.
    pub async fn run_batch(
        &self,
        app_id: &str,
        caller: Caller,
        jobs: Vec<Job>,
        config: &AppConfig,
    ) -> Result<Vec<JobOutcome>> {
        let user = caller.user.clone();
        let (pre, mut store) = self.prepare(app_id, Some(caller), config, true)?;
        let clock = (store.data().started, store.data().tools.clone());
        let run = async {
            let instance = pre.instance_pre().instantiate_async(&mut store).await?;
            let run_batch = instance.get_typed_func::<(&User, &[Job]), (Vec<JobOutcome>,)>(&mut store, "run-batch")?;
            let (outcomes,) = run_batch.call_async(&mut store, (&user, &jobs)).await?;
            Ok(outcomes)
        };
        finish(app_id, config.limits, clock, run).await
    }

    /// A store for one call of an app, with its limits and the clock that
    /// enforces them.
    fn prepare(
        &self,
        app_id: &str,
        caller: Option<Caller>,
        config: &AppConfig,
        background: bool,
    ) -> Result<(bindings::AppPre<Instance>, Store<Instance>)> {
        let pre = self.apps.read().unwrap_or_else(|e| e.into_inner()).get(app_id).cloned();
        let pre = pre.ok_or_else(|| Error::not_found("APP_NOT_FOUND", "No such app"))?;
        let mut instance = Instance::new(app_id, caller.map(|c| c.services), config, self.http.clone());
        instance.background = background;
        instance.storage = self.storage.clone();
        instance.ffmpeg = self.ffmpeg.clone();
        instance.onnx = self.onnx.clone();
        let mut store = Store::new(&self.engine, instance);
        store.limiter(|state| &mut state.store_limits);
        store.set_epoch_deadline(1);
        // At every tick: stop if the app itself used up its time (the rest
        // was spent waiting for the server), else give way to other work, so
        // a busy app never holds a thread of the server for long.
        store.epoch_deadline_callback(move |store| {
            let state = store.data();
            let own = state.started.elapsed().saturating_sub(state.host_time);
            if own >= state.limits.time || state.wall_left().is_zero() {
                return Err(wasmtime::Error::msg("the app ran out of time"));
            }
            Ok(UpdateDeadline::Yield(1))
        });
        Ok((pre, store))
    }
}

/// Waits for a call of an app within its wall limit, not counting the time
/// of native tools, and turns its failure into `APP_FAILED`. The deadline is
/// checked only while the app runs; a call into the server that never
/// returns is cut off here.
async fn finish<T>(
    app_id: &str,
    limits: Limits,
    (started, tools): (Instant, Arc<ToolClock>),
    run: impl Future<Output = wasmtime::Result<T>>,
) -> Result<T> {
    let wall = limits.wall.max(limits.time);
    tokio::pin!(run);
    let result = loop {
        let deadline = started + wall + tools.spent();
        tokio::select! {
            result = &mut run => break result,
            _ = tokio::time::sleep_until(deadline.into()) => {
                if started.elapsed() >= wall + tools.spent() {
                    break Err(wasmtime::Error::msg("the request took too long"));
                }
            }
        }
    };
    result.map_err(|err| {
        tracing::warn!(app = app_id, "app failed: {err:#}");
        Error::new(rustcloud_core::ErrorKind::Internal, "APP_FAILED", format!("The app \"{app_id}\" failed"))
    })
}

/// Whether a component exports `run-batch` (world `app-with-jobs`).
fn exports_run_batch(component: &Component) -> bool {
    component.get_export_index(None, "run-batch").is_some()
}

/// Reads an app from its directory: `manifest.toml` and `app.wasm`. The
/// directory name must be the app's id.
pub fn read_app_dir(dir: &Path) -> std::result::Result<(Manifest, Vec<u8>), String> {
    let text = std::fs::read_to_string(dir.join("manifest.toml"))
        .map_err(|err| format!("{}: manifest.toml: {err}", dir.display()))?;
    let manifest = Manifest::from_toml(&text).map_err(|err| format!("{}: {err}", dir.display()))?;
    if dir.file_name().and_then(|name| name.to_str()) != Some(manifest.id.as_str()) {
        return Err(format!("{}: the folder must be named after the app id \"{}\"", dir.display(), manifest.id));
    }
    let wasm = std::fs::read(dir.join("app.wasm")).map_err(|err| format!("{}: app.wasm: {err}", dir.display()))?;
    Ok((manifest, wasm))
}

/// The state of one app instance (one request).
struct Instance {
    app_id: Arc<str>,
    started: Instant,
    limits: Limits,
    /// A batch of jobs, not a request.
    background: bool,
    /// Time spent in calls into the server, which the time limit doesn't count.
    host_time: Duration,
    tools: Arc<ToolClock>,
    services: Option<Arc<dyn UserServices>>,
    /// The app's server settings as JSON.
    settings: String,
    self_url: Option<String>,
    /// Where the app may send HTTP requests.
    network: Arc<[NetworkRule]>,
    http: http::Clients,
    storage: Option<Arc<dyn AppStorage>>,
    ffmpeg: Option<Arc<dyn UseFfmpeg>>,
    onnx: Option<Arc<dyn UseOnnx>>,
    /// The native tools it may use.
    uses: Arc<[String]>,
    wasi: WasiCtx,
    table: ResourceTable,
    store_limits: StoreLimits,
}

impl Instance {
    fn new(app_id: &str, services: Option<Arc<dyn UserServices>>, config: &AppConfig, http: http::Clients) -> Self {
        let limits = config.limits;
        Self {
            app_id: app_id.into(),
            started: Instant::now(),
            limits,
            background: false,
            host_time: Duration::ZERO,
            tools: Arc::default(),
            services,
            settings: config.settings.clone(),
            self_url: config.self_url.clone(),
            network: config.network.as_slice().into(),
            http,
            storage: None,
            ffmpeg: None,
            onnx: None,
            uses: config.uses.as_slice().into(),
            // Nothing from the host: no files, network, environment or arguments.
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            // The memory limit is per linear memory: few memories, tables and
            // instances, so a component of many modules can't multiply it.
            store_limits: StoreLimitsBuilder::new()
                .memory_size(usize::try_from(limits.memory).unwrap_or(usize::MAX))
                .memories(4)
                .tables(64)
                .table_elements(1 << 20)
                .instances(64)
                .build(),
        }
    }
}

impl WasiView for Instance {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl bindings::rustcloud::app::types::Host for Instance {}

impl bindings::rustcloud::app::settings::Host for Instance {
    async fn values(&mut self) -> String {
        self.settings.clone()
    }
}

impl bindings::rustcloud::app::proxy::Host for Instance {
    async fn self_url(&mut self) -> Option<String> {
        self.self_url.clone()
    }
}

impl bindings::rustcloud::app::http::Host for Instance {
    async fn fetch(&mut self, request: HttpRequest) -> std::result::Result<HttpResponse, HttpError> {
        // The host only: the rest of a URL may hold a key.
        let host = url::Url::parse(&request.url).ok().and_then(|url| url.host_str().map(str::to_owned));
        let method = request.method.clone();
        let time_left = self.wall_left();
        let (client, rules) = (self.http.clone(), self.network.clone());
        let started = Instant::now();
        let limit = self.limits.data;
        let result = self.host(http::fetch(&client, &rules, request, time_left, limit)).await;
        let host = host.as_deref().unwrap_or("?");
        match &result {
            Ok(response) => tracing::debug!(
                app = &*self.app_id,
                "{method} {host}: {} in {} ms",
                response.status,
                started.elapsed().as_millis()
            ),
            Err(HttpError::NotAllowed) => {
                tracing::warn!(app = &*self.app_id, "{method} {host}: not among the app's network addresses")
            }
            Err(err) => tracing::info!(app = &*self.app_id, "{method} {host} failed: {err:?}"),
        }
        result
    }
}

impl bindings::rustcloud::app::log::Host for Instance {
    async fn info(&mut self, message: String) {
        tracing::info!(app = &*self.app_id, "{message}");
    }

    async fn warn(&mut self, message: String) {
        tracing::warn!(app = &*self.app_id, "{message}");
    }
}

impl Instance {
    /// Runs a call into the server, counting its time as the server's.
    async fn host<T>(&mut self, call: impl Future<Output = T>) -> T {
        let started = Instant::now();
        let result = call.await;
        self.host_time += started.elapsed();
        result
    }

    /// Runs a native tool, whose time not even the wall limit counts.
    async fn tool<T>(&mut self, call: impl Future<Output = T>) -> T {
        self.tools.start();
        let result = self.host(call).await;
        self.tools.stop();
        result
    }

    /// What is left of the wall limit, not counting native tools.
    fn wall_left(&self) -> Duration {
        let spent = self.started.elapsed().saturating_sub(self.tools.spent());
        self.limits.wall.max(self.limits.time).saturating_sub(spent)
    }

    /// The user's services, or the error for a request without a user.
    fn user_services(&self) -> std::result::Result<Arc<dyn UserServices>, AppDataError> {
        self.services.clone().ok_or(AppDataError::NoUser)
    }
}

/// The WIT error for a server error.
fn appdata_error(err: Error) -> AppDataError {
    match err.code {
        "FILE_NOT_FOUND" => AppDataError::NotFound,
        "INVALID_NAME" => AppDataError::InvalidPath,
        "QUOTA_EXCEEDED" | "FILE_TOO_LARGE" => AppDataError::NoSpace,
        "TOO_LARGE_TO_READ" => AppDataError::TooLarge,
        "NAME_TAKEN" | "ALREADY_EXISTS" => AppDataError::Conflict,
        _ => AppDataError::Other(err.message),
    }
}

impl bindings::rustcloud::app::appdata::Host for Instance {
    async fn read(&mut self, path: String) -> std::result::Result<Vec<u8>, AppDataError> {
        let limit = self.limits.data;
        self.host(self.user_services()?.appdata_read(&path, limit)).await.map_err(appdata_error)
    }

    async fn read_range(
        &mut self,
        path: String,
        offset: u64,
        length: u64,
    ) -> std::result::Result<Vec<u8>, AppDataError> {
        let length = length.min(self.limits.data);
        self.host(self.user_services()?.appdata_read_range(&path, offset, length)).await.map_err(appdata_error)
    }

    async fn write(&mut self, path: String, data: Vec<u8>) -> std::result::Result<AppDataEntry, AppDataError> {
        self.host(self.user_services()?.appdata_write(&path, data)).await.map_err(appdata_error)
    }

    async fn list(&mut self, dir: String) -> std::result::Result<Vec<AppDataEntry>, AppDataError> {
        self.host(self.user_services()?.appdata_list(&dir)).await.map_err(appdata_error)
    }

    async fn delete(&mut self, path: String) -> std::result::Result<(), AppDataError> {
        self.host(self.user_services()?.appdata_delete(&path)).await.map_err(appdata_error)
    }
}

/// What services that don't offer something answer.
fn unavailable() -> Error {
    Error::internal("not available here")
}

/// The WIT error for a server error.
fn files_error(err: Error) -> FilesError {
    match err.code {
        "FILE_NOT_FOUND" | "NOT_FOUND" | "VERSION_NOT_FOUND" => FilesError::NotFound,
        "READ_ONLY" => FilesError::ReadOnly,
        "INVALID_NAME" => FilesError::InvalidName,
        "QUOTA_EXCEEDED" | "FILE_TOO_LARGE" => FilesError::NoSpace,
        "TOO_LARGE_TO_READ" => FilesError::TooLarge,
        "NAME_TAKEN" | "ALREADY_EXISTS" | "FILE_CHANGED" | "FILE_LOCKED" => FilesError::Conflict,
        _ => FilesError::Other(err.message),
    }
}

/// The WIT lock error for a server error.
fn lock_error(err: Error) -> LockError {
    match files_error(err) {
        FilesError::NoUser => LockError::NoUser,
        FilesError::NotFound => LockError::NotFound,
        FilesError::ReadOnly => LockError::ReadOnly,
        FilesError::Other(message) => LockError::Other(message),
        other => LockError::Other(other.to_string()),
    }
}

impl bindings::rustcloud::app::files::Host for Instance {
    async fn granted(&mut self) -> std::result::Result<Vec<FileNode>, FilesError> {
        self.host(self.file_services()?.files_granted()).await.map_err(files_error)
    }

    async fn get(&mut self, id: String) -> std::result::Result<FileNode, FilesError> {
        self.host(self.file_services()?.files_get(&id)).await.map_err(files_error)
    }

    async fn children(&mut self, folder_id: String) -> std::result::Result<Vec<FileNode>, FilesError> {
        self.host(self.file_services()?.files_children(&folder_id)).await.map_err(files_error)
    }

    async fn read(&mut self, id: String) -> std::result::Result<Vec<u8>, FilesError> {
        let limit = self.limits.data;
        self.host(self.file_services()?.files_read(&id, limit)).await.map_err(files_error)
    }

    async fn read_range(&mut self, id: String, offset: u64, length: u64) -> std::result::Result<Vec<u8>, FilesError> {
        let length = length.min(self.limits.data);
        self.host(self.file_services()?.files_read_range(&id, offset, length)).await.map_err(files_error)
    }

    async fn write(&mut self, id: String, data: Vec<u8>) -> std::result::Result<FileNode, FilesError> {
        self.host(self.file_services()?.files_write(&id, data)).await.map_err(files_error)
    }

    async fn create_file(
        &mut self,
        folder_id: String,
        name: String,
        data: Vec<u8>,
    ) -> std::result::Result<FileNode, FilesError> {
        self.host(self.file_services()?.files_create_file(&folder_id, &name, data)).await.map_err(files_error)
    }

    async fn create_folder(&mut self, parent_id: String, name: String) -> std::result::Result<FileNode, FilesError> {
        self.host(self.file_services()?.files_create_folder(&parent_id, &name)).await.map_err(files_error)
    }

    async fn delete(&mut self, id: String) -> std::result::Result<(), FilesError> {
        self.host(self.file_services()?.files_delete(&id)).await.map_err(files_error)
    }

    async fn write_if_match(
        &mut self,
        id: String,
        data: Vec<u8>,
        if_match: String,
    ) -> std::result::Result<FileNode, FilesError> {
        self.host(self.file_services()?.files_write_if_match(&id, data, &if_match)).await.map_err(files_error)
    }

    async fn write_request_body(
        &mut self,
        id: String,
        if_match: Option<String>,
    ) -> std::result::Result<FileNode, FilesError> {
        let services = self.file_services()?;
        // Copying a big body takes as long as the disk needs: like a native
        // tool, not the app's time.
        self.tool(services.files_write_request_body(&id, if_match.as_deref())).await.map_err(files_error)
    }

    async fn lock(&mut self, id: String, lock_id: String, seconds: u32) -> std::result::Result<FileLock, LockError> {
        let services = self.services.clone().ok_or(LockError::NoUser)?;
        match self.host(services.files_lock(&id, &lock_id, seconds)).await.map_err(lock_error)? {
            Ok(held) => Ok(held),
            Err(theirs) => Err(LockError::Locked(theirs)),
        }
    }

    async fn unlock(&mut self, id: String, lock_id: String) -> std::result::Result<(), LockError> {
        let services = self.services.clone().ok_or(LockError::NoUser)?;
        match self.host(services.files_unlock(&id, &lock_id)).await.map_err(lock_error)? {
            Some(theirs) => Err(LockError::Locked(theirs)),
            None => Ok(()),
        }
    }

    async fn get_lock(&mut self, id: String) -> std::result::Result<Option<FileLock>, FilesError> {
        self.host(self.file_services()?.files_get_lock(&id)).await.map_err(files_error)
    }

    async fn versions(&mut self, id: String) -> std::result::Result<Vec<FileVersion>, FilesError> {
        self.host(self.file_services()?.files_versions(&id)).await.map_err(files_error)
    }

    async fn read_version(
        &mut self,
        id: String,
        version_id: String,
        offset: u64,
        length: u64,
    ) -> std::result::Result<Vec<u8>, FilesError> {
        let length = length.min(self.limits.data);
        self.host(self.file_services()?.files_read_version(&id, &version_id, offset, length)).await.map_err(files_error)
    }
}

impl Instance {
    fn file_services(&self) -> std::result::Result<Arc<dyn UserServices>, FilesError> {
        self.services.clone().ok_or(FilesError::NoUser)
    }
}

/// The WIT error for a server error.
fn shares_error(err: Error) -> SharesError {
    match err.code {
        "FILE_NOT_FOUND" | "SHARE_NOT_FOUND" => SharesError::NotFound,
        "READ_ONLY" => SharesError::ReadOnly,
        "INVALID_NAME" => SharesError::InvalidPath,
        "QUOTA_EXCEEDED" | "FILE_TOO_LARGE" => SharesError::NoSpace,
        "TOO_LARGE_TO_READ" => SharesError::TooLarge,
        "NAME_TAKEN" | "ALREADY_EXISTS" => SharesError::Conflict,
        _ => SharesError::Other(err.message),
    }
}

impl bindings::rustcloud::app::shares::Host for Instance {
    async fn received(&mut self) -> std::result::Result<Vec<Share>, SharesError> {
        self.host(self.share_services()?.shares_received()).await.map_err(shares_error)
    }

    async fn read(&mut self, share_id: String, path: String) -> std::result::Result<Vec<u8>, SharesError> {
        let limit = self.limits.data;
        self.host(self.share_services()?.shared_read(&share_id, &path, limit)).await.map_err(shares_error)
    }

    async fn write(
        &mut self,
        share_id: String,
        path: String,
        data: Vec<u8>,
    ) -> std::result::Result<AppDataEntry, SharesError> {
        self.host(self.share_services()?.shared_write(&share_id, &path, data)).await.map_err(shares_error)
    }

    async fn list(&mut self, share_id: String, dir: String) -> std::result::Result<Vec<AppDataEntry>, SharesError> {
        self.host(self.share_services()?.shared_list(&share_id, &dir)).await.map_err(shares_error)
    }

    async fn delete(&mut self, share_id: String, path: String) -> std::result::Result<(), SharesError> {
        self.host(self.share_services()?.shared_delete(&share_id, &path)).await.map_err(shares_error)
    }

    async fn leave(&mut self, share_id: String) -> std::result::Result<(), SharesError> {
        self.host(self.share_services()?.shares_leave(&share_id)).await.map_err(shares_error)
    }
}

impl Instance {
    fn share_services(&self) -> std::result::Result<Arc<dyn UserServices>, SharesError> {
        self.services.clone().ok_or(SharesError::NoUser)
    }
}

/// The WIT error for a server error.
fn storage_error(err: Error) -> StorageError {
    match err.code {
        "FILE_NOT_FOUND" => StorageError::NotFound,
        "INVALID_NAME" => StorageError::InvalidPath,
        _ => StorageError::Other(err.message),
    }
}

impl Instance {
    fn app_storage(&self) -> std::result::Result<Arc<dyn AppStorage>, StorageError> {
        self.storage.clone().ok_or_else(|| StorageError::Other("this server has no storage for apps".to_owned()))
    }
}

impl bindings::rustcloud::app::storage::Host for Instance {
    async fn list(&mut self, dir: String) -> std::result::Result<Vec<StoredFile>, StorageError> {
        let app_id = self.app_id.clone();
        self.host(self.app_storage()?.list(&app_id, &dir)).await.map_err(storage_error)
    }

    async fn stat(&mut self, path: String) -> std::result::Result<StoredFile, StorageError> {
        let app_id = self.app_id.clone();
        self.host(self.app_storage()?.stat(&app_id, &path)).await.map_err(storage_error)
    }

    async fn read(&mut self, path: String, offset: u64, length: u64) -> std::result::Result<Vec<u8>, StorageError> {
        let app_id = self.app_id.clone();
        let length = length.min(self.limits.data);
        self.host(self.app_storage()?.read(&app_id, &path, offset, length)).await.map_err(storage_error)
    }

    async fn write(&mut self, path: String, data: Vec<u8>) -> std::result::Result<StoredFile, StorageError> {
        let app_id = self.app_id.clone();
        self.host(self.app_storage()?.write(&app_id, &path, data)).await.map_err(storage_error)
    }

    async fn delete(&mut self, path: String) -> std::result::Result<(), StorageError> {
        let app_id = self.app_id.clone();
        self.host(self.app_storage()?.delete(&app_id, &path)).await.map_err(storage_error)
    }
}

/// The WIT error for a server error.
fn jobs_error(err: Error) -> JobsError {
    match err.code {
        "UNKNOWN_JOB_KIND" => JobsError::UnknownKind(err.message),
        "INVALID_JOB" => JobsError::InvalidJob(err.message),
        _ => JobsError::Other(err.message),
    }
}

impl bindings::rustcloud::app::jobs::Host for Instance {
    async fn add(&mut self, jobs: Vec<NewJob>) -> std::result::Result<(), JobsError> {
        let services = self.services.clone().ok_or(JobsError::NoUser)?;
        self.host(services.jobs_add(jobs)).await.map_err(jobs_error)
    }

    async fn progress(&mut self) -> std::result::Result<Vec<JobProgress>, JobsError> {
        let services = self.services.clone().ok_or(JobsError::NoUser)?;
        self.host(services.jobs_progress()).await.map_err(jobs_error)
    }
}

/// The WIT error for a server error.
fn ffmpeg_error(err: Error) -> FfmpegError {
    match err.code {
        "FILE_NOT_FOUND" | "NOT_FOUND" => FfmpegError::NotFound,
        "MEDIA_UNSUPPORTED" => FfmpegError::Unsupported(err.message),
        "INVALID_OPTIONS" | "INVALID_NAME" => FfmpegError::InvalidOptions(err.message),
        "MEDIA_BUSY" => FfmpegError::Busy,
        "MEDIA_TIMEOUT" => FfmpegError::Timeout,
        _ => FfmpegError::Failed(err.message),
    }
}

impl Instance {
    /// ffmpeg, if the app may use it.
    fn ffmpeg(&self) -> std::result::Result<Arc<dyn UseFfmpeg>, FfmpegError> {
        if !self.uses.iter().any(|tool| tool == "ffmpeg") {
            return Err(FfmpegError::NotAllowed);
        }
        match &self.ffmpeg {
            Some(ffmpeg) if ffmpeg.capabilities().available => Ok(ffmpeg.clone()),
            _ => Err(FfmpegError::Unsupported("this server has no ffmpeg".to_owned())),
        }
    }

    /// Opens what a source names, checking access as for reading it.
    async fn open_source(&mut self, source: ffmpeg::Source) -> std::result::Result<std::fs::File, FfmpegError> {
        let app_id = self.app_id.clone();
        let opened = match source {
            ffmpeg::Source::File(id) => {
                let services = self.services.clone().ok_or(FfmpegError::NoUser)?;
                self.host(async move { services.files_open(&id).await }).await
            }
            ffmpeg::Source::Appdata(path) => {
                let services = self.services.clone().ok_or(FfmpegError::NoUser)?;
                self.host(async move { services.appdata_open(&path).await }).await
            }
            ffmpeg::Source::Storage(path) => {
                let storage = self.storage.clone().ok_or_else(|| FfmpegError::Failed("no storage".to_owned()))?;
                self.host(async move { storage.open(&app_id, &path).await }).await
            }
        };
        opened.map_err(ffmpeg_error)
    }
}

impl bindings::rustcloud::app::use_ffmpeg::Host for Instance {
    async fn get_capabilities(&mut self) -> ffmpeg::Capabilities {
        match self.ffmpeg() {
            Ok(ffmpeg) => ffmpeg.capabilities(),
            Err(_) => ffmpeg::Capabilities {
                available: false,
                ffmpeg: None,
                image_formats: Vec::new(),
                video_codecs: Vec::new(),
                audio_codecs: Vec::new(),
            },
        }
    }

    async fn get_metadata(&mut self, source: ffmpeg::Source) -> std::result::Result<ffmpeg::Metadata, FfmpegError> {
        let ffmpeg = self.ffmpeg()?;
        let file = self.open_source(source).await?;
        let background = self.background;
        self.tool(ffmpeg.metadata(file, background)).await.map_err(ffmpeg_error)
    }

    async fn thumbnail(
        &mut self,
        source: ffmpeg::Source,
        options: ffmpeg::PictureOptions,
    ) -> std::result::Result<Vec<u8>, FfmpegError> {
        let ffmpeg = self.ffmpeg()?;
        let file = self.open_source(source).await?;
        let (limit, background) = (self.limits.data, self.background);
        self.tool(ffmpeg.thumbnail(file, options, limit, background)).await.map_err(ffmpeg_error)
    }
}

/// The WIT error for a server error.
fn onnx_error(err: Error) -> OnnxError {
    match err.code {
        "FILE_NOT_FOUND" | "NOT_FOUND" => OnnxError::NotFound,
        "ONNX_UNSUPPORTED" | "MEDIA_UNSUPPORTED" => OnnxError::Unsupported(err.message),
        "ONNX_INVALID_INPUT" | "INVALID_OPTIONS" | "INVALID_NAME" => OnnxError::InvalidInput(err.message),
        "ONNX_BUSY" | "MEDIA_BUSY" => OnnxError::Busy,
        "ONNX_TIMEOUT" | "MEDIA_TIMEOUT" => OnnxError::Timeout,
        _ => OnnxError::Failed(err.message),
    }
}

impl Instance {
    /// The neural networks, if the app may use them.
    fn onnx(&self) -> std::result::Result<Arc<dyn UseOnnx>, OnnxError> {
        if !self.uses.iter().any(|tool| tool == "onnx") {
            return Err(OnnxError::NotAllowed);
        }
        match &self.onnx {
            Some(onnx) if onnx.capabilities().available => Ok(onnx.clone()),
            _ => Err(OnnxError::Unsupported("this server runs no neural networks".to_owned())),
        }
    }

    /// Opens a helper's file, checking access as for reading it.
    async fn open_onnx_source(&mut self, source: onnx::Source) -> std::result::Result<std::fs::File, OnnxError> {
        let source = match source {
            onnx::Source::File(id) => ffmpeg::Source::File(id),
            onnx::Source::Appdata(path) => ffmpeg::Source::Appdata(path),
            onnx::Source::Storage(path) => ffmpeg::Source::Storage(path),
        };
        self.open_source(source).await.map_err(|err| match err {
            FfmpegError::NoUser => OnnxError::NoUser,
            FfmpegError::NotFound => OnnxError::NotFound,
            FfmpegError::InvalidOptions(why) => OnnxError::InvalidInput(why),
            FfmpegError::Failed(why) | FfmpegError::Unsupported(why) => OnnxError::Failed(why),
            other => OnnxError::Failed(format!("{other:?}")),
        })
    }
}

impl bindings::rustcloud::app::use_onnx::Host for Instance {
    async fn get_capabilities(&mut self) -> onnx::Capabilities {
        match self.onnx() {
            Ok(onnx) => onnx.capabilities(),
            Err(_) => onnx::Capabilities { available: false, engine: None, keep_loaded_seconds: 0 },
        }
    }

    async fn get_model_info(&mut self, model: String) -> std::result::Result<onnx::ModelInfo, OnnxError> {
        let onnx = self.onnx()?;
        let (app_id, background) = (self.app_id.clone(), self.background);
        self.tool(onnx.model_info(&app_id, &model, background)).await.map_err(onnx_error)
    }

    async fn run(
        &mut self,
        model: String,
        inputs: Vec<onnx::NamedTensor>,
    ) -> std::result::Result<Vec<onnx::NamedTensor>, OnnxError> {
        let onnx = self.onnx()?;
        let (app_id, limit, background) = (self.app_id.clone(), self.limits.data, self.background);
        self.tool(onnx.run(&app_id, &model, inputs, limit, background)).await.map_err(onnx_error)
    }

    async fn image_tensor(
        &mut self,
        source: onnx::Source,
        options: onnx::ImageOptions,
    ) -> std::result::Result<onnx::ImageInput, OnnxError> {
        let onnx = self.onnx()?;
        let file = self.open_onnx_source(source).await?;
        let (limit, background) = (self.limits.data, self.background);
        self.tool(onnx.image_tensor(file, options, limit, background)).await.map_err(onnx_error)
    }

    async fn audio_tensor(
        &mut self,
        source: onnx::Source,
        options: onnx::AudioOptions,
    ) -> std::result::Result<onnx::Tensor, OnnxError> {
        let onnx = self.onnx()?;
        let file = self.open_onnx_source(source).await?;
        let (limit, background) = (self.limits.data, self.background);
        self.tool(onnx.audio_tensor(file, options, limit, background)).await.map_err(onnx_error)
    }
}

/// At most this many links at once.
const LINKS_AT_ONCE: usize = 1000;

impl bindings::rustcloud::app::links::Host for Instance {
    async fn create(&mut self, targets: Vec<LinkTarget>) -> Vec<std::result::Result<String, LinkError>> {
        let Some(services) = self.services.clone() else {
            return targets.iter().map(|_| Err(LinkError::NoSession)).collect();
        };
        if targets.len() > LINKS_AT_ONCE {
            let too_many = LinkError::Other(format!("at most {LINKS_AT_ONCE} links at once"));
            return targets.iter().map(|_| Err(too_many.clone())).collect();
        }
        let links = self.host(services.links_create(targets)).await;
        links.into_iter().map(|link| link.map_err(link_error)).collect()
    }

    async fn video_stream(&mut self, target: LinkTarget, mode: StreamMode) -> std::result::Result<String, LinkError> {
        if !self.uses.iter().any(|tool| tool == "ffmpeg") {
            return Err(LinkError::Other("streams need use.ffmpeg, which the administrator didn't approve".into()));
        }
        let services = self.services.clone().ok_or(LinkError::NoSession)?;
        self.host(services.links_video_stream(target, mode)).await.map_err(link_error)
    }

    async fn upload(
        &mut self,
        folder: String,
        name: String,
        overwrite: bool,
    ) -> std::result::Result<String, LinkError> {
        let services = self.services.clone().ok_or(LinkError::NoSession)?;
        self.host(services.links_upload(&folder, &name, overwrite)).await.map_err(|err| match err.code {
            "NO_SESSION" => LinkError::NoSession,
            "FILE_NOT_FOUND" | "NOT_A_FOLDER" => LinkError::NotFound,
            _ => LinkError::Other(err.message),
        })
    }

    async fn replace(&mut self, file: String, if_match: Option<String>) -> std::result::Result<String, LinkError> {
        let services = self.services.clone().ok_or(LinkError::NoSession)?;
        self.host(services.links_replace(&file, if_match.as_deref())).await.map_err(link_error)
    }
}

impl bindings::rustcloud::app::tokens::Host for Instance {
    async fn issue(&mut self, file: String, write: bool, seconds: u32) -> std::result::Result<AppToken, TokenError> {
        let services = self.services.clone().ok_or(TokenError::NoUser)?;
        self.host(services.tokens_issue(&file, write, seconds)).await.map_err(|err| match err.code {
            "NO_USER" => TokenError::NoUser,
            "FILE_NOT_FOUND" | "NOT_A_FILE" => TokenError::NotFound,
            "READ_ONLY" => TokenError::ReadOnly,
            _ => TokenError::Other(err.message),
        })
    }

    async fn current(&mut self) -> Option<TokenScope> {
        self.services.as_ref().and_then(|services| services.token_scope())
    }
}

/// The WIT error for a server error.
fn link_error(err: Error) -> LinkError {
    match err.code {
        "NO_SESSION" => LinkError::NoSession,
        "FILE_NOT_FOUND" | "NOT_FOUND" | "NOT_A_FILE" | "INVALID_NAME" | "INVALID_PATH" => LinkError::NotFound,
        _ => LinkError::Other(err.message),
    }
}

impl bindings::rustcloud::app::limits::Host for Instance {
    async fn get_limits(&mut self) -> bindings::rustcloud::app::limits::Limits {
        let millis = |time: Duration| u64::try_from(time.as_millis()).unwrap_or(u64::MAX);
        bindings::rustcloud::app::limits::Limits {
            memory_bytes: self.limits.memory,
            data_bytes: self.limits.data,
            time_ms: millis(self.limits.time),
            wall_ms: millis(self.limits.wall.max(self.limits.time)),
        }
    }
}
