//! `/app/<id>/…`: requests to WASM apps.
//!
//! The app sees the method, the path below `/app/<id>`, the query, the body
//! and the headers — except `Cookie` and `Authorization`, so it never gets the
//! user's session. It gets the user instead, if the request has a session.
//! Whatever the app answers is served with a CSP `sandbox`, so its pages never
//! run with the cloud's origin, and it can't set cookies. Its pages may be
//! framed by the cloud's own pages only (the panel shows them in an <iframe>).
//!
//! An answer with `x-rustcloud-send` asks the server to send a file instead
//! of its body ([`app_links`](crate::app_links)).

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Request as HttpRequest, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::any;
use axum::Router;
use rustcloud_auth::{basic_credentials, from_a_web_page, Access, CurrentUser};
use rustcloud_core::{audit, ClientInfo, Error, Result};
use rustcloud_files::{Files, Locking};
use rustcloud_plugin_api::Images;
use rustcloud_wasm_host::{
    AppDataEntry, AppToken, Caller, FileLock, FileNode, FileVersion, JobProgress, LinkTarget, NewJob, Request, Share,
    StreamMode, TokenScope, User, UserServices, BODY_SIZE_HEADER, SEND_HEADER,
};
use uuid::Uuid;

use crate::app_bodies::{Keep, RequestBody};
use crate::app_links::{AppLinks, Target};
use crate::app_tokens::{token_of, TokenClaims, TOKEN_MAX_SECONDS};
use crate::plugin_jobs::Jobs;
use crate::AppState;

/// The Content-Security-Policy of an app's responses. In the browser its
/// page runs scripts and forms in a unique origin, nothing with the cloud's
/// own, and may be framed only by the cloud. It loads and sends things only
/// to this server, as `permissions.network` promises the administrator for
/// the app's code: anything else could carry the user's data away. The
/// exceptions are what the manifest asks for: the services the panel shows
/// for it (`frames`, from `permissions.frame`) and, with
/// `permissions.images = "any"`, pictures from any site.
fn page_policy(frames: &[String], images: Images) -> String {
    let frames: String = frames.iter().map(|origin| format!(" {origin}")).collect();
    let pictures = match images {
        Images::Own => "",
        Images::Any => " *",
    };
    format!(
        "sandbox allow-scripts allow-forms allow-downloads; \
         default-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' data: blob:; \
         img-src 'self' data: blob:{pictures}; frame-src 'self' data: blob:{frames}; \
         form-action 'self'{frames}; base-uri 'self'; frame-ancestors 'self'"
    )
}

/// Whether an app may send the browser to `location`: somewhere on this
/// server (a relative address) or to a service it may show (`frames`).
/// Anything else would make the cloud's address a way to other sites, the
/// kind of link people trust (phishing).
fn redirect_allowed(location: &str, frames: &[String]) -> bool {
    // Browsers skip spaces around it and tabs in it, read `\` as `/`, and
    // take `//host` for another host.
    let path: String =
        location.trim().chars().filter(|c| *c != '\t').map(|c| if c == '\\' { '/' } else { c }).collect();
    let first = path.split(['/', '?', '#']).next().unwrap_or_default();
    let relative = !path.starts_with("//") && !first.contains(':');
    relative || frames.iter().any(|origin| path == *origin || path.starts_with(&format!("{origin}/")))
}

/// The largest body of a request without a user: anyone may send those.
const ANONYMOUS_BODY: usize = 256 << 10;

/// Headers of an app's answer that reach the browser; `x-…` ones of its own
/// too (but not those in [`FORBIDDEN_X`]). Anything else — `Clear-Site-Data`,
/// `Strict-Transport-Security`, CORS, `Service-Worker-Allowed`, `Refresh`,
/// `Link` — could act on the whole cloud, not just the app's page.
const ANSWER_HEADERS: &[&str] = &[
    "accept-ranges",
    "allow",
    "cache-control",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-range",
    "content-type",
    "dav",
    "etag",
    "expires",
    "last-modified",
    "location",
    "lock-token",
    "ms-author-via",
    "pragma",
    "preference-applied",
    "retry-after",
    "schedule-tag",
    "vary",
    "www-authenticate",
];

/// `x-…` headers an app may not set: the server's own say.
const FORBIDDEN_X: &[&str] = &["x-frame-options", "x-content-type-options", "x-xss-protection"];

/// Whether an app's answer may carry this header.
fn answer_header_allowed(name: &HeaderName) -> bool {
    let name = name.as_str();
    ANSWER_HEADERS.contains(&name) || (name.starts_with("x-") && !FORBIDDEN_X.contains(&name))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/app/{id}", any(app_root))
        .route("/app/{id}/", any(app_root))
        .route("/app/{id}/{*path}", any(app_path))
        // Each app's own limit applies (`forward`).
        .layer(DefaultBodyLimit::disable())
}

async fn app_root(
    State(state): State<AppState>,
    client: ClientInfo,
    Path(id): Path<String>,
    request: HttpRequest,
) -> Result<HttpResponse> {
    forward(&state, client, &id, String::new(), request).await
}

async fn app_path(
    State(state): State<AppState>,
    client: ClientInfo,
    Path((id, path)): Path<(String, String)>,
    request: HttpRequest,
) -> Result<HttpResponse> {
    forward(&state, client, &id, path, request).await
}

async fn forward(
    state: &AppState,
    client: ClientInfo,
    id: &str,
    path: String,
    request: HttpRequest,
) -> Result<HttpResponse> {
    let (mut parts, body) = request.into_parts();
    // A service worker would be the app's script working for the whole site.
    if parts.headers.contains_key("service-worker") {
        return Err(Error::forbidden("Apps can't install service workers").with_code("NO_SERVICE_WORKER"));
    }
    let authorization = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let token = token_of(parts.uri.query(), authorization);
    let services = if let Some(token) = token {
        match token_services(state, id, &token).await? {
            Some(services) => Some(services),
            None => return Err(Error::unauthorized("The token is not valid or has run out")),
        }
    } else {
        match basic_credentials(&parts.headers) {
            // A browser keeps Basic credentials it was given and sends them
            // with requests other sites make: not for changes.
            Some(_) if !reads_only(&parts.method) && from_a_web_page(&parts.headers) => {
                return Err(Error::forbidden("App passwords don't work from web pages").with_code("CSRF_CHECK_FAILED"))
            }
            Some((username, password)) => {
                match app_password_services(state, client, id, &parts.method, &username, &password).await? {
                    Some(services) => Some(services),
                    None => return Ok(basic_challenge()),
                }
            }
            // A link or form of another site: its cookie doesn't act for the
            // user, even on GET (CSRF), whatever the app does with it.
            None if from_another_site(&parts.headers) && !parts.headers.contains_key(header::AUTHORIZATION) => None,
            // No session is fine (the app decides); a failed check (e.g. CSRF) is not.
            None => match CurrentUser::from_request_parts(&mut parts, state).await {
                Ok(current) => {
                    let session = Some(current.0.session_id);
                    Some(services_for(
                        &state.files,
                        &state.jobs,
                        &state.links,
                        current.user().clone(),
                        session,
                        id,
                        false,
                    ))
                }
                Err(err) if err.code == "UNAUTHORIZED" => None,
                Err(err) => return Err(err),
            },
        }
    };
    if state.plugins.get(id).is_none() || !state.plugins.host().has(id) {
        return Err(Error::not_found("APP_NOT_FOUND", format!("There is no app \"{id}\"")));
    }

    // The app gets the whole body in memory: at most what it may take at
    // once. One that saves larger ones into files gets those kept on disk,
    // for a user who may change files.
    let mut config = state.plugins.config(id);
    config.self_url = state.proxy.self_url(&state.plugins, id).await;
    let mut in_memory = usize::try_from(config.limits.data).unwrap_or(usize::MAX);
    if services.is_none() {
        in_memory = in_memory.min(ANONYMOUS_BODY);
    }
    // A kept body goes into a file of the user's: at most what the user may
    // still upload (the limit, the quota and the disk's free space).
    let large = state.plugins.get(id).is_some_and(|app| app.large_bodies);
    let keep = match services.as_ref().filter(|services| large && !services.read_only) {
        Some(services) => Some(Keep { dir: &state.links.bodies, max: state.files.upload_room(&services.user).await? }),
        None => None,
    };
    let body = RequestBody::receive(body, in_memory, keep).await?;
    // A token of another server reaches only its own file.
    let token_file = services.as_ref().and_then(|services| services.token.as_ref().map(|token| token.f));
    let request = to_app_request(&parts.method, &parts.uri, &parts.headers, path, &body);
    let caller = services.map(|mut services| {
        services.body = Some(body);
        services.into_caller()
    });
    let user_id = caller.as_ref().and_then(|caller| Uuid::parse_str(&caller.user.id).ok());
    let images = state.plugins.get(id).map(|app| app.permissions.images).unwrap_or_default();
    let frames = state.plugins.frame_origins(id);
    let response = state.plugins.host().handle(id, caller, request, &config).await?;
    if let Some((_, send)) = response.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case(SEND_HEADER)) {
        let target = Target::from_header(send)?;
        if let (Target::File(file), Some(allowed)) = (&target, token_file) {
            if *file != allowed {
                return Err(Error::not_found("FILE_NOT_FOUND", "File not found"));
            }
        }
        let user = match user_id {
            Some(user_id) => Some(state.auth.find_user(user_id).await?),
            None => None,
        };
        let sent = state.links.send(user.as_ref(), id, &target, &parts.headers).await?;
        if let Target::File(_) = target {
            let file = state.links.file(user.as_ref(), id, &target).await?;
            let user = user.as_ref().map(|u| u.username.as_str());
            audit!("file.download", name = file.name, size = file.size, app = id, user, "file sent for an app");
        }
        return Ok(with_app_headers(sent, &response.headers));
    }
    Ok(from_app_response(response, &frames, images))
}

/// A file sent for an app, with the headers the app may set on it
/// (`Content-Disposition`, `Cache-Control`) and its page's sandbox.
fn with_app_headers(mut sent: HttpResponse, app: &[(String, String)]) -> HttpResponse {
    let headers = sent.headers_mut();
    for (name, value) in app {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name.as_str()), HeaderValue::try_from(value.as_str())) else {
            continue;
        };
        if name == header::CONTENT_DISPOSITION || name == header::CACHE_CONTROL {
            headers.insert(name, value);
        }
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    sent
}

/// Whether the browser says the request comes from another site (a link,
/// a form, a picture there) rather than from the cloud's own pages or the
/// address bar. `same-site` is another site too: a neighbouring subdomain.
fn from_another_site(headers: &HeaderMap) -> bool {
    let site = headers.get("sec-fetch-site").and_then(|value| value.to_str().ok());
    matches!(site, Some("cross-site" | "same-site"))
}

/// The caller of a request with an app password. `None`: wrong credentials.
/// A password that doesn't open this app, or opens it read-only and the
/// method changes something, is refused.
async fn app_password_services(
    state: &AppState,
    client: ClientInfo,
    id: &str,
    method: &Method,
    username: &str,
    password: &str,
) -> Result<Option<ServicesFor>> {
    let Some(found) = state.auth.app_password_user(username, password, client.ip).await? else {
        return Ok(None);
    };
    tracing::Span::current().record("user", found.user.username.as_str());
    let read_only = match found.access(id) {
        None => {
            return Err(Error::forbidden("This app password does not open this app").with_code("APP_PASSWORD_SCOPE"))
        }
        Some(Access::Read) if !reads_only(method) => {
            return Err(Error::forbidden("This app password may only read").with_code("READ_ONLY"))
        }
        Some(access) => access == Access::Read,
    };
    Ok(Some(services_for(&state.files, &state.jobs, &state.links, found.user, None, id, read_only)))
}

/// The user an app works for, with what the server does for it on the
/// user's behalf.
pub(crate) fn caller_for(
    files: &Arc<Files>,
    jobs: &Arc<Jobs>,
    links: &Arc<AppLinks>,
    user: rustcloud_auth::User,
    session: Option<Uuid>,
    app_id: &str,
    read_only: bool,
) -> Caller {
    services_for(files, jobs, links, user, session, app_id, read_only).into_caller()
}

fn services_for(
    files: &Arc<Files>,
    jobs: &Arc<Jobs>,
    links: &Arc<AppLinks>,
    user: rustcloud_auth::User,
    session: Option<Uuid>,
    app_id: &str,
    read_only: bool,
) -> ServicesFor {
    ServicesFor {
        files: files.clone(),
        jobs: jobs.clone(),
        user,
        links: links.clone(),
        session,
        app_id: app_id.to_owned(),
        read_only,
        token: None,
        body: None,
        reads: Mutex::default(),
    }
}

/// The caller of a request from another server with a token of the app: the
/// user it was given for, reaching only its file. `None`: no such token, or
/// it no longer works.
async fn token_services(state: &AppState, id: &str, token: &str) -> Result<Option<ServicesFor>> {
    let Some(claims) = state.links.tokens.check(token).filter(|claims| claims.a == id) else {
        return Ok(None);
    };
    let Ok(user) = state.auth.find_user(claims.u).await else {
        return Ok(None);
    };
    if user.disabled || !state.files.grant_covers(&user, id, claims.g, claims.f).await? {
        return Ok(None);
    }
    if let Some(session) = claims.s {
        if state.auth.session_by_id(session).await?.is_none() {
            return Ok(None);
        }
    }
    tracing::Span::current().record("user", user.username.as_str());
    let mut services = services_for(&state.files, &state.jobs, &state.links, user, None, id, !claims.w);
    services.token = Some(claims);
    Ok(Some(services))
}

/// Methods that only read, in HTTP and in WebDAV (CalDAV, CardDAV).
fn reads_only(method: &Method) -> bool {
    matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS" | "PROPFIND" | "REPORT")
}

/// 401 that asks for Basic credentials, so DAV clients ask the user for an app password.
fn basic_challenge() -> HttpResponse {
    let mut response = Error::unauthorized("Wrong username or app password").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"RustCloud\", charset=\"UTF-8\""));
    response
}

fn to_app_request(method: &Method, uri: &Uri, headers: &HeaderMap, path: String, body: &RequestBody) -> Request {
    let mut headers: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| *name != header::COOKIE && *name != header::AUTHORIZATION)
        // Only the server says a body was kept.
        .filter(|(name, _)| name.as_str() != BODY_SIZE_HEADER)
        .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
        .collect();
    if let Some(size) = body.kept_size() {
        headers.push((BODY_SIZE_HEADER.to_owned(), size.to_string()));
    }
    Request {
        method: method.as_str().to_owned(),
        path: format!("/{path}"),
        query: uri.query().unwrap_or_default().to_owned(),
        headers,
        body: body.for_app(),
    }
}

/// An app's answer as the browser gets it: with the headers it may set, and
/// the policy of its page (`frames`, `images`: see [`page_policy`]).
fn from_app_response(response: rustcloud_wasm_host::Response, frames: &[String], images: Images) -> HttpResponse {
    let mut http = HttpResponse::new(Body::from(response.body));
    *http.status_mut() = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = http.headers_mut();
    for (name, value) in response.headers {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) else {
            continue;
        };
        if name == header::LOCATION && !redirect_allowed(value.to_str().unwrap_or_default(), frames) {
            continue;
        }
        if answer_header_allowed(&name) {
            headers.append(name, value);
        }
    }
    // Origins in the policy are checked hosts and ports; without them, the
    // strictest policy there is.
    let policy = HeaderValue::try_from(page_policy(frames, images))
        .unwrap_or(HeaderValue::from_static("sandbox; default-src 'none'"));
    headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    http
}

/// What the server does for one app on behalf of one user.
struct ServicesFor {
    files: Arc<Files>,
    jobs: Arc<Jobs>,
    links: Arc<AppLinks>,
    user: rustcloud_auth::User,
    /// The browser session of the request; none for app passwords and jobs.
    session: Option<Uuid>,
    app_id: String,
    /// Logged in with a read-only app password, or a token to read a file.
    read_only: bool,
    /// The token of a request from another server: only its file.
    token: Option<TokenClaims>,
    /// The body of the request being answered; none for jobs.
    body: Option<RequestBody>,
    reads: Mutex<Reads>,
}

/// The user's files an app read in one request or batch, written to the
/// journal as one line when it ends: indexing a library reads thousands.
#[derive(Debug, Default)]
struct Reads {
    count: u64,
    bytes: u64,
    /// The first file's name, for a request that read one.
    first: Option<String>,
}

impl Drop for ServicesFor {
    fn drop(&mut self) {
        let reads = self.reads.get_mut().unwrap_or_else(|e| e.into_inner());
        let (app, user, size) = (self.app_id.as_str(), self.user.username.as_str(), reads.bytes);
        match (reads.count, reads.first.take()) {
            (0, _) => {}
            (1, Some(name)) => audit!("file.download", name, size, app, user, "file read by an app"),
            (count, _) => audit!("file.download", count, size, app, user, "files read by an app"),
        }
    }
}

impl ServicesFor {
    fn into_caller(self) -> Caller {
        let user = User {
            id: self.user.id.to_string(),
            username: self.user.username.clone(),
            display_name: self.user.display_name.clone(),
        };
        Caller { user, services: Arc::new(self) }
    }

    fn read(&self, file: &rustcloud_files::Node) {
        let mut reads = self.reads.lock().unwrap_or_else(|e| e.into_inner());
        reads.count += 1;
        reads.bytes += file.size.max(0) as u64;
        reads.first.get_or_insert_with(|| file.name.clone());
    }

    /// A file id of the app's request; with a token, only the token's file.
    fn file_id(&self, id: &str) -> Result<Uuid> {
        let id = node_id(id)?;
        match &self.token {
            Some(token) if token.f != id => Err(Error::not_found("FILE_NOT_FOUND", "File not found")),
            _ => Ok(id),
        }
    }

    fn check_write(&self) -> Result<()> {
        if self.read_only {
            return Err(Error::forbidden("This app password may only read").with_code("READ_ONLY"));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl UserServices for ServicesFor {
    async fn appdata_read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        self.files.appdata_read(&self.user, &self.app_id, path, limit).await
    }

    async fn appdata_write(&self, path: &str, data: Vec<u8>) -> Result<AppDataEntry> {
        self.check_write()?;
        self.files.appdata_write(&self.user, &self.app_id, path, data).await.map(appdata_entry)
    }

    async fn appdata_list(&self, dir: &str) -> Result<Vec<AppDataEntry>> {
        let nodes = self.files.appdata_list(&self.user, &self.app_id, dir).await?;
        Ok(nodes.into_iter().map(appdata_entry).collect())
    }

    async fn appdata_delete(&self, path: &str) -> Result<()> {
        self.check_write()?;
        self.files.appdata_delete(&self.user, &self.app_id, path).await
    }

    async fn files_granted(&self) -> Result<Vec<FileNode>> {
        if let Some(token) = &self.token {
            return Ok(vec![self.files.app_get(&self.user, &self.app_id, token.f).await.map(file_node)?]);
        }
        Ok(self.files.app_granted(&self.user, &self.app_id).await?.into_iter().map(file_node).collect())
    }

    async fn files_get(&self, id: &str) -> Result<FileNode> {
        self.files.app_get(&self.user, &self.app_id, self.file_id(id)?).await.map(file_node)
    }

    async fn files_children(&self, folder_id: &str) -> Result<Vec<FileNode>> {
        let nodes = self.files.app_children(&self.user, &self.app_id, self.file_id(folder_id)?).await?;
        Ok(nodes.into_iter().map(file_node).collect())
    }

    async fn files_read_range(&self, id: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        let (data, file) =
            self.files.app_read_range(&self.user, &self.app_id, self.file_id(id)?, offset, length).await?;
        self.read(&file);
        Ok(data)
    }

    async fn appdata_read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        self.files.appdata_read_range(&self.user, &self.app_id, path, offset, length).await
    }

    async fn links_create(&self, targets: Vec<LinkTarget>) -> Vec<Result<String>> {
        let Some(session) = self.session else {
            let no_session = || Error::forbidden("Links are for pages in a browser session").with_code("NO_SESSION");
            return targets.iter().map(|_| Err(no_session())).collect();
        };
        let mut links = Vec::with_capacity(targets.len());
        for target in targets {
            let user_file = matches!(target, LinkTarget::File(_));
            let link = self.links.create(session, &self.user, &self.app_id, target).await;
            links.push(link.map(|(link, file)| {
                if user_file {
                    self.read(&file);
                }
                link
            }));
        }
        links
    }

    async fn links_video_stream(&self, target: LinkTarget, mode: StreamMode) -> Result<String> {
        let session = self.session.ok_or_else(|| Error::forbidden("Links are for pages").with_code("NO_SESSION"))?;
        let mode = match mode {
            StreamMode::Copy => rustcloud_media::StreamMode::Copy,
            StreamMode::H264 => rustcloud_media::StreamMode::H264,
        };
        let user_file = matches!(target, LinkTarget::File(_));
        let (link, file) = self.links.create_stream(session, &self.user, &self.app_id, target, mode).await?;
        if user_file {
            self.read(&file);
        }
        Ok(link)
    }

    async fn links_upload(&self, folder: &str, name: &str, overwrite: bool) -> Result<String> {
        let session = self.session.ok_or_else(|| Error::forbidden("Links are for pages").with_code("NO_SESSION"))?;
        self.check_write()?;
        self.links.create_upload(session, &self.user, &self.app_id, self.file_id(folder)?, name, overwrite).await
    }

    async fn files_read(&self, id: &str, limit: u64) -> Result<Vec<u8>> {
        let (data, file) = self.files.app_read(&self.user, &self.app_id, self.file_id(id)?, limit).await?;
        self.read(&file);
        Ok(data)
    }

    async fn files_write(&self, id: &str, data: Vec<u8>) -> Result<FileNode> {
        self.check_write()?;
        self.files.app_write(&self.user, &self.app_id, self.file_id(id)?, data, None).await.map(file_node)
    }

    async fn files_write_if_match(&self, id: &str, data: Vec<u8>, if_match: &str) -> Result<FileNode> {
        self.check_write()?;
        let id = self.file_id(id)?;
        self.files.app_write(&self.user, &self.app_id, id, data, Some(if_match)).await.map(file_node)
    }

    async fn files_write_request_body(&self, id: &str, if_match: Option<&str>) -> Result<FileNode> {
        self.check_write()?;
        let id = self.file_id(id)?;
        let body = self.body.as_ref().ok_or_else(|| Error::bad_request("NO_REQUEST_BODY", "Not in a request"))?;
        let body = body.stream().await?;
        self.files.app_replace(&self.user, &self.app_id, id, body, if_match).await.map(file_node)
    }

    async fn files_lock(
        &self,
        id: &str,
        lock_id: &str,
        seconds: u32,
    ) -> Result<std::result::Result<FileLock, FileLock>> {
        self.check_write()?;
        Ok(match self.files.app_lock(&self.user, &self.app_id, self.file_id(id)?, lock_id, seconds).await? {
            Locking::Held(lock) => Ok(file_lock(lock)),
            Locking::Taken(lock) => Err(file_lock(lock)),
        })
    }

    async fn files_unlock(&self, id: &str, lock_id: &str) -> Result<Option<FileLock>> {
        self.check_write()?;
        Ok(self.files.app_unlock(&self.user, &self.app_id, self.file_id(id)?, lock_id).await?.map(file_lock))
    }

    async fn files_get_lock(&self, id: &str) -> Result<Option<FileLock>> {
        Ok(self.files.app_get_lock(&self.user, &self.app_id, self.file_id(id)?).await?.map(file_lock))
    }

    async fn files_versions(&self, id: &str) -> Result<Vec<FileVersion>> {
        let versions = self.files.app_versions(&self.user, &self.app_id, self.file_id(id)?).await?;
        Ok(versions
            .into_iter()
            .map(|v| FileVersion {
                id: v.id.to_string(),
                size: v.size.max(0) as u64,
                etag: v.etag,
                modified_at: v.modified_at.to_rfc3339(),
                created_at: v.created_at.to_rfc3339(),
            })
            .collect())
    }

    async fn files_read_version(&self, id: &str, version: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        let version = Uuid::parse_str(version)
            .map_err(|_| Error::not_found("VERSION_NOT_FOUND", "This version no longer exists"))?;
        let (user, app) = (&self.user, &self.app_id);
        self.files.app_read_version(user, app, self.file_id(id)?, version, offset, length).await
    }

    async fn tokens_issue(&self, file: &str, write: bool, seconds: u32) -> Result<AppToken> {
        if self.token.is_some() {
            return Err(Error::forbidden("A token can't give tokens").with_code("NO_USER"));
        }
        if write {
            self.check_write()?;
        }
        let file = self.file_id(file)?;
        let Some(grant) = self.files.grant_for(&self.user, &self.app_id, file, write).await? else {
            let readable = write && self.files.grant_for(&self.user, &self.app_id, file, false).await?.is_some();
            return Err(match readable {
                true => Error::forbidden("The app may only read this").with_code("READ_ONLY"),
                false => Error::not_found("FILE_NOT_FOUND", "File not found"),
            });
        };
        let seconds = seconds.clamp(1, TOKEN_MAX_SECONDS);
        let claims = TokenClaims {
            a: self.app_id.clone(),
            u: self.user.id,
            g: grant,
            f: file,
            w: write,
            x: chrono::Utc::now().timestamp() + i64::from(seconds),
            s: self.session,
        };
        let token = self.links.tokens.issue(&claims)?;
        Ok(AppToken { token, expires_at: claims.expires_at().to_rfc3339() })
    }

    fn token_scope(&self) -> Option<TokenScope> {
        self.token.as_ref().map(|token| TokenScope { file: token.f.to_string(), write: token.w })
    }

    async fn links_replace(&self, file: &str, if_match: Option<&str>) -> Result<String> {
        let session = self.session.ok_or_else(|| Error::forbidden("Links are for pages").with_code("NO_SESSION"))?;
        self.check_write()?;
        self.links.create_replace(session, &self.user, &self.app_id, self.file_id(file)?, if_match).await
    }

    async fn files_create_file(&self, folder_id: &str, name: &str, data: Vec<u8>) -> Result<FileNode> {
        self.check_write()?;
        let folder = self.file_id(folder_id)?;
        self.files.app_create_file(&self.user, &self.app_id, folder, name, data).await.map(file_node)
    }

    async fn files_create_folder(&self, parent_id: &str, name: &str) -> Result<FileNode> {
        self.check_write()?;
        let parent = self.file_id(parent_id)?;
        self.files.app_create_folder(&self.user, &self.app_id, parent, name).await.map(file_node)
    }

    async fn files_delete(&self, id: &str) -> Result<()> {
        self.check_write()?;
        self.files.app_delete(&self.user, &self.app_id, self.file_id(id)?).await
    }

    async fn shares_received(&self) -> Result<Vec<Share>> {
        let shares = self.files.received_appdata_shares(&self.user, &self.app_id).await?;
        Ok(shares
            .into_iter()
            .map(|share| Share {
                id: share.id.to_string(),
                owner: User {
                    id: share.owner.id.to_string(),
                    username: share.owner.username,
                    display_name: share.owner.display_name,
                },
                path: share.path,
                label: share.label,
                write: share.write,
            })
            .collect())
    }

    async fn shared_read(&self, share_id: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
        self.files.shared_read(&self.user, &self.app_id, share_uuid(share_id)?, path, limit).await
    }

    async fn shared_write(&self, share_id: &str, path: &str, data: Vec<u8>) -> Result<AppDataEntry> {
        self.check_write()?;
        let share = share_uuid(share_id)?;
        self.files.shared_write(&self.user, &self.app_id, share, path, data).await.map(appdata_entry)
    }

    async fn shared_list(&self, share_id: &str, dir: &str) -> Result<Vec<AppDataEntry>> {
        let nodes = self.files.shared_list(&self.user, &self.app_id, share_uuid(share_id)?, dir).await?;
        Ok(nodes.into_iter().map(appdata_entry).collect())
    }

    async fn shared_delete(&self, share_id: &str, path: &str) -> Result<()> {
        self.check_write()?;
        self.files.shared_delete(&self.user, &self.app_id, share_uuid(share_id)?, path).await
    }

    async fn shares_leave(&self, share_id: &str) -> Result<()> {
        self.check_write()?;
        let share = share_uuid(share_id)?;
        // Only shares of this app: the app can't end the user's shares in other apps.
        if !self.files.received_appdata_shares(&self.user, &self.app_id).await?.iter().any(|s| s.id == share) {
            return Err(Error::not_found("SHARE_NOT_FOUND", "This share does not exist or has ended"));
        }
        self.files.revoke_appdata_share(&self.user, share).await
    }

    async fn jobs_add(&self, jobs: Vec<NewJob>) -> Result<()> {
        self.check_write()?;
        self.jobs.add(&self.app_id, self.user.id, jobs).await
    }

    async fn jobs_progress(&self) -> Result<Vec<JobProgress>> {
        self.jobs.progress(&self.app_id, self.user.id).await
    }

    async fn files_open(&self, id: &str) -> Result<std::fs::File> {
        let (opened, file) = self.files.app_open(&self.user, &self.app_id, self.file_id(id)?).await?;
        self.read(&file);
        Ok(opened)
    }

    async fn appdata_open(&self, path: &str) -> Result<std::fs::File> {
        self.files.appdata_open(&self.user, &self.app_id, path).await
    }
}

/// A share id from an app; anything that isn't a UUID is simply not found.
fn share_uuid(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error::not_found("SHARE_NOT_FOUND", "This share does not exist or has ended"))
}

/// A node id from an app; anything that isn't a UUID is simply not found.
fn node_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error::not_found("FILE_NOT_FOUND", "File not found"))
}

fn appdata_entry(node: rustcloud_files::Node) -> AppDataEntry {
    AppDataEntry {
        name: node.name,
        is_dir: node.is_dir,
        size: node.size.max(0) as u64,
        etag: node.etag.unwrap_or_default(),
        updated_at: node.updated_at.to_rfc3339(),
    }
}

fn file_lock(lock: rustcloud_files::FileLock) -> FileLock {
    FileLock {
        lock_id: lock.lock_id,
        app_id: lock.app_id,
        user_id: lock.user_id.to_string(),
        expires_at: lock.expires_at.to_rfc3339(),
    }
}

fn file_node(node: rustcloud_files::Node) -> FileNode {
    FileNode {
        id: node.id.to_string(),
        parent_id: node.parent_id.map(|id| id.to_string()),
        name: node.name,
        is_dir: node.is_dir,
        size: node.size.max(0) as u64,
        mime_type: node.mime_type,
        etag: node.etag,
        updated_at: node.updated_at.to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_of_web_pages_are_told_apart_from_dav_clients() {
        let headers = |pairs: &[(&str, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(HeaderName::try_from(*name).unwrap(), HeaderValue::from_str(value).unwrap());
            }
            map
        };
        assert!(!from_a_web_page(&headers(&[("user-agent", "DAVx5"), ("content-type", "text/calendar")])));
        assert!(!from_a_web_page(&headers(&[("sec-fetch-site", "none")])), "typed into the address bar");
        assert!(from_a_web_page(&headers(&[("origin", "https://evil.example")])));
        assert!(from_a_web_page(&headers(&[("sec-fetch-site", "cross-site")])));
    }

    #[test]
    fn apps_redirect_only_within_the_server_or_to_their_services() {
        let frames = ["https://office.example".to_owned()];
        for local in ["/app/notes/", "edit?id=1", "../x", "?page=2", "#top"] {
            assert!(redirect_allowed(local, &frames), "{local}");
        }
        assert!(redirect_allowed("https://office.example/cool/x", &frames), "a service it may show");
        for away in [
            "https://evil.example/",
            "//evil.example/",
            "/\\evil.example/",
            "\\\\evil.example",
            " //evil.example",
            "/\t/evil.example",
            "javascript:alert(1)",
            "https://office.example.evil.example/",
        ] {
            assert!(!redirect_allowed(away, &frames), "{away:?}");
        }
    }

    #[test]
    fn apps_answer_only_with_headers_for_their_own_pages() {
        let allowed = |name: &str| answer_header_allowed(&HeaderName::try_from(name).unwrap());
        for name in ["content-type", "etag", "dav", "location", "x-request-id", "cache-control"] {
            assert!(allowed(name), "{name}");
        }
        for name in [
            "set-cookie",
            "content-security-policy",
            "x-frame-options",
            "clear-site-data",
            "strict-transport-security",
            "access-control-allow-origin",
            "service-worker-allowed",
            "refresh",
            "link",
        ] {
            assert!(!allowed(name), "{name}");
        }
    }
}
