//! Notes: Markdown notes kept as `.md` files in a folder of the user's files.
//!
//! The app serves its page (`/`, `/app.js`, `/app.css`) and a small JSON API
//! under `/api/` that the page reaches through the panel's bridge:
//!
//! | request                   | what it does                                     |
//! |---------------------------|--------------------------------------------------|
//! | `GET /api/folder`         | the notes folder, or `null` before access is given |
//! | `GET /api/notes?q=`       | notes, newest first, optionally searched         |
//! | `POST /api/notes`         | a new note `{name}`                              |
//! | `GET /api/notes/<id>`     | a note's content and etag                        |
//! | `PUT /api/notes/<id>`     | save `{content, etag}`; 409 if changed elsewhere |
//! | `DELETE /api/notes/<id>`  | delete a note                                    |
//! | `POST /api/preview`       | Markdown (body) → HTML                           |

wit_bindgen::generate!({ path: "../../crates/plugin-api/wit", world: "app" });

mod markdown;
mod notes;

use rustcloud::app::files;
use serde::Deserialize;
use serde_json::json;

struct Notes;

impl Guest for Notes {
    fn handle(user: Option<User>, request: Request) -> Response {
        let method = request.method.as_str();
        let path = request.path.as_str();
        match (method, path) {
            ("GET", "/") => asset("text/html", include_str!("../ui/index.html")),
            ("GET", "/app.js") => asset("text/javascript", include_str!("../ui/app.js")),
            ("GET", "/app.css") => asset("text/css", include_str!("../ui/app.css")),
            _ if path.starts_with("/api/") => {
                if user.is_none() {
                    return error(401, "not logged in");
                }
                api(method, &path["/api".len()..], &request.query, &request.body)
            }
            _ => error(404, "not found"),
        }
    }
}

export!(Notes);

#[derive(Deserialize)]
struct NewNote {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Save {
    content: String,
    etag: String,
}

fn api(method: &str, path: &str, query: &str, body: &[u8]) -> Response {
    if (method, path) == ("POST", "/preview") {
        return asset("text/html", &markdown::to_html(&String::from_utf8_lossy(body)));
    }
    let folder = match notes::folder() {
        Ok(folder) => folder,
        Err(err) => return files_error(err),
    };
    if (method, path) == ("GET", "/folder") {
        return ok(json!({ "folder": folder.map(|f| json!({ "id": f.id, "name": f.name })) }));
    }
    let Some(folder) = folder else {
        return error(409, "no folder: ask the user for access first");
    };

    match (method, path.strip_prefix("/notes")) {
        ("GET", Some("")) => respond(notes::list(&folder, &query_param(query, "q"))),
        ("POST", Some("")) => match serde_json::from_slice::<NewNote>(body) {
            Ok(new) => respond(notes::create(&folder, &new.name)),
            Err(err) => error(400, &err.to_string()),
        },
        (_, Some(rest)) if rest.starts_with('/') => {
            let id = &rest[1..];
            match method {
                "GET" => respond(notes::open(id)),
                "PUT" => match serde_json::from_slice::<Save>(body) {
                    Ok(save) => match notes::save(id, &save.content, &save.etag) {
                        Ok(note) => ok(json!(note)),
                        Err(notes::SaveError::Changed(etag)) => {
                            json_response(409, json!({ "error": "changed elsewhere", "etag": etag }))
                        }
                        Err(notes::SaveError::Files(err)) => files_error(err),
                    },
                    Err(err) => error(400, &err.to_string()),
                },
                "DELETE" => match notes::delete(id) {
                    Ok(()) => ok(json!({})),
                    Err(err) => files_error(err),
                },
                _ => error(405, "method not allowed"),
            }
        }
        _ => error(404, "not found"),
    }
}

fn respond<T: serde::Serialize>(result: Result<T, files::Error>) -> Response {
    match result {
        Ok(value) => ok(json!(value)),
        Err(err) => files_error(err),
    }
}

fn files_error(err: files::Error) -> Response {
    let status = match err {
        files::Error::NoUser => 401,
        files::Error::NotFound => 404,
        files::Error::ReadOnly => 403,
        files::Error::InvalidName => 400,
        files::Error::NoSpace | files::Error::TooLarge => 413,
        files::Error::Conflict => 409,
        files::Error::Other(_) => 500,
    };
    error(status, &format!("{err:?}"))
}

/// A parameter of a query string, decoded.
fn query_param(query: &str, name: &str) -> String {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| decode(value))
        .unwrap_or_default()
}

/// Decodes `%XX` and `+` in a query value.
fn decode(value: &str) -> String {
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
            (b'+', _, _) => {
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

fn ok(value: serde_json::Value) -> Response {
    json_response(200, value)
}

fn error(status: u16, message: &str) -> Response {
    json_response(status, json!({ "error": message }))
}

fn json_response(status: u16, value: serde_json::Value) -> Response {
    Response {
        status,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: value.to_string().into_bytes(),
    }
}

fn asset(content_type: &str, body: &str) -> Response {
    Response {
        status: 200,
        headers: vec![("content-type".to_owned(), format!("{content_type}; charset=utf-8"))],
        body: body.as_bytes().to_vec(),
    }
}
