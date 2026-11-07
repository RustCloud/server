//! WebDAV (RFC 4918) over the user's files: `/dav/` is their root folder.
//! File managers of the OS (Windows, macOS Finder, GNOME, KDE), davfs2,
//! rclone and mobile apps mount or sync it.
//!
//! Clients log in with HTTP Basic: the username and an app password with
//! access to `files` (read, or read and write). Client apps that hold an
//! access token send it as `Bearer`. The session cookie of the panel is not
//! accepted: a browser would send it with requests other sites make.
//!
//! What differs from a full WebDAV server:
//! - DELETE moves to the trash, as in the panel; the trash folder itself is
//!   not shown;
//! - `Depth: infinity` of PROPFIND is answered as `Depth: 1`;
//! - locks (class 2, which Finder and Windows need to write) are granted but
//!   not kept: anyone may still change a "locked" file;
//! - PROPPATCH sets only the modification time (`getlastmodified`, Windows'
//!   `Win32LastModifiedTime`); Windows' other properties are accepted and
//!   dropped, other properties are refused;
//! - `X-OC-Mtime` of PUT (Unix seconds, sent by rclone and Nextcloud
//!   clients) sets the modification time as well.

mod xml;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRef, FromRequestParts, Request, State};
use axum::http::header::{self, AsHeaderName, HeaderName};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use chrono::{DateTime, SecondsFormat, Utc};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rustcloud_auth::{basic_credentials, from_a_web_page, Access, Auth, CurrentUser, User};
use rustcloud_core::{ClientInfo, Error, ErrorKind, Result};
use rustcloud_db::node;
use uuid::Uuid;

use crate::content;
use crate::http::{byte_stream, parse_mtime};
use crate::service::{Files, Node, NodeUpdate};
use crate::trash::TRASH_FOLDER;
use xml::{empty_element, escape, status_line, Element, DAV};

/// Where the root folder is.
pub const PREFIX: &str = "/dav";
/// The app an app password must open.
const APP: &str = "files";
/// Largest XML body of a request.
const BODY_LIMIT: usize = 1 << 20;
/// What a lock says it lasts; nothing ends it, as nothing keeps it.
const LOCK_SECONDS: u32 = 3600;
/// Characters of a name left as they are in a path; the rest is %-encoded.
const NAME_AS_IS: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
/// The namespace of Windows' properties.
const MS: &str = "urn:schemas-microsoft-com:";
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, MKCOL, COPY, MOVE, LOCK, UNLOCK";
/// Properties of every file and folder, as `allprop` and `propname` list them.
const PROPERTIES: &[&str] = &[
    "resourcetype",
    "displayname",
    "getcontentlength",
    "getcontenttype",
    "getetag",
    "getlastmodified",
    "creationdate",
    "supportedlock",
    "lockdiscovery",
];

/// Routes of WebDAV.
pub fn router<S>() -> Router<S>
where
    Arc<Files>: FromRef<S>,
    Arc<Auth>: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    Router::new().route(PREFIX, any(dav)).route("/dav/", any(dav)).route("/dav/{*path}", any(dav))
}

async fn dav(
    State(files): State<Arc<Files>>,
    State(auth): State<Arc<Auth>>,
    client: ClientInfo,
    request: Request,
) -> Response {
    let (mut parts, body) = request.into_parts();
    // Windows asks before it has credentials; the answer tells nothing.
    if parts.method == Method::OPTIONS {
        return options();
    }
    let answer = match caller(&auth, client, &mut parts).await {
        Ok(Some(user)) => handle(&files, &user, &parts, body).await,
        Ok(None) => Err(Error::unauthorized("Log in with your username and an app password")),
        Err(err) => Err(err),
    };
    answer.unwrap_or_else(error_response)
}

/// The user of the request: Basic with an app password, or a Bearer token.
/// `None`: no credentials, or wrong ones.
async fn caller(auth: &Arc<Auth>, client: ClientInfo, parts: &mut Parts) -> Result<Option<User>> {
    let writes = !reads_only(&parts.method);
    if let Some((username, password)) = basic_credentials(&parts.headers) {
        // A browser keeps Basic credentials and sends them with requests
        // other sites make: not for changes.
        if writes && from_a_web_page(&parts.headers) {
            return Err(Error::forbidden("App passwords don't work from web pages").with_code("CSRF_CHECK_FAILED"));
        }
        let Some(found) = auth.app_password_user(&username, &password, client.ip).await? else {
            return Ok(None);
        };
        tracing::Span::current().record("user", found.user.username.as_str());
        return match found.access(APP) {
            None => Err(Error::forbidden("This app password does not open the files").with_code("APP_PASSWORD_SCOPE")),
            Some(Access::Read) if writes => {
                Err(Error::forbidden("This app password may only read").with_code("READ_ONLY"))
            }
            Some(_) => Ok(Some(found.user)),
        };
    }
    let bearer = header(&parts.headers, header::AUTHORIZATION).is_some_and(|value| value.starts_with("Bearer "));
    if !bearer {
        return Ok(None);
    }
    match CurrentUser::from_request_parts(parts, auth).await {
        Ok(current) => Ok(Some(current.0.user)),
        Err(err) if err.kind == ErrorKind::Unauthorized => Ok(None),
        Err(err) => Err(err),
    }
}

fn reads_only(method: &Method) -> bool {
    matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS" | "PROPFIND")
}

async fn handle(files: &Files, user: &User, parts: &Parts, body: Body) -> Result<Response> {
    let target = files.dav_target(user, segments(parts.uri.path())?).await?;
    let headers = &parts.headers;
    match parts.method.as_str() {
        "GET" | "HEAD" => get(files, user, &target, parts).await,
        "PROPFIND" => propfind(files, user, &target, headers, body).await,
        "PROPPATCH" => proppatch(files, user, &target, body).await,
        "PUT" => put(files, user, &target, headers, body).await,
        "DELETE" => delete(files, user, &target).await,
        "MKCOL" => mkcol(files, user, &target, body).await,
        "MOVE" => transfer(files, user, &target, headers, true).await,
        "COPY" => transfer(files, user, &target, headers, false).await,
        "LOCK" => lock(files, user, &target, headers, body).await,
        "UNLOCK" => target.found().map(|_| StatusCode::NO_CONTENT.into_response()),
        _ => Err(method_not_allowed("WebDAV has no such method")),
    }
}

// ----- paths -----

/// What a path names: the node there, if any, and the folder it is (or
/// would be) in, if that exists.
struct Target {
    /// Names from the root folder down.
    segments: Vec<String>,
    found: Option<node::Model>,
    parent: Option<node::Model>,
}

impl Target {
    fn found(&self) -> Result<&node::Model> {
        self.found.as_ref().ok_or_else(|| Error::not_found("FILE_NOT_FOUND", "File or folder not found"))
    }

    /// The last name of the path.
    fn name(&self) -> &str {
        self.segments.last().map(String::as_str).unwrap_or_default()
    }

    /// The folder something new at this path goes into.
    fn new_parent(&self) -> Result<&node::Model> {
        match self.segments.as_slice() {
            [] => Err(Error::forbidden("This is the root folder")),
            [name] if name == TRASH_FOLDER => Err(Error::forbidden("This name is the trash's")),
            _ => self
                .parent
                .as_ref()
                .ok_or_else(|| Error::conflict("PARENT_NOT_FOUND", "The folder above does not exist")),
        }
    }
}

/// The names of a path below [`PREFIX`], %-decoded.
fn segments(path: &str) -> Result<Vec<String>> {
    let bad = || Error::bad_request("BAD_PATH", "The path is not one of a file or folder");
    let rest = path.strip_prefix(PREFIX).ok_or_else(bad)?;
    rest.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let name = percent_decode_str(segment).decode_utf8().map_err(|_| bad())?;
            if name == "." || name == ".." {
                return Err(bad());
            }
            Ok(name.into_owned())
        })
        .collect()
}

/// The path of the `Destination` header (a full URL or a path), which must
/// be below [`PREFIX`] of this server.
fn destination_path(value: &str) -> Result<&str> {
    let value = value.trim();
    let path = match value.find("://") {
        Some(at) => {
            let rest = &value[at + 3..];
            rest.find('/').map_or("/", |slash| &rest[slash..])
        }
        None => value,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path == PREFIX || path.starts_with("/dav/") {
        Ok(path)
    } else {
        Err(Error::bad_request("BAD_DESTINATION", "The destination is not a WebDAV path of this server"))
    }
}

/// The path of a node in answers; folders end with `/`.
fn href(segments: &[String], is_dir: bool) -> String {
    let names: Vec<String> = segments.iter().map(|name| utf8_percent_encode(name, NAME_AS_IS).to_string()).collect();
    let mut href = format!("{PREFIX}/{}", names.join("/"));
    if is_dir && !segments.is_empty() {
        href.push('/');
    }
    href
}

impl Files {
    /// Finds what `segments` name in the user's tree. The trash is not there.
    async fn dav_target(&self, user: &User, segments: Vec<String>) -> Result<Target> {
        let root = self.row(self.root(user).await?.id).await?;
        let mut current = Some(root);
        let mut parent = None;
        for (depth, name) in segments.iter().enumerate() {
            let folder = match current.take() {
                Some(folder) if folder.is_dir => folder,
                // A folder on the way is missing (or a file).
                _ => return Ok(Target { segments, found: None, parent: None }),
            };
            if depth > 0 || name != TRASH_FOLDER {
                current = self.child_by_name(folder.id, name).await?;
            }
            parent = Some(folder);
        }
        Ok(Target { segments, found: current, parent })
    }

    /// Bytes used, and how many more fit (quota and disk), for the quota
    /// properties (RFC 4331).
    async fn dav_room(&self, user: &User) -> Result<(i64, Option<u64>)> {
        let used = self.usage(user).await?.used_bytes;
        let quota = self.remaining_quota(user).await?.map(|left| u64::try_from(left).unwrap_or(0));
        let available = match (quota, self.disk_room().await?) {
            (Some(quota), Some(disk)) => Some(quota.min(disk)),
            (quota, disk) => quota.or(disk),
        };
        Ok((used, available))
    }
}

// ----- methods -----

fn options() -> Response {
    let headers = [
        (header::ALLOW, ALLOW),
        (HeaderName::from_static("dav"), "1, 2"),
        (HeaderName::from_static("ms-author-via"), "DAV"),
    ];
    (StatusCode::OK, headers).into_response()
}

async fn get(files: &Files, user: &User, target: &Target, parts: &Parts) -> Result<Response> {
    let node = Node::from(target.found()?.clone());
    if node.is_dir {
        let text = "This is a WebDAV folder of RustCloud: open it in a WebDAV client.\n";
        return Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response());
    }
    let modified = http_date(node.modified_at);
    let mut response = if parts.method == Method::HEAD {
        let mut headers = HeaderMap::new();
        set(&mut headers, header::CONTENT_LENGTH, node.size.to_string());
        set(&mut headers, header::CONTENT_TYPE, node.mime_type.clone().unwrap_or_default());
        set(&mut headers, header::ACCEPT_RANGES, "bytes".into());
        if let Some(etag) = &node.etag {
            set(&mut headers, header::ETAG, format!("\"{etag}\""));
        }
        (StatusCode::OK, headers).into_response()
    } else {
        let id = node.id;
        content::serve(node, &parts.headers, false, |range| files.download(user, id, range)).await?
    };
    set(response.headers_mut(), header::LAST_MODIFIED, modified);
    Ok(response)
}

/// What a PROPFIND asks for.
enum Wanted {
    All,
    Names,
    These(Vec<(String, String)>),
}

async fn propfind(files: &Files, user: &User, target: &Target, headers: &HeaderMap, body: Body) -> Result<Response> {
    let node = target.found()?;
    let wanted = match xml::parse(&read_body(body).await?)? {
        None => Wanted::All,
        Some(root) if root.is(DAV, "propfind") => match (root.dav_child("prop"), root.dav_child("propname")) {
            (Some(prop), _) => {
                Wanted::These(prop.children.iter().map(|wanted| (wanted.ns.clone(), wanted.name.clone())).collect())
            }
            (None, Some(_)) => Wanted::Names,
            (None, None) => Wanted::All,
        },
        Some(_) => return Err(Error::bad_request("INVALID_XML", "Expected <propfind>")),
    };

    let mut nodes = vec![(target.segments.clone(), Node::from(node.clone()))];
    // `infinity` (or no Depth) is answered as 1: a whole tree at once is too much.
    if node.is_dir && header(headers, "depth").map(str::trim) != Some("0") {
        let mut children = files.child_rows(node.id).await?;
        children.retain(|child| node.parent_id.is_some() || child.name != TRASH_FOLDER);
        children.sort_by(|a, b| a.name.cmp(&b.name));
        for child in children {
            let mut segments = target.segments.clone();
            segments.push(child.name.clone());
            nodes.push((segments, child.into()));
        }
    }
    let quota_wanted = matches!(&wanted, Wanted::These(names) if names.iter().any(|(ns, name)| ns == DAV && name.starts_with("quota-")));
    let room = if quota_wanted { Some(files.dav_room(user).await?) } else { None };

    let mut answer = multistatus_start();
    for (segments, node) in &nodes {
        let mut found = String::new();
        let mut missing = String::new();
        match &wanted {
            Wanted::All => {
                for name in PROPERTIES {
                    if let Some(value) = property(name, node, room) {
                        found.push_str(&element(name, &value));
                    }
                }
            }
            Wanted::Names => {
                for name in PROPERTIES {
                    if property(name, node, room).is_some() {
                        found.push_str(&format!("<D:{name}/>"));
                    }
                }
            }
            Wanted::These(names) => {
                for (ns, name) in names {
                    match property(name, node, room).filter(|_| ns == DAV) {
                        Some(value) => found.push_str(&element(name, &value)),
                        None => missing.push_str(&empty_element(ns, name)),
                    }
                }
            }
        }
        answer.push_str(&format!("<D:response><D:href>{}</D:href>", escape(&href(segments, node.is_dir))));
        if !found.is_empty() || missing.is_empty() {
            answer.push_str(&propstat(&found, StatusCode::OK));
        }
        if !missing.is_empty() {
            answer.push_str(&propstat(&missing, StatusCode::NOT_FOUND));
        }
        answer.push_str("</D:response>");
    }
    Ok(multistatus(answer))
}

/// The value of a DAV: property of `node` as XML; `None`: it has none.
fn property(name: &str, node: &Node, room: Option<(i64, Option<u64>)>) -> Option<String> {
    let file = !node.is_dir;
    match name {
        "resourcetype" => Some(if file { String::new() } else { "<D:collection/>".into() }),
        "displayname" => Some(escape(&node.name)),
        "getcontentlength" => file.then(|| node.size.to_string()),
        "getcontenttype" => node.mime_type.as_deref().filter(|_| file).map(escape),
        "getetag" => node.etag.as_ref().map(|etag| escape(&format!("\"{etag}\""))),
        "getlastmodified" => Some(http_date(node.modified_at)),
        "creationdate" => Some(node.created_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "supportedlock" => Some(
            "<D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>\
             <D:lockentry><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>"
                .into(),
        ),
        "lockdiscovery" => Some(String::new()),
        "quota-used-bytes" => room.filter(|_| !file).map(|(used, _)| used.to_string()),
        "quota-available-bytes" => room.filter(|_| !file).and_then(|(_, available)| available).map(|a| a.to_string()),
        _ => None,
    }
}

async fn proppatch(files: &Files, user: &User, target: &Target, body: Body) -> Result<Response> {
    let node = target.found()?;
    let update = xml::parse(&read_body(body).await?)?
        .filter(|root| root.is(DAV, "propertyupdate"))
        .ok_or_else(|| Error::bad_request("INVALID_XML", "Expected <propertyupdate>"))?;

    let mut results: Vec<(&Element, StatusCode)> = Vec::new();
    let mut mtime = None;
    for change in &update.children {
        let set = change.is(DAV, "set");
        if !set && !change.is(DAV, "remove") {
            continue;
        }
        for prop in change.dav_child("prop").into_iter().flat_map(|prop| &prop.children) {
            let status = if prop.is(DAV, "getlastmodified") || prop.is(MS, "Win32LastModifiedTime") {
                // Folders have no time of their own; removing it changes nothing.
                if set && !node.is_dir {
                    match DateTime::parse_from_rfc2822(prop.text.trim()) {
                        Ok(time) => {
                            mtime = Some(time.with_timezone(&Utc));
                            StatusCode::OK
                        }
                        Err(_) => StatusCode::CONFLICT,
                    }
                } else {
                    StatusCode::OK
                }
            } else if prop.ns == MS {
                // Windows' other times and attributes: not kept.
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            results.push((prop, status));
        }
    }
    // All or nothing.
    if results.iter().any(|(_, status)| *status != StatusCode::OK) {
        for (_, status) in &mut results {
            if *status == StatusCode::OK {
                *status = StatusCode::FAILED_DEPENDENCY;
            }
        }
    } else if let Some(mtime) = mtime {
        files.update(user, node.id, NodeUpdate { modified_at: Some(mtime), ..Default::default() }).await?;
    }

    let mut answer = multistatus_start();
    answer.push_str(&format!("<D:response><D:href>{}</D:href>", escape(&href(&target.segments, node.is_dir))));
    let mut statuses: Vec<StatusCode> = results.iter().map(|(_, status)| *status).collect();
    statuses.sort();
    statuses.dedup();
    for status in statuses {
        let props: String = results
            .iter()
            .filter(|(_, of)| *of == status)
            .map(|(prop, _)| empty_element(&prop.ns, &prop.name))
            .collect();
        answer.push_str(&propstat(&props, status));
    }
    answer.push_str("</D:response>");
    Ok(multistatus(answer))
}

async fn put(files: &Files, user: &User, target: &Target, headers: &HeaderMap, body: Body) -> Result<Response> {
    let mtime = header(headers, "x-oc-mtime").map(parse_mtime).transpose()?;
    let if_match = header(headers, header::IF_MATCH);
    let body = byte_stream(body);
    let (node, status) = match &target.found {
        Some(node) if node.is_dir => return Err(method_not_allowed("This is a folder")),
        Some(node) => {
            if header(headers, header::IF_NONE_MATCH).map(str::trim) == Some("*") {
                return Err(precondition_failed());
            }
            let etag = if_match.map(unquote).filter(|etag| etag != "*");
            (files.replace_content(user, node.id, body, etag.as_deref(), mtime).await?, StatusCode::NO_CONTENT)
        }
        None => {
            if if_match.is_some() {
                return Err(precondition_failed());
            }
            let parent = target.new_parent()?;
            (files.upload(user, parent.id, target.name(), body, false, mtime).await?, StatusCode::CREATED)
        }
    };
    let mut response = status.into_response();
    if let Some(etag) = &node.etag {
        set(response.headers_mut(), header::ETAG, format!("\"{etag}\""));
    }
    if mtime.is_some() {
        set(response.headers_mut(), HeaderName::from_static("x-oc-mtime"), "accepted".into());
    }
    Ok(response)
}

async fn delete(files: &Files, user: &User, target: &Target) -> Result<Response> {
    let node = target.found()?;
    if node.parent_id.is_none() {
        return Err(Error::forbidden("The root folder can't be deleted"));
    }
    files.delete(user, node.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn mkcol(files: &Files, user: &User, target: &Target, body: Body) -> Result<Response> {
    if !read_body(body).await?.is_empty() {
        return Err(Error::bad_request("UNSUPPORTED_MEDIA_TYPE", "MKCOL takes no body"));
    }
    if target.found.is_some() {
        return Err(method_not_allowed("It already exists"));
    }
    let parent = target.new_parent()?;
    files.create_folder(user, parent.id, target.name()).await?;
    Ok(StatusCode::CREATED.into_response())
}

/// MOVE (`moving`) or COPY to the `Destination` header.
async fn transfer(files: &Files, user: &User, source: &Target, headers: &HeaderMap, moving: bool) -> Result<Response> {
    let node = source.found()?;
    if node.parent_id.is_none() {
        return Err(Error::forbidden("The root folder can't be moved or copied"));
    }
    let destination = header(headers, "destination")
        .ok_or_else(|| Error::bad_request("NO_DESTINATION", "The Destination header is missing"))?;
    let target = files.dav_target(user, segments(destination_path(destination)?)?).await?;
    let parent = target.new_parent()?;
    let overwrite = !header(headers, "overwrite").is_some_and(|value| value.trim().eq_ignore_ascii_case("F"));
    let into_itself = || Error::forbidden("A folder can't go into itself").with_code("INTO_ITSELF");
    if node.is_dir && files.is_inside(parent, node.id).await? {
        return Err(into_itself());
    }
    let replaced = match &target.found {
        Some(existing) if existing.id == node.id => {
            return Err(Error::forbidden("The source and the destination are the same"))
        }
        Some(_) if !overwrite => return Err(precondition_failed()),
        Some(existing) => {
            // The source is inside what it would replace.
            if files.is_inside(node, existing.id).await? {
                return Err(into_itself());
            }
            files.delete(user, existing.id).await?;
            true
        }
        None => false,
    };

    let name = target.name();
    if moving {
        let update = NodeUpdate {
            name: (name != node.name).then(|| name.to_owned()),
            parent_id: (node.parent_id != Some(parent.id)).then_some(parent.id),
            modified_at: None,
        };
        files.update(user, node.id, update).await?;
    } else if node.is_dir && header(headers, "depth").map(str::trim) == Some("0") {
        files.create_folder(user, parent.id, name).await?;
    } else {
        files.copy(user, node.id, parent.id, Some(name)).await?;
    }
    Ok(if replaced { StatusCode::NO_CONTENT } else { StatusCode::CREATED }.into_response())
}

/// Grants a lock without keeping it (see the module's notes). Locking a
/// path where nothing is creates an empty file there.
async fn lock(files: &Files, user: &User, target: &Target, headers: &HeaderMap, body: Body) -> Result<Response> {
    let info = xml::parse(&read_body(body).await?)?;
    let token = match &info {
        Some(_) => format!("opaquelocktoken:{}", Uuid::new_v4()),
        // A refresh names its lock in the If header.
        None => if_token(headers)
            .ok_or_else(|| Error::bad_request("NO_LOCK", "A refresh of a lock names it in the If header"))?,
    };
    let shared = info
        .as_ref()
        .and_then(|info| info.dav_child("lockscope"))
        .is_some_and(|scope| scope.dav_child("shared").is_some());
    let (is_dir, status) = match &target.found {
        Some(node) => (node.is_dir, StatusCode::OK),
        None if info.is_none() => return Err(target.found().unwrap_err()),
        None => {
            let parent = target.new_parent()?;
            let empty: rustcloud_core::ByteStream = Box::pin(futures_util::stream::empty());
            files.upload(user, parent.id, target.name(), empty, false, None).await?;
            (false, StatusCode::CREATED)
        }
    };
    let depth = if is_dir && header(headers, "depth").map(str::trim) != Some("0") { "infinity" } else { "0" };
    let scope = if shared { "shared" } else { "exclusive" };
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype><D:lockscope><D:{scope}/></D:lockscope><D:depth>{depth}</D:depth>\
         <D:timeout>Second-{LOCK_SECONDS}</D:timeout><D:locktoken><D:href>{}</D:href></D:locktoken>\
         <D:lockroot><D:href>{}</D:href></D:lockroot></D:activelock></D:lockdiscovery></D:prop>",
        escape(&token),
        escape(&href(&target.segments, is_dir)),
    );
    let mut response = (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response();
    set(response.headers_mut(), HeaderName::from_static("lock-token"), format!("<{token}>"));
    Ok(response)
}

/// The lock token of an `If` header: `(<opaquelocktoken:…>)`.
fn if_token(headers: &HeaderMap) -> Option<String> {
    let value = header(headers, "if")?;
    let start = value.find("<opaquelocktoken:")?;
    let rest = &value[start + 1..];
    Some(rest[..rest.find('>')?].to_owned())
}

// ----- answers -----

fn multistatus_start() -> String {
    String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">")
}

fn multistatus(mut body: String) -> Response {
    body.push_str("</D:multistatus>");
    (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

fn propstat(props: &str, status: StatusCode) -> String {
    format!("<D:propstat><D:prop>{props}</D:prop><D:status>{}</D:status></D:propstat>", status_line(status))
}

/// `<D:name>value</D:name>`, or `<D:name/>` without a value.
fn element(name: &str, value: &str) -> String {
    if value.is_empty() {
        format!("<D:{name}/>")
    } else {
        format!("<D:{name}>{value}</D:{name}>")
    }
}

/// A time as HTTP writes it: `Sun, 06 Nov 1994 08:49:37 GMT`.
fn http_date(time: DateTime<Utc>) -> String {
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// The first ETag of an `If-Match` list, without quotes.
fn unquote(value: &str) -> String {
    let first = value.split(',').next().unwrap_or_default().trim();
    first.trim_start_matches("W/").trim_matches('"').to_owned()
}

fn header(headers: &HeaderMap, name: impl AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn set(headers: &mut HeaderMap, name: HeaderName, value: String) {
    if let Ok(value) = HeaderValue::try_from(value) {
        headers.insert(name, value);
    }
}

async fn read_body(body: Body) -> Result<axum::body::Bytes> {
    axum::body::to_bytes(body, BODY_LIMIT).await.map_err(|_| {
        Error::new(ErrorKind::PayloadTooLarge, "BODY_TOO_LARGE", "The request body is too large or broken")
    })
}

fn method_not_allowed(message: &str) -> Error {
    Error::bad_request("METHOD_NOT_ALLOWED", message)
}

fn precondition_failed() -> Error {
    Error::bad_request("PRECONDITION_FAILED", "The file is not as the request expects")
}

/// An error as WebDAV clients expect it: some codes get the statuses of
/// RFC 4918, and 401 asks for Basic credentials.
fn error_response(err: Error) -> Response {
    let status = match err.code {
        "METHOD_NOT_ALLOWED" => Some(StatusCode::METHOD_NOT_ALLOWED),
        "PRECONDITION_FAILED" | "FILE_CHANGED" => Some(StatusCode::PRECONDITION_FAILED),
        "UNSUPPORTED_MEDIA_TYPE" => Some(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        "QUOTA_EXCEEDED" | "STORAGE_FULL" => Some(StatusCode::INSUFFICIENT_STORAGE),
        "FILE_LOCKED" => Some(StatusCode::LOCKED),
        "BAD_DESTINATION" => Some(StatusCode::BAD_GATEWAY),
        _ => None,
    };
    let unauthorized = err.kind == ErrorKind::Unauthorized;
    let mut response = err.into_response();
    if let Some(status) = status {
        *response.status_mut() = status;
    }
    if status == Some(StatusCode::METHOD_NOT_ALLOWED) {
        set(response.headers_mut(), header::ALLOW, ALLOW.into());
    }
    if unauthorized {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"RustCloud\", charset=\"UTF-8\""));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_names_and_back() {
        assert_eq!(segments("/dav").unwrap(), Vec::<String>::new());
        assert_eq!(segments("/dav/").unwrap(), Vec::<String>::new());
        assert_eq!(segments("/dav/a%20b/%D0%BF.txt").unwrap(), ["a b", "п.txt"]);
        assert!(segments("/dav/a/../b").is_err());
        assert!(segments("/dav/a/%2e%2e/b").is_err());
        assert!(segments("/other").is_err());
        let names = vec!["a b".to_owned(), "п&.txt".to_owned()];
        assert_eq!(href(&names, false), "/dav/a%20b/%D0%BF%26.txt");
        assert_eq!(href(&names[..1], true), "/dav/a%20b/");
        assert_eq!(href(&[], true), "/dav/");
    }

    #[test]
    fn destinations_are_paths_of_this_server() {
        assert_eq!(destination_path("https://cloud.example/dav/a%20b").unwrap(), "/dav/a%20b");
        assert_eq!(destination_path("/dav/x?y").unwrap(), "/dav/x");
        assert!(destination_path("https://cloud.example/other/x").is_err());
        assert!(destination_path("/davx").is_err());
    }

    #[test]
    fn lock_tokens_come_from_if_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("if", HeaderValue::from_static("(<opaquelocktoken:1234-ab>)"));
        assert_eq!(if_token(&headers).as_deref(), Some("opaquelocktoken:1234-ab"));
        assert_eq!(unquote("W/\"abc\", \"def\""), "abc");
    }

    #[test]
    fn times_are_written_as_http_dates() {
        let time = DateTime::parse_from_rfc3339("1994-11-06T08:49:37Z").unwrap().with_timezone(&Utc);
        assert_eq!(http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
        let windows = DateTime::parse_from_rfc2822("Wed, 20 Feb 2008 17:44:48 GMT").unwrap();
        assert_eq!(windows.to_rfc3339(), "2008-02-20T17:44:48+00:00");
    }
}
