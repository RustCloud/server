//! Neural networks for plugins with `use = ["onnx"]`: models in ONNX format
//! from a plugin's storage, run by [tract](https://github.com/sonos/tract),
//! which is memory-safe Rust.
//!
//! Each loaded model lives in a process of its own, `rustcloud onnx-worker`
//! ([`worker`]), which confines itself before it reads the model: no files,
//! no network, no programs, a memory limit. The server sends it the model
//! and then the inputs of each run through a pipe ([`protocol`]); the worker
//! answers with the outputs. A run that takes too long kills the process.
//!
//! Loading a model takes far longer than a run, so a model stays loaded
//! after a run for `[onnx] keep_loaded_seconds` (five minutes by default):
//! a plugin going through a list of files loads it once. At most
//! `[onnx] workers` models are loaded at once; one more unloads the one
//! unused the longest. Runs of one model wait for each other.

mod protocol;
mod settings;
mod tensor;
pub mod worker;

use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use rustcloud_core::{Error, ErrorKind, Live, Result};
use rustcloud_sandbox::Layers;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub use settings::OnnxSettings;
pub use tensor::{image_tensor, Channels, DType, Layout, ModelInfo, NamedTensor, Normalize, Tensor, TensorInfo};

use protocol::{Header, Reply, Run};

/// The engine that runs the models.
pub const ENGINE: &str = "tract 0.23";

/// The longest a request waits for a model's turn before it hears `busy`.
const WAIT_FOR_TURN: Duration = Duration::from_secs(30);
/// The longest a batch of jobs waits: it has time, and a job that hears
/// `busy` is only tried again later.
const BACKGROUND_WAIT: Duration = Duration::from_secs(300);
/// How often models unused for too long are unloaded.
const UNLOAD_EVERY: Duration = Duration::from_secs(10);

/// How the server starts a worker: itself as `rustcloud onnx-worker`.
#[derive(Debug, Clone)]
pub struct Launcher {
    pub program: PathBuf,
    /// Arguments before the worker's own: `["onnx-worker"]`.
    pub prefix: Vec<OsString>,
}

impl Launcher {
    /// The running server: `<this program> onnx-worker …`.
    pub fn this_program() -> std::io::Result<Self> {
        Ok(Self { program: std::env::current_exe()?, prefix: vec!["onnx-worker".into()] })
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix).args(args).env_clear().kill_on_drop(true);
        command
    }
}

/// A model file of a plugin's storage. Another `sha256` is another model:
/// an updated file is loaded anew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub app_id: String,
    pub path: String,
    pub sha256: String,
}

/// Who asks: a request someone waits for, or a batch of jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Request,
    Background,
}

/// How the engine stands, for the admin page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub available: bool,
    pub engine: Option<String>,
    /// What confines the workers here.
    pub layers: Option<Layers>,
    /// Why it isn't available, or what is weaker than it should be.
    pub problem: Option<String>,
}

/// A model loaded now, for the admin page.
#[derive(Debug, Clone, Serialize)]
pub struct LoadedModel {
    pub app_id: String,
    pub path: String,
    /// Seconds since its last run.
    pub idle_seconds: u64,
    /// Running now.
    pub busy: bool,
}

pub struct Onnx {
    launcher: Launcher,
    settings: Arc<Live<OnnxSettings>>,
    status: Status,
    models: Mutex<Vec<Arc<Loaded>>>,
    /// One model is loaded at a time: two requests for the same model load
    /// it once.
    loading: tokio::sync::Mutex<()>,
}

/// A model in its worker.
struct Loaded {
    file: ModelFile,
    info: ModelInfo,
    worker: tokio::sync::Mutex<Worker>,
    last_used: Mutex<Instant>,
}

impl Loaded {
    fn touch(&self) {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle(&self) -> Duration {
        self.last_used.lock().unwrap_or_else(|e| e.into_inner()).elapsed()
    }

    /// Not running now (nor waited for).
    fn is_free(&self) -> bool {
        self.worker.try_lock().is_ok()
    }
}

impl Onnx {
    /// Checks that workers run confined here, and starts unloading unused
    /// models. A server where they don't works on; the engine answers
    /// `unsupported`.
    pub async fn start(launcher: Launcher, settings: Arc<Live<OnnxSettings>>) -> Arc<Self> {
        let mut onnx = Self {
            launcher,
            settings,
            status: Status::default(),
            models: Mutex::default(),
            loading: tokio::sync::Mutex::default(),
        };
        onnx.status = onnx.check().await;
        match &onnx.status.problem {
            Some(problem) if !onnx.status.available => tracing::warn!("use.onnx unavailable: {problem}"),
            Some(problem) => tracing::warn!("use.onnx: {problem}"),
            None => tracing::info!("use.onnx: {ENGINE}"),
        }
        let onnx = Arc::new(onnx);
        tokio::spawn(unload_unused(Arc::downgrade(&onnx)));
        onnx
    }

    async fn check(&self) -> Status {
        let unavailable = |problem: String| Status { problem: Some(problem), ..Status::default() };
        if !self.settings.read(|s| s.enabled) {
            return unavailable("turned off in [onnx]".to_owned());
        }
        let output = self.launcher.command(&["--report".to_owned()]).stdin(Stdio::null()).output().await;
        let layers = match output {
            Ok(output) if output.status.success() => serde_json::from_slice::<Layers>(&output.stdout).ok(),
            Ok(output) => {
                return unavailable(format!("the sandbox doesn't work here: {}", stderr_line(&output.stderr)))
            }
            Err(err) => return unavailable(format!("the worker doesn't start: {err}")),
        };
        let Some(layers) = layers else {
            return unavailable("the worker gave no report".to_owned());
        };
        let problem = (!layers.landlock).then(|| {
            "Landlock is not enabled in this kernel; seccomp alone keeps the models away from files".to_owned()
        });
        Status { available: true, engine: Some(ENGINE.to_owned()), layers: Some(layers), problem }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// How long a model stays loaded after its last run, in seconds.
    pub fn keep_loaded_seconds(&self) -> u32 {
        self.settings.read(|s| s.keep_loaded_seconds)
    }

    /// The models loaded now, the most recently used first.
    pub fn loaded(&self) -> Vec<LoadedModel> {
        let mut loaded: Vec<LoadedModel> = self
            .models()
            .iter()
            .map(|model| LoadedModel {
                app_id: model.file.app_id.clone(),
                path: model.file.path.clone(),
                idle_seconds: model.idle().as_secs(),
                busy: !model.is_free(),
            })
            .collect();
        loaded.sort_by_key(|model| model.idle_seconds);
        loaded
    }

    /// Unloads the models of a plugin (disabled or removed); runs going on
    /// finish first.
    pub fn forget(&self, app_id: &str) {
        self.models().retain(|model| model.file.app_id != app_id);
    }

    /// What a model takes and gives; loads it if it isn't loaded. `open`
    /// opens the model file, and is called only to load it.
    pub async fn info<F>(&self, file: &ModelFile, open: impl FnOnce() -> F, priority: Priority) -> Result<ModelInfo>
    where
        F: Future<Output = Result<std::fs::File>>,
    {
        Ok(self.model(file, open, priority).await?.info.clone())
    }

    /// Runs a model on `inputs` (one per input of the model, by name) and
    /// gives its outputs, at most `output_limit` bytes together.
    pub async fn run<F>(
        &self,
        file: &ModelFile,
        open: impl FnOnce() -> F,
        inputs: Vec<NamedTensor>,
        output_limit: u64,
        priority: Priority,
    ) -> Result<Vec<NamedTensor>>
    where
        F: Future<Output = Result<std::fs::File>>,
    {
        for input in &inputs {
            input.tensor.check(&input.name)?;
        }
        let model = self.model(file, open, priority).await?;
        let mut worker = tokio::time::timeout(wait(priority), model.worker.lock()).await.map_err(|_| busy())?;
        let time = self.time();
        let result = tokio::time::timeout(time, worker.run(&inputs, output_limit)).await;
        model.touch();
        drop(worker);
        let outputs = match result {
            Ok(Ok(Ok(outputs))) => outputs,
            Ok(Ok(Err(why))) => return Err(failed(why)),
            Ok(Err(err)) => {
                self.unload(&model);
                tracing::info!(app = %file.app_id, model = %file.path, "the model's process failed: {err}");
                return Err(failed("the model's process stopped: out of memory?".to_owned()));
            }
            Err(_) => {
                self.unload(&model);
                return Err(Error::new(ErrorKind::TooManyRequests, "ONNX_TIMEOUT", "The model took too long"));
            }
        };
        if self.keep_loaded_seconds() == 0 {
            self.unload(&model);
        }
        Ok(outputs)
    }

    /// The model, loaded now if it isn't.
    async fn model<F>(&self, file: &ModelFile, open: impl FnOnce() -> F, priority: Priority) -> Result<Arc<Loaded>>
    where
        F: Future<Output = Result<std::fs::File>>,
    {
        if !self.status.available {
            let why = self.status.problem.as_deref().unwrap_or("use.onnx is not available");
            return Err(Error::bad_request("ONNX_UNSUPPORTED", why));
        }
        if let Some(model) = self.find(file) {
            return Ok(model);
        }
        let deadline = tokio::time::Instant::now() + wait(priority);
        let _loading = tokio::time::timeout_at(deadline, self.loading.lock()).await.map_err(|_| busy())?;
        // Loaded while this waited.
        if let Some(model) = self.find(file) {
            return Ok(model);
        }
        while !self.make_room() {
            if tokio::time::Instant::now() >= deadline {
                return Err(busy());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let model = Arc::new(self.load(file, open().await?).await?);
        self.models().push(model.clone());
        Ok(model)
    }

    fn find(&self, file: &ModelFile) -> Option<Arc<Loaded>> {
        let model = self.models().iter().find(|model| model.file == *file).cloned();
        if let Some(model) = &model {
            model.touch();
        }
        model
    }

    /// Whether there is room for one more model, unloading the one unused
    /// the longest if need be; no room while all of them run.
    fn make_room(&self) -> bool {
        let workers = self.settings.read(|s| s.workers) as usize;
        let mut models = self.models();
        if models.len() < workers {
            return true;
        }
        let unused = models.iter().enumerate().filter(|(_, model)| model.is_free()).max_by_key(|(_, m)| m.idle());
        match unused.map(|(index, _)| index) {
            Some(index) => {
                let model = models.remove(index);
                tracing::info!(app = %model.file.app_id, model = %model.file.path, "unloaded for another model");
                true
            }
            None => false,
        }
    }

    /// Starts a worker and gives it the model.
    async fn load(&self, file: &ModelFile, input: std::fs::File) -> Result<Loaded> {
        let size = input.metadata().map_err(Error::internal)?.len();
        let (max_model, memory) =
            self.settings.read(|s| (u64::from(s.max_model_mib) << 20, u64::from(s.memory_mib) << 20));
        if size > max_model {
            return Err(failed(format!(
                "the model is {} MiB; the server takes up to {} MiB",
                size >> 20,
                max_model >> 20
            )));
        }
        let started = Instant::now();
        let mut child = self
            .launcher
            .command(&["--memory".to_owned(), memory.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::internal)?;
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Error::internal("the worker has no pipes"));
        };
        tokio::spawn(log_lines(stderr, file.clone()));
        let mut stdout = BufReader::new(stdout);
        let send_and_hear = async {
            stdin.write_u64_le(size).await?;
            let mut model = tokio::fs::File::from_std(input);
            tokio::io::copy(&mut tokio::io::AsyncReadExt::take(&mut model, size), &mut stdin).await?;
            stdin.flush().await?;
            protocol::read_message::<Reply>(&mut stdout).await
        };
        let reply = match tokio::time::timeout(self.time(), send_and_hear).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(err)) => {
                tracing::info!(app = %file.app_id, model = %file.path, "the worker failed: {err}");
                return Err(failed("the model can't be loaded: its process stopped (out of memory?)".to_owned()));
            }
            Err(_) => {
                return Err(Error::new(ErrorKind::TooManyRequests, "ONNX_TIMEOUT", "The model took too long to load"))
            }
        };
        let info = match reply {
            Reply::Loaded(info) => info,
            Reply::Failed(why) => return Err(failed(format!("the model can't be loaded: {why}"))),
            Reply::Outputs(_) => return Err(Error::internal("the worker answered out of turn")),
        };
        tracing::info!(app = %file.app_id, model = %file.path, "loaded in {} ms", started.elapsed().as_millis());
        Ok(Loaded {
            file: file.clone(),
            info,
            worker: tokio::sync::Mutex::new(Worker { _child: child, stdin, stdout }),
            last_used: Mutex::new(Instant::now()),
        })
    }

    fn unload(&self, model: &Arc<Loaded>) {
        self.models().retain(|loaded| !Arc::ptr_eq(loaded, model));
    }

    /// Unloads the models unused for longer than `keep_loaded_seconds`.
    fn unload_unused(&self) {
        let keep = Duration::from_secs(self.keep_loaded_seconds().into());
        self.models().retain(|model| {
            let unused = model.idle() >= keep && model.is_free();
            if unused {
                tracing::info!(app = %model.file.app_id, model = %model.file.path, "unloaded: unused");
            }
            !unused
        });
    }

    fn models(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Loaded>>> {
        self.models.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn time(&self) -> Duration {
        Duration::from_secs(self.settings.read(|s| s.time_seconds).into())
    }
}

/// Unloads unused models every little while, as long as the engine is there.
async fn unload_unused(onnx: Weak<Onnx>) {
    loop {
        tokio::time::sleep(UNLOAD_EVERY).await;
        match onnx.upgrade() {
            Some(onnx) => onnx.unload_unused(),
            None => return,
        }
    }
}

/// A worker process with a model; dropping it kills the process.
struct Worker {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    /// One run: the outputs, or why the model refused the inputs. An I/O
    /// error means the process is gone.
    async fn run(
        &mut self,
        inputs: &[NamedTensor],
        output_limit: u64,
    ) -> std::io::Result<std::result::Result<Vec<NamedTensor>, String>> {
        let headers = inputs
            .iter()
            .map(|input| Header {
                name: input.name.clone(),
                dtype: input.tensor.dtype,
                shape: input.tensor.shape.clone(),
            })
            .collect();
        protocol::write_message_async(&mut self.stdin, &Run { inputs: headers, output_limit }).await?;
        for input in inputs {
            protocol::write_frame_async(&mut self.stdin, &input.tensor.data).await?;
        }
        self.stdin.flush().await?;
        let headers = match protocol::read_message::<Reply>(&mut self.stdout).await? {
            Reply::Outputs(headers) => headers,
            Reply::Failed(why) => return Ok(Err(why)),
            Reply::Loaded(_) => return Err(std::io::Error::other("the worker answered out of turn")),
        };
        let mut outputs = Vec::with_capacity(headers.len());
        for header in headers {
            let data = protocol::read_frame_async(&mut self.stdout, output_limit).await?;
            let tensor = Tensor { dtype: header.dtype, shape: header.shape, data };
            tensor.check(&header.name).map_err(|err| std::io::Error::other(err.message))?;
            outputs.push(NamedTensor { name: header.name, tensor });
        }
        Ok(Ok(outputs))
    }
}

/// What a worker writes to stderr (tract's warnings, a panic), in the log.
async fn log_lines(stderr: tokio::process::ChildStderr, file: ModelFile) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::info!(app = %file.app_id, model = %file.path, "{line}");
    }
}

fn stderr_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().map(str::trim).rfind(|line| !line.is_empty()).unwrap_or("it failed").to_owned()
}

fn wait(priority: Priority) -> Duration {
    match priority {
        Priority::Request => WAIT_FOR_TURN,
        Priority::Background => BACKGROUND_WAIT,
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::TooManyRequests, "ONNX_BUSY", "All the models are busy; try later")
}

fn failed(message: String) -> Error {
    Error::bad_request("ONNX_FAILED", message)
}
