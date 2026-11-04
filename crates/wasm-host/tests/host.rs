//! The WASM host against a real app (tests/apps/echo, see `testing`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustcloud_wasm_host::testing::{echo_manifest, echo_wasm};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rustcloud_core::{Error, Result};
use rustcloud_plugin_api::NetworkRule;
use rustcloud_wasm_host::{
    ffmpeg, onnx, read_app_dir, AppConfig, AppDataEntry, Caller, FileNode, Job, JobOutcome, JobProgress, Limits,
    LinkTarget, NewJob, Request, Response, Share, StreamMode, UseFfmpeg, UseOnnx, User, UserServices, WasmHost,
};

/// AppData of one user, in memory; writes take the given time, as on a busy server.
#[derive(Default)]
struct MemoryData(Mutex<HashMap<String, Vec<u8>>>, Duration);

#[async_trait::async_trait]
impl UserServices for MemoryData {
    async fn appdata_read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let data = self.0.lock().unwrap().get(path).cloned().ok_or_else(missing)?;
        match data.len() as u64 > limit {
            true => Err(Error::bad_request("TOO_LARGE_TO_READ", "too large")),
            false => Ok(data),
        }
    }
    async fn appdata_write(&self, path: &str, data: Vec<u8>) -> Result<AppDataEntry> {
        tokio::time::sleep(self.1).await;
        let size = data.len() as u64;
        self.0.lock().unwrap().insert(path.to_owned(), data);
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        Ok(AppDataEntry { name, is_dir: false, size, etag: format!("\"{size}\""), updated_at: String::new() })
    }
    async fn appdata_list(&self, _dir: &str) -> Result<Vec<AppDataEntry>> {
        Ok(Vec::new())
    }
    async fn appdata_delete(&self, path: &str) -> Result<()> {
        self.0.lock().unwrap().remove(path);
        Ok(())
    }
    // No files granted: the host's own tests cover AppData; files go end to
    // end through the server's tests.
    async fn files_granted(&self) -> Result<Vec<FileNode>> {
        Ok(Vec::new())
    }
    async fn files_get(&self, _id: &str) -> Result<FileNode> {
        Err(missing())
    }
    async fn files_children(&self, _folder_id: &str) -> Result<Vec<FileNode>> {
        Err(missing())
    }
    async fn files_read_range(&self, _id: &str, _offset: u64, _length: u64) -> Result<Vec<u8>> {
        Err(missing())
    }
    async fn appdata_read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        let data = self.0.lock().unwrap().get(path).cloned().ok_or_else(missing)?;
        let start = (offset as usize).min(data.len());
        Ok(data[start..(start + length as usize).min(data.len())].to_vec())
    }
    async fn links_video_stream(&self, _target: LinkTarget, _mode: StreamMode) -> Result<String> {
        Err(Error::forbidden("no session").with_code("NO_SESSION"))
    }
    async fn links_upload(&self, _folder: &str, _name: &str, _overwrite: bool) -> Result<String> {
        Err(Error::forbidden("no session").with_code("NO_SESSION"))
    }
    async fn links_create(&self, targets: Vec<LinkTarget>) -> Vec<Result<String>> {
        targets.iter().map(|_| Err(Error::forbidden("no session").with_code("NO_SESSION"))).collect()
    }
    async fn files_read(&self, _id: &str, _limit: u64) -> Result<Vec<u8>> {
        Err(missing())
    }
    async fn files_write(&self, _id: &str, _data: Vec<u8>) -> Result<FileNode> {
        Err(missing())
    }
    async fn files_create_file(&self, _folder_id: &str, _name: &str, _data: Vec<u8>) -> Result<FileNode> {
        Err(missing())
    }
    async fn files_create_folder(&self, _parent_id: &str, _name: &str) -> Result<FileNode> {
        Err(missing())
    }
    async fn files_delete(&self, _id: &str) -> Result<()> {
        Err(missing())
    }
    async fn shares_received(&self) -> Result<Vec<Share>> {
        Ok(Vec::new())
    }
    async fn shared_read(&self, _share_id: &str, _path: &str, _limit: u64) -> Result<Vec<u8>> {
        Err(missing())
    }
    async fn shared_write(&self, _share_id: &str, _path: &str, _data: Vec<u8>) -> Result<AppDataEntry> {
        Err(missing())
    }
    async fn shared_list(&self, _share_id: &str, _dir: &str) -> Result<Vec<AppDataEntry>> {
        Err(missing())
    }
    async fn shared_delete(&self, _share_id: &str, _path: &str) -> Result<()> {
        Err(missing())
    }
    async fn files_open(&self, _id: &str) -> Result<std::fs::File> {
        Err(missing())
    }
    async fn appdata_open(&self, _path: &str) -> Result<std::fs::File> {
        std::fs::File::open("/dev/null").map_err(Error::internal)
    }
    async fn jobs_add(&self, _jobs: Vec<NewJob>) -> Result<()> {
        Ok(())
    }
    async fn jobs_progress(&self) -> Result<Vec<JobProgress>> {
        Ok(Vec::new())
    }
    async fn shares_leave(&self, _share_id: &str) -> Result<()> {
        Err(missing())
    }
}

fn missing() -> Error {
    Error::not_found("FILE_NOT_FOUND", "missing")
}

fn caller(username: &str, data: &Arc<MemoryData>) -> Option<Caller> {
    let user = User { id: "1".into(), username: username.into(), display_name: username.into() };
    Some(Caller { user, services: data.clone() })
}

async fn host() -> WasmHost {
    let host = WasmHost::new().unwrap();
    host.load(&echo_manifest(), &echo_wasm()).unwrap();
    host
}

/// Runs a request with the default limits.
async fn handle(host: &WasmHost, app_id: &str, caller: Option<Caller>, request: Request) -> Result<Response> {
    host.handle(app_id, caller, request, &AppConfig::default()).await
}

fn get(path: &str) -> Request {
    Request { method: "GET".into(), path: path.into(), query: String::new(), headers: Vec::new(), body: Vec::new() }
}

fn text(response: &Response) -> String {
    String::from_utf8(response.body.clone()).unwrap()
}

#[tokio::test]
async fn requests_reach_the_app_with_the_user() {
    let host = host().await;
    let data = Arc::new(MemoryData::default());
    assert_eq!(text(&handle(&host, "echo", caller("alice", &data), get("/hello")).await.unwrap()), "Hello, alice!");
    assert_eq!(text(&handle(&host, "echo", None, get("/hello")).await.unwrap()), "Hello, guest!");

    let request = Request { method: "PUT".into(), query: "a=1".into(), body: b"data".to_vec(), ..get("/echo") };
    let response = handle(&host, "echo", None, request).await.unwrap();
    assert_eq!(response.body, b"data");
    assert!(response.headers.contains(&("x-method".into(), "PUT".into())));
    assert!(response.headers.contains(&("x-query".into(), "a=1".into())));
    assert_eq!(handle(&host, "echo", None, get("/log")).await.unwrap().status, 200);
}

#[tokio::test]
async fn waiting_for_the_server_does_not_count_against_the_time_limit() {
    let host = host().await;
    let slow = Arc::new(MemoryData(Mutex::default(), Limits::default().time + Duration::from_secs(1)));
    let request = Request { method: "POST".into(), body: b"note".to_vec(), ..get("/remember") };
    let response = handle(&host, "echo", caller("alice", &slow), request).await.unwrap();
    assert_eq!(text(&response), "saved");
}

#[tokio::test]
async fn apps_see_nothing_of_the_host() {
    let host = host().await;
    assert_eq!(text(&handle(&host, "echo", None, get("/sandbox")).await.unwrap()), "env=0 file=false");
}

#[tokio::test]
async fn a_failing_app_fails_only_its_request() {
    let host = host().await;
    let err = handle(&host, "echo", None, get("/panic")).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED");

    let err = handle(&host, "echo", None, get("/memory")).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED", "more memory than allowed");

    let started = Instant::now();
    let err = handle(&host, "echo", None, get("/forever")).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED", "stopped by the time limit");
    assert!(started.elapsed() < Limits::default().time * 2);

    assert_eq!(handle(&host, "echo", None, get("/hello")).await.unwrap().status, 200, "the next request works");
    assert_eq!(handle(&host, "nope", None, get("/hello")).await.unwrap_err().code, "APP_NOT_FOUND");
}

#[tokio::test]
async fn each_app_can_have_its_own_limits() {
    let host = host().await;
    let started = Instant::now();
    let short =
        AppConfig { limits: Limits { time: Duration::from_secs(1), ..Limits::default() }, ..Default::default() };
    let err = host.handle("echo", None, get("/forever"), &short).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED");
    assert!(started.elapsed() < Duration::from_secs(5), "stopped after its own limit");

    let small = AppConfig { limits: Limits { memory: 1024 * 1024, ..Limits::default() }, ..Default::default() };
    assert_eq!(host.handle("echo", None, get("/hello"), &small).await.unwrap_err().code, "APP_FAILED");
}

#[tokio::test]
async fn apps_know_their_limits_and_get_too_large_beyond_them() {
    let host = host().await;
    let big = AppConfig { limits: Limits::scaled(4, 2), ..Default::default() };
    let said = text(&host.handle("echo", None, get("/limits"), &big).await.unwrap());
    assert_eq!(said, format!("{} {} 20000 120000", 256u64 << 20, 64u64 << 20));

    let data = Arc::new(MemoryData::default());
    data.0.lock().unwrap().insert("notes/last.txt".into(), vec![0; 2000]);
    let tiny = AppConfig { limits: Limits { data: 1000, ..Limits::default() }, ..Default::default() };
    let reply = host.handle("echo", caller("alice", &data), get("/recall"), &tiny).await.unwrap();
    assert!(text(&reply).contains("TooLarge"), "{}", text(&reply));
}

/// ffmpeg that takes its time, as with a big video or a busy queue.
struct SlowFfmpeg(Duration);

#[async_trait::async_trait]
impl UseFfmpeg for SlowFfmpeg {
    fn capabilities(&self) -> ffmpeg::Capabilities {
        ffmpeg::Capabilities {
            available: true,
            ffmpeg: None,
            image_formats: Vec::new(),
            video_codecs: Vec::new(),
            audio_codecs: Vec::new(),
        }
    }
    async fn metadata(&self, _file: std::fs::File, _background: bool) -> Result<ffmpeg::Metadata> {
        tokio::time::sleep(self.0).await;
        Err(Error::bad_request("MEDIA_FAILED", "slow"))
    }
    async fn thumbnail(
        &self,
        _file: std::fs::File,
        _options: ffmpeg::PictureOptions,
        _limit: u64,
        _background: bool,
    ) -> Result<Vec<u8>> {
        Err(Error::bad_request("MEDIA_FAILED", "slow"))
    }
}

#[tokio::test]
async fn time_in_native_tools_does_not_count_against_the_wall_limit() {
    let host = WasmHost::new().unwrap().with_ffmpeg(Arc::new(SlowFfmpeg(Duration::from_millis(1500))));
    host.load(&echo_manifest(), &echo_wasm()).unwrap();
    let limits = Limits { wall: Duration::from_secs(1), time: Duration::from_secs(1), ..Limits::default() };
    let config = AppConfig { limits, uses: vec!["ffmpeg".into()], ..Default::default() };
    let data = Arc::new(MemoryData::default());
    let request = Request { query: "photo.jpg".into(), ..get("/ffmpeg") };
    let reply = host.handle("echo", caller("alice", &data), request, &config).await.unwrap();
    assert!(text(&reply).contains("Failed"), "the app got ffmpeg's answer: {}", text(&reply));
}

/// A neural network engine whose every model doubles its inputs; it notes
/// which app asked for which model, and which apps it forgot.
#[derive(Default)]
struct DoublingOnnx {
    asked: std::sync::Mutex<Vec<String>>,
    forgotten: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl UseOnnx for DoublingOnnx {
    fn capabilities(&self) -> onnx::Capabilities {
        onnx::Capabilities { available: true, engine: Some("test".into()), keep_loaded_seconds: 300 }
    }
    fn forget(&self, app_id: &str) {
        self.forgotten.lock().unwrap().push(app_id.to_owned());
    }
    async fn model_info(&self, _app_id: &str, _model: &str, _background: bool) -> Result<onnx::ModelInfo> {
        Err(Error::bad_request("ONNX_FAILED", "not here"))
    }
    async fn run(
        &self,
        app_id: &str,
        model: &str,
        inputs: Vec<onnx::NamedTensor>,
        _limit: u64,
        _background: bool,
    ) -> Result<Vec<onnx::NamedTensor>> {
        self.asked.lock().unwrap().push(format!("{app_id}:{model}"));
        let double = |bytes: &[u8; 4]| (f32::from_le_bytes(*bytes) * 2.0).to_le_bytes();
        let input = &inputs[0].tensor;
        let data = input.data.as_chunks::<4>().0.iter().flat_map(double).collect();
        let tensor = onnx::Tensor { dtype: input.dtype, shape: input.shape.clone(), data };
        Ok(vec![onnx::NamedTensor { name: "y".into(), tensor }])
    }
    async fn image_tensor(
        &self,
        _file: std::fs::File,
        _options: onnx::ImageOptions,
        _limit: u64,
        _background: bool,
    ) -> Result<onnx::ImageInput> {
        Err(Error::bad_request("ONNX_FAILED", "not here"))
    }
    async fn audio_tensor(
        &self,
        _file: std::fs::File,
        _options: onnx::AudioOptions,
        _limit: u64,
        _background: bool,
    ) -> Result<onnx::Tensor> {
        Err(Error::bad_request("ONNX_FAILED", "not here"))
    }
}

#[tokio::test]
async fn apps_run_models_only_with_use_onnx_approved() {
    let onnx = Arc::new(DoublingOnnx::default());
    let host = WasmHost::new().unwrap().with_onnx(onnx.clone());
    host.load(&echo_manifest(), &echo_wasm()).unwrap();
    let request = || Request { query: "models/double.onnx".into(), ..get("/onnx") };

    let denied = host.handle("echo", None, request(), &AppConfig::default()).await.unwrap();
    assert_eq!(text(&denied), "Error::NotAllowed");

    let config = AppConfig { uses: vec!["onnx".into()], ..Default::default() };
    let reply = host.handle("echo", None, request(), &config).await.unwrap();
    assert_eq!(text(&reply), "y [4.0]", "works without a user too: models are the app's own");
    assert_eq!(*onnx.asked.lock().unwrap(), ["echo:models/double.onnx"]);

    host.unload("echo");
    assert_eq!(*onnx.forgotten.lock().unwrap(), ["echo"], "a removed app's models are unloaded");
}

#[tokio::test]
async fn apps_read_their_server_settings() {
    let host = host().await;
    assert_eq!(text(&handle(&host, "echo", None, get("/settings")).await.unwrap()), "{}");
    let config = AppConfig { settings: r#"{"api_key":"k"}"#.to_owned(), ..Default::default() };
    let response = host.handle("echo", None, get("/settings"), &config).await.unwrap();
    assert_eq!(text(&response), r#"{"api_key":"k"}"#);
}

/// A server on 127.0.0.1 for the apps to reach; returns its port.
async fn remote_server() -> u16 {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;
    use axum::routing::get;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new()
        .route(
            "/hello",
            get(|headers: HeaderMap| async move {
                let key = headers.get(header::AUTHORIZATION).map_or("no key", |v| v.to_str().unwrap()).to_owned();
                format!("hi, {key}")
            }),
        )
        .route("/here", get(|| async { Redirect::to("/hello") }))
        .route("/away", get(move || async move { Redirect::to(&format!("http://localhost:{port}/hello")) }))
        .route("/big", get(|| async { vec![0u8; 17 * 1024 * 1024] }));
    tokio::spawn(async move { axum::serve(listener, app).await });
    port
}

#[tokio::test]
async fn apps_reach_only_the_addresses_of_their_manifest() {
    let host = host().await;
    let port = remote_server().await;
    let fetch = |config: AppConfig, path: &str| {
        let request = Request { query: format!("http://127.0.0.1:{port}{path}"), ..get("/fetch") };
        let host = &host;
        async move { text(&host.handle("echo", None, request, &config).await.unwrap()) }
    };
    let rules = |rules: &[&str]| AppConfig {
        network: rules.iter().map(|rule| NetworkRule::parse(rule).unwrap()).collect(),
        ..Default::default()
    };
    let local = format!("http://127.0.0.1:{port}");
    let localhost = format!("http://localhost:{port}");

    assert_eq!(fetch(AppConfig::default(), "/hello").await, "Error::NotAllowed", "no addresses, no network");
    let secure = format!("https://127.0.0.1:{port}");
    assert_eq!(fetch(rules(&[&secure]), "/hello").await, "Error::NotAllowed", "https only");
    assert_eq!(fetch(rules(&[&local]), "/hello").await, "hi, Bearer secret");
    assert_eq!(fetch(rules(&[&local]), "/here").await, "hi, Bearer secret", "a redirect on the same host");
    assert_eq!(fetch(rules(&[&local]), "/away").await, "Error::NotAllowed", "a redirect elsewhere");
    // A name of the manifest that leads to this machine (its DNS could say anything) is not reached…
    let refused = fetch(rules(&[&local, &localhost]), "/away").await;
    assert!(refused.starts_with("Error::Failed") && refused.contains("private"), "{refused}");
    // …the same address set by the administrator is.
    let mut set = rules(&[&local]);
    set.network.push(NetworkRule::from_url(&localhost).unwrap());
    assert_eq!(fetch(set, "/away").await, "hi, no key", "the key stays home");
    assert_eq!(fetch(rules(&[&local]), "/big").await, "Error::TooLarge");
}

fn jobs(keys: &[&str]) -> Vec<Job> {
    keys.iter().map(|key| Job { kind: "scan".into(), key: (*key).into(), data: "d".into(), attempts: 0 }).collect()
}

#[tokio::test]
async fn apps_run_batches_of_jobs_for_a_user() {
    let host = host().await;
    assert!(host.runs_jobs("echo"));
    let data = Arc::new(MemoryData::default());
    let caller = caller("alice", &data).unwrap();
    let outcomes =
        host.run_batch("echo", caller.clone(), jobs(&["a", "fail", "again"]), &AppConfig::default()).await.unwrap();
    assert!(
        matches!(&outcomes[..], [JobOutcome::Done, JobOutcome::Failed(why), JobOutcome::Retry(_)] if why == "broken"),
        "{outcomes:?}"
    );
    assert_eq!(data.0.lock().unwrap().get("done/scan-a").map(Vec::as_slice), Some(&b"d"[..]), "as the user");

    let err = host.run_batch("echo", caller, jobs(&["a", "panic"]), &AppConfig::default()).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED");
}

#[tokio::test]
async fn long_jobs_give_way_to_other_work() {
    // One thread (the default of tokio::test): the counter only moves if the job lets it.
    let host = host().await;
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let ticking = counter.clone();
    tokio::spawn(async move {
        loop {
            ticking.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tokio::task::yield_now().await;
        }
    });
    let data = Arc::new(MemoryData::default());
    let short =
        AppConfig { limits: Limits { time: Duration::from_secs(1), ..Limits::default() }, ..Default::default() };
    let err = host.run_batch("echo", caller("alice", &data).unwrap(), jobs(&["spin"]), &short).await.unwrap_err();
    assert_eq!(err.code, "APP_FAILED", "stopped by its time limit");
    assert!(counter.load(std::sync::atomic::Ordering::Relaxed) > 5, "other work went on meanwhile");
}

#[tokio::test]
async fn apps_are_loaded_and_unloaded_while_running() {
    let host = host().await;
    host.unload("echo");
    assert!(!host.has("echo"));
    assert_eq!(handle(&host, "echo", None, get("/hello")).await.unwrap_err().code, "APP_NOT_FOUND");
    host.load(&echo_manifest(), &echo_wasm()).unwrap();
    assert_eq!(handle(&host, "echo", None, get("/hello")).await.unwrap().status, 200);
}

#[test]
fn apps_are_read_from_a_folder_named_after_them() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, id: &str| -> PathBuf {
        let app = dir.path().join(name);
        std::fs::create_dir(&app).unwrap();
        let manifest = format!("id = \"{id}\"\nversion = \"0.1.0\"\napi = \"^0.2\"\n[name]\nen = \"Echo\"\n");
        std::fs::write(app.join("manifest.toml"), manifest).unwrap();
        std::fs::write(app.join("app.wasm"), b"\0asm").unwrap();
        app
    };
    let (manifest, wasm) = read_app_dir(&write("echo", "echo")).unwrap();
    assert_eq!(manifest.id, "echo");
    assert_eq!(wasm, b"\0asm");
    assert!(read_app_dir(&write("other", "echo")).unwrap_err().contains("named after the app id"));
}

#[tokio::test]
async fn invalid_components_are_refused() {
    let host = WasmHost::new().unwrap();
    assert!(host.load(&echo_manifest(), b"not wasm").is_err());
    assert!(!host.has("echo"));
}

#[tokio::test]
async fn apps_keep_data_for_the_user_between_requests() {
    let host = host().await;
    let data = Arc::new(MemoryData::default());
    let remember = Request { method: "POST".into(), body: b"milk".to_vec(), ..get("/remember") };
    assert_eq!(text(&handle(&host, "echo", caller("alice", &data), remember.clone()).await.unwrap()), "saved");
    assert_eq!(text(&handle(&host, "echo", caller("alice", &data), get("/recall")).await.unwrap()), "milk");

    let other = Arc::new(MemoryData::default());
    assert_eq!(text(&handle(&host, "echo", caller("bob", &other), get("/recall")).await.unwrap()), "Error::NotFound");
    assert_eq!(text(&handle(&host, "echo", None, remember).await.unwrap()), "Error::NoUser", "no user, no AppData");
}
