//! CalDAV (RFC 4791 on top of WebDAV, RFC 4918): what phones and desktop
//! calendars use to sync. Clients log in with an app password and find the
//! calendars through `/.well-known/caldav`, which the server redirects here.
//!
//! | path                                      | resource                 |
//! |-------------------------------------------|--------------------------|
//! | `/app/calendar/dav/`                      | the start: who is logged in |
//! | `/app/calendar/dav/principal/`            | the user                 |
//! | `/app/calendar/dav/calendars/`            | the user's calendars     |
//! | `/app/calendar/dav/calendars/<id>/`       | a calendar               |
//! | `/app/calendar/dav/calendars/<id>/<name>` | an event (`.ics`)        |
//!
//! Calendars other users shared are in the same list (`shared-<share id>`),
//! named with the owner's name; read-only ones say so in
//! `current-user-privilege-set`, and deleting one only gives it up.
//!
//! Only events (`VEVENT`) are kept; sync is by `getctag` and etags
//! (no `sync-collection` yet).

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::expand;
use crate::ical;
use crate::rustcloud::app::appdata::{Entry, Error};
use crate::store::{self, Calendar};
use crate::xml::{self, Element, Name, APPLE, CALDAV, CALSERVER, DAV};
use crate::{Request, Response, User};

/// Where this app's CalDAV tree starts, as clients see it.
pub const BASE: &str = "/app/calendar/dav";
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR";

enum Target<'a> {
    Root,
    Principal,
    Home,
    Calendar(&'a str),
    Object(&'a str, &'a str),
}

/// Answers a request to `/dav/…` (`path` is what follows `/dav`).
pub fn handle(user: Option<&User>, request: &Request, path: &str) -> Response {
    let method = request.method.as_str();
    if method == "OPTIONS" {
        return Response {
            status: 200,
            headers: vec![header("dav", "1, 3, calendar-access"), header("allow", ALLOW)],
            body: Vec::new(),
        };
    }
    let Some(user) = user else {
        // Ask for credentials: the client then sends the app password.
        let mut response = status(401);
        response.headers.push(header("www-authenticate", "Basic realm=\"RustCloud\", charset=\"UTF-8\""));
        return response;
    };
    let Some(target) = target(path) else {
        return status(404);
    };
    let body = xml::parse(&request.body);
    let result = match method {
        "PROPFIND" => propfind(user, &target, depth(request), body.as_ref()),
        "PROPPATCH" => proppatch(&target, body.as_ref()),
        "REPORT" => report(&target, body.as_ref()),
        "MKCALENDAR" => mkcalendar(&target, body.as_ref()),
        "GET" | "HEAD" => get(&target, method == "HEAD"),
        "PUT" => put(&target, request),
        "DELETE" => delete(&target, request),
        _ => Ok(status(405)),
    };
    result.unwrap_or_else(|err| status(error_status(&err)))
}

fn target(path: &str) -> Option<Target<'_>> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    Some(match segments.as_slice() {
        [] => Target::Root,
        ["principal"] => Target::Principal,
        ["calendars"] => Target::Home,
        ["calendars", id] => Target::Calendar(id),
        ["calendars", id, name] => Target::Object(id, name),
        _ => return None,
    })
}

/// `Depth: 0` or 1; `infinity` is treated as 1 (the tree is that shallow).
fn depth(request: &Request) -> u8 {
    match request_header(request, "depth") {
        Some("0") => 0,
        _ => 1,
    }
}

// --- PROPFIND ---------------------------------------------------------------

/// A resource and the properties it has.
enum Resource {
    Root,
    Principal,
    Home,
    Calendar(Calendar, Vec<Entry>),
    Object(String, Entry),
}

/// Which properties a request asks for.
enum Wanted {
    All,
    Names,
    These(Vec<Name>),
}

fn wanted(body: Option<&Element>) -> Wanted {
    match body {
        Some(root) if root.child(DAV, "propname").is_some() => Wanted::Names,
        Some(root) => match root.child(DAV, "prop") {
            Some(prop) => Wanted::These(prop.children.iter().map(|child| child.name.clone()).collect()),
            None => Wanted::All,
        },
        None => Wanted::All,
    }
}

fn propfind(user: &User, target: &Target, depth: u8, body: Option<&Element>) -> Result<Response, Error> {
    let mut resources = Vec::new();
    match target {
        Target::Root => resources.push(Resource::Root),
        Target::Principal => resources.push(Resource::Principal),
        Target::Home => {
            resources.push(Resource::Home);
            if depth > 0 {
                for calendar in store::calendars()? {
                    let objects = store::objects(&calendar.id)?;
                    resources.push(Resource::Calendar(calendar, objects));
                }
            }
        }
        Target::Calendar(id) => {
            let Some(calendar) = store::calendar(id)? else {
                return Ok(status(404));
            };
            let objects = store::objects(id)?;
            let children: Vec<Resource> = if depth > 0 {
                objects.iter().map(|entry| Resource::Object(calendar.id.clone(), entry.clone())).collect()
            } else {
                Vec::new()
            };
            resources.push(Resource::Calendar(calendar, objects));
            resources.extend(children);
        }
        Target::Object(id, name) => match store::object(id, name)? {
            Some(entry) => resources.push(Resource::Object(id.to_string(), entry)),
            None => return Ok(status(404)),
        },
    }
    let wanted = wanted(body);
    let responses: Vec<String> =
        resources.iter().map(|resource| response(&href(resource), &props(user, resource), &wanted)).collect();
    Ok(multistatus(&responses))
}

/// A shared calendar's name says whose it is: clients show one list.
fn display_name(calendar: &Calendar) -> String {
    match &calendar.owner {
        Some(owner) => format!("{} ({})", calendar.name, owner.display_name),
        None => calendar.name.clone(),
    }
}

fn href(resource: &Resource) -> String {
    match resource {
        Resource::Root => format!("{BASE}/"),
        Resource::Principal => principal_href(),
        Resource::Home => home_href(),
        Resource::Calendar(calendar, _) => calendar_href(&calendar.id),
        Resource::Object(calendar, entry) => object_href(calendar, &entry.name),
    }
}

fn principal_href() -> String {
    format!("{BASE}/principal/")
}

fn home_href() -> String {
    format!("{BASE}/calendars/")
}

fn calendar_href(id: &str) -> String {
    format!("{BASE}/calendars/{}/", encode(id))
}

fn object_href(calendar: &str, name: &str) -> String {
    format!("{BASE}/calendars/{}/{}", encode(calendar), encode(name))
}

fn name(ns: &str, local: &str) -> Name {
    Name { ns: ns.to_owned(), local: local.to_owned() }
}

fn href_xml(href: &str) -> String {
    format!("<d:href>{}</d:href>", xml::escape(href))
}

/// The properties of a resource, as XML inside each property element.
fn props(user: &User, resource: &Resource) -> Vec<(Name, String)> {
    let principal = href_xml(&principal_href());
    let mut props =
        vec![(name(DAV, "current-user-principal"), principal.clone()), (name(DAV, "owner"), principal.clone())];
    let privileges = "<d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege>\
        <d:privilege><d:write-properties/></d:privilege><d:privilege><d:write-content/></d:privilege>\
        <d:privilege><d:bind/></d:privilege><d:privilege><d:unbind/></d:privilege>";
    let read_only = "<d:privilege><d:read/></d:privilege>";
    match resource {
        Resource::Root | Resource::Principal => {
            let kind = if matches!(resource, Resource::Principal) {
                "<d:collection/><d:principal/>"
            } else {
                "<d:collection/>"
            };
            props.extend([
                (name(DAV, "resourcetype"), kind.to_owned()),
                (name(DAV, "displayname"), xml::escape(&user.display_name)),
                (name(DAV, "principal-URL"), principal),
                (name(CALDAV, "calendar-home-set"), href_xml(&home_href())),
            ]);
        }
        Resource::Home => props.extend([
            (name(DAV, "resourcetype"), "<d:collection/>".to_owned()),
            (name(DAV, "displayname"), "Calendars".to_owned()),
            (name(DAV, "current-user-privilege-set"), privileges.to_owned()),
        ]),
        Resource::Calendar(calendar, objects) => props.extend([
            (name(DAV, "resourcetype"), "<d:collection/><c:calendar/>".to_owned()),
            (name(DAV, "displayname"), xml::escape(&display_name(calendar))),
            (name(APPLE, "calendar-color"), xml::escape(&calendar.color)),
            (name(CALSERVER, "getctag"), store::tag(objects)),
            (name(CALDAV, "supported-calendar-component-set"), "<c:comp name=\"VEVENT\"/>".to_owned()),
            (
                name(DAV, "supported-report-set"),
                "<d:supported-report><d:report><c:calendar-multiget/></d:report></d:supported-report>\
                 <d:supported-report><d:report><c:calendar-query/></d:report></d:supported-report>"
                    .to_owned(),
            ),
            (
                name(DAV, "current-user-privilege-set"),
                if calendar.writable { privileges } else { read_only }.to_owned(),
            ),
        ]),
        Resource::Object(_, entry) => props.extend([
            (name(DAV, "resourcetype"), String::new()),
            (name(DAV, "getetag"), xml::escape(&etag(entry))),
            (name(DAV, "getcontenttype"), "text/calendar; charset=utf-8; component=vevent".to_owned()),
            (name(DAV, "getcontentlength"), entry.size.to_string()),
            (name(DAV, "getlastmodified"), http_date(&entry.updated_at)),
        ]),
    }
    props
}

/// One `<d:response>`: the wanted properties found (200) and not found (404).
fn response(href: &str, props: &[(Name, String)], wanted: &Wanted) -> String {
    let (found, missing): (Vec<(&Name, &str)>, Vec<&Name>) = match wanted {
        Wanted::All => (props.iter().map(|(n, v)| (n, v.as_str())).collect(), Vec::new()),
        Wanted::Names => (props.iter().map(|(n, _)| (n, "")).collect(), Vec::new()),
        Wanted::These(names) => {
            let found = names
                .iter()
                .filter_map(|wanted| props.iter().find(|(n, _)| n == wanted).map(|(n, v)| (n, v.as_str())))
                .collect();
            let missing = names.iter().filter(|wanted| !props.iter().any(|(n, _)| n == *wanted)).collect();
            (found, missing)
        }
    };
    let mut out = format!("<d:response>{}", href_xml(href));
    if !found.is_empty() {
        out.push_str("<d:propstat><d:prop>");
        for (name, value) in found {
            out.push_str(&element(name, value));
        }
        out.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>");
    }
    if !missing.is_empty() {
        out.push_str("<d:propstat><d:prop>");
        for name in missing {
            out.push_str(&element(name, ""));
        }
        out.push_str("</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>");
    }
    out.push_str("</d:response>");
    out
}

/// `<prefix:local>value</prefix:local>`, declaring namespaces we don't know.
fn element(name: &Name, value: &str) -> String {
    let prefix = match name.ns.as_str() {
        DAV => "d",
        CALDAV => "c",
        CALSERVER => "cs",
        APPLE => "a",
        _ => {
            let ns = xml::escape(&name.ns);
            return format!("<x:{local} xmlns:x=\"{ns}\">{value}</x:{local}>", local = name.local);
        }
    };
    if value.is_empty() {
        format!("<{prefix}:{}/>", name.local)
    } else {
        format!("<{prefix}:{local}>{value}</{prefix}:{local}>", local = name.local)
    }
}

fn multistatus(responses: &[String]) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"{DAV}\" xmlns:c=\"{CALDAV}\" \
         xmlns:cs=\"{CALSERVER}\" xmlns:a=\"{APPLE}\">{}</d:multistatus>",
        responses.concat()
    );
    Response {
        status: 207,
        headers: vec![header("content-type", "application/xml; charset=utf-8")],
        body: body.into_bytes(),
    }
}

// --- REPORT -----------------------------------------------------------------

fn report(target: &Target, body: Option<&Element>) -> Result<Response, Error> {
    let (Target::Calendar(id), Some(body)) = (target, body) else {
        return Ok(unsupported_report());
    };
    let Some(calendar) = store::calendar(id)? else {
        return Ok(status(404));
    };
    let wanted = wanted(Some(body));
    let objects = store::objects(id)?;

    if body.is(CALDAV, "calendar-multiget") {
        let mut responses = Vec::new();
        for href in body.children.iter().filter(|child| child.is(DAV, "href")) {
            let name = decode(href.text.trim().trim_end_matches('/').rsplit('/').next().unwrap_or_default());
            match objects.iter().find(|entry| entry.name == name) {
                Some(entry) => responses.push(object_response(&calendar.id, entry, &wanted)?),
                None => responses.push(format!(
                    "<d:response>{}<d:status>HTTP/1.1 404 Not Found</d:status></d:response>",
                    href_xml(href.text.trim())
                )),
            }
        }
        return Ok(multistatus(&responses));
    }
    if body.is(CALDAV, "calendar-query") {
        let names = match query_filter(body) {
            Filter::Nothing => Vec::new(),
            Filter::All => objects.iter().map(|entry| entry.name.clone()).collect(),
            Filter::Between(from, to) => in_range(id, from, to)?,
        };
        let mut responses = Vec::new();
        for entry in objects.iter().filter(|entry| names.contains(&entry.name)) {
            responses.push(object_response(&calendar.id, entry, &wanted)?);
        }
        return Ok(multistatus(&responses));
    }
    Ok(unsupported_report())
}

/// An event in a REPORT answer; `calendar-data` is read only when asked for.
fn object_response(calendar: &str, entry: &Entry, wanted: &Wanted) -> Result<String, Error> {
    let mut props = vec![
        (name(DAV, "getetag"), xml::escape(&etag(entry))),
        (name(DAV, "getcontenttype"), "text/calendar; charset=utf-8; component=vevent".to_owned()),
    ];
    if let Wanted::These(names) = wanted {
        if names.iter().any(|n| n.is(CALDAV, "calendar-data")) {
            let data = store::read_object(calendar, &entry.name)?;
            props.push((name(CALDAV, "calendar-data"), xml::escape(&String::from_utf8_lossy(&data))));
        }
    }
    Ok(response(&object_href(calendar, &entry.name), &props, wanted))
}

enum Filter {
    Nothing,
    All,
    Between(DateTime<Utc>, DateTime<Utc>),
}

/// What a `calendar-query` asks for: events (maybe in a time range), or
/// other components, which this calendar doesn't keep.
fn query_filter(body: &Element) -> Filter {
    let Some(calendar) = body.descendants(CALDAV, "comp-filter").into_iter().next() else {
        return Filter::All;
    };
    let Some(component) = calendar.children.iter().find(|child| child.is(CALDAV, "comp-filter")) else {
        return Filter::All;
    };
    if !component.attribute("name").is_some_and(|name| name.eq_ignore_ascii_case("VEVENT")) {
        return Filter::Nothing;
    }
    let Some(range) = component.child(CALDAV, "time-range") else {
        return Filter::All;
    };
    let from = range.attribute("start").and_then(utc_time).unwrap_or(DateTime::<Utc>::MIN_UTC);
    let to = range.attribute("end").and_then(utc_time).unwrap_or(DateTime::<Utc>::MAX_UTC);
    Filter::Between(from, to)
}

fn utc_time(value: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok().map(|at| at.and_utc())
}

/// Files with an event that happens in `[from, to)`.
fn in_range(calendar: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for (name, events) in store::events(calendar)? {
        let moved: Vec<DateTime<Utc>> =
            events.iter().filter_map(|e| e.recurrence_id.as_ref()).map(expand::instant).collect();
        if events.iter().any(|event| !expand::occurrences(event, &moved, from, to).is_empty()) {
            names.push(name);
        }
    }
    Ok(names)
}

fn unsupported_report() -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"{DAV}\"><d:supported-report/></d:error>"
    );
    Response {
        status: 403,
        headers: vec![header("content-type", "application/xml; charset=utf-8")],
        body: body.into_bytes(),
    }
}

// --- Changes ----------------------------------------------------------------

/// The name and colour set in a PROPPATCH or MKCALENDAR body, and the
/// properties that can't be set.
fn settable(body: Option<&Element>, calendar: &mut Calendar) -> Vec<Name> {
    let mut refused = Vec::new();
    let Some(body) = body else {
        return refused;
    };
    for set in body.children.iter().filter(|c| c.is(DAV, "set") || c.is(DAV, "remove")) {
        let removing = set.is(DAV, "remove");
        for prop in set.child(DAV, "prop").map(|p| p.children.as_slice()).unwrap_or_default() {
            match (prop.name.ns.as_str(), prop.name.local.as_str(), removing) {
                (DAV, "displayname", false) if !prop.text.trim().is_empty() => {
                    calendar.name = prop.text.trim().to_owned()
                }
                (APPLE, "calendar-color", false) if prop.text.trim().starts_with('#') => {
                    calendar.color = prop.text.trim().to_owned()
                }
                _ => refused.push(prop.name.clone()),
            }
        }
    }
    refused
}

fn proppatch(target: &Target, body: Option<&Element>) -> Result<Response, Error> {
    let Target::Calendar(id) = target else {
        return Ok(status(403));
    };
    let Some(mut calendar) = store::calendar(id)? else {
        return Ok(status(404));
    };
    if calendar.owner.is_some() {
        // Only the owner renames or recolours a shared calendar.
        return Ok(status(403));
    }
    let refused = settable(body, &mut calendar);
    store::save_calendar(&calendar)?;

    let mut out = format!("<d:response>{}", href_xml(&calendar_href(id)));
    let changed: Vec<&Element> = body
        .into_iter()
        .flat_map(|b| b.children.iter())
        .filter_map(|set| set.child(DAV, "prop"))
        .flat_map(|prop| prop.children.iter())
        .filter(|prop| !refused.contains(&prop.name))
        .collect();
    for (props, line) in
        [(changed.iter().map(|p| &p.name).collect::<Vec<_>>(), "200 OK"), (refused.iter().collect(), "403 Forbidden")]
    {
        if !props.is_empty() {
            out.push_str("<d:propstat><d:prop>");
            for name in props {
                out.push_str(&element(name, ""));
            }
            out.push_str(&format!("</d:prop><d:status>HTTP/1.1 {line}</d:status></d:propstat>"));
        }
    }
    out.push_str("</d:response>");
    Ok(multistatus(&[out]))
}

fn mkcalendar(target: &Target, body: Option<&Element>) -> Result<Response, Error> {
    let Target::Calendar(id) = target else {
        return Ok(status(403));
    };
    if !store::valid_id(id) || store::is_shared(id) {
        return Ok(status(403));
    }
    if store::calendar(id)?.is_some() {
        return Ok(status(405));
    }
    let mut calendar = Calendar::own(id.to_string(), id.to_string(), store::DEFAULT_COLOR.to_owned());
    settable(body, &mut calendar);
    store::save_calendar(&calendar)?;
    Ok(status(201))
}

fn get(target: &Target, head: bool) -> Result<Response, Error> {
    let Target::Object(calendar, name) = target else {
        return Ok(status(405));
    };
    let Some(entry) = store::object(calendar, name)? else {
        return Ok(status(404));
    };
    let body = if head { Vec::new() } else { store::read_object(calendar, name)? };
    Ok(Response {
        status: 200,
        headers: vec![header("content-type", "text/calendar; charset=utf-8"), header("etag", &etag(&entry))],
        body,
    })
}

fn put(target: &Target, request: &Request) -> Result<Response, Error> {
    let Target::Object(calendar, name) = target else {
        return Ok(status(405));
    };
    match store::calendar(calendar)? {
        None => return Ok(status(409)),
        Some(found) if !found.writable => return Ok(status(403)),
        Some(_) => {}
    }
    if !store::is_object(name) {
        return Ok(status(403));
    }
    let text = String::from_utf8_lossy(&request.body);
    let events = ical::events(&text);
    if !text.contains("BEGIN:VCALENDAR") || events.is_empty() || events.iter().any(|e| e.uid.is_empty()) {
        return Ok(precondition_failed(CALDAV, "valid-calendar-data"));
    }
    let current = store::object(calendar, name)?;
    if !preconditions_hold(request, current.as_ref()) {
        return Ok(status(412));
    }
    let entry = store::write_object(calendar, name, &request.body)?;
    let mut response = status(if current.is_some() { 204 } else { 201 });
    response.headers.push(header("etag", &etag(&entry)));
    Ok(response)
}

fn delete(target: &Target, request: &Request) -> Result<Response, Error> {
    match target {
        Target::Calendar(id) => {
            if store::calendar(id)?.is_none() {
                return Ok(status(404));
            }
            store::delete_calendar(id)?;
            Ok(status(204))
        }
        Target::Object(calendar, name) => {
            if !store::calendar(calendar)?.is_some_and(|found| found.writable) {
                return Ok(status(403));
            }
            let Some(current) = store::object(calendar, name)? else {
                return Ok(status(404));
            };
            if !preconditions_hold(request, Some(&current)) {
                return Ok(status(412));
            }
            store::delete_object(calendar, name)?;
            Ok(status(204))
        }
        _ => Ok(status(403)),
    }
}

/// `If-Match` and `If-None-Match` against the file as it is now.
fn preconditions_hold(request: &Request, current: Option<&Entry>) -> bool {
    let current = current.map(etag);
    let matches = |list: &str| {
        list.split(',').map(str::trim).any(|tag| {
            tag == "*" && current.is_some() || current.as_deref().is_some_and(|c| tag.trim_start_matches("W/") == c)
        })
    };
    if let Some(list) = request_header(request, "if-match") {
        if !matches(list) {
            return false;
        }
    }
    if let Some(list) = request_header(request, "if-none-match") {
        if matches(list) {
            return false;
        }
    }
    true
}

fn precondition_failed(ns: &str, condition: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"{DAV}\">{}</d:error>",
        element(&name(ns, condition), "")
    );
    Response {
        status: 403,
        headers: vec![header("content-type", "application/xml; charset=utf-8")],
        body: body.into_bytes(),
    }
}

// --- Helpers ----------------------------------------------------------------

fn etag(entry: &Entry) -> String {
    format!("\"{}\"", entry.etag)
}

/// RFC 3339 → the HTTP date format of `getlastmodified`.
fn http_date(rfc3339: &str) -> String {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|at| at.with_timezone(&Utc).format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

fn error_status(err: &Error) -> u16 {
    match err {
        Error::NotFound => 404,
        Error::InvalidPath => 400,
        Error::NoSpace => 507,
        Error::TooLarge => 413,
        Error::Conflict => 409,
        Error::NoUser => 401,
        Error::Other(message) if message == store::READ_ONLY => 403,
        Error::Other(_) => 500,
    }
}

fn status(status: u16) -> Response {
    Response { status, headers: Vec::new(), body: Vec::new() }
}

fn header(name: &str, value: &str) -> (String, String) {
    (name.to_owned(), value.to_owned())
}

fn request_header<'r>(request: &'r Request, name: &str) -> Option<&'r str> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

/// A path segment with everything but unreserved characters percent-encoded.
fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn decode(segment: &str) -> String {
    crate::decode(segment, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_hrefs() {
        assert!(matches!(target("/"), Some(Target::Root)));
        assert!(matches!(target("/calendars/work"), Some(Target::Calendar("work"))));
        assert!(matches!(target("/calendars/work/a b.ics"), Some(Target::Object("work", "a b.ics"))));
        assert!(target("/calendars/a/b/c").is_none());
        assert_eq!(object_href("work", "a b@x.ics"), "/app/calendar/dav/calendars/work/a%20b@x.ics");
        assert_eq!(decode("a%20b@x.ics"), "a b@x.ics");
    }

    #[test]
    fn unknown_properties_are_reported_missing() {
        let wanted = Wanted::These(vec![name(DAV, "displayname"), name("urn:other", "thing")]);
        let out = response("/x/", &[(name(DAV, "displayname"), "Home".into())], &wanted);
        assert!(out.contains("<d:displayname>Home</d:displayname>"));
        assert!(out.contains("<x:thing xmlns:x=\"urn:other\"></x:thing>"));
        assert!(out.contains("404 Not Found"));
    }

    fn request(headers: &[(&str, &str)]) -> Request {
        Request {
            method: "PUT".into(),
            path: String::new(),
            query: String::new(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn if_match_and_if_none_match() {
        let entry =
            Entry { name: "a.ics".into(), is_dir: false, size: 1, etag: "e1".into(), updated_at: String::new() };
        assert!(preconditions_hold(&request(&[]), Some(&entry)));
        assert!(preconditions_hold(&request(&[("If-Match", "\"e1\"")]), Some(&entry)));
        assert!(!preconditions_hold(&request(&[("If-Match", "\"e0\"")]), Some(&entry)));
        assert!(!preconditions_hold(&request(&[("If-None-Match", "*")]), Some(&entry)), "create only");
        assert!(preconditions_hold(&request(&[("If-None-Match", "*")]), None));
        assert!(!preconditions_hold(&request(&[("If-Match", "*")]), None));
    }

    #[test]
    fn time_range_filters_are_read() {
        let body = xml::parse(
            br#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
                <d:prop><d:getetag/></d:prop>
                <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
                  <c:time-range start="20261001T000000Z"/>
                </c:comp-filter></c:comp-filter></c:filter>
              </c:calendar-query>"#,
        )
        .unwrap();
        let Filter::Between(from, to) = query_filter(&body) else { panic!("a range") };
        assert_eq!(from.to_rfc3339(), "2026-10-01T00:00:00+00:00");
        assert_eq!(to, DateTime::<Utc>::MAX_UTC);

        let todos = xml::parse(
            br#"<c:calendar-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:filter>
                <c:comp-filter name="VCALENDAR"><c:comp-filter name="VTODO"/></c:comp-filter>
              </c:filter></c:calendar-query>"#,
        )
        .unwrap();
        assert!(matches!(query_filter(&todos), Filter::Nothing));
    }
}
