//! Calendar: events kept as `.ics` files in the user's AppData, shown on the
//! app's page and synced with phones and desktop calendars over CalDAV.
//!
//! | path        | what                                              |
//! |-------------|---------------------------------------------------|
//! | `/`         | the page (`/app.js`, `/app.css`)                  |
//! | `/api/…`    | its JSON API ([`api`])                            |
//! | `/dav/…`    | CalDAV ([`dav`]), where `/.well-known/caldav` leads |

wit_bindgen::generate!({ path: "../../crates/plugin-api/wit", world: "app" });

mod api;
mod dav;
mod expand;
mod ical;
mod store;
mod xml;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

struct App;

impl Guest for App {
    fn handle(user: Option<User>, request: Request) -> Response {
        let path = request.path.as_str();
        if let Some(rest) = path.strip_prefix("/dav").filter(|rest| rest.is_empty() || rest.starts_with('/')) {
            return dav::handle(user.as_ref(), &request, rest);
        }
        match (request.method.as_str(), path) {
            ("GET", "/") => asset("text/html", include_str!("../ui/index.html")),
            ("GET", "/app.js") => asset("text/javascript", include_str!("../ui/app.js")),
            ("GET", "/app.css") => asset("text/css", include_str!("../ui/app.css")),
            (method, _) if path.starts_with("/api/") => match &user {
                Some(user) => api::handle(user, method, &path["/api".len()..], &request.query, &request.body),
                None => Response { status: 401, headers: Vec::new(), body: b"not logged in".to_vec() },
            },
            _ => Response { status: 404, headers: Vec::new(), body: b"not found".to_vec() },
        }
    }
}

export!(App);

/// 128 random bits as hex, for event UIDs and calendar ids. `RandomState`
/// is seeded from the system's random source (WASI `random`).
pub fn random_id() -> String {
    let part = || RandomState::new().build_hasher().finish();
    format!("{:016x}{:016x}", part(), part())
}

/// A parameter of a query string, decoded.
pub fn query_param(query: &str, name: &str) -> String {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| decode(value, true))
        .unwrap_or_default()
}

/// Decodes `%XX` (and `+` as a space in query values).
pub fn decode(value: &str, plus_is_space: bool) -> String {
    let hex = |byte: u8| (byte as char).to_digit(16);
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (b'+', _, _) if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn asset(content_type: &str, body: &str) -> Response {
    Response {
        status: 200,
        headers: vec![("content-type".to_owned(), format!("{content_type}; charset=utf-8"))],
        body: body.as_bytes().to_vec(),
    }
}
