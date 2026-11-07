//! Text editor: any text file of the user's, wherever it lies, opened with
//! "Edit as text" in the files' menu (`[[open_with]]`). The panel gives the
//! app that one file while the page is open; the app reads and saves it
//! through the `files` import.
//!
//! | request                      | what it does                                    |
//! |------------------------------|-------------------------------------------------|
//! | `GET /edit?file=<id>`        | the page                                        |
//! | `GET /api/file?id=`          | the file: name, type, etag and its text         |
//! | `PUT /api/file?id=`          | save `{content, etag}`; 409 if changed elsewhere |
//! | `POST /api/preview`          | Markdown (body) → HTML                          |
//!
//! Only UTF-8 text is edited: anything else is refused (415), so a binary
//! file is never saved back broken. Highlighting is done by the page.

wit_bindgen::generate!({ path: "../../crates/plugin-api/wit", world: "app" });

mod markdown;

use rustcloud::app::files;
use serde::Deserialize;
use serde_json::json;

struct Editor;

impl Guest for Editor {
    fn handle(user: Option<User>, request: Request) -> Response {
        let method = request.method.as_str();
        let path = request.path.as_str();
        match (method, path) {
            ("GET", "/edit") => asset("text/html", include_str!("../ui/edit.html")),
            ("GET", "/app.js") => asset("text/javascript", include_str!("../ui/app.js")),
            ("GET", "/highlight.js") => asset("text/javascript", include_str!("../ui/highlight.js")),
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

export!(Editor);

#[derive(Deserialize)]
struct Save {
    content: String,
    etag: String,
}

fn api(method: &str, path: &str, query: &str, body: &[u8]) -> Response {
    match (method, path) {
        ("POST", "/preview") => {
            let external_images = query_param(query, "external_images") == "1";
            let preview = markdown::to_html(&String::from_utf8_lossy(body), external_images);
            ok(json!({ "html": preview.html, "blocked_images": preview.blocked_images }))
        }
        ("GET", "/file") => open(&query_param(query, "id")),
        ("PUT", "/file") => match serde_json::from_slice::<Save>(body) {
            Ok(save) => match files::write_if_match(&query_param(query, "id"), save.content.as_bytes(), &save.etag) {
                Ok(node) => ok(json!({ "etag": node.etag, "size": node.size })),
                Err(err) => files_error(err),
            },
            Err(err) => error(400, &format!("invalid body: {err}")),
        },
        _ => error(404, "not found"),
    }
}

/// The file and its text, if it is UTF-8.
fn open(id: &str) -> Response {
    let node = match files::get(id) {
        Ok(node) if node.is_dir => return error(400, "this is a folder"),
        Ok(node) => node,
        Err(err) => return files_error(err),
    };
    let bytes = match files::read(id) {
        Ok(bytes) => bytes,
        Err(err) => return files_error(err),
    };
    let Ok(content) = String::from_utf8(bytes) else {
        return error(415, "not UTF-8 text");
    };
    ok(json!({
        "id": node.id,
        "name": node.name,
        "mime_type": node.mime_type,
        "size": node.size,
        "etag": node.etag.unwrap_or_default(),
        "content": content,
    }))
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

/// The value of `name` in a query string, %-decoded.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(query_param("id=a%2Db&x=1", "id"), "a-b");
        assert_eq!(query_param("x=1", "id"), "");
        assert_eq!(decode("50%"), "50%");
        assert_eq!(decode("a+b%20c"), "a b c");
    }
}
