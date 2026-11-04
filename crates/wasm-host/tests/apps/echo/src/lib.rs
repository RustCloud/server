//! Test app: each path exercises one thing the host must get right.

wit_bindgen::generate!({ path: "../../../../plugin-api/wit", world: "app-with-jobs" });

use rustcloud::app::jobs::NewJob;
use rustcloud::app::{
    appdata, files, http, jobs, limits, links, log, proxy, settings, storage, tokens, use_ffmpeg, use_onnx,
};

struct Echo;

impl Guest for Echo {
    /// Each job's key says how it goes: `fail`, `again`, `panic`, `spin`
    /// (forever); anything else is done, and remembered in AppData.
    fn run_batch(_user: User, jobs: Vec<Job>) -> Vec<Outcome> {
        jobs.into_iter()
            .map(|job| match job.key.as_str() {
                "fail" => Outcome::Failed("broken".to_owned()),
                "again" => Outcome::Retry("busy".to_owned()),
                "panic" => panic!("on purpose"),
                "spin" => loop {
                    std::hint::black_box(());
                },
                key => {
                    let _ = appdata::write(&format!("done/{}-{key}", job.kind), job.data.as_bytes());
                    Outcome::Done
                }
            })
            .collect()
    }

    fn handle(user: Option<User>, request: Request) -> Response {
        match request.path.as_str() {
            "/" => Response {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/html; charset=utf-8".to_owned())],
                body: PAGE.as_bytes().to_vec(),
            },
            "/settings" => text(200, settings::values()),
            "/self-url" => text(200, proxy::self_url().unwrap_or_else(|| "none".to_owned())),
            // Adds a job of kind "scan" for each line of the body.
            "/jobs" => {
                let body = String::from_utf8(request.body).unwrap_or_default();
                let new: Vec<NewJob> = body
                    .lines()
                    .map(|key| NewJob { kind: "scan".to_owned(), key: key.to_owned(), data: "d".to_owned() })
                    .collect();
                match jobs::add(&new) {
                    Ok(()) => text(200, "added"),
                    Err(err) => text(400, format!("{err:?}")),
                }
            }
            // Asks the server to send what the query names: `file:<id>`, `appdata:<path>`…
            "/send" => Response {
                status: 200,
                headers: vec![("x-rustcloud-send".to_owned(), request.query.clone())],
                body: Vec::new(),
            },
            // A link for the page to what the query names.
            "/link" => {
                let target = match request.query.split_once(':') {
                    Some(("file", id)) => links::Target::File(id.to_owned()),
                    Some(("appdata", path)) => links::Target::Appdata(path.to_owned()),
                    _ => links::Target::Storage(request.query.trim_start_matches("storage:").to_owned()),
                };
                match links::create(&[target]).remove(0) {
                    Ok(link) => text(200, link),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // A link to upload `<name>` into the folder `<id>`: `<id>:<name>`;
            // `<id>:<name>:overwrite` may replace a file.
            "/upload" => {
                let mut parts = request.query.splitn(3, ':');
                let (folder, name) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
                match links::upload(folder, name, parts.next() == Some("overwrite")) {
                    Ok(link) => text(200, link),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // A token for another server: `<file id>:rw` to change the file, `<file id>:r` to read it.
            "/token" => {
                let (file, rights) = request.query.split_once(':').unwrap_or_default();
                match tokens::issue(file, rights == "rw", 600) {
                    Ok(token) => text(200, token.token),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // A call of another server with a token, WOPI-like: `?file=<id>&access_token=…`.
            // GET: the token's file, whether it may write, and the content;
            // POST: locks the file and writes the body.
            "/wopi" => {
                let file = request.query.split('&').find_map(|p| p.strip_prefix("file=")).unwrap_or_default();
                let Some(scope) = tokens::current() else { return text(401, "no token") };
                if request.method == "POST" {
                    let written = files::lock(file, "L1", 60)
                        .map_err(|err| format!("{err:?}"))
                        .and_then(|_| files::write(file, &request.body).map_err(|err| format!("{err:?}")));
                    return match written {
                        Ok(node) => text(200, node.etag.unwrap_or_default()),
                        Err(err) => text(500, err),
                    };
                }
                match files::read(file) {
                    Ok(data) => text(200, format!("{} {} {}", scope.file == file, scope.write, String::from_utf8_lossy(&data))),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // Saves the request's body into the file `?<id>` or `?<id>:<etag>`,
            // streamed by the server: the body size it saw (`-` if it got the
            // body itself) and the new ETag.
            "/save" => {
                let (file, etag) = match request.query.split_once(':') {
                    Some((file, etag)) => (file, Some(etag)),
                    None => (request.query.as_str(), None),
                };
                let kept = request.headers.iter().find(|(name, _)| name == "x-rustcloud-body-size");
                let kept = kept.map_or("-".to_owned(), |(_, size)| size.clone());
                match files::write_request_body(file, etag) {
                    Ok(node) => text(200, format!("{kept} {} {}", request.body.len(), node.etag.unwrap_or_default())),
                    Err(err) => text(409, format!("{err:?}")),
                }
            }
            // An HLS stream of a granted file: `copy:<id>` or `h264:<id>`.
            "/stream" => {
                let (mode, id) = request.query.split_once(':').unwrap_or_default();
                let mode = if mode == "h264" { links::StreamMode::H264 } else { links::StreamMode::Copy };
                match links::video_stream(&links::Target::File(id.to_owned()), mode) {
                    Ok(link) => text(200, link),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // `<file id>:<offset>:<length>` of a granted file: its bytes.
            "/range" => {
                let mut parts = request.query.split(':');
                let (id, offset, length) = (parts.next().unwrap_or(""), parts.next(), parts.next());
                let number = |part: Option<&str>| part.and_then(|p| p.parse().ok()).unwrap_or(0);
                match files::read_range(id, number(offset), number(length)) {
                    Ok(data) => Response { status: 200, headers: Vec::new(), body: data },
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // What this call may use: memory, data, time, wall.
            "/limits" => {
                let l = limits::get_limits();
                text(200, format!("{} {} {} {}", l.memory_bytes, l.data_bytes, l.time_ms, l.wall_ms))
            }
            // ffmpeg on a file of AppData: its container, or why not.
            "/ffmpeg" => match use_ffmpeg::get_metadata(&use_ffmpeg::Source::Appdata(request.query)) {
                Ok(metadata) => text(200, metadata.container),
                Err(err) => text(500, format!("{err:?}")),
            },
            // The model named by the query on one f32 input `x` = [2.0]:
            // the first output's name and values, or why not.
            "/onnx" => {
                let x = use_onnx::Tensor {
                    dtype: use_onnx::TensorType::F32,
                    shape: vec![1],
                    data: 2.0f32.to_le_bytes().to_vec(),
                };
                let inputs = [use_onnx::NamedTensor { name: "x".to_owned(), tensor: x }];
                match use_onnx::run(&request.query, &inputs) {
                    Ok(outputs) => {
                        let output = &outputs[0];
                        let values: Vec<f32> = output
                            .tensor
                            .data
                            .chunks_exact(4)
                            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                            .collect();
                        text(200, format!("{} {values:?}", output.name))
                    }
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // A file of AppData as a model's input: the tensor's shape, the
            // picture's size and the first value, or why not.
            "/onnx-image" => {
                let options = use_onnx::ImageOptions {
                    width: 32,
                    height: 32,
                    fit: use_onnx::ImageFit::Inside,
                    crop: None,
                    channels: use_onnx::Channels::Rgb,
                    layout: use_onnx::Layout::Nchw,
                    mean: (0.0, 0.0, 0.0),
                    std: (255.0, 255.0, 255.0),
                };
                match use_onnx::image_tensor(&use_onnx::Source::Appdata(request.query), options) {
                    Ok(image) => {
                        let first = f32::from_le_bytes(image.tensor.data[..4].try_into().unwrap());
                        text(200, format!("{:?} {}x{} {first:.3}", image.tensor.shape, image.width, image.height))
                    }
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // Half a second of a file of AppData's sound: its samples.
            "/onnx-audio" => {
                let options = use_onnx::AudioOptions { rate: 16000, start_ms: 0, duration_ms: Some(500) };
                match use_onnx::audio_tensor(&use_onnx::Source::Appdata(request.query), options) {
                    Ok(samples) => text(200, format!("{:?}", samples.shape)),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            // What a done job of kind "scan" with this key was given.
            "/done" => match appdata::read(&format!("done/scan-{}", request.query)) {
                Ok(data) => Response { status: 200, headers: Vec::new(), body: data },
                Err(err) => text(404, format!("{err:?}")),
            },
            "/progress" => match jobs::progress() {
                Ok(kinds) => {
                    let lines: Vec<String> =
                        kinds.iter().map(|k| format!("{}: {} waiting, {} failed", k.kind, k.waiting, k.failed)).collect();
                    text(200, lines.join("\n"))
                }
                Err(err) => text(500, format!("{err:?}")),
            },
            // The app's storage, at the path in the query string.
            "/store" => match storage::write(&request.query, b"kept") {
                Ok(file) => text(200, file.sha256),
                Err(err) => text(500, format!("{err:?}")),
            },
            "/stored" => match storage::read(&request.query, 0, u64::MAX) {
                Ok(data) => Response { status: 200, headers: Vec::new(), body: data },
                Err(err) => text(404, format!("{err:?}")),
            },
            // GET the URL in the query string, with a key the server must not
            // pass on to another host.
            "/fetch" => {
                let outgoing = http::Request {
                    method: "GET".to_owned(),
                    url: request.query,
                    headers: vec![("authorization".to_owned(), "Bearer secret".to_owned())],
                    body: Vec::new(),
                };
                match http::fetch(&outgoing) {
                    Ok(answer) => Response { status: answer.status, headers: Vec::new(), body: answer.body },
                    Err(err) => text(502, format!("{err:?}")),
                }
            }
            "/hello" => text(200, format!("Hello, {}!", user.map_or("guest".to_owned(), |u| u.username))),
            "/echo" => Response {
                status: 200,
                headers: vec![("x-method".to_owned(), request.method), ("x-query".to_owned(), request.query)],
                body: request.body,
            },
            "/headers" => {
                let names: Vec<String> = request.headers.into_iter().map(|(name, _)| name).collect();
                text(200, names.join(","))
            }
            "/cookie" => Response {
                status: 200,
                headers: vec![("set-cookie".to_owned(), "rustcloud_session=stolen".to_owned())],
                body: Vec::new(),
            },
            "/remember" => match appdata::write("notes/last.txt", &request.body) {
                Ok(_) => text(200, "saved"),
                Err(err) => text(500, format!("{err:?}")),
            },
            "/recall" => match appdata::read("notes/last.txt") {
                Ok(data) => Response { status: 200, headers: Vec::new(), body: data },
                Err(err) => text(404, format!("{err:?}")),
            },
            "/granted" => match files::granted() {
                Ok(nodes) => text(200, nodes.into_iter().map(|n| n.name).collect::<Vec<_>>().join(",")),
                Err(err) => text(500, format!("{err:?}")),
            },
            // Reads every file of the first granted folder; says how many.
            "/read-all" => {
                let read = files::granted().and_then(|granted| {
                    let folder = granted.into_iter().find(|n| n.is_dir).ok_or(files::Error::NotFound)?;
                    let children = files::children(&folder.id)?;
                    children.iter().filter(|n| !n.is_dir).map(|n| files::read(&n.id)).collect::<Result<Vec<_>, _>>()
                });
                match read {
                    Ok(all) => text(200, all.len().to_string()),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            "/read-first" => {
                let read = files::granted().and_then(|granted| {
                    let folder = granted.into_iter().find(|n| n.is_dir).ok_or(files::Error::NotFound)?;
                    let file = files::children(&folder.id)?.into_iter().find(|n| !n.is_dir);
                    files::read(&file.ok_or(files::Error::NotFound)?.id)
                });
                match read {
                    Ok(data) => Response { status: 200, headers: Vec::new(), body: data },
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            "/create" => {
                let created = files::granted().and_then(|granted| {
                    let folder = granted.into_iter().find(|n| n.is_dir).ok_or(files::Error::NotFound)?;
                    files::create_file(&folder.id, "from-echo.txt", &request.body)
                });
                match created {
                    Ok(node) => text(200, node.name),
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            "/get" => match files::get(&request.query) {
                Ok(node) => text(200, node.name),
                Err(err) => text(404, format!("{err:?}")),
            },
            "/log" => {
                log::warn("something looks odd");
                text(200, "logged")
            }
            "/panic" => panic!("on purpose"),
            "/forever" => {
                let mut n = 0u64;
                loop {
                    n = std::hint::black_box(n.wrapping_add(1));
                }
            }
            "/memory" => {
                let big = vec![1u8; 256 * 1024 * 1024];
                text(200, std::hint::black_box(big).len().to_string())
            }
            "/sandbox" => {
                let env = std::env::vars().count();
                let file = std::fs::read_to_string("/etc/passwd").is_ok();
                text(200, format!("env={env} file={file}"))
            }
            _ => text(404, "not found"),
        }
    }
}

/// A page for the panel's <iframe>: asks the app's own route through the bridge.
const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Echo</title>
<p id="out">…</p>
<p id="bytes">…</p>
<button id="ask">Ask for a folder</button>
<p id="folder"></p>
<script src="/app-sdk.js"></script>
<script>
  (async () => {
    const context = await rustcloud.context;
    const reply = await rustcloud.fetch('/hello');
    const blocked = await rustcloud.fetch('/../../api/v1/auth/me').then(() => 'reached', () => 'blocked');
    document.getElementById('out').textContent = `${await reply.text()} ${context.locale} api:${blocked}`;
    // Bytes there and back through the bridge, a Blob with its type too.
    const sent = new Uint8Array([0, 255, 1, 128]);
    const echoed = await rustcloud.fetch('/echo', { method: 'PUT', body: sent });
    const back = new Uint8Array(await echoed.arrayBuffer());
    const blob = await rustcloud.fetch('/echo', { method: 'PUT', body: new Blob([sent], { type: 'image/png' }) });
    document.getElementById('bytes').textContent = `bytes: ${Array.from(back).join(',')} blob: ${(await blob.blob()).size}`;
  })();
  document.getElementById('ask').onclick = async () => {
    const out = document.getElementById('folder');
    try {
      const folder = await rustcloud.requestAccess({ folder: 'Echo', write: true });
      const granted = await (await rustcloud.fetch('/granted')).text();
      out.textContent = `got ${folder.name}, granted: ${granted}`;
    } catch (e) {
      out.textContent = `refused: ${e.message}`;
    }
  };
</script>
"#;

fn text(status: u16, body: impl Into<String>) -> Response {
    Response {
        status,
        headers: vec![("content-type".to_owned(), "text/plain; charset=utf-8".to_owned())],
        body: body.into().into_bytes(),
    }
}

export!(Echo);
