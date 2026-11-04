//! Services of plugins shown through the server's own address (`[[proxy]]`
//! in the manifest): an office editor such as Collabora gets paths of the
//! server (`/browser`, `/cool`), so the browser reaches it, WebSocket
//! included, without an address of its own.
//!
//! The service runs on the server's origin, so the administrator trusts it as
//! part of the server when approving the plugin. What reaches it is kept
//! narrow:
//!
//! - **who and when:** a browser session (the cookie, not a token) of a user
//!   who has a file open in the plugin now (a session grant); a WebSocket ends
//!   once that file is closed;
//! - **where:** only the paths the manifest lists, the rest of the service is
//!   out of reach;
//! - **from where:** WebSocket and requests that change something must come
//!   from the server's own pages (`Origin`), instead of the `X-Requested-With`
//!   check the service's pages can't make;
//! - **how much:** connections and requests at a time per user and on the
//!   whole server, the size of a body, waiting times (`[plugins]` settings);
//! - the server's credentials (its session cookie, `Authorization`) never
//!   reach the service, and the service can't set the server's cookie.
//!
//! The service calls the plugin back itself (WOPI). How it reaches the server
//! is found out here ([`ServiceProxy::self_url`]): the server's own address on
//! a connection to the service. The plugin gets it through its `proxy`
//! import; the network guard accepts that address from the service.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{StatusCode, Version};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use hyper_util::rt::TokioIo;
use rustcloud_auth::{Auth, User, SESSION_COOKIE};
use rustcloud_core::{ClientInfo, Error, ErrorKind, Live, Result};
use rustcloud_files::Files;
use rustcloud_plugin_api::proxy_path_matches;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use url::Url;
use uuid::Uuid;

use crate::network::InnerHosts;
use crate::plugins::{PluginSettings, Plugins, ProxyRoute};
use crate::AppState;

/// How long connecting to the service may take.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// How long the service may take to start its answer.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// How long a whole answer may take (saving a large document).
const WHOLE_ANSWER: Duration = Duration::from_secs(600);
/// A WebSocket without traffic for this long is closed (the service pings
/// its own every few seconds while a document is open).
const IDLE: Duration = Duration::from_secs(300);
/// How often a WebSocket checks that its file is still open and the
/// connection not idle.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// How long a user's open file counts as checked.
const OPEN_CHECKED: Duration = Duration::from_secs(10);
/// How long the service's way back to the server counts as known.
const SELF_URL_CHECKED: Duration = Duration::from_secs(60);

/// Headers of one connection only, never passed on (RFC 9110, 7.6.1).
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
];

/// Proxies requests to the services of plugins and keeps their limits.
pub struct ServiceProxy {
    client: reqwest::Client,
    settings: Arc<Live<PluginSettings>>,
    /// Where this server listens: the port of [`Self::self_url`].
    listen: SocketAddr,
    /// Requests and WebSocket connections under way, by user.
    load: Mutex<HashMap<Uuid, Load>>,
    /// Users with a file open in a plugin, by user and plugin: when checked.
    open: Mutex<HashMap<(Uuid, String), Instant>>,
    /// The service's way back to the server, by plugin.
    self_urls: Mutex<HashMap<String, SelfUrl>>,
    inner_hosts: Arc<InnerHosts>,
}

#[derive(Debug, Default)]
struct Load {
    requests: u32,
    connections: u32,
}

#[derive(Debug, Clone)]
struct SelfUrl {
    /// The service's origin it was found for.
    target: String,
    url: Option<String>,
    checked: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Request,
    Connection,
}

/// A request or a WebSocket under way: counted until dropped.
struct Slot {
    proxy: Arc<ServiceProxy>,
    user: Uuid,
    kind: Kind,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut load = self.proxy.load.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(user) = load.get_mut(&self.user) {
            match self.kind {
                Kind::Request => user.requests = user.requests.saturating_sub(1),
                Kind::Connection => user.connections = user.connections.saturating_sub(1),
            }
            if user.requests == 0 && user.connections == 0 {
                load.remove(&self.user);
            }
        }
    }
}

impl ServiceProxy {
    pub fn new(settings: Arc<Live<PluginSettings>>, listen: SocketAddr, inner_hosts: Arc<InnerHosts>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_WAIT)
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .http1_only()
            .build()
            .map_err(Error::internal)?;
        Ok(Self {
            client,
            settings,
            listen,
            load: Mutex::default(),
            open: Mutex::default(),
            self_urls: Mutex::default(),
            inner_hosts,
        })
    }

    /// The inner addresses of services, for the network guard.
    pub fn inner_hosts(&self) -> Arc<InnerHosts> {
        self.inner_hosts.clone()
    }

    /// Takes a place for a request or a WebSocket of `user`, or says there
    /// are too many.
    fn take(self: &Arc<Self>, user: Uuid, kind: Kind) -> Result<Slot> {
        let (per_user, total) = self.settings.read(|s| match kind {
            Kind::Request => (s.proxy_requests_per_user, None),
            Kind::Connection => (s.proxy_connections_per_user, Some(s.proxy_connections)),
        });
        let mut load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        let connections: u32 = load.values().map(|l| l.connections).sum();
        let mine = load.entry(user).or_default();
        let used = match kind {
            Kind::Request => &mut mine.requests,
            Kind::Connection => &mut mine.connections,
        };
        if *used >= per_user || total.is_some_and(|total| connections >= total) {
            return Err(Error::new(
                ErrorKind::TooManyRequests,
                "TOO_MANY_REQUESTS",
                "Too many requests to the service",
            ));
        }
        *used += 1;
        Ok(Slot { proxy: self.clone(), user, kind })
    }

    /// Whether the user has a file open in the plugin; a yes counts for a
    /// few seconds, as a page loads many files at once.
    async fn has_open_file(&self, files: &Files, user: &User, app_id: &str) -> Result<bool> {
        let key = (user.id, app_id.to_owned());
        let fresh = |checked: &Instant| checked.elapsed() < OPEN_CHECKED;
        if self.open.lock().unwrap_or_else(|e| e.into_inner()).get(&key).is_some_and(fresh) {
            return Ok(true);
        }
        let open = files.has_open_file(user, app_id).await?;
        let mut checked = self.open.lock().unwrap_or_else(|e| e.into_inner());
        checked.retain(|_, at| fresh(at));
        if open {
            checked.insert(key, Instant::now());
        }
        Ok(open)
    }

    /// How the service of the plugin reaches this server, e.g.
    /// `http://172.19.0.3:8080`: the server's own address on a connection to
    /// the service, with the port it listens on. None without a service, or
    /// when the server listens on another address than the one that
    /// connection leaves from. While the service doesn't answer, the address
    /// found before stays.
    pub async fn self_url(&self, plugins: &Plugins, app_id: &str) -> Option<String> {
        let route = plugins.proxies().into_iter().find(|route| route.app_id == app_id)?;
        let known = self.self_urls.lock().unwrap_or_else(|e| e.into_inner()).get(app_id).cloned();
        let known = known.filter(|known| known.target == route.target);
        if let Some(known) = known.as_ref().filter(|known| known.checked.elapsed() < SELF_URL_CHECKED) {
            return known.url.clone();
        }
        let url = match self.find_self_url(&route.target).await {
            Some((url, service)) => {
                let host = url.trim_start_matches("http://").to_owned();
                self.inner_hosts.set(app_id, service, host);
                Some(url)
            }
            None => known.and_then(|known| known.url),
        };
        let found = SelfUrl { target: route.target, url: url.clone(), checked: Instant::now() };
        self.self_urls.lock().unwrap_or_else(|e| e.into_inner()).insert(app_id.to_owned(), found);
        url
    }

    /// Connects to the service: this server's address on the connection and
    /// the service's.
    async fn find_self_url(&self, target: &str) -> Option<(String, IpAddr)> {
        let url = Url::parse(target).ok()?;
        let host = url.host_str()?.trim_start_matches('[').trim_end_matches(']').to_owned();
        let port = url.port_or_known_default()?;
        let stream =
            tokio::time::timeout(CONNECT_WAIT, tokio::net::TcpStream::connect((host, port))).await.ok()?.ok()?;
        let local = stream.local_addr().ok()?.ip();
        let service = stream.peer_addr().ok()?.ip();
        if !self.listen.ip().is_unspecified() && self.listen.ip() != local {
            return None;
        }
        let host = match local {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        Some((format!("http://{host}:{}", self.listen.port()), service))
    }

    /// Passes a request on and its answer back, both as streams.
    async fn forward(self: &Arc<Self>, user: Uuid, route: &ProxyRoute, request: Request) -> Result<Response> {
        let slot = self.take(user, Kind::Request)?;
        let max_body = u64::from(self.settings.read(|s| s.proxy_body_mib)) * 1024 * 1024;
        let (parts, body) = request.into_parts();
        let declared = parts.headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        if declared.is_some_and(|size| size > max_body) {
            return Err(Error::new(
                ErrorKind::PayloadTooLarge,
                "TOO_LARGE",
                "The request is too large for the service",
            ));
        }
        let has_body = declared.is_some_and(|size| size > 0) || parts.headers.contains_key(header::TRANSFER_ENCODING);
        let client = parts.extensions.get::<ClientInfo>().copied().unwrap_or_default();
        let mut sent = self
            .client
            .request(parts.method.clone(), target_url(route, &parts.uri))
            .headers(request_headers(&parts.headers, client));
        if has_body {
            sent = sent.body(reqwest::Body::wrap_stream(limited(body.into_data_stream(), max_body)));
        }
        let answer = self.answer(route, sent).await?;
        let status = answer.status();
        let headers = response_headers(answer.headers(), &route.target, &own_origin(&parts.headers, client));
        let mut response = Response::new(Body::from_stream(until(answer.bytes_stream(), slot)));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(response)
    }

    /// Sends a request to the service and waits for the start of its answer.
    async fn answer(&self, route: &ProxyRoute, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        match tokio::time::timeout(ANSWER_WAIT, request.send()).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(err)) => {
                tracing::warn!("the service of plugin \"{}\" at {} failed: {err}", route.app_id, route.target);
                Err(Error::new(ErrorKind::BadGateway, "SERVICE_UNREACHABLE", "The service doesn't answer"))
            }
            Err(_) => {
                tracing::warn!("the service of plugin \"{}\" at {} took too long", route.app_id, route.target);
                Err(Error::new(ErrorKind::GatewayTimeout, "SERVICE_TIMEOUT", "The service took too long to answer"))
            }
        }
    }

    /// A WebSocket: the service agrees to it first, then bytes go both ways
    /// as they are, until a side closes it, it is idle, or the user's file
    /// is closed.
    async fn tunnel(
        self: &Arc<Self>,
        files: Arc<Files>,
        user: User,
        route: ProxyRoute,
        mut request: Request,
    ) -> Result<Response> {
        let slot = self.take(user.id, Kind::Connection)?;
        let browser = request
            .extensions_mut()
            .remove::<hyper::upgrade::OnUpgrade>()
            .ok_or_else(|| Error::bad_request("NO_UPGRADE", "This connection can't become a WebSocket"))?;
        let (parts, _) = request.into_parts();
        let client = parts.extensions.get::<ClientInfo>().copied().unwrap_or_default();
        let mut headers = request_headers(&parts.headers, client);
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        let sent = self.client.get(target_url(&route, &parts.uri)).version(Version::HTTP_11).headers(headers);
        let answer = self.answer(&route, sent).await?;
        let mut headers = response_headers(answer.headers(), &route.target, &own_origin(&parts.headers, client));
        if answer.status() != StatusCode::SWITCHING_PROTOCOLS {
            // The service said no (a wrong document, a full server): its answer as it is.
            let status = answer.status();
            let mut response = Response::new(Body::from_stream(until(answer.bytes_stream(), slot)));
            *response.status_mut() = status;
            *response.headers_mut() = headers;
            return Ok(response);
        }
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        let proxy = self.clone();
        tokio::spawn(async move {
            let (service, browser) = tokio::join!(answer.upgrade(), browser);
            match (service, browser) {
                (Ok(service), Ok(browser)) => {
                    proxy.carry(TokioIo::new(browser), service, &files, &user, &route.app_id).await;
                }
                (Err(err), _) => tracing::debug!("a WebSocket to the service failed: {err}"),
                (_, Err(err)) => tracing::debug!("a WebSocket from the browser failed: {err}"),
            }
            drop(slot);
        });
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        *response.headers_mut() = headers;
        Ok(response)
    }

    /// Carries the bytes of a WebSocket both ways while it is allowed to last.
    async fn carry<A, B>(&self, browser: A, service: B, files: &Files, user: &User, app_id: &str)
    where
        A: AsyncRead + AsyncWrite + Unpin,
        B: AsyncRead + AsyncWrite + Unpin,
    {
        let started = Instant::now();
        let last = Arc::new(AtomicU64::new(0));
        let mut browser = Watched { inner: browser, started, last: last.clone() };
        let mut service = Watched { inner: service, started, last: last.clone() };
        let watch = async {
            loop {
                tokio::time::sleep(CHECK_EVERY).await;
                let idle = started.elapsed().saturating_sub(Duration::from_millis(last.load(Ordering::Relaxed)));
                if idle >= IDLE {
                    tracing::debug!("a WebSocket to the service of plugin \"{app_id}\" was idle");
                    return;
                }
                if !self.has_open_file(files, user, app_id).await.unwrap_or(true) {
                    tracing::debug!("a WebSocket to the service of plugin \"{app_id}\" ended with its file");
                    return;
                }
            }
        };
        tokio::select! {
            _ = tokio::io::copy_bidirectional(&mut browser, &mut service) => {}
            () = watch => {}
        }
    }
}

/// Middleware: requests for the paths of plugins' services go to them;
/// everything else goes on.
pub async fn route(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let found = state
        .plugins
        .proxies()
        .into_iter()
        .find(|route| route.paths.iter().any(|prefix| proxy_path_matches(prefix, path)));
    let Some(route) = found else {
        return next.run(request).await;
    };
    let started = Instant::now();
    let method = request.method().clone();
    let response = match handle(&state, route.clone(), request).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    };
    // The query is never logged: it carries the service's access tokens.
    tracing::debug!(
        app = route.app_id,
        "{method} via the proxy: {} in {} ms",
        response.status().as_u16(),
        started.elapsed().as_millis()
    );
    response
}

async fn handle(state: &AppState, route: ProxyRoute, request: Request) -> Result<Response> {
    let user = session_user(&state.auth, request.headers()).await?;
    let websocket = is_websocket(request.headers());
    if (websocket || !request.method().is_safe()) && !from_own_page(request.headers()) {
        return Err(Error::forbidden("Only the server's own pages may do this").with_code("ORIGIN_NOT_ALLOWED"));
    }
    if !state.proxy.has_open_file(&state.files, &user, &route.app_id).await? {
        return Err(Error::forbidden("Open a file in the app first").with_code("NO_OPEN_FILE"));
    }
    if websocket {
        state.proxy.tunnel(state.files.clone(), user, route, request).await
    } else {
        state.proxy.forward(user.id, &route, request).await
    }
}

/// The user of the browser session (the cookie only: the service's pages
/// are what calls), or 401.
async fn session_user(auth: &Auth, headers: &HeaderMap) -> Result<User> {
    let token = cookies(headers).find(|(name, _)| *name == SESSION_COOKIE).map(|(_, value)| value);
    let token = token.ok_or_else(|| Error::unauthorized("Not logged in"))?;
    let session = auth
        .session_user(token)
        .await?
        .ok_or_else(|| Error::unauthorized("Session has expired, please log in again"))?;
    tracing::Span::current().record("user", session.user.username.as_str());
    Ok(session.user)
}

/// The `name=value` pairs of the request's `Cookie` headers.
fn cookies(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
}

fn is_websocket(headers: &HeaderMap) -> bool {
    let has = |name: HeaderName, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    };
    has(header::CONNECTION, "upgrade") && has(header::UPGRADE, "websocket")
}

/// Whether the request comes from a page of this server: its `Origin` names
/// the host it was sent to. Browsers always send it with WebSocket and
/// with requests that change something.
fn from_own_page(headers: &HeaderMap) -> bool {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).and_then(|v| Url::parse(v).ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()?.parse::<axum::http::uri::Authority>().ok());
    let (Some(origin), Some(host)) = (origin, host) else { return false };
    let port = origin.port_or_known_default();
    origin.host_str().is_some_and(|name| name.eq_ignore_ascii_case(host.host())) && host.port_u16().or(port) == port
}

/// The service's address for a request: its origin, then the path and the
/// query as the browser sent them (not decoded).
fn target_url(route: &ProxyRoute, uri: &axum::http::Uri) -> String {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    format!("{}{path}", route.target)
}

/// The server's origin as the browser sees it: `https://cloud.example.com`.
fn own_origin(headers: &HeaderMap, client: ClientInfo) -> String {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    format!("{}://{host}", if client.https { "https" } else { "http" })
}

/// Names of the headers of one connection: the fixed ones and those the
/// `Connection` header lists.
fn connection_headers(headers: &HeaderMap) -> Vec<String> {
    let listed = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase());
    HOP_BY_HOP.iter().map(|name| (*name).to_owned()).chain(["upgrade".to_owned()]).chain(listed).collect()
}

/// The browser's headers for the service: without those of the connection,
/// without the server's credentials, with where it really came from.
fn request_headers(headers: &HeaderMap, client: ClientInfo) -> HeaderMap {
    let mut out = HeaderMap::new();
    let dropped = connection_headers(headers);
    let forwarded = ["forwarded", "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host", "x-real-ip"];
    for (name, value) in headers {
        let name_text = name.as_str();
        if dropped.iter().any(|d| d == name_text)
            || forwarded.contains(&name_text)
            || name == header::AUTHORIZATION
            || name == header::COOKIE
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    // The service's own cookies go on; the server's session doesn't.
    let others: Vec<String> = cookies(headers)
        .filter(|(name, _)| *name != SESSION_COOKIE)
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    if !others.is_empty() {
        if let Ok(value) = HeaderValue::try_from(others.join("; ")) {
            out.insert(header::COOKIE, value);
        }
    }
    if let Some(ip) = client.ip.and_then(|ip| HeaderValue::try_from(ip.to_string()).ok()) {
        out.insert(HeaderName::from_static("x-forwarded-for"), ip);
    }
    let proto = HeaderValue::from_static(if client.https { "https" } else { "http" });
    out.insert(HeaderName::from_static("x-forwarded-proto"), proto);
    if let Some(host) = headers.get(header::HOST) {
        out.insert(HeaderName::from_static("x-forwarded-host"), host.clone());
    }
    out
}

/// The service's headers for the browser: without those of the connection,
/// without cookies it may not set, redirects to itself pointing at the
/// server. It may be framed by the server's pages (the panel shows it).
fn response_headers(headers: &HeaderMap, target: &str, own: &str) -> HeaderMap {
    let mut out = HeaderMap::new();
    let dropped = connection_headers(headers);
    for (name, value) in headers {
        if dropped.iter().any(|d| d == name.as_str()) {
            continue;
        }
        if name == header::SET_COOKIE && !allowed_cookie(value) {
            continue;
        }
        if name == header::LOCATION {
            let location = value.to_str().ok().and_then(|text| text.strip_prefix(target));
            if let Some(rest) = location.filter(|rest| rest.is_empty() || rest.starts_with('/')) {
                if let Ok(value) = HeaderValue::try_from(format!("{own}{rest}")) {
                    out.append(name.clone(), value);
                    continue;
                }
            }
        }
        out.append(name.clone(), value.clone());
    }
    if !out.contains_key(header::X_FRAME_OPTIONS) {
        out.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    }
    out
}

/// Whether the service may set this cookie: not the server's session, and
/// only for this host (no `Domain`: not for the server's neighbours).
fn allowed_cookie(value: &HeaderValue) -> bool {
    let Ok(text) = value.to_str() else { return false };
    let mut parts = text.split(';');
    let name = parts.next().and_then(|pair| pair.split_once('=')).map(|(name, _)| name.trim()).unwrap_or_default();
    let has_domain =
        parts.any(|attribute| attribute.split('=').next().is_some_and(|key| key.trim().eq_ignore_ascii_case("domain")));
    !name.is_empty() && name != SESSION_COOKIE && !has_domain
}

/// The body of a request, ending with an error once it carries more than `max` bytes.
fn limited(
    body: impl Stream<Item = std::result::Result<Bytes, axum::Error>> + Send + 'static,
    max: u64,
) -> impl Stream<Item = io::Result<Bytes>> + Send + 'static {
    let mut seen = 0u64;
    body.map(move |chunk| {
        let chunk = chunk.map_err(io::Error::other)?;
        seen += chunk.len() as u64;
        if seen > max {
            return Err(io::Error::other("the request is too large for the service"));
        }
        Ok(chunk)
    })
}

/// The service's answer, ended after [`WHOLE_ANSWER`]; `slot` stays taken
/// until it has gone through.
fn until(
    answer: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    slot: Slot,
) -> impl Stream<Item = io::Result<Bytes>> + Send + 'static {
    let deadline = tokio::time::Instant::now() + WHOLE_ANSWER;
    futures_util::stream::unfold(Some((Box::pin(answer), slot)), move |state| async move {
        let (mut answer, slot) = state?;
        match tokio::time::timeout_at(deadline, answer.next()).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), Some((answer, slot)))),
            Ok(Some(Err(err))) => Some((Err(io::Error::other(err)), None)),
            Ok(None) => None,
            Err(_) => Some((Err(io::Error::other("the service took too long")), None)),
        }
    })
}

/// A side of a WebSocket that notes when bytes last came from it.
struct Watched<T> {
    inner: T,
    started: Instant,
    /// Milliseconds after `started`.
    last: Arc<AtomicU64>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Watched<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.last.store(now, Ordering::Relaxed);
        }
        polled
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Watched<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(HeaderName::try_from(*name).unwrap(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn the_service_gets_no_credentials_of_the_server() {
        let browser = headers(&[
            ("host", "cloud.example.com"),
            ("cookie", "theme=dark; rustcloud_session=secret; cool_id=7"),
            ("authorization", "Bearer secret"),
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("x-forwarded-for", "6.6.6.6"),
            ("accept", "text/html"),
        ]);
        let client = ClientInfo { ip: Some("10.0.0.5".parse().unwrap()), https: true };
        let sent = request_headers(&browser, client);
        assert_eq!(sent["cookie"], "theme=dark; cool_id=7");
        assert!(!sent.contains_key("authorization"));
        assert!(!sent.contains_key("x-private"), "listed in Connection");
        assert!(!sent.contains_key("connection"));
        assert_eq!(sent["host"], "cloud.example.com", "the service builds its addresses from it");
        assert_eq!(sent["x-forwarded-for"], "10.0.0.5", "the client as the server knows it, not as it claims");
        assert_eq!(sent["x-forwarded-proto"], "https");
        assert_eq!(sent["x-forwarded-host"], "cloud.example.com");
        assert_eq!(sent["accept"], "text/html");

        let only_session = request_headers(&headers(&[("cookie", "rustcloud_session=secret")]), client);
        assert!(!only_session.contains_key("cookie"));
    }

    #[test]
    fn the_service_can_not_set_the_servers_cookies() {
        let answer = headers(&[
            ("set-cookie", "rustcloud_session=forged; Path=/"),
            ("set-cookie", "wide=1; Domain=example.com"),
            ("set-cookie", "cool_id=7; Path=/cool; HttpOnly"),
            ("location", "http://collabora:9980/browser/dist/x.html?a=1"),
            ("transfer-encoding", "chunked"),
            ("content-security-policy", "frame-ancestors cloud.example.com"),
        ]);
        let out = response_headers(&answer, "http://collabora:9980", "https://cloud.example.com");
        let cookies: Vec<&str> = out.get_all("set-cookie").iter().map(|v| v.to_str().unwrap()).collect();
        assert_eq!(cookies, ["cool_id=7; Path=/cool; HttpOnly"]);
        assert_eq!(out["location"], "https://cloud.example.com/browser/dist/x.html?a=1");
        assert!(!out.contains_key("transfer-encoding"));
        assert_eq!(out["content-security-policy"], "frame-ancestors cloud.example.com", "the service's own");
        assert_eq!(out["x-frame-options"], "SAMEORIGIN", "the panel frames it");

        let elsewhere = response_headers(
            &headers(&[("location", "http://collabora:99800/x")]),
            "http://collabora:9980",
            "https://c",
        );
        assert_eq!(elsewhere["location"], "http://collabora:99800/x", "another server is left alone");
    }

    #[test]
    fn only_the_servers_own_pages_count() {
        let check = |origin: &str, host: &str| from_own_page(&headers(&[("origin", origin), ("host", host)]));
        assert!(check("https://cloud.example.com", "cloud.example.com"));
        assert!(check("http://192.168.1.5:8080", "192.168.1.5:8080"));
        assert!(check("https://cloud.example.com", "cloud.example.com:443"));
        assert!(!check("https://evil.example", "cloud.example.com"));
        assert!(!check("http://192.168.1.5:8081", "192.168.1.5:8080"));
        assert!(!check("null", "cloud.example.com"), "a sandboxed page");
        assert!(!from_own_page(&headers(&[("host", "cloud.example.com")])), "no Origin");
    }

    #[test]
    fn a_websocket_is_recognised() {
        assert!(is_websocket(&headers(&[("connection", "keep-alive, Upgrade"), ("upgrade", "WebSocket")])));
        assert!(!is_websocket(&headers(&[("connection", "upgrade"), ("upgrade", "h2c")])));
        assert!(!is_websocket(&headers(&[("upgrade", "websocket")])));
    }
}
