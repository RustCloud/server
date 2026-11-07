//! End-to-end tests of the HTTP API: the whole application router, as a browser
//! or API client sees it.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use http_body_util::BodyExt;
use rustcloud_db::testing::{test_db, TestDb};
use rustcloud_plugin_api::Manifest;
use rustcloud_server::{app, journal, AppState, Config};
use rustcloud_storage_local::LocalBlobStore;
use rustcloud_wasm_host::testing::{echo_manifest_toml, echo_wasm, install_echo, install_shipped};
use serde_json::{json, Value};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

struct TestApp {
    router: Router,
    _db: TestDb,
    /// Blobs and the config file.
    dir: tempfile::TempDir,
    /// The data folder of tests with WASM apps (plugins are installed there).
    data: Option<tempfile::TempDir>,
    /// Sends this test's events to its own activity log.
    _journal: tracing::subscriber::DefaultGuard,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", self.body))
    }

    fn text(&self) -> String {
        String::from_utf8(self.body.to_vec()).unwrap()
    }

    fn header(&self, name: header::HeaderName) -> &str {
        self.headers.get(&name).map(|v| v.to_str().unwrap()).unwrap_or_default()
    }

    /// The session cookie (`name=value`) set by this response.
    fn session_cookie(&self) -> String {
        self.header(header::SET_COOKIE).split(';').next().unwrap().to_owned()
    }
}

/// How a request authenticates.
#[derive(Clone, Copy)]
enum As<'a> {
    Anonymous,
    /// Browser: session cookie plus the CSRF header the web UI always sends.
    Browser(&'a str),
    /// Browser cookie without the CSRF header (what a cross-site form would send).
    CookieOnly(&'a str),
    /// API client: `Authorization: Bearer`.
    Token(&'a str),
    /// CalDAV and other clients: `Authorization: Basic` with a username and an app password.
    Basic(&'a str, &'a str),
}

impl TestApp {
    async fn new() -> Self {
        Self::with_config("").await
    }

    /// An app whose config file starts with `config_file`.
    async fn with_config(config_file: &str) -> Self {
        Self::build(config_file, Vec::new()).await
    }

    /// With the built-in apps plus `extra` ones (plugins under test).
    async fn build(config_file: &str, extra: Vec<Manifest>) -> Self {
        let db = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let store = LocalBlobStore::open(dir.path().join("blobs")).await.unwrap();
        let file = dir.path().join("rustcloud.toml");
        std::fs::write(&file, config_file).unwrap();
        let config = Config::load(&file).unwrap();
        let (layer, queue) = journal::channel();
        let _journal = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
        let state = AppState::new(db.conn.clone(), Arc::new(store), &config, queue).await.unwrap();
        for manifest in extra {
            state.plugins.add_built_in(manifest);
        }
        Self { router: app(state), _db: db, dir, data: None, _journal }
    }

    /// With the echo WASM app installed and open registration.
    async fn with_echo_app() -> Self {
        Self::with_wasm_apps(install_echo).await
    }

    /// With WASM apps that `install` puts into the apps folder, and open registration.
    async fn with_wasm_apps(install: impl FnOnce(&std::path::Path)) -> Self {
        let data = tempfile::tempdir().unwrap();
        install(&data.path().join("apps"));
        let config = format!("data_dir = {:?}\n[accounts]\nopen_registration = true\n", data.path());
        Self { data: Some(data), ..Self::build(&config, Vec::new()).await }
    }

    fn config_file(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("rustcloud.toml")).unwrap()
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        who: As<'_>,
        body: Body,
        extra: &[(header::HeaderName, &str)],
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(uri);
        match who {
            As::Anonymous => {}
            As::Browser(cookie) => {
                request = request.header(header::COOKIE, cookie).header("x-requested-with", "rustcloud");
            }
            As::CookieOnly(cookie) => request = request.header(header::COOKIE, cookie),
            As::Token(token) => request = request.header(header::AUTHORIZATION, format!("Bearer {token}")),
            As::Basic(username, password) => {
                let credentials = STANDARD.encode(format!("{username}:{password}"));
                request = request.header(header::AUTHORIZATION, format!("Basic {credentials}"));
            }
        }
        for (name, value) in extra {
            request = request.header(name, *value);
        }
        let response = self.router.clone().oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply { status, headers, body }
    }

    async fn get(&self, uri: &str, who: As<'_>) -> Reply {
        self.send(Method::GET, uri, who, Body::empty(), &[]).await
    }

    async fn json(&self, method: Method, uri: &str, who: As<'_>, body: Value) -> Reply {
        let json = [(header::CONTENT_TYPE, "application/json")];
        self.send(method, uri, who, Body::from(body.to_string()), &json).await
    }

    /// Registers an account and returns its session cookie.
    async fn register(&self, username: &str) -> String {
        let reply = self
            .json(
                Method::POST,
                "/api/v1/auth/register",
                As::Anonymous,
                json!({ "username": username, "password": "password123" }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        reply.session_cookie()
    }

    async fn root_id(&self, cookie: &str) -> String {
        self.get("/api/v1/files/root", As::Browser(cookie)).await.json()["id"].as_str().unwrap().to_owned()
    }

    async fn upload(&self, cookie: &str, folder: &str, name: &str, content: &'static str) -> Value {
        let uri = format!("/api/v1/files/{folder}/upload?name={name}");
        let reply = self.send(Method::POST, &uri, As::Browser(cookie), Body::from(content), &[]).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        reply.json()
    }
}

#[tokio::test]
async fn first_registration_sets_up_the_admin_and_a_session_cookie() {
    let app = TestApp::new().await;
    let status = app.get("/api/v1/auth/status", As::Anonymous).await.json();
    assert_eq!(status["needs_setup"], true);

    let reply = app
        .json(
            Method::POST,
            "/api/v1/auth/register",
            As::Anonymous,
            json!({ "username": "admin", "password": "password123" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json()["user"]["is_admin"], true);
    let cookie_header = reply.header(header::SET_COOKIE);
    assert!(cookie_header.contains("HttpOnly"), "{cookie_header}");
    assert!(cookie_header.contains("SameSite=Lax"), "{cookie_header}");

    let me = app.get("/api/v1/auth/me", As::Browser(&reply.session_cookie())).await;
    assert_eq!(me.json()["username"], "admin");

    let closed = app
        .json(
            Method::POST,
            "/api/v1/auth/register",
            As::Anonymous,
            json!({ "username": "eve", "password": "password123" }),
        )
        .await;
    assert_eq!(closed.status, StatusCode::FORBIDDEN);
    assert_eq!(closed.json()["error"]["code"], "REGISTRATION_CLOSED");
}

#[tokio::test]
async fn cookie_requests_that_change_state_need_the_csrf_header() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let update = json!({ "display_name": "Boss" });

    let forged = app.json(Method::PATCH, "/api/v1/auth/me", As::CookieOnly(&cookie), update.clone()).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(forged.json()["error"]["code"], "CSRF_CHECK_FAILED");

    // Reading is fine without the header…
    assert_eq!(app.get("/api/v1/auth/me", As::CookieOnly(&cookie)).await.status, StatusCode::OK);
    // …and the web UI's requests carry it.
    let real = app.json(Method::PATCH, "/api/v1/auth/me", As::Browser(&cookie), update).await;
    assert_eq!(real.json()["display_name"], "Boss");
}

#[tokio::test]
async fn api_clients_use_bearer_tokens() {
    let app = TestApp::new().await;
    app.register("admin").await;
    let login = app
        .json(
            Method::POST,
            "/api/v1/auth/login",
            As::Anonymous,
            json!({ "username": "admin", "password": "password123" }),
        )
        .await
        .json();
    let token = login["token"].as_str().unwrap();

    // A web page gets only the cookie: its scripts can't carry the session away.
    let page = [
        (header::ORIGIN, "http://localhost"),
        (header::HeaderName::from_static("sec-fetch-site"), "same-origin"),
        (header::CONTENT_TYPE, "application/json"),
    ];
    let body = json!({ "username": "admin", "password": "password123" }).to_string();
    let reply = app.send(Method::POST, "/api/v1/auth/login", As::Anonymous, Body::from(body), &page).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(reply.json().get("token").is_none(), "{}", reply.text());
    assert!(reply.header(header::SET_COOKIE).contains("rustcloud_session="));

    let me = app.json(Method::PATCH, "/api/v1/auth/me", As::Token(token), json!({ "email": "a@b.co" })).await;
    assert_eq!(me.status, StatusCode::OK, "no CSRF header is needed with a bearer token");
    assert_eq!(app.get("/api/v1/auth/me", As::Token("wrong")).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn client_apps_log_in_through_the_browser() {
    use sha2::Digest;
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let verifier = "verifier-of-the-desktop-client-0123456789-abcdefghij";
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier));
    let redirect = "http://127.0.0.1:53682/callback";

    let metadata = app.get("/.well-known/oauth-authorization-server", As::Anonymous).await.json();
    assert_eq!(metadata["token_endpoint"], "http://localhost/api/v1/auth/token");

    // The app opens the authorization endpoint; the consent page is the web UI's.
    let query = format!(
        "client_id=Desktop&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=xyz",
        "http%3A%2F%2F127.0.0.1%3A53682%2Fcallback"
    );
    let page = app.get(&format!("/api/v1/auth/authorize?{query}"), As::Anonymous).await;
    assert_eq!(page.header(header::LOCATION), format!("/#/connect?{query}"));
    let elsewhere = query.replace("http%3A%2F%2F127.0.0.1%3A53682", "https%3A%2F%2Fevil.example");
    let page = app.get(&format!("/api/v1/auth/authorize?{elsewhere}"), As::Anonymous).await;
    assert_eq!(page.header(header::LOCATION), "/#/connect?invalid=1");

    let asked = json!({
        "client_id": "Desktop", "redirect_uri": redirect,
        "code_challenge": challenge, "code_challenge_method": "S256",
    });
    let code = app.json(Method::POST, "/api/v1/auth/authorize", As::Browser(&cookie), asked.clone()).await.json();
    let code = code["code"].as_str().unwrap();
    let form = [(header::CONTENT_TYPE, "application/x-www-form-urlencoded")];
    let body = format!("grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri={redirect}");
    let reply = app.send(Method::POST, "/api/v1/auth/token", As::Anonymous, Body::from(body.clone()), &form).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    let tokens = reply.json();
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(tokens["token_type"], "Bearer");

    let again = app.send(Method::POST, "/api/v1/auth/token", As::Anonymous, Body::from(body), &form).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(again.json()["error"], "invalid_grant");

    // The app works with files, but not with what would take the account.
    assert_eq!(app.get("/api/v1/files/root", As::Token(access)).await.status, StatusCode::OK);
    assert_eq!(app.get("/api/v1/admin/users", As::Token(access)).await.json()["error"]["code"], "BROWSER_ONLY");
    let password = json!({ "current_password": "password123", "new_password": "another-password-1" });
    let reply = app.json(Method::POST, "/api/v1/auth/password", As::Token(access), password).await;
    assert_eq!(reply.json()["error"]["code"], "BROWSER_ONLY");
    let reply = app.json(Method::POST, "/api/v1/auth/authorize", As::Token(access), asked).await;
    assert_eq!(reply.json()["error"]["code"], "BROWSER_ONLY");

    // A new pair for the refresh token, JSON works too.
    let refresh = json!({ "grant_type": "refresh_token", "refresh_token": tokens["refresh_token"] });
    let renewed = app.json(Method::POST, "/api/v1/auth/token", As::Anonymous, refresh).await;
    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.text());
    let unknown = json!({ "grant_type": "password", "username": "admin" });
    let reply = app.json(Method::POST, "/api/v1/auth/token", As::Anonymous, unknown).await;
    assert_eq!(reply.json()["error"], "unsupported_grant_type");

    // Ending the session in the browser stops the app at once.
    let sessions = app.get("/api/v1/auth/sessions", As::Browser(&cookie)).await.json();
    let device = sessions.as_array().unwrap().iter().find(|s| s["device"] == true).unwrap();
    assert_eq!(device["name"], "Desktop");
    let uri = format!("/api/v1/auth/sessions/{}", device["id"].as_str().unwrap());
    assert_eq!(
        app.send(Method::DELETE, &uri, As::Browser(&cookie), Body::empty(), &[]).await.status,
        StatusCode::NO_CONTENT
    );
    let access = renewed.json()["access_token"].as_str().unwrap().to_owned();
    assert_eq!(app.get("/api/v1/files/root", As::Token(&access)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let reply = app.send(Method::POST, "/api/v1/auth/logout", As::Browser(&cookie), Body::empty(), &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.header(header::SET_COOKIE).contains("Max-Age=0"), "the cookie is removed");
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&cookie)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_admins_manage_accounts() {
    let app = TestApp::new().await;
    let admin = app.register("admin").await;

    let created = app
        .json(
            Method::POST,
            "/api/v1/admin/users",
            As::Browser(&admin),
            json!({ "username": "bob", "password": "password123" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(app.get("/api/v1/admin/users", As::Browser(&admin)).await.json().as_array().unwrap().len(), 2);

    let bob = app
        .json(
            Method::POST,
            "/api/v1/auth/login",
            As::Anonymous,
            json!({ "username": "bob", "password": "password123" }),
        )
        .await
        .session_cookie();
    assert_eq!(app.get("/api/v1/admin/users", As::Browser(&bob)).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn files_round_trip_with_ranges_and_etags() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;

    let file = app.upload(&cookie, &root, "hello.txt", "hello world").await;
    let id = file["id"].as_str().unwrap();
    let etag = format!("\"{}\"", file["etag"].as_str().unwrap());

    let children = app.get(&format!("/api/v1/files/{root}/children"), As::Browser(&cookie)).await.json();
    assert_eq!(children[0]["name"], "hello.txt");

    let content_uri = format!("/api/v1/files/{id}/content");
    let full = app.get(&content_uri, As::Browser(&cookie)).await;
    assert_eq!(full.text(), "hello world");
    assert_eq!(full.header(header::ETAG), etag);
    assert_eq!(full.header(header::CONTENT_DISPOSITION), "inline; filename=\"hello.txt\"; filename*=UTF-8''hello.txt");

    let partial =
        app.send(Method::GET, &content_uri, As::Browser(&cookie), Body::empty(), &[(header::RANGE, "bytes=6-")]).await;
    assert_eq!(partial.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(partial.text(), "world");
    assert_eq!(partial.header(header::CONTENT_RANGE), "bytes 6-10/11");

    let beyond =
        app.send(Method::GET, &content_uri, As::Browser(&cookie), Body::empty(), &[(header::RANGE, "bytes=50-")]).await;
    assert_eq!(beyond.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(beyond.header(header::CONTENT_RANGE), "bytes */11");

    let cached = app
        .send(Method::GET, &content_uri, As::Browser(&cookie), Body::empty(), &[(header::IF_NONE_MATCH, &etag)])
        .await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    assert!(cached.body.is_empty());

    // Deleted once it goes to the trash, deleted there it is gone.
    for _ in 0..2 {
        let deleted =
            app.send(Method::DELETE, &format!("/api/v1/files/{id}"), As::Browser(&cookie), Body::empty(), &[]).await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(app.get(&content_uri, As::Browser(&cookie)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pdfs_of_public_links_are_downloaded_not_shown() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;
    for (name, disposition) in [("paper.pdf", "attachment"), ("photo.png", "inline")] {
        let file = app.upload(&cookie, &root, name, "%PDF-1.4").await;
        let share = app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), json!({ "node_id": file["id"] }));
        let token = share.await.json()["token"].as_str().unwrap().to_owned();
        let reply = app.get(&format!("/api/v1/public/{token}/content"), As::Anonymous).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(reply.header(header::CONTENT_DISPOSITION).starts_with(disposition), "{name}");
    }
}

#[tokio::test]
async fn deleted_files_wait_in_the_trash_until_restored_or_emptied() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;
    let file = app.upload(&cookie, &root, "report.txt", "numbers").await;
    let id = file["id"].as_str().unwrap();
    let share = app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), json!({ "node_id": id })).await.json();
    let public = format!("/api/v1/public/{}", share["token"].as_str().unwrap());

    let deleted =
        app.send(Method::DELETE, &format!("/api/v1/files/{id}"), As::Browser(&cookie), Body::empty(), &[]).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let trash = app.get("/api/v1/files/trash", As::Browser(&cookie)).await.json();
    assert_eq!(trash["name"], ".trash");
    let trash_children = format!("/api/v1/files/{}/children", trash["id"].as_str().unwrap());
    let in_trash = app.get(&trash_children, As::Browser(&cookie)).await.json();
    assert_eq!(in_trash[0]["name"], "report.txt");
    assert!(in_trash[0]["trashed_at"].is_string());
    assert_eq!(app.get(&public, As::Anonymous).await.status, StatusCode::NOT_FOUND);

    let restore = format!("/api/v1/files/{id}/restore");
    let restored = app.json(Method::POST, &restore, As::Browser(&cookie), json!(null)).await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.text());
    assert_eq!(restored.json()["parent_id"], root.as_str());
    assert_eq!(app.get(&public, As::Anonymous).await.status, StatusCode::OK);

    app.send(Method::DELETE, &format!("/api/v1/files/{id}"), As::Browser(&cookie), Body::empty(), &[]).await;
    let emptied = app.send(Method::DELETE, "/api/v1/files/trash", As::Browser(&cookie), Body::empty(), &[]).await;
    assert_eq!(emptied.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get(&format!("/api/v1/files/{id}"), As::Browser(&cookie)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploaded_html_is_never_rendered_on_our_origin() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;
    let file = app.upload(&cookie, &root, "page.html", "<script>alert(1)</script>").await;

    let reply = app.get(&format!("/api/v1/files/{}/content", file["id"].as_str().unwrap()), As::Browser(&cookie)).await;
    assert!(reply.header(header::CONTENT_DISPOSITION).starts_with("attachment"));
    assert!(reply.header(header::CONTENT_SECURITY_POLICY).contains("sandbox"));
    assert_eq!(reply.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
}

#[tokio::test]
async fn public_links_work_without_login() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&cookie), json!({ "name": "Photos" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap();
    let file = app.upload(&cookie, folder_id, "cat.txt", "meow").await;

    let share =
        app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), json!({ "node_id": folder_id })).await.json();
    let token = share["token"].as_str().unwrap();

    let view = app.get(&format!("/api/v1/public/{token}"), As::Anonymous).await.json();
    assert_eq!(view["shared"]["name"], "Photos");
    assert_eq!(view["children"][0]["name"], "cat.txt");

    let uri = format!("/api/v1/public/{token}/content?node={}&download=1", file["id"].as_str().unwrap());
    let download = app.get(&uri, As::Anonymous).await;
    assert_eq!(download.text(), "meow");
    assert!(download.header(header::CONTENT_DISPOSITION).starts_with("attachment"));

    assert_eq!(app.get("/api/v1/public/not-a-token", As::Anonymous).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn files_need_a_session() {
    let app = TestApp::new().await;
    let reply = app.get("/api/v1/files/root", As::Anonymous).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json()["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn logged_in_users_get_the_list_of_apps() {
    let app = TestApp::new().await;
    assert_eq!(app.get("/api/v1/apps", As::Anonymous).await.status, StatusCode::UNAUTHORIZED);

    let cookie = app.register("alice").await;
    let reply = app.get("/api/v1/apps", As::Browser(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["api_version"], rustcloud_plugin_api::API_VERSION);
    let files = &body["apps"][0];
    assert_eq!(files["id"], "files");
    assert_eq!(files["name"]["en"], "Files");
    assert_eq!(files["ui"]["entry"], "/files");
    assert_eq!(files["provides"]["rustcloud:files"], "0.1.0");
}

/// A plugin that may open files for writing while they are open.
fn editor_app() -> Manifest {
    Manifest::from_toml(&format!(
        "id = \"editor\"\nversion = \"1.0.0\"\napi = \"={}\"\n[name]\nen = \"Editor\"\n\
         [permissions]\nfiles = \"opened\"\nfiles_write = true\n",
        rustcloud_plugin_api::API_VERSION
    ))
    .unwrap()
}

#[tokio::test]
async fn users_give_apps_access_to_open_files_and_end_it() {
    let app = TestApp::build("[accounts]\nopen_registration = true\n", vec![editor_app()]).await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let file = app.upload(&alice, &root, "doc.md", "# Hi").await;
    let file_id = file["id"].as_str().unwrap();

    let grant = |app_id: &str, kind: &str| json!({ "app_id": app_id, "node_id": file_id, "write": true, "kind": kind });
    let reply = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant("editor", "session")).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let id = reply.json()["id"].as_str().unwrap().to_owned();

    let reply = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant("editor", "standing")).await;
    assert_eq!(reply.json()["error"]["code"], "PERMISSION_NOT_DECLARED");
    let reply = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant("nope", "session")).await;
    assert_eq!(reply.json()["error"]["code"], "APP_NOT_FOUND");

    let keep_alive = format!("/api/v1/grants/{id}/keep-alive");
    let reply = app.json(Method::POST, &keep_alive, As::Browser(&alice), json!({})).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let list = app.get("/api/v1/grants", As::Browser(&alice)).await.json();
    assert_eq!(list[0]["app_id"], "editor");
    assert_eq!(list[0]["node"]["name"], "doc.md");

    let bob = app.register("bob").await;
    assert_eq!(app.get("/api/v1/grants", As::Browser(&bob)).await.json(), json!([]));
    let reply = app.send(Method::DELETE, &format!("/api/v1/grants/{id}"), As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "only the owner ends it");

    let reply =
        app.send(Method::DELETE, &format!("/api/v1/grants/{id}"), As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/api/v1/grants", As::Browser(&alice)).await.json(), json!([]));
}

#[tokio::test]
async fn wasm_apps_answer_under_their_prefix_in_a_sandbox() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let apps = app.get("/api/v1/apps", As::Browser(&alice)).await.json();
    assert_eq!(apps["apps"][1]["id"], "echo");

    let reply = app.get("/app/echo/hello", As::Browser(&alice)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "Hello, alice!");
    assert!(reply.header(header::CONTENT_SECURITY_POLICY).starts_with("sandbox"), "never our origin");
    assert!(reply.header(header::CONTENT_SECURITY_POLICY).contains("frame-ancestors 'self'"));
    assert_eq!(reply.header(header::X_FRAME_OPTIONS), "SAMEORIGIN", "the panel frames app pages");
    let api = app.get("/api/v1/auth/me", As::Browser(&alice)).await;
    assert_eq!(api.header(header::X_FRAME_OPTIONS), "DENY", "nothing else can be framed");
    assert_eq!(app.get("/app/echo/hello", As::Anonymous).await.text(), "Hello, guest!");

    let headers = app.get("/app/echo/headers", As::Token(&alice)).await.text();
    assert!(!headers.contains("cookie") && !headers.contains("authorization"), "no session for apps: {headers}");
    let reply = app.get("/app/echo/cookie", As::Browser(&alice)).await;
    assert_eq!(reply.header(header::SET_COOKIE), "", "apps can't set cookies");

    let reply = app.send(Method::POST, "/app/echo/echo", As::CookieOnly(&alice), Body::from("x"), &[]).await;
    assert_eq!(reply.json()["error"]["code"], "CSRF_CHECK_FAILED", "same rules as the API");
    let reply = app.send(Method::POST, "/app/echo/echo", As::Browser(&alice), Body::from("x"), &[]).await;
    assert_eq!(reply.text(), "x");

    let remember = app.send(Method::POST, "/app/echo/remember", As::Browser(&alice), Body::from("milk"), &[]).await;
    assert_eq!(remember.text(), "saved");
    assert_eq!(app.get("/app/echo/recall", As::Browser(&alice)).await.text(), "milk");
    let root = app.root_id(&alice).await;
    let top = app.get(&format!("/api/v1/files/{root}/children"), As::Browser(&alice)).await.json();
    assert_eq!(top[0]["name"], ".appdata", "app data is the user's own hidden folder");

    assert_eq!(app.get("/app/nope/", As::Browser(&alice)).await.json()["error"]["code"], "APP_NOT_FOUND");
}

#[tokio::test]
async fn a_request_that_reads_many_files_is_one_line_in_the_activity_log() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&alice), json!({ "name": "Photos" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    for name in ["a.txt", "b.txt", "c.txt"] {
        app.upload(&alice, &folder_id, name, "xy").await;
    }
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": false, "kind": "standing" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    assert_eq!(app.get("/app/echo/read-all", As::Browser(&alice)).await.text(), "3");

    let mut reads = Vec::new();
    for _ in 0..1000 {
        let entries = app.get("/api/v1/admin/journal?q=echo", As::Browser(&alice)).await.json()["entries"].clone();
        reads = entries.as_array().unwrap().iter().filter(|e| e["action"] == "file.download").cloned().collect();
        if !reads.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(reads.len(), 1, "{reads:#?}");
    assert_eq!(reads[0]["details"]["count"], 3);
    assert_eq!(reads[0]["details"]["size"], 6);
    assert_eq!(reads[0]["username"], "alice");
}

#[tokio::test]
async fn apps_send_files_of_any_size_and_give_their_pages_links_to_them() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&alice), json!({ "name": "Videos" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    // More than the app may take at once (16 MiB at ×1): it never passes through the app.
    let big: Vec<u8> = (0..17 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let upload = format!("/api/v1/files/{folder_id}/upload?name=film.mp4");
    let film = app.send(Method::POST, &upload, As::Browser(&alice), Body::from(big.clone()), &[]).await.json();
    let film_id = film["id"].as_str().unwrap().to_owned();
    let secret = app.upload(&alice, &root, "secret.txt", "x").await;
    let secret_id = secret["id"].as_str().unwrap().to_owned();
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": false, "kind": "standing" });
    let grant = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.json();

    // The app answers with the file: all of it, or the range the browser asks for.
    let sent = app.get(&format!("/app/echo/send?file:{film_id}"), As::Browser(&alice)).await;
    assert_eq!((sent.status, sent.body.len()), (StatusCode::OK, big.len()));
    assert_eq!(sent.header(header::CONTENT_TYPE), "video/mp4");
    let range = [(header::RANGE, "bytes=1000-1999")];
    let send = format!("/app/echo/send?file:{film_id}");
    let part = app.send(Method::GET, &send, As::Browser(&alice), Body::empty(), &range).await;
    assert_eq!((part.status, &part.body[..]), (StatusCode::PARTIAL_CONTENT, &big[1000..2000]));
    let outside = app.get(&format!("/app/echo/send?file:{secret_id}"), As::Browser(&alice)).await;
    assert_eq!(outside.status, StatusCode::NOT_FOUND, "only what the user gave the app");

    // It reads a part of it itself.
    let read = app.get(&format!("/app/echo/range?{film_id}:5:3"), As::Browser(&alice)).await;
    assert_eq!(&read.body[..], &big[5..8]);

    // Its page gets a link the browser follows without a cookie, as an <img> or <video> does.
    let link = app.get(&format!("/app/echo/link?file:{film_id}"), As::Browser(&alice)).await.text();
    assert!(link.starts_with("/api/v1/app-links/"), "{link}");
    let again = app.get(&format!("/app/echo/link?file:{film_id}"), As::Browser(&alice)).await.text();
    assert_eq!(link, again, "the same file and version, the same link: the browser caches it");
    let followed = app.send(Method::GET, &link, As::Anonymous, Body::empty(), &range).await;
    assert_eq!((followed.status, &followed.body[..]), (StatusCode::PARTIAL_CONTENT, &big[1000..2000]));
    let forged = format!("{}x", &link[..link.len() - 1]);
    assert_eq!(app.get(&forged, As::Anonymous).await.status, StatusCode::NOT_FOUND);
    let none = app.get(&format!("/app/echo/link?file:{secret_id}"), As::Browser(&alice)).await.text();
    assert_eq!(none, "Error::NotFound");
    let anonymous = app.get(&format!("/app/echo/link?file:{film_id}"), As::Anonymous).await.text();
    assert_eq!(anonymous, "Error::NoSession", "no session, no links");
    // A stream of it needs use.ffmpeg, which echo doesn't have here.
    let stream = app.get(&format!("/app/echo/stream?copy:{film_id}"), As::Browser(&alice)).await.text();
    assert!(stream.contains("use.ffmpeg"), "{stream}");
    let not_a_stream = format!("{link}/index.m3u8");
    assert_eq!(app.get(&not_a_stream, As::Anonymous).await.status, StatusCode::NOT_FOUND, "a file's link is no stream");
    assert_eq!(followed.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), "*", "hls.js fetches from the page's origin");

    // Taking access back ends the link; so does logging out.
    let revoke = format!("/api/v1/grants/{}", grant["id"].as_str().unwrap());
    app.send(Method::DELETE, &revoke, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(app.get(&link, As::Anonymous).await.status, StatusCode::NOT_FOUND, "access taken back");
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": false, "kind": "standing" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    assert_eq!(app.get(&link, As::Anonymous).await.status, StatusCode::OK, "given again");
    app.send(Method::POST, "/api/v1/auth/logout", As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(app.get(&link, As::Anonymous).await.status, StatusCode::NOT_FOUND, "logged out");
}

#[tokio::test]
async fn app_pages_upload_files_of_any_size_through_links() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&alice), json!({ "name": "Photos" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": true, "kind": "standing" });
    let grant = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.json();
    let link_for = |query: String| {
        let app = &app;
        let alice = alice.clone();
        async move { app.get(&format!("/app/echo/upload?{query}"), As::Browser(&alice)).await.text() }
    };

    // The page sends the file from its sandbox: a cross-origin POST without a cookie.
    let link = link_for(format!("{folder_id}:film.mp4")).await;
    assert!(link.starts_with("/api/v1/app-links/"), "{link}");
    let preflight = [
        (header::ORIGIN, "null"),
        (header::ACCESS_CONTROL_REQUEST_METHOD, "POST"),
        (header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type"),
    ];
    let allowed = app.send(Method::OPTIONS, &link, As::Anonymous, Body::empty(), &preflight).await;
    assert_eq!(allowed.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), "null", "the page's origin may send");
    // More than the app may take at once (16 MiB at ×1): it never passes through the app.
    let big: Vec<u8> = (0..17 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let uploaded = app.send(Method::POST, &link, As::Anonymous, Body::from(big.clone()), &[]).await;
    assert_eq!(uploaded.status, StatusCode::CREATED, "{}", uploaded.text());
    let film = uploaded.json();
    assert_eq!((film["name"].as_str(), film["parent_id"].as_str()), (Some("film.mp4"), Some(folder_id.as_str())));
    let content =
        app.get(&format!("/api/v1/files/{}/content", film["id"].as_str().unwrap()), As::Browser(&alice)).await;
    assert!(content.body[..] == big[..], "stored as sent");
    let again = app.send(Method::POST, &link, As::Anonymous, Body::from("x"), &[]).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND, "a link serves one upload");

    // The name must be free, unless the app asks to replace the file.
    assert!(link_for(format!("{folder_id}:film.mp4")).await.contains("already exists"));
    let replace = link_for(format!("{folder_id}:film.mp4:overwrite")).await;
    let replaced = app.send(Method::POST, &replace, As::Anonymous, Body::from("short"), &[]).await;
    assert_eq!((replaced.status, replaced.json()["size"].as_i64()), (StatusCode::CREATED, Some(5)));
    assert_eq!(link_for(format!("{root}:x.txt")).await, "Error::NotFound", "only folders the user gave");
    let anonymous = app.get(&format!("/app/echo/upload?{folder_id}:x.txt"), As::Anonymous).await.text();
    assert_eq!(anonymous, "Error::NoSession", "no session, no links");

    // Access is checked again when the upload comes; a failed upload may be tried again.
    let link = link_for(format!("{folder_id}:later.txt")).await;
    let revoke = format!("/api/v1/grants/{}", grant["id"].as_str().unwrap());
    app.send(Method::DELETE, &revoke, As::Browser(&alice), Body::empty(), &[]).await;
    let refused = app.send(Method::POST, &link, As::Anonymous, Body::from("later"), &[]).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "access taken back");
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": false, "kind": "standing" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    let read_only = app.send(Method::POST, &link, As::Anonymous, Body::from("later"), &[]).await;
    assert_eq!(read_only.status, StatusCode::FORBIDDEN, "given to read only");
    assert!(link_for(format!("{folder_id}:other.txt")).await.contains("only read"), "no links to read-only folders");
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": true, "kind": "standing" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    let later = app.send(Method::POST, &link, As::Anonymous, Body::from("later"), &[]).await;
    assert_eq!(later.status, StatusCode::CREATED, "{}", later.text());

    // Logging out ends the links.
    let link = link_for(format!("{folder_id}:after.txt")).await;
    app.send(Method::POST, "/api/v1/auth/logout", As::Browser(&alice), Body::empty(), &[]).await;
    let logged_out = app.send(Method::POST, &link, As::Anonymous, Body::from("x"), &[]).await;
    assert_eq!(logged_out.status, StatusCode::NOT_FOUND, "logged out");
}

#[tokio::test]
async fn wasm_apps_reach_only_the_files_they_were_given() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&alice), json!({ "name": "Notes" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    app.upload(&alice, &folder_id, "todo.md", "- milk").await;
    let secret = app.upload(&alice, &root, "secret.txt", "x").await;

    assert_eq!(
        app.get("/app/echo/granted", As::Browser(&alice)).await.text(),
        "",
        "nothing before the user gives access"
    );
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": true, "kind": "standing" });
    assert_eq!(app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.status, StatusCode::CREATED);

    assert_eq!(app.get("/app/echo/granted", As::Browser(&alice)).await.text(), "Notes");
    assert_eq!(app.get("/app/echo/read-first", As::Browser(&alice)).await.text(), "- milk");
    let create = app.send(Method::POST, "/app/echo/create", As::Browser(&alice), Body::from("hi"), &[]).await;
    assert_eq!(create.text(), "from-echo.txt");
    let names = app.get(&format!("/api/v1/files/{folder_id}/children"), As::Browser(&alice)).await.json();
    assert!(names.as_array().unwrap().iter().any(|n| n["name"] == "from-echo.txt"));

    let outside = app.get(&format!("/app/echo/get?{}", secret["id"].as_str().unwrap()), As::Browser(&alice)).await;
    assert_eq!(outside.text(), "Error::NotFound", "files outside the grant don't exist for the app");
    assert_eq!(app.get("/app/echo/granted", As::Anonymous).await.text(), "Error::NoUser");
    assert_eq!(app.get("/app/echo/panic", As::Browser(&alice)).await.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn other_servers_reach_one_file_with_a_token_of_the_app() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let doc = app.upload(&alice, &root, "doc.md", "# Hi").await;
    let doc_id = doc["id"].as_str().unwrap().to_owned();
    let other = app.upload(&alice, &root, "other.md", "x").await;
    let other_id = other["id"].as_str().unwrap();

    let no_access = app.get(&format!("/app/echo/token?{doc_id}:rw"), As::Browser(&alice)).await;
    assert_eq!(no_access.text(), "Error::NotFound");
    let grant = json!({ "app_id": "echo", "node_id": doc_id, "write": true, "kind": "session" });
    let grant = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.json();
    let grant_id = grant["id"].as_str().unwrap().to_owned();
    let token = app.get(&format!("/app/echo/token?{doc_id}:rw"), As::Browser(&alice)).await.text();
    assert!(token.starts_with("rct1."), "{token}");

    // No session, no cookie: only the token.
    let call = |file: &str| format!("/app/echo/wopi?file={file}&access_token={token}");
    assert_eq!(app.get(&call(&doc_id), As::Anonymous).await.text(), "true true # Hi");
    let wrote = app.send(Method::POST, &call(&doc_id), As::Anonymous, Body::from("# New"), &[]).await;
    assert_eq!(wrote.status, StatusCode::OK, "{}", wrote.text());
    assert!(app.get(&call(other_id), As::Anonymous).await.text().contains("NotFound"), "only its file");
    let bearer = format!("Bearer {token}");
    let header = [(header::AUTHORIZATION, bearer.as_str())];
    let by_header = format!("/app/echo/wopi?file={doc_id}");
    let by_header = app.send(Method::GET, &by_header, As::Anonymous, Body::empty(), &header).await;
    assert_eq!(by_header.text(), "true true # New");

    // The app's lock keeps the panel out; the owner may break it.
    let content = format!("/api/v1/files/{doc_id}/content");
    let panel = app.send(Method::PUT, &content, As::Browser(&alice), Body::from("x"), &[]).await;
    assert_eq!(panel.json()["error"]["code"], "FILE_LOCKED");
    let lock = format!("/api/v1/files/{doc_id}/lock");
    let unlock = app.send(Method::DELETE, &lock, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(unlock.status, StatusCode::NO_CONTENT);

    // Closing the page leaves a minute for the editor; revoking ends it at once.
    let closing = format!("/api/v1/grants/{grant_id}?closing=true");
    assert_eq!(
        app.send(Method::DELETE, &closing, As::Browser(&alice), Body::empty(), &[]).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.get(&call(&doc_id), As::Anonymous).await.status, StatusCode::OK);
    let revoke = format!("/api/v1/grants/{grant_id}");
    app.send(Method::DELETE, &revoke, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(app.get(&call(&doc_id), As::Anonymous).await.status, StatusCode::UNAUTHORIZED);
    let forged = format!("/app/echo/wopi?file={doc_id}&access_token=rct1.x.y");
    assert_eq!(app.get(&forged, As::Anonymous).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_app_saves_request_bodies_too_large_for_it_into_files() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let doc = app.upload(&alice, &root, "doc.bin", "v1").await;
    let doc_id = doc["id"].as_str().unwrap().to_owned();
    let first_etag = doc["etag"].as_str().unwrap().to_owned();
    // As when the user opens the file in the app: access to this file only.
    let grant = json!({ "app_id": "echo", "node_id": doc_id, "write": true, "kind": "session" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    let save = format!("/app/echo/save?{doc_id}");

    // A small body: the app gets it, and may save it all the same; a size
    // header from the client means nothing.
    let spoofed = [(header::HeaderName::from_static("x-rustcloud-body-size"), "999")];
    let small = app.send(Method::POST, &save, As::Browser(&alice), Body::from("small"), &spoofed).await;
    assert!(small.text().starts_with("- 5 "), "{}", small.text());

    // Larger than the app takes at once (16 MiB at ×1): kept on disk, saved
    // by the server; with an ETag that no longer holds, not at all.
    let big = vec![b'x'; (16 << 20) + 1];
    let stale = format!("{save}:{first_etag}");
    let refused = app.send(Method::POST, &stale, As::Browser(&alice), Body::from(big.clone()), &[]).await;
    assert_eq!(refused.text(), "Error::Conflict");
    let saved = app.send(Method::POST, &save, As::Browser(&alice), Body::from(big.clone()), &[]).await;
    assert!(saved.text().starts_with(&format!("{} 0 ", big.len())), "{}", saved.text());
    let content = app.get(&format!("/api/v1/files/{doc_id}/content"), As::Browser(&alice)).await;
    assert_eq!(content.body.len(), big.len());
    let kept = app.data.as_ref().unwrap().path().join("tmp").join("request-bodies");
    assert_eq!(std::fs::read_dir(kept).unwrap().count(), 0, "kept bodies are gone once answered");

    // Without a user who may write, a large body is refused as before.
    let anonymous = app.send(Method::POST, &save, As::Anonymous, Body::from(big), &[]).await;
    assert_eq!(anonymous.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn app_passwords_let_programs_into_their_apps_only() {
    let app = TestApp::with_echo_app().await;
    let alice = app.register("alice").await;
    let create = |access: &str, app_id: &str| {
        let body = json!({ "name": "Phone", "scopes": [{ "app": app_id, "access": access }] });
        app.json(Method::POST, "/api/v1/app-passwords", As::Browser(&alice), body)
    };
    let reply = create("read", "echo").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let read = reply.json()["password"].as_str().unwrap().to_owned();
    let write = create("write", "echo").await.json()["password"].as_str().unwrap().to_owned();
    let other = create("write", "files").await.json()["password"].as_str().unwrap().to_owned();
    assert_eq!(create("read", "nope").await.json()["error"]["code"], "APP_NOT_FOUND");

    let listed = app.get("/api/v1/app-passwords", As::Browser(&alice)).await.json();
    assert_eq!(listed.as_array().unwrap().len(), 3);
    assert!(listed[0].get("password").is_none(), "shown only once");

    assert_eq!(app.get("/app/echo/hello", As::Basic("alice", &read)).await.text(), "Hello, alice!");
    let refused = app.get("/app/echo/hello", As::Basic("alice", "wrong")).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(refused.header(header::WWW_AUTHENTICATE).starts_with("Basic"), "clients ask for the password");
    let refused = app.get("/app/echo/hello", As::Basic("alice", &other)).await;
    assert_eq!(refused.json()["error"]["code"], "APP_PASSWORD_SCOPE");
    let api = app.get("/api/v1/auth/me", As::Basic("alice", &write)).await;
    assert_eq!(api.status, StatusCode::UNAUTHORIZED, "app passwords open apps, not the API");

    // Read-only: methods that change something are refused, and so are
    // changes the app tries to make while answering a reading method.
    let post = app.send(Method::POST, "/app/echo/remember", As::Basic("alice", &read), Body::from("x"), &[]).await;
    assert_eq!(post.json()["error"]["code"], "READ_ONLY");
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    let sneaky = app.send(propfind, "/app/echo/remember", As::Basic("alice", &read), Body::from("x"), &[]).await;
    assert!(sneaky.text().contains("may only read"), "{}", sneaky.text());
    let post = app.send(Method::POST, "/app/echo/remember", As::Basic("alice", &write), Body::from("x"), &[]).await;
    assert_eq!(post.text(), "saved");

    let id = listed.as_array().unwrap().iter().find(|p| p["scopes"][0]["access"] == "read").unwrap()["id"].clone();
    let uri = format!("/api/v1/app-passwords/{}", id.as_str().unwrap());
    let revoke = app.send(Method::DELETE, &uri, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/app/echo/hello", As::Basic("alice", &read)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn notes_are_markdown_files_in_the_folder_the_user_gave() {
    let app = TestApp::with_wasm_apps(|apps| install_shipped(apps, "notes")).await;
    let alice = app.register("alice").await;
    let call = |method: Method, path: &str, body: Value| {
        let path = format!("/app/notes/api{path}");
        let alice = alice.clone();
        let app = &app;
        async move { app.json(method, &path, As::Browser(&alice), body).await }
    };
    assert_eq!(call(Method::GET, "/folder", json!(null)).await.json()["folder"], Value::Null);

    // What the panel's dialog does when the user allows it.
    let root = app.root_id(&alice).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&alice), json!({ "name": "Notes" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    let grant = json!({ "app_id": "notes", "node_id": folder_id, "write": true, "kind": "standing" });
    assert_eq!(app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.status, StatusCode::CREATED);
    assert_eq!(call(Method::GET, "/folder", json!(null)).await.json()["folder"]["name"], "Notes");

    let note = call(Method::POST, "/notes", json!({ "name": "Groceries" })).await.json();
    let id = note["id"].as_str().unwrap().to_owned();
    let saved =
        call(Method::PUT, &format!("/notes/{id}"), json!({ "content": "# Shopping\n- milk\n", "etag": note["etag"] }))
            .await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(saved.json()["title"], "Shopping");
    let stale = call(Method::PUT, &format!("/notes/{id}"), json!({ "content": "lost", "etag": note["etag"] })).await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "changed since it was opened");

    let list = call(Method::GET, "/notes", json!(null)).await.json();
    assert_eq!(list[0]["title"], "Shopping");
    assert_eq!(list[0]["excerpt"], "- milk");
    assert_eq!(call(Method::GET, "/notes?q=MILK", json!(null)).await.json().as_array().unwrap().len(), 1);
    assert_eq!(call(Method::GET, "/notes?q=bread", json!(null)).await.json().as_array().unwrap().len(), 0);

    // An ordinary file of the user: edited elsewhere, the list follows.
    let files = app.get(&format!("/api/v1/files/{folder_id}/children"), As::Browser(&alice)).await.json();
    assert_eq!(files[0]["name"], "Groceries.md");
    let put = app
        .send(Method::PUT, &format!("/api/v1/files/{id}/content"), As::Browser(&alice), Body::from("# Market\n"), &[])
        .await;
    assert_eq!(put.status, StatusCode::OK);
    assert_eq!(call(Method::GET, "/notes", json!(null)).await.json()[0]["title"], "Market");

    let preview = app
        .send(
            Method::POST,
            "/app/notes/api/preview",
            As::Browser(&alice),
            Body::from("<script>x</script>\n\n[a](javascript:alert(1)) **b** ![t](https://t.example/x.png)"),
            &[],
        )
        .await
        .json();
    assert_eq!(preview["blocked_images"], 1, "images from other sites wait for the reader: {preview}");
    let preview = preview["html"].as_str().unwrap();
    assert!(!preview.contains("<script") && preview.contains("&lt;script"), "raw HTML is text: {preview}");
    assert!(preview.contains("href=\"#\"") && preview.contains("<strong>b</strong>"), "{preview}");

    assert_eq!(call(Method::DELETE, &format!("/notes/{id}"), json!(null)).await.status, StatusCode::OK);
    assert_eq!(call(Method::GET, "/notes", json!(null)).await.json(), json!([]));
    assert_eq!(app.get("/app/notes/api/notes", As::Anonymous).await.status, StatusCode::UNAUTHORIZED);
}

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:ev1@test\r\nDTSTART:20261029T090000Z\r\nDTEND:20261029T100000Z\r\nSUMMARY:Dentist\r\n\
    END:VEVENT\r\nEND:VCALENDAR\r\n";

/// A WebDAV request as alice's CalDAV client makes it, with an app password.
async fn dav(
    app: &TestApp,
    password: &str,
    method: &str,
    uri: &str,
    body: &str,
    headers: &[(&'static str, &str)],
) -> Reply {
    dav_as(app, "alice", password, method, uri, body, headers).await
}

/// A WebDAV request of `user`'s CalDAV client.
async fn dav_as(
    app: &TestApp,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    body: &str,
    headers: &[(&'static str, &str)],
) -> Reply {
    let method = Method::from_bytes(method.as_bytes()).unwrap();
    let headers: Vec<(header::HeaderName, &str)> =
        headers.iter().map(|(name, value)| (header::HeaderName::from_static(name), *value)).collect();
    app.send(method, uri, As::Basic(user, password), Body::from(body.to_owned()), &headers).await
}

fn calendar_query(start: &str) -> String {
    format!(
        r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/></d:prop><c:filter><c:comp-filter name="VCALENDAR">
        <c:comp-filter name="VEVENT"><c:time-range start="{start}"/></c:comp-filter>
        </c:comp-filter></c:filter></c:calendar-query>"#
    )
}

#[tokio::test]
async fn the_calendar_syncs_with_caldav_clients() {
    let app = TestApp::with_wasm_apps(|apps| install_shipped(apps, "calendar")).await;
    let alice = app.register("alice").await;
    let mut passwords = Vec::new();
    for access in ["write", "read"] {
        let body = json!({ "name": "Phone", "scopes": [{ "app": "calendar", "access": access }] });
        let reply = app.json(Method::POST, "/api/v1/app-passwords", As::Browser(&alice), body).await;
        passwords.push(reply.json()["password"].as_str().unwrap().to_owned());
    }
    let (phone, reader) = (&passwords[0], &passwords[1]);

    // Discovery: the well-known address leads to the app, which asks for a password.
    let found = app.get("/.well-known/caldav", As::Anonymous).await;
    assert_eq!(found.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(found.header(header::LOCATION), "/app/calendar/dav/");
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    let asked = app.send(propfind.clone(), "/app/calendar/dav/", As::Anonymous, Body::empty(), &[]).await;
    assert_eq!(asked.status, StatusCode::UNAUTHORIZED);
    assert!(asked.header(header::WWW_AUTHENTICATE).starts_with("Basic"));

    let principal = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
    let root = dav(&app, phone, "PROPFIND", "/app/calendar/dav/", principal, &[("depth", "0")]).await;
    assert_eq!(root.status.as_u16(), 207);
    assert!(root.text().contains("<d:href>/app/calendar/dav/principal/</d:href>"), "{}", root.text());
    let home = dav(&app, phone, "PROPFIND", "/app/calendar/dav/calendars/", "", &[("depth", "1")]).await;
    assert!(home.text().contains("/app/calendar/dav/calendars/personal/"), "a calendar from the start");
    let ctag = |text: String| text.split("<cs:getctag>").nth(1).unwrap().split('<').next().unwrap().to_owned();
    let empty_tag = ctag(home.text());

    // A client adds an event; adding it again over it is refused.
    let uri = "/app/calendar/dav/calendars/personal/ev1.ics";
    let put = dav(&app, phone, "PUT", uri, EVENT, &[("if-none-match", "*")]).await;
    assert_eq!(put.status, StatusCode::CREATED);
    let etag = put.header(header::ETAG).to_owned();
    assert!(etag.starts_with('"'));
    let again = dav(&app, phone, "PUT", uri, EVENT, &[("if-none-match", "*")]).await;
    assert_eq!(again.status, StatusCode::PRECONDITION_FAILED);
    let home = dav(&app, phone, "PROPFIND", "/app/calendar/dav/calendars/", "", &[("depth", "1")]).await;
    assert_ne!(ctag(home.text()), empty_tag, "clients see that something changed");

    let calendar = "/app/calendar/dav/calendars/personal/";
    let multiget = r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:prop><d:getetag/><c:calendar-data/></d:prop>
        <d:href>/app/calendar/dav/calendars/personal/ev1.ics</d:href>
        <d:href>/app/calendar/dav/calendars/personal/gone.ics</d:href></c:calendar-multiget>"#;
    let got = dav(&app, phone, "REPORT", calendar, multiget, &[]).await.text();
    assert!(got.contains("SUMMARY:Dentist") && got.contains("404 Not Found"), "{got}");
    let in_range = dav(&app, phone, "REPORT", calendar, &calendar_query("20261001T000000Z"), &[]).await;
    assert!(in_range.text().contains("ev1.ics"));
    let later = dav(&app, phone, "REPORT", calendar, &calendar_query("20261101T000000Z"), &[]).await;
    assert!(!later.text().contains("ev1.ics"), "past the range");

    // The page sees the event and adds one of its own, which the client then sees.
    let range = "from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:00Z";
    let events = app.get(&format!("/app/calendar/api/events?{range}"), As::Browser(&alice)).await.json();
    assert_eq!(events[0]["summary"], "Dentist");
    assert_eq!(events[0]["start"], "2026-10-29T09:00:00Z");
    let form = json!({ "summary": "Gym", "all_day": false, "start": "2026-10-30T18:00", "end": "2026-10-30T19:00",
                       "tz": "Europe/Berlin", "repeat": "weekly" });
    let created = app.json(Method::POST, "/app/calendar/api/events/personal", As::Browser(&alice), form.clone()).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    let listing = dav(&app, phone, "PROPFIND", calendar, "", &[("depth", "1")]).await.text();
    assert_eq!(listing.matches(".ics</d:href>").count(), 2, "{listing}");
    let events = app.get(&format!("/app/calendar/api/events?{range}"), As::Browser(&alice)).await.json();
    let gym = events.as_array().unwrap().iter().find(|e| e["summary"] == "Gym").unwrap();
    assert_eq!(gym["start"], "2026-10-30T17:00:00Z", "18:00 in Berlin, in winter time");

    // Saving over a change made elsewhere is refused.
    let name = created.json()["name"].as_str().unwrap().to_owned();
    let mut update = form.clone();
    update["etag"] = json!("old");
    let event_api = format!("/app/calendar/api/events/personal/{name}");
    assert_eq!(app.json(Method::PUT, &event_api, As::Browser(&alice), update).await.status, StatusCode::CONFLICT);

    // Deleting needs the current etag when the client sends one.
    let stale = dav(&app, phone, "DELETE", uri, "", &[("if-match", "\"old\"")]).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(dav(&app, phone, "DELETE", uri, "", &[("if-match", &etag)]).await.status, StatusCode::NO_CONTENT);

    // Clients may make calendars; a read-only password can't change anything.
    let make = dav(&app, phone, "MKCALENDAR", "/app/calendar/dav/calendars/work/", "", &[]).await;
    assert_eq!(make.status, StatusCode::CREATED);
    let calendars = app.get("/app/calendar/api/calendars", As::Browser(&alice)).await.json();
    assert_eq!(calendars.as_array().unwrap().len(), 2);
    assert_eq!(dav(&app, reader, "PUT", uri, EVENT, &[]).await.json()["error"]["code"], "READ_ONLY");
    let listing = dav(&app, reader, "PROPFIND", "/app/calendar/dav/calendars/", "", &[]).await;
    assert_eq!(listing.status.as_u16(), 207, "reading is fine");
}

#[tokio::test]
async fn calendars_are_shared_read_only_or_for_changes() {
    let app = TestApp::with_wasm_apps(|apps| install_shipped(apps, "calendar")).await;
    let alice = app.register("alice").await;
    let bob = app.register("bob").await;
    let carol = app.register("carol").await;
    let calendars = "/app/calendar/api/calendars";
    app.get(calendars, As::Browser(&alice)).await; // the first visit makes her "Calendar"
    let work = app.json(Method::POST, calendars, As::Browser(&alice), json!({ "name": "Work" })).await.json();
    let work = work["id"].as_str().unwrap().to_owned();
    let form = json!({ "summary": "Standup", "all_day": false, "start": "2026-10-29T09:00", "end": "2026-10-29T09:15",
                       "tz": "UTC" });
    let alices_events = format!("/app/calendar/api/events/{work}");
    assert_eq!(app.json(Method::POST, &alices_events, As::Browser(&alice), form.clone()).await.status, StatusCode::OK);

    // Only folders of the owner's data, with other users.
    let share = |path: &str, username: &str, write: bool| json!({ "app_id": "calendar", "path": path, "label": "Work", "username": username, "write": write });
    let path = format!("calendars/{work}");
    for (body, code) in [
        (share(&path, "alice", false), "SHARE_WITH_SELF"),
        (share("calendars/missing", "bob", false), "FILE_NOT_FOUND"),
        (share("", "bob", false), "INVALID_NAME"),
        (json!({ "app_id": "nope", "path": path, "username": "bob" }), "APP_NOT_FOUND"),
    ] {
        let reply = app.json(Method::POST, "/api/v1/app-shares", As::Browser(&alice), body).await;
        assert_eq!(reply.json()["error"]["code"], code);
    }
    let mut ids = Vec::new();
    for (username, write) in [("bob", false), ("carol", true)] {
        let reply =
            app.json(Method::POST, "/api/v1/app-shares", As::Browser(&alice), share(&path, username, write)).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        ids.push(format!("shared-{}", reply.json()["id"].as_str().unwrap()));
    }
    let (bobs, carols) = (&ids[0], &ids[1]);

    // A name nobody has gets the same answer and is listed the same: names can't be guessed.
    let nobody = app.json(Method::POST, "/api/v1/app-shares", As::Browser(&alice), share(&path, "Nobody", false)).await;
    assert_eq!(nobody.status, StatusCode::CREATED);
    let to_bob = app.get("/api/v1/app-shares", As::Browser(&alice)).await.json()["given"]
        .as_array()
        .unwrap()
        .iter()
        .find(|given| given["user"]["username"] == "bob")
        .unwrap()
        .clone();
    let shape = |share: &Value| {
        let mut keys: Vec<String> = share.as_object().unwrap().keys().cloned().collect();
        keys.extend(share["user"].as_object().unwrap().keys().cloned());
        keys
    };
    assert_eq!(shape(&nobody.json()), shape(&to_bob));
    assert_eq!(nobody.json()["user"]["display_name"], "nobody");
    assert_eq!(to_bob["user"]["display_name"], "bob", "the owner sees usernames, not display names");

    // Bob sees it next to his own calendar, read-only.
    let list = app.get(calendars, As::Browser(&bob)).await.json();
    let shared = list.as_array().unwrap().iter().find(|c| c["id"] == *bobs).unwrap().clone();
    assert_eq!((shared["name"].as_str(), shared["writable"].as_bool()), (Some("Work"), Some(false)));
    assert_eq!(shared["owner"]["username"], "alice");
    let range = "/app/calendar/api/events?from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:00Z";
    assert_eq!(app.get(range, As::Browser(&bob)).await.json()[0]["summary"], "Standup");
    let bobs_events = format!("/app/calendar/api/events/{bobs}");
    let refused = app.json(Method::POST, &bobs_events, As::Browser(&bob), form.clone()).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let bobs_calendar = format!("{calendars}/{bobs}");
    let rename = app.json(Method::PUT, &bobs_calendar, As::Browser(&bob), json!({ "name": "Mine" })).await;
    assert_eq!(rename.status, StatusCode::FORBIDDEN, "only the owner renames it");
    let shares = app.get("/api/v1/app-shares", As::Browser(&bob)).await.json();
    assert_eq!(shares["received"][0]["write"], false);
    assert_eq!(shares["given"], json!([]));

    // Over CalDAV too: named with the owner's name, and only for reading.
    let body = json!({ "name": "Phone", "scopes": [{ "app": "calendar", "access": "write" }] });
    let reply = app.json(Method::POST, "/api/v1/app-passwords", As::Browser(&bob), body).await;
    let password = reply.json()["password"].as_str().unwrap().to_owned();
    let home = dav_as(&app, "bob", &password, "PROPFIND", "/app/calendar/dav/calendars/", "", &[("depth", "1")]).await;
    let text = home.text();
    assert!(text.contains(&format!("/calendars/{bobs}/")) && text.contains("Work (alice)"), "{text}");
    let uri = format!("/app/calendar/dav/calendars/{bobs}/ev1.ics");
    assert_eq!(dav_as(&app, "bob", &password, "PUT", &uri, EVENT, &[]).await.status, StatusCode::FORBIDDEN);

    // Carol may change it; alice sees her event.
    let carols_events = format!("/app/calendar/api/events/{carols}");
    let added = app.json(Method::POST, &carols_events, As::Browser(&carol), form).await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.text());
    assert_eq!(app.get(range, As::Browser(&alice)).await.json().as_array().unwrap().len(), 2);

    // Bob gives it up; alice deleting the calendar ends carol's share.
    let left = app.send(Method::DELETE, &bobs_calendar, As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(left.status, StatusCode::OK);
    assert_eq!(app.get("/api/v1/app-shares", As::Browser(&bob)).await.json()["received"], json!([]));
    let given = app.get("/api/v1/app-shares", As::Browser(&alice)).await.json()["given"].clone();
    assert_eq!(given.as_array().unwrap().len(), 2, "carol's and nobody's");
    let alices_calendar = format!("{calendars}/{work}");
    let deleted = app.send(Method::DELETE, &alices_calendar, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(app.get("/api/v1/app-shares", As::Browser(&carol)).await.json()["received"], json!([]));
    assert_eq!(app.get("/api/v1/app-shares", As::Browser(&alice)).await.json()["given"], json!([]));
    let list = app.get(calendars, As::Browser(&carol)).await.json();
    assert!(list.as_array().unwrap().iter().all(|c| c["id"] != *carols));
}

#[tokio::test]
async fn admins_disable_plugins_and_set_their_limits() {
    let app = TestApp::with_wasm_apps(|apps| {
        install_shipped(apps, "calendar");
        std::fs::create_dir_all(apps.join("broken")).unwrap();
        std::fs::write(apps.join("broken/manifest.toml"), "id = \"broken\"").unwrap();
    })
    .await;
    let admin = app.register("admin").await;
    let bob = app.register("bob").await;
    assert_eq!(app.get("/api/v1/admin/plugins", As::Browser(&bob)).await.status, StatusCode::FORBIDDEN);

    let list = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
    let states: Vec<(&str, &str, &str)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["source"].as_str().unwrap(), p["status"]["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        states,
        [("files", "built_in", "running"), ("broken", "installed", "failed"), ("calendar", "installed", "running")]
    );
    assert!(list[1]["status"]["error"].as_str().unwrap().contains("manifest"), "{}", list[1]);

    let set = |id: &str, body: Value| {
        let uri = format!("/api/v1/admin/plugins/{id}");
        let admin = admin.clone();
        let app = &app;
        async move { app.json(Method::PUT, &uri, As::Browser(&admin), body).await }
    };
    assert_eq!(set("files", json!({ "enabled": false })).await.json()["error"]["code"], "PLUGIN_REQUIRED");
    assert_eq!(set("broken", json!({ "enabled": true })).await.json()["error"]["code"], "PLUGIN_CANT_RUN");
    assert_eq!(set("nope", json!({ "enabled": true })).await.json()["error"]["code"], "PLUGIN_NOT_FOUND");
    let odd = set("calendar", json!({ "enabled": true, "limits": { "memory": 3, "time": 1, "workers": 1 } })).await;
    assert_eq!(odd.json()["error"]["code"], "INVALID_SETTINGS");

    // Disabled: no page, no routes, no discovery address; enabled again with a limit of its own.
    let off = set("calendar", json!({ "enabled": false })).await;
    assert_eq!(off.json()["status"]["state"], "disabled");
    let apps = app.get("/api/v1/apps", As::Browser(&bob)).await.json();
    assert!(apps["apps"].as_array().unwrap().iter().all(|a| a["id"] != "calendar"));
    let page = app.get("/app/calendar/api/calendars", As::Browser(&bob)).await;
    assert_eq!(page.json()["error"]["code"], "APP_NOT_FOUND");
    assert_eq!(app.get("/.well-known/caldav", As::Anonymous).await.status, StatusCode::NOT_FOUND);

    let on = set("calendar", json!({ "enabled": true, "limits": { "memory": 4, "time": 2, "workers": 1 } })).await;
    let on = on.json();
    assert_eq!(on["status"]["state"], "running");
    assert_eq!(on["limits"], json!({ "memory": 4, "time": 2, "workers": 1 }));
    let effective = &on["effective"];
    assert_eq!((effective["memory_mib"].as_u64(), effective["data_mib"].as_u64()), (Some(256), Some(64)));
    assert_eq!((effective["time_seconds"].as_u64(), effective["wall_seconds"].as_u64()), (Some(20), Some(120)));
    let apps = app.get("/api/v1/apps", As::Browser(&bob)).await.json();
    assert_eq!(apps["limits"]["calendar"]["data_bytes"], 64 << 20, "the panel's bridge takes as much");
    assert_eq!(app.get("/app/calendar/api/calendars", As::Browser(&bob)).await.status, StatusCode::OK);
    assert_eq!(app.get("/.well-known/caldav", As::Anonymous).await.status, StatusCode::MOVED_PERMANENTLY);
}

/// A plugin archive as a release would be: the files in a folder, with a README.
/// How long tests wait for background work (downloads, jobs): up to a minute,
/// for a busy CI machine running every test at once; they go on as soon as it's done.
const WAIT_ROUND: std::time::Duration = std::time::Duration::from_millis(50);
const WAIT_ROUNDS: usize = 1200;

fn plugin_zip(manifest: &str, wasm: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in
        [("echo/manifest.toml", manifest.as_bytes()), ("echo/app.wasm", wasm), ("echo/README.md", b"hi")]
    {
        zip.start_file(name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[tokio::test]
async fn admins_install_update_and_remove_plugins() {
    let app = TestApp::with_wasm_apps(|_| {}).await;
    let admin = app.register("admin").await;
    let bob = app.register("bob").await;
    let install = |archive: Vec<u8>, who: &str| {
        let who = who.to_owned();
        let app = &app;
        async move { app.send(Method::POST, "/api/v1/admin/plugins", As::Browser(&who), Body::from(archive), &[]).await }
    };
    let manifest = echo_manifest_toml();
    let wasm = echo_wasm();
    assert_eq!(install(plugin_zip(&manifest, &wasm), &bob).await.status, StatusCode::FORBIDDEN);

    // New plugins start disabled: enabling approves what they ask for.
    let installed = install(plugin_zip(&manifest, &wasm), &admin).await;
    assert_eq!(installed.status, StatusCode::CREATED, "{}", installed.text());
    let installed = installed.json();
    assert_eq!(
        (installed["source"].as_str(), installed["status"]["state"].as_str()),
        (Some("installed"), Some("disabled"))
    );
    assert!(app.data.as_ref().unwrap().path().join("apps/echo/app.wasm").exists());
    assert_eq!(app.get("/app/echo/hello", As::Browser(&bob)).await.json()["error"]["code"], "APP_NOT_FOUND");
    let enable = json!({ "enabled": true });
    app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), enable.clone()).await;
    assert_eq!(app.get("/app/echo/hello", As::Browser(&bob)).await.text(), "Hello, bob!");

    // An update keeps running if it asks for nothing more; otherwise it waits for approval.
    let newer = manifest.replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    let updated = install(plugin_zip(&newer, &wasm), &admin).await.json();
    assert_eq!(
        (updated["manifest"]["version"].as_str(), updated["status"]["state"].as_str()),
        (Some("0.2.0"), Some("running"))
    );
    let more = format!("{newer}[well_known]\necho = \"/\"\n");
    let updated = install(plugin_zip(&more, &wasm), &admin).await.json();
    assert_eq!(updated["status"]["state"], "disabled");
    app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), enable.clone()).await;
    let online = more.replace("files_write = true\n", "files_write = true\nnetwork = [\"api.example.com\"]\n");
    let updated = install(plugin_zip(&online, &wasm), &admin).await.json();
    assert_eq!(updated["manifest"]["permissions"]["network"][0], "api.example.com");
    assert_eq!(updated["status"]["state"], "disabled", "network addresses need approval too");

    // Broken archives and taken ids are refused.
    for (archive, code) in [
        (b"not a zip".to_vec(), "INVALID_PLUGIN"),
        (plugin_zip(&manifest, b"not wasm"), "INVALID_PLUGIN"),
        (plugin_zip("id = \"x\"", &wasm), "INVALID_PLUGIN"),
        (plugin_zip(&manifest.replace("id = \"echo\"", "id = \"files\""), &wasm), "PLUGIN_EXISTS"),
    ] {
        assert_eq!(install(archive, &admin).await.json()["error"]["code"], code);
    }
    let still = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
    assert_eq!(still[1]["manifest"]["version"], "0.2.0", "a failed update leaves the installed version");

    // Removed: gone from the list and the disk; files can't be removed.
    let remove = |id: &str| {
        let uri = format!("/api/v1/admin/plugins/{id}");
        let app = &app;
        let admin = admin.clone();
        async move { app.send(Method::DELETE, &uri, As::Browser(&admin), Body::empty(), &[]).await }
    };
    assert_eq!(remove("files").await.json()["error"]["code"], "PLUGIN_NOT_REMOVABLE");
    assert_eq!(remove("echo").await.status, StatusCode::NO_CONTENT);
    assert_eq!(remove("echo").await.json()["error"]["code"], "PLUGIN_NOT_FOUND");
    assert!(!app.data.as_ref().unwrap().path().join("apps/echo").exists());
    assert_eq!(app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn admins_install_and_update_plugins_from_the_catalog() {
    use sha2::{Digest, Sha256};

    let api = format!("={}", rustcloud_plugin_api::API_VERSION);
    let manifest = echo_manifest_toml();
    let newer = manifest.replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    let not_echo = manifest.replace("id = \"echo\"", "id = \"other\"");
    let archives: Vec<(&str, Vec<u8>)> = vec![
        ("echo-0.1.0.zip", plugin_zip(&manifest, &echo_wasm())),
        ("echo-0.2.0.zip", plugin_zip(&newer, &echo_wasm())),
        ("liar.zip", plugin_zip(&not_echo, &echo_wasm())),
    ];
    let hex = |data: &[u8]| -> String { Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect() };
    let release = |version: &str, api: &str, file: &str, sha256: String| {
        let size = archives.iter().find(|(name, _)| *name == file).map_or(1, |(_, data)| data.len());
        json!({ "version": version, "api": api, "url": file, "sha256": sha256, "size": size })
    };
    let entry = |id: &str, versions: Vec<Value>| {
        json!({ "id": id, "name": { "en": id }, "description": { "en": "A plugin." }, "author": "Test",
                "license": "MIT", "source": "https://example.com/src", "versions": versions })
    };
    let sha = |file: &str| hex(&archives.iter().find(|(name, _)| *name == file).unwrap().1);
    let index = json!({ "schema": 1, "plugins": [
        entry("echo", vec![
            release("0.1.0", &api, "echo-0.1.0.zip", sha("echo-0.1.0.zip")),
            release("0.2.0", &api, "echo-0.2.0.zip", sha("echo-0.2.0.zip")),
            release("9.0.0", "^99", "echo-9.zip", "0".repeat(64)),
        ]),
        entry("future", vec![release("1.0.0", "^99", "future.zip", "0".repeat(64))]),
        entry("files", vec![release("1.0.0", &api, "files.zip", "0".repeat(64))]),
        entry("tampered", vec![release("0.1.0", &api, "echo-0.1.0.zip", "0".repeat(64))]),
        entry("liar", vec![release("0.1.0", &api, "liar.zip", sha("liar.zip"))]),
        json!({ "id": "broken" }),
    ]});

    // The catalog and its archives, on a server of this machine.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut remote =
        Router::new().route("/catalog/index.json", axum::routing::get(move || async move { index.to_string() }));
    for (name, data) in archives.clone() {
        remote = remote.route(&format!("/catalog/{name}"), axum::routing::get(move || async move { data }));
    }
    tokio::spawn(async move { axum::serve(listener, remote).await });

    let app = TestApp::with_wasm_apps(|_| {}).await;
    let admin = app.register("admin").await;
    let bob = app.register("bob").await;
    let catalog_url = json!({ "catalog_url": format!("http://127.0.0.1:{port}/catalog/index.json") });
    let saved = app.json(Method::PUT, "/api/v1/admin/settings/plugins", As::Browser(&admin), catalog_url).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());

    assert_eq!(app.get("/api/v1/admin/catalog", As::Browser(&bob)).await.status, StatusCode::FORBIDDEN);
    let actions = || async {
        let view = app.get("/api/v1/admin/catalog?fresh=true", As::Browser(&admin)).await.json();
        let plugins = view["plugins"].as_array().unwrap().clone();
        let actions: Vec<(String, Value, Value)> = plugins
            .iter()
            .map(|p| (p["id"].as_str().unwrap().to_owned(), p["action"].clone(), p["release"]["version"].clone()))
            .collect();
        (actions, view["skipped"].clone())
    };
    let (listed, skipped) = actions().await;
    assert_eq!(
        listed[..3],
        [
            ("echo".to_owned(), json!("install"), json!("0.2.0")),
            ("future".to_owned(), json!("needs_newer_server"), Value::Null),
            ("files".to_owned(), json!("built_in"), json!("1.0.0")),
        ]
    );
    assert_eq!(skipped.as_array().unwrap().len(), 1, "{skipped}");

    let install = |id: &str, version: &str| {
        let uri = format!("/api/v1/admin/catalog/{id}/install");
        let body = json!({ "version": version });
        let (app, admin) = (&app, admin.clone());
        async move { app.json(Method::POST, &uri, As::Browser(&admin), body).await }
    };
    // An older version first: a new plugin starts disabled, as from an archive.
    let installed = install("echo", "0.1.0").await;
    assert_eq!(installed.status, StatusCode::CREATED, "{}", installed.text());
    let installed = installed.json();
    assert_eq!(
        (installed["manifest"]["version"].as_str(), installed["status"]["state"].as_str()),
        (Some("0.1.0"), Some("disabled"))
    );
    assert_eq!(actions().await.0[0], ("echo".to_owned(), json!("update"), json!("0.2.0")));
    assert_eq!(install("echo", "0.2.0").await.json()["manifest"]["version"], "0.2.0");
    assert_eq!(actions().await.0[0].1, json!("latest"));

    // Only what the catalog lists, checked against it.
    for (id, version, code) in [
        ("echo", "0.3.0", "CATALOG_VERSION_NOT_FOUND"),
        ("echo", "9.0.0", "NEEDS_NEWER_SERVER"),
        ("nothing", "0.1.0", "PLUGIN_NOT_FOUND"),
        ("tampered", "0.1.0", "INVALID_PLUGIN"),
        ("liar", "0.1.0", "INVALID_PLUGIN"),
    ] {
        assert_eq!(install(id, version).await.json()["error"]["code"], code, "{id} {version}");
    }
    let plugins = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
    let ids: Vec<&str> = plugins.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["files", "echo"], "nothing else was installed");

    // Without an address there is no catalog.
    let off = json!({ "catalog_url": "" });
    app.json(Method::PUT, "/api/v1/admin/settings/plugins", As::Browser(&admin), off).await;
    assert_eq!(app.get("/api/v1/admin/catalog", As::Browser(&admin)).await.json()["error"]["code"], "CATALOG_OFF");
}

#[tokio::test]
async fn plugins_download_files_into_their_storage() {
    use sha2::{Digest, Sha256};

    // The model, on a server of this machine.
    let model = vec![7u8; 300_000];
    let hex = |data: &[u8]| -> String { Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect() };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = model.clone();
    let remote = Router::new()
        .route("/model.bin", axum::routing::get(move || async move { served }))
        .route("/other.bin", axum::routing::get(|| async { "something else" }));
    tokio::spawn(async move { axum::serve(listener, remote).await });

    let download = |path: &str, file: &str, sha: &str| {
        format!("[[downloads]]\npath = \"{path}\"\nurl = \"http://127.0.0.1:{port}/{file}\"\nsha256 = \"{sha}\"\n")
    };
    let manifest = format!(
        "{}network = [\"http://127.0.0.1:{port}\"]\n{}",
        echo_manifest_toml(),
        download("models/model.bin", "model.bin", &hex(&model))
    );
    let app = TestApp::with_wasm_apps(|_| {}).await;
    let admin = app.register("admin").await;
    let install = |manifest: String| {
        let app = &app;
        let admin = admin.clone();
        async move {
            let archive = Body::from(plugin_zip(&manifest, &echo_wasm()));
            app.send(Method::POST, "/api/v1/admin/plugins", As::Browser(&admin), archive, &[]).await.json()
        }
    };
    // The plugin once its downloads are done or failed.
    let settled = || async {
        for _ in 0..WAIT_ROUNDS {
            let plugins = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
            let echo = plugins.as_array().unwrap().iter().find(|p| p["id"] == "echo").unwrap().clone();
            if echo["download"]["state"] != "running" {
                return echo;
            }
            tokio::time::sleep(WAIT_ROUND).await;
        }
        panic!("the download never ended");
    };

    // Nothing is downloaded before the administrator approves the plugin.
    let installed = install(manifest.clone()).await;
    assert_eq!(
        (installed["status"]["state"].as_str(), installed["storage"]["files"].as_u64()),
        (Some("disabled"), Some(0))
    );
    app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), json!({ "enabled": true })).await;
    let echo = settled().await;
    assert_eq!(echo["download"], Value::Null, "{echo}");
    assert_eq!(echo["storage"], json!({ "files": 1, "bytes": 300_000 }));
    let read = app.get("/app/echo/stored?models/model.bin", As::Anonymous).await;
    assert_eq!(read.body.len(), model.len(), "the plugin reads it");

    // The plugin keeps its own files; the administrator sees and clears them.
    assert_eq!(app.get("/app/echo/store?cache/index.json", As::Anonymous).await.status, StatusCode::OK);
    let files = app.get("/api/v1/admin/plugins/echo/storage", As::Browser(&admin)).await.json();
    let paths: Vec<&str> = files.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["cache/index.json", "models/model.bin"]);
    app.send(Method::DELETE, "/api/v1/admin/plugins/echo/storage", As::Browser(&admin), Body::empty(), &[]).await;
    let echo = settled().await;
    assert_eq!(echo["storage"]["files"], 1, "the downloads come again, the rest is gone");

    // A file that doesn't match its checksum is not kept.
    let broken = format!("{manifest}{}", download("models/other.bin", "other.bin", &hex(&model)));
    let updated = install(broken).await;
    assert_eq!(updated["status"]["state"], "disabled", "new downloads wait for the administrator");
    app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), json!({ "enabled": true })).await;
    let echo = settled().await;
    assert_eq!(echo["download"]["state"], "failed");
    assert_eq!(echo["download"]["path"], "models/other.bin");
    assert!(echo["download"]["error"].as_str().unwrap().contains("SHA-256"), "{echo}");
    assert_eq!(echo["storage"]["files"], 1);
    let retried =
        app.send(Method::POST, "/api/v1/admin/plugins/echo/downloads", As::Browser(&admin), Body::empty(), &[]);
    assert_eq!(retried.await.status, StatusCode::OK);
}

#[tokio::test]
async fn plugins_run_background_jobs_in_batches_for_their_users() {
    let manifest = format!("{}[[jobs]]\nkind = \"scan\"\nbatch = 50\n", echo_manifest_toml());
    let app = TestApp::with_wasm_apps(|apps| {
        std::fs::create_dir_all(apps.join("echo")).unwrap();
        std::fs::write(apps.join("echo/manifest.toml"), &manifest).unwrap();
        std::fs::write(apps.join("echo/app.wasm"), echo_wasm()).unwrap();
    })
    .await;
    let admin = app.register("admin").await;
    let bob = app.register("bob").await;
    let progress = || async { app.get("/app/echo/progress", As::Browser(&bob)).await.text() };
    // Bob's jobs once only the ones waiting for a retry are left.
    let settled = || async {
        for _ in 0..WAIT_ROUNDS {
            let now = progress().await;
            if now == "scan: 2 waiting, 1 failed" {
                return now;
            }
            tokio::time::sleep(WAIT_ROUND).await;
        }
        progress().await
    };

    // One job per line; "panic" breaks the whole batch, so the jobs go on
    // one by one: the good ones are done, "fail" fails, "again" and "panic" wait.
    let lines = "a\nb\nfail\nagain\npanic\nc";
    let added = app.send(Method::POST, "/app/echo/jobs", As::Browser(&bob), Body::from(lines), &[]).await;
    assert_eq!(added.text(), "added");
    assert_eq!(settled().await, "scan: 2 waiting, 1 failed");
    let plugins = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
    let echo = plugins.as_array().unwrap().iter().find(|p| p["id"] == "echo").unwrap().clone();
    assert_eq!(echo["queue"], json!({ "scan": { "waiting": 2, "failed": 1 } }));
    assert_eq!(app.get("/app/echo/progress", As::Anonymous).await.text(), "Error::NoUser", "jobs belong to a user");

    // Taking back the last access ends the user's jobs.
    let root = app.root_id(&bob).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&bob), json!({ "name": "Photos" }))
        .await
        .json();
    let grant = json!({ "app_id": "echo", "node_id": folder["id"], "write": false, "kind": "standing" });
    let grant = app.json(Method::POST, "/api/v1/grants", As::Browser(&bob), grant).await.json();
    let revoke = format!("/api/v1/grants/{}", grant["id"].as_str().unwrap());
    app.send(Method::DELETE, &revoke, As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(progress().await, "", "no jobs left");
}

#[tokio::test]
async fn file_events_start_jobs_in_the_folders_users_give_plugins() {
    let on = r#"["file.granted", "file.created", "file.changed", "file.deleted", "file.moved"]"#;
    let manifest = format!("{}[[jobs]]\nkind = \"scan\"\non = {on}\nmime = [\"text/*\"]\n", echo_manifest_toml());
    let app = TestApp::with_wasm_apps(|apps| {
        std::fs::create_dir_all(apps.join("echo")).unwrap();
        std::fs::write(apps.join("echo/manifest.toml"), &manifest).unwrap();
        std::fs::write(apps.join("echo/app.wasm"), echo_wasm()).unwrap();
    })
    .await;
    let bob = app.register("bob").await;
    let root = app.root_id(&bob).await;
    let folder = app
        .json(Method::POST, &format!("/api/v1/files/{root}/folders"), As::Browser(&bob), json!({ "name": "Notes" }))
        .await
        .json();
    let folder_id = folder["id"].as_str().unwrap().to_owned();
    let id = |node: Value| node["id"].as_str().unwrap().to_owned();
    let old = id(app.upload(&bob, &folder_id, "old.txt", "old").await);
    let outside = id(app.upload(&bob, &root, "outside.txt", "x").await);
    let picture = id(app.upload(&bob, &folder_id, "picture.png", "png").await);
    // What the job for a file was started by, once it says `expected` (or what it says in the end).
    let done = |file: String, expected: &'static str| {
        let app = &app;
        let bob = bob.clone();
        async move {
            let mut said = "never".to_owned();
            for _ in 0..WAIT_ROUNDS {
                let reply = app.get(&format!("/app/echo/done?{file}"), As::Browser(&bob)).await;
                if reply.status == StatusCode::OK {
                    said = reply.text();
                    if said == expected {
                        break;
                    }
                }
                tokio::time::sleep(WAIT_ROUND).await;
            }
            said
        }
    };

    // Giving a folder starts jobs for what is in it.
    let grant = json!({ "app_id": "echo", "node_id": folder_id, "write": true, "kind": "standing" });
    assert_eq!(app.json(Method::POST, "/api/v1/grants", As::Browser(&bob), grant).await.status, StatusCode::CREATED);
    assert_eq!(done(old.clone(), "file.granted").await, "file.granted");

    // New and changed files there start them too.
    let new = id(app.upload(&bob, &folder_id, "new.txt", "new").await);
    assert_eq!(done(new.clone(), "file.created").await, "file.created");
    let content = format!("/api/v1/files/{old}/content");
    app.send(Method::PUT, &content, As::Browser(&bob), Body::from("changed"), &[]).await;
    assert_eq!(done(old, "file.changed").await, "file.changed");

    // Not outside the folder, not other types, not the plugin's own changes.
    app.send(Method::POST, "/app/echo/create", As::Browser(&bob), Body::from("mine"), &[]).await;
    let last = id(app.upload(&bob, &folder_id, "last.txt", "last").await);
    assert_eq!(done(last.clone(), "file.created").await, "file.created", "the events before it are handled too");
    let children = app.get(&format!("/api/v1/files/{folder_id}/children"), As::Browser(&bob)).await.json();
    let own = children.as_array().unwrap().iter().find(|n| n["name"] == "from-echo.txt").unwrap().clone();
    for file in [outside.clone(), picture, id(own)] {
        let reply = app.get(&format!("/app/echo/done?{file}"), As::Browser(&bob)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "no job for {file}");
    }

    // Moving a file in or out, renaming one there: `file.moved`.
    let patch = |file: &str, body: Value| {
        let app = &app;
        let bob = bob.clone();
        let path = format!("/api/v1/files/{file}");
        async move { app.json(Method::PATCH, &path, As::Browser(&bob), body).await.status }
    };
    assert_eq!(patch(&outside, json!({ "parent_id": folder_id })).await, StatusCode::OK);
    assert_eq!(done(outside, "file.moved").await, "file.moved", "moved in");
    assert_eq!(patch(&last, json!({ "parent_id": root })).await, StatusCode::OK);
    assert_eq!(done(last, "file.moved").await, "file.moved", "moved out");
    assert_eq!(patch(&new, json!({ "name": "renamed.txt" })).await, StatusCode::OK);
    assert_eq!(done(new.clone(), "file.moved").await, "file.moved", "renamed");

    // Deleting a folder there: `file.deleted` for each file in it.
    let inner = app
        .json(Method::POST, &format!("/api/v1/files/{folder_id}/folders"), As::Browser(&bob), json!({ "name": "In" }))
        .await
        .json();
    let inner_id = id(inner);
    let deep = id(app.upload(&bob, &inner_id, "deep.txt", "deep").await);
    assert_eq!(done(deep.clone(), "file.created").await, "file.created");
    let gone =
        app.send(Method::DELETE, &format!("/api/v1/files/{inner_id}"), As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    assert_eq!(done(deep, "file.deleted").await, "file.deleted");
    let gone = app.send(Method::DELETE, &format!("/api/v1/files/{new}"), As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    assert_eq!(done(new, "file.deleted").await, "file.deleted");
}

#[tokio::test]
async fn plugins_use_ffmpeg_only_once_the_administrator_approves_it() {
    let app = TestApp::with_wasm_apps(|_| {}).await;
    let admin = app.register("admin").await;
    let install = |manifest: String| {
        let app = &app;
        let admin = admin.clone();
        async move {
            let archive = Body::from(plugin_zip(&manifest, &echo_wasm()));
            app.send(Method::POST, "/api/v1/admin/plugins", As::Browser(&admin), archive, &[]).await.json()
        }
    };
    let enable =
        || app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), json!({ "enabled": true }));
    let ffmpeg = || async { app.get("/app/echo/ffmpeg?photo.jpg", As::Browser(&admin)).await.text() };

    install(echo_manifest_toml()).await;
    enable().await;
    assert_eq!(ffmpeg().await, "Error::NotAllowed", "not without use.ffmpeg");

    // Asking for it is asking for more: the update waits for approval.
    let with_ffmpeg = echo_manifest_toml().replace("files_write = true\n", "files_write = true\nuse = [\"ffmpeg\"]\n");
    let updated = install(with_ffmpeg).await;
    assert_eq!(updated["manifest"]["permissions"]["use"], json!(["ffmpeg"]));
    assert_eq!(updated["status"]["state"], "disabled");
    enable().await;
    // These tests run without ffmpeg: the server says so, to the plugin and the administrator.
    assert!(ffmpeg().await.starts_with("Error::Unsupported"), "{}", ffmpeg().await);
    let status = app.get("/api/v1/admin/use/ffmpeg", As::Browser(&admin)).await.json();
    assert_eq!(status["available"], false);
    assert!(status["problem"].as_str().unwrap().contains("media_tools_dir"), "{status}");
    assert_eq!(status["capabilities"]["available"], false);
}

#[tokio::test]
async fn plugins_run_neural_networks_only_once_the_administrator_approves_it() {
    let app = TestApp::with_wasm_apps(|_| {}).await;
    let admin = app.register("admin").await;
    let install = |manifest: String| {
        let app = &app;
        let admin = admin.clone();
        async move {
            let archive = Body::from(plugin_zip(&manifest, &echo_wasm()));
            app.send(Method::POST, "/api/v1/admin/plugins", As::Browser(&admin), archive, &[]).await.json()
        }
    };
    let enable =
        || app.json(Method::PUT, "/api/v1/admin/plugins/echo", As::Browser(&admin), json!({ "enabled": true }));
    let run = || async { app.get("/app/echo/onnx?models/double.onnx", As::Browser(&admin)).await.text() };

    install(echo_manifest_toml()).await;
    enable().await;
    assert_eq!(run().await, "Error::NotAllowed", "not without use.onnx");

    let with_onnx = echo_manifest_toml().replace("files_write = true\n", "files_write = true\nuse = [\"onnx\"]\n");
    let updated = install(with_onnx).await;
    assert_eq!(updated["manifest"]["permissions"]["use"], json!(["onnx"]));
    assert_eq!(updated["status"]["state"], "disabled", "the update waits for approval");
    enable().await;
    // These tests have no `rustcloud onnx-worker`: the server says so, to the
    // plugin and the administrator.
    assert!(run().await.starts_with("Error::Unsupported"), "{}", run().await);
    let status = app.get("/api/v1/admin/use/onnx", As::Browser(&admin)).await.json();
    assert_eq!(status["available"], false);
    assert_eq!(status["keep_loaded_seconds"], 300);
    assert_eq!(status["loaded"], json!([]));
}

#[tokio::test]
async fn a_service_at_the_address_the_admin_set_may_be_shown_for_the_plugin() {
    let manifest = echo_manifest_toml().replace(
        "files_write = true\n",
        "files_write = true\nnetwork = [\"{office_url}\"]\nframe = [\"{office_url}\"]\n\
         [[server_settings]]\nkey = \"office_url\"\nkind = \"text\"\nlabel.en = \"Office\"\n",
    );
    let app = TestApp::with_wasm_apps(|apps| {
        std::fs::create_dir_all(apps.join("echo")).unwrap();
        std::fs::write(apps.join("echo/manifest.toml"), &manifest).unwrap();
        std::fs::write(apps.join("echo/app.wasm"), echo_wasm()).unwrap();
    })
    .await;
    let admin = app.register("admin").await;
    let csp = || async { app.get("/", As::Anonymous).await.header(header::CONTENT_SECURITY_POLICY).to_owned() };
    assert!(csp().await.contains("frame-src 'self';"), "nothing until the address is set");
    assert!(app.get("/api/v1/apps", As::Browser(&admin)).await.json()["frames"].get("echo").is_none());

    let office = json!({ "office_url": "http://127.0.0.1:9/" });
    let saved = app.json(Method::PUT, "/api/v1/admin/plugins/echo/settings", As::Browser(&admin), office).await;
    assert_eq!(saved.status, StatusCode::OK);
    let policy = csp().await;
    assert!(policy.contains("frame-src 'self' http://127.0.0.1:9;"), "{policy}");
    assert!(policy.contains("form-action 'self' http://127.0.0.1:9"), "{policy}");
    let frames = app.get("/api/v1/apps", As::Browser(&admin)).await.json()["frames"]["echo"].clone();
    assert_eq!(frames, json!(["http://127.0.0.1:9"]));
    // The plugin may reach it too (nothing listens there: no answer, but no refusal).
    let fetch = app.get("/app/echo/fetch?http://127.0.0.1:9/hosting/discovery", As::Anonymous).await.text();
    assert!(!fetch.contains("NotAllowed"), "{fetch}");
    let other = app.get("/app/echo/fetch?http://127.0.0.2:9/", As::Anonymous).await.text();
    assert!(other.contains("NotAllowed"), "{other}");
}

/// A service next to the server, such as an office editor: says what it
/// got, redirects, takes its time, and echoes a WebSocket's bytes.
async fn start_service() -> (String, tokio::task::JoinHandle<()>) {
    use axum::response::{AppendHeaders, IntoResponse};
    use axum::routing::{get, post};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let own = address.clone();
    let router = Router::new()
        .route(
            "/cool/echo",
            get(|headers: HeaderMap| async move {
                let get = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
                axum::Json(json!({
                    "cookie": get("cookie"),
                    "authorization": get("authorization"),
                    "host": get("host"),
                    "forwarded_host": get("x-forwarded-host"),
                    "forwarded_proto": get("x-forwarded-proto"),
                }))
            }),
        )
        .route("/cool/upload", post(|body: Bytes| async move { body.len().to_string() }))
        .route(
            "/cool/slow",
            get(|| async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                "slow"
            }),
        )
        .route(
            "/cool/redirect",
            get(move || async move {
                let headers = AppendHeaders([
                    (header::LOCATION, format!("{own}/browser/x.html")),
                    (header::SET_COOKIE, "rustcloud_session=forged; Path=/".to_owned()),
                    (header::SET_COOKIE, "cool=1; Path=/cool".to_owned()),
                ]);
                (StatusCode::FOUND, headers).into_response()
            }),
        )
        .route(
            "/cool/ws",
            get(|mut request: Request<Body>| async move {
                let upgrade = request.extensions_mut().remove::<hyper::upgrade::OnUpgrade>().unwrap();
                tokio::spawn(async move {
                    let mut io = hyper_util::rt::TokioIo::new(upgrade.await.unwrap());
                    let mut buf = [0u8; 64];
                    while let Ok(n) = io.read(&mut buf).await {
                        if n == 0 || io.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
                axum::response::Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .body(Body::empty())
                    .unwrap()
            }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (address, task)
}

/// Opens a WebSocket to `/cool/ws` of the server at `address`: the status of
/// the answer, and the connection.
async fn websocket(address: std::net::SocketAddr, cookie: &str, origin: Option<&str>) -> (u16, tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let origin = origin.map(|origin| format!("Origin: {origin}\r\n")).unwrap_or_default();
    // Any 16 bytes in base64 (RFC 6455 shows these).
    let key = STANDARD.encode("the sample nonce");
    let request = format!(
        "GET /cool/ws HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nCookie: {cookie}\r\n{origin}\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let status = head.split_whitespace().nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
    (status, stream)
}

#[tokio::test]
async fn a_plugins_service_is_reached_through_the_server_while_a_file_is_open() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (service, _task) = start_service().await;
    let manifest = format!(
        "{}[[server_settings]]\nkey = \"office_url\"\nkind = \"text\"\nlabel.en = \"Office\"\n\
         [[proxy]]\ntarget = \"{{office_url}}\"\npaths = [\"/browser\", \"/cool\"]\n",
        echo_manifest_toml()
    );
    let data = tempfile::tempdir().unwrap();
    let apps = data.path().join("apps");
    std::fs::create_dir_all(apps.join("echo")).unwrap();
    std::fs::write(apps.join("echo/manifest.toml"), &manifest).unwrap();
    std::fs::write(apps.join("echo/app.wasm"), echo_wasm()).unwrap();
    let config = format!(
        "data_dir = {:?}\n[accounts]\nopen_registration = true\n\
         [plugins]\nproxy_requests_per_user = 2\nproxy_connections_per_user = 1\nproxy_body_mib = 1\n",
        data.path()
    );
    let app = TestApp { data: Some(data), ..TestApp::build(&config, Vec::new()).await };
    let admin = app.register("admin").await;
    let alice = app.register("alice").await;
    let office = |url: String| {
        let body = json!({ "office_url": url });
        let admin = admin.clone();
        let app = &app;
        async move { app.json(Method::PUT, "/api/v1/admin/plugins/echo/settings", As::Browser(&admin), body).await }
    };
    assert!(app.get("/api/v1/apps", As::Browser(&alice)).await.json()["proxies"].get("echo").is_none());
    assert_eq!(app.get("/cool/echo", As::Browser(&alice)).await.status, StatusCode::OK, "the panel's until set");
    assert_eq!(office(service.clone()).await.status, StatusCode::OK);
    let apps = app.get("/api/v1/apps", As::Browser(&alice)).await.json();
    assert_eq!(apps["proxies"]["echo"], json!(["/browser", "/cool"]), "the panel may show them");
    let rights = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.json();
    let echo = rights.as_array().unwrap().iter().find(|p| p["id"] == "echo").unwrap().clone();
    assert_eq!(echo["manifest"]["proxy"][0]["paths"], json!(["/browser", "/cool"]), "the administrator sees it");

    // The service tells how it reaches the server: the server's address on
    // the connection to it, with the port the server listens on.
    assert_eq!(app.get("/app/echo/self-url", As::Anonymous).await.text(), "http://127.0.0.1:8080");

    let host = [(header::HOST, "cloud.test")];
    let echo = |who| {
        let (app, host) = (&app, &host);
        async move { app.send(Method::GET, "/cool/echo", who, Body::empty(), host).await }
    };
    assert_eq!(echo(As::Anonymous).await.status, StatusCode::UNAUTHORIZED);
    let token = alice.split_once('=').unwrap().1;
    assert_eq!(echo(As::Token(token)).await.status, StatusCode::UNAUTHORIZED, "a browser session only");
    let closed = echo(As::Browser(&alice)).await;
    assert_eq!(closed.status, StatusCode::FORBIDDEN);
    assert_eq!(closed.json()["error"]["code"], "NO_OPEN_FILE", "nobody reaches it without a file open in the app");

    // Alice opens a file in the app: now her browser reaches the service.
    let root = app.root_id(&alice).await;
    let file = app.upload(&alice, &root, "report.odt", "document").await;
    let grant = json!({ "app_id": "echo", "node_id": file["id"], "kind": "session", "write": true });
    assert_eq!(app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await.status, StatusCode::CREATED);
    let with_theme = format!("theme=dark; {alice}");
    let seen = echo(As::Browser(&with_theme)).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.text());
    let seen = seen.json();
    assert_eq!(seen["cookie"], "theme=dark", "the server's session never reaches the service");
    assert_eq!(seen["authorization"], Value::Null);
    assert_eq!(seen["host"], "cloud.test", "the service builds its addresses from the server's");
    assert_eq!(seen["forwarded_host"], "cloud.test");
    assert_eq!(seen["forwarded_proto"], "http");
    assert_eq!(echo(As::Browser(&admin)).await.status, StatusCode::FORBIDDEN, "only users with a file open");
    let elsewhere = app.send(Method::GET, "/coolness", As::Browser(&alice), Body::empty(), &host).await;
    assert!(elsewhere.header(header::CONTENT_SECURITY_POLICY).contains("default-src"), "other paths stay the panel's");

    // Changes come from the server's own pages only, and not too large.
    let upload = |extra: Vec<(header::HeaderName, String)>, body: Vec<u8>| {
        let app = &app;
        let alice = alice.clone();
        async move {
            let mut headers: Vec<(header::HeaderName, &str)> = vec![(header::HOST, "cloud.test")];
            headers.extend(extra.iter().map(|(name, value)| (name.clone(), value.as_str())));
            app.send(Method::POST, "/cool/upload", As::Browser(&alice), Body::from(body), &headers).await
        }
    };
    let foreign = upload(vec![(header::ORIGIN, "https://evil.example".into())], b"hello".to_vec()).await;
    assert_eq!(foreign.json()["error"]["code"], "ORIGIN_NOT_ALLOWED");
    let missing = upload(Vec::new(), b"hello".to_vec()).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN, "no Origin, no change");
    let own = vec![(header::ORIGIN, "http://cloud.test".to_owned()), (header::CONTENT_LENGTH, "5".to_owned())];
    assert_eq!(upload(own, b"hello".to_vec()).await.text(), "5");
    let size = (2 * 1024 * 1024).to_string();
    let big = vec![(header::ORIGIN, "http://cloud.test".to_owned()), (header::CONTENT_LENGTH, size)];
    assert_eq!(upload(big, vec![0; 2 * 1024 * 1024]).await.status, StatusCode::PAYLOAD_TOO_LARGE);

    // Redirects point at the server; the service can't set its session cookie.
    let redirect = app.send(Method::GET, "/cool/redirect", As::Browser(&alice), Body::empty(), &host).await;
    assert_eq!(redirect.status, StatusCode::FOUND);
    assert_eq!(redirect.header(header::LOCATION), "http://cloud.test/browser/x.html");
    let cookies: Vec<&str> = redirect.headers.get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect();
    assert_eq!(cookies, ["cool=1; Path=/cool"]);
    assert_eq!(redirect.header(header::X_FRAME_OPTIONS), "SAMEORIGIN", "the panel shows it in a frame");

    // A user can't take all the server's connections to the service.
    let slow = || app.send(Method::GET, "/cool/slow", As::Browser(&alice), Body::empty(), &host);
    let (a, b, c) = tokio::join!(slow(), slow(), slow());
    let mut statuses = [a.status, b.status, c.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::OK, StatusCode::TOO_MANY_REQUESTS]);

    // WebSocket: bytes go both ways as they are, from the server's own pages.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = app.router.clone().into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let page = format!("http://{address}");
    let (status, _) = websocket(address, &alice, None).await;
    assert_eq!(status, 403, "no Origin");
    let (status, _) = websocket(address, &alice, Some("https://evil.example")).await;
    assert_eq!(status, 403);
    let (status, _) = websocket(address, &admin, Some(&page)).await;
    assert_eq!(status, 403, "no file open");
    let (status, mut open) = websocket(address, &alice, Some(&page)).await;
    assert_eq!(status, 101);
    open.write_all(b"frame").await.unwrap();
    let mut echoed = [0u8; 5];
    open.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"frame");
    let (status, _) = websocket(address, &alice, Some(&page)).await;
    assert_eq!(status, 429, "one connection per user here");
    drop(open);
    let mut again = 0;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        again = websocket(address, &alice, Some(&page)).await.0;
        if again == 101 {
            break;
        }
    }
    assert_eq!(again, 101, "a closed connection gives its place back");

    // The service is down: the server says so.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    assert_eq!(office(format!("http://{dead}")).await.status, StatusCode::OK);
    let down = echo(As::Browser(&alice)).await;
    assert_eq!(down.status, StatusCode::BAD_GATEWAY);
    assert_eq!(down.json()["error"]["code"], "SERVICE_UNREACHABLE");
}

#[tokio::test]
async fn plugins_get_the_server_settings_the_admin_fills_in() {
    let manifest = format!(
        "{}[[server_settings]]\nkey = \"model\"\nkind = \"text\"\nlabel.en = \"Model\"\n\
         [[server_settings]]\nkey = \"api_key\"\nkind = \"secret\"\nlabel.en = \"API key\"\n\
         [[server_settings]]\nkey = \"verbose\"\nkind = \"bool\"\nlabel.en = \"Verbose\"\n",
        echo_manifest_toml()
    );
    let app = TestApp::with_wasm_apps(|apps| {
        std::fs::create_dir_all(apps.join("echo")).unwrap();
        std::fs::write(apps.join("echo/manifest.toml"), &manifest).unwrap();
        std::fs::write(apps.join("echo/app.wasm"), echo_wasm()).unwrap();
    })
    .await;
    let admin = app.register("admin").await;
    let bob = app.register("bob").await;
    let save = |body: Value, who: &str| {
        let who = who.to_owned();
        let app = &app;
        async move { app.json(Method::PUT, "/api/v1/admin/plugins/echo/settings", As::Browser(&who), body).await }
    };
    let settings = || async { app.get("/app/echo/settings", As::Anonymous).await.json() };
    assert_eq!(settings().await, json!({}));
    assert_eq!(save(json!({ "model": "small" }), &bob).await.status, StatusCode::FORBIDDEN);

    // The plugin gets everything; the browser never gets a secret back.
    let saved = save(json!({ "model": "small", "api_key": "k-123", "verbose": true }), &admin).await.json();
    assert_eq!(saved["settings"], json!({ "model": "small", "verbose": true }));
    assert_eq!(saved["secrets_set"], json!(["api_key"]));
    let list = app.get("/api/v1/admin/plugins", As::Browser(&admin)).await.text();
    assert!(!list.contains("k-123"), "{list}");
    assert_eq!(settings().await, json!({ "model": "small", "api_key": "k-123", "verbose": true }));

    // A secret left out (or empty) stays; other settings left out are cleared; null clears a secret.
    save(json!({ "model": "large", "api_key": "" }), &admin).await;
    assert_eq!(settings().await, json!({ "model": "large", "api_key": "k-123" }));
    save(json!({ "model": "large", "api_key": null }), &admin).await;
    assert_eq!(settings().await, json!({ "model": "large" }));

    for bad in [json!({ "model": 5 }), json!({ "nope": 1 }), json!({ "verbose": "yes" })] {
        assert_eq!(save(bad, &admin).await.json()["error"]["code"], "INVALID_SETTINGS");
    }
}

#[tokio::test]
async fn web_ui_and_unknown_api_paths() {
    let app = TestApp::new().await;

    let page = app.get("/files/some/app/route", As::Anonymous).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert!(page.header(header::CONTENT_SECURITY_POLICY).contains("default-src 'self'"));
    assert_eq!(page.header(header::X_FRAME_OPTIONS), "DENY");

    let missing = app.get("/api/v1/nope", As::Anonymous).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["error"]["code"], "NOT_FOUND");

    let health = app.get("/health", As::Anonymous).await;
    assert_eq!(health.json()["status"], "ok");
}

#[tokio::test]
async fn admins_change_settings_and_they_are_saved_to_the_config_file() {
    let app = TestApp::with_config("# my notes\n[accounts]\nsession_days = 7 # a week\n").await;
    let admin = app.register("admin").await;

    let sections = app.get("/api/v1/admin/settings", As::Browser(&admin)).await.json();
    let names: Vec<&str> = sections.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["server", "accounts", "files", "plugins", "media", "onnx", "journal"]);
    assert_eq!(sections[1]["values"]["session_days"], 7);
    assert_eq!(sections[1]["fields"][0], json!({ "key": "open_registration", "kind": "bool" }));

    let new_values =
        json!({ "open_registration": true, "session_days": 30, "max_login_failures": 10, "default_quota_mib": null });
    let reply = app.json(Method::PUT, "/api/v1/admin/settings/accounts", As::Browser(&admin), new_values).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.json()["values"]["session_days"], 30);

    let status = app.get("/api/v1/auth/status", As::Anonymous).await.json();
    assert_eq!(status["registration_open"], true, "applied without a restart");
    let file = app.config_file();
    assert!(file.contains("# my notes") && file.contains("session_days = 30 # a week"), "{file}");
    assert!(file.contains("open_registration = true"), "{file}");
    assert!(file.contains("default_quota_mib = 0"), "no limit is saved as 0, not as the default: {file}");
    assert_eq!(reply.json()["values"]["default_quota_mib"], Value::Null);

    let invalid = json!({ "session_days": 0 });
    let reply = app.json(Method::PUT, "/api/v1/admin/settings/accounts", As::Browser(&admin), invalid).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(app.config_file().contains("session_days = 30"), "an invalid save changes nothing");

    let storage = app.get("/api/v1/admin/storage", As::Browser(&admin)).await;
    assert_eq!(storage.status, StatusCode::OK);
    assert_eq!(storage.json()["kind"], "local");

    let bob = app.register("bob").await;
    for path in ["/api/v1/admin/settings", "/api/v1/admin/storage"] {
        assert_eq!(app.get(path, As::Browser(&bob)).await.status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn only_listed_addresses_are_served_when_enabled() {
    let app =
        TestApp::with_config("[server]\npublic_url = \"https://cloud.example.com\"\nonly_listed_urls = true\n").await;
    let host = |name: &'static str| [(header::HOST, name)];

    let listed = app.send(Method::GET, "/api/v1/info", As::Anonymous, Body::empty(), &host("cloud.example.com")).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json()["public_url"], "https://cloud.example.com");

    let local = app.send(Method::GET, "/health", As::Anonymous, Body::empty(), &host("127.0.0.1:8080")).await;
    assert_eq!(local.status, StatusCode::OK, "health checks use localhost");

    let other = app.send(Method::GET, "/health", As::Anonymous, Body::empty(), &host("rebind.example.net")).await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
    assert_eq!(other.json()["error"]["code"], "HOST_NOT_ALLOWED");
}

#[tokio::test]
async fn cors_is_allowed_only_for_listed_origins_and_without_cookies() {
    let app = TestApp::with_config("[server]\ncors_origins = [\"https://app.example.com\"]\n").await;
    let preflight = |origin: &'static str| {
        [
            (header::ORIGIN, origin),
            (header::ACCESS_CONTROL_REQUEST_METHOD, "POST"),
            (header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization"),
        ]
    };

    let listed = app
        .send(
            Method::OPTIONS,
            "/api/v1/auth/login",
            As::Anonymous,
            Body::empty(),
            &preflight("https://app.example.com"),
        )
        .await;
    assert_eq!(listed.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), "https://app.example.com");
    assert_eq!(listed.header(header::ACCESS_CONTROL_ALLOW_CREDENTIALS), "", "cookies are never shared");

    let other = app
        .send(
            Method::OPTIONS,
            "/api/v1/auth/login",
            As::Anonymous,
            Body::empty(),
            &preflight("https://evil.example.com"),
        )
        .await;
    assert_eq!(other.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), "");
}

#[tokio::test]
async fn the_activity_log_shows_who_did_what() {
    let app = TestApp::with_config("[accounts]\nopen_registration = true\n").await;
    let admin = app.register("admin").await;
    let root = app.root_id(&admin).await;
    let upload = format!("/api/v1/files/{root}/upload?name=report.txt");
    let file = app.send(Method::POST, &upload, As::Browser(&admin), Body::from("data"), &[]).await.json();
    app.get(&format!("/api/v1/files/{}/content", file["id"].as_str().unwrap()), As::Browser(&admin)).await;
    let wrong = json!({ "username": "admin", "password": "wrong-password" });
    app.json(Method::POST, "/api/v1/auth/login", As::Anonymous, wrong).await;

    // The log is written in the background: wait for the last entry. Up to
    // 10 s, because a busy CI database can be slow (it usually takes a few ms).
    let mut entries = Value::Null;
    for _ in 0..1000 {
        entries = app.get("/api/v1/admin/journal", As::Browser(&admin)).await.json()["entries"].clone();
        if entries.as_array().unwrap().iter().any(|e| e["action"] == "auth.login_failed") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let find = |action: &str| {
        entries.as_array().unwrap().iter().find(|e| e["action"] == action).cloned().unwrap_or_else(|| {
            panic!("no {action} in {entries:#}");
        })
    };
    assert_eq!(find("auth.login")["username"], "admin");
    assert_eq!(find("file.upload")["username"], "admin", "the request's user is attached");
    assert_eq!(find("file.download")["details"]["name"], "report.txt");
    assert_eq!(find("auth.login_failed")["level"], "WARN");

    let problems = app.get("/api/v1/admin/journal?problems=true", As::Browser(&admin)).await.json();
    assert!(problems["entries"].as_array().unwrap().iter().all(|e| e["level"] != "INFO"));
    let search = app.get("/api/v1/admin/journal?q=report.txt", As::Browser(&admin)).await.json();
    assert_eq!(search["entries"].as_array().unwrap().len(), 2, "upload and download");

    let bob = app.register("bob").await;
    assert_eq!(app.get("/api/v1/admin/journal", As::Browser(&bob)).await.status, StatusCode::FORBIDDEN);
}

/// A login service for tests: OpenID Connect discovery, keys and a token
/// endpoint on a port of this machine. The test plays the user at the
/// service: it reads the login request the server sent the browser with and
/// gives a code for the claims it wants.
struct TestIdp {
    issuer: String,
    /// By code: the claims of the ID token and the PKCE challenge it was asked with.
    codes: Arc<std::sync::Mutex<std::collections::HashMap<String, (Value, String)>>>,
}

impl TestIdp {
    async fn start() -> Self {
        use openidconnect::core::{CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey};
        use openidconnect::{JsonWebKeyId, PrivateSigningKey};
        use sha2::{Digest, Sha256};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // A fresh key every run: a private key in the repository would look
        // like a leaked secret to scanners.
        let pem = {
            use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
            let key = rsa::RsaPrivateKey::new(&mut rand08::rngs::OsRng, 2048).unwrap();
            key.to_pkcs1_pem(LineEnding::LF).unwrap().to_string()
        };
        let key = Arc::new(CoreRsaPrivateSigningKey::from_pem(&pem, Some(JsonWebKeyId::new("test".into()))).unwrap());
        let codes = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<String, (Value, String)>::new()));

        let discovery = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        });
        let jwks = json!({ "keys": [key.as_verification_key()] });
        let token = {
            let (codes, key) = (codes.clone(), key.clone());
            move |headers: HeaderMap, axum::extract::Form(form): axum::extract::Form<Vec<(String, String)>>| {
                let (codes, key) = (codes.clone(), key.clone());
                async move {
                    let field = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
                    let basic = format!("Basic {}", STANDARD.encode("rustcloud:s3cret"));
                    if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some(basic.as_str()) {
                        return (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "invalid_client" })));
                    }
                    let Some((claims, challenge)) = field("code").and_then(|code| codes.lock().unwrap().remove(&code))
                    else {
                        return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" })));
                    };
                    let verifier = field("code_verifier").unwrap_or_default();
                    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier));
                    if computed != challenge {
                        return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" })));
                    }
                    // The ID token, signed with the key of /jwks.
                    let header = json!({ "alg": "RS256", "typ": "JWT", "kid": "test" });
                    let encode = |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
                    let input = format!("{}.{}", encode(&header), encode(&claims));
                    let signature = key.sign(&CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, input.as_bytes()).unwrap();
                    let id_token =
                        format!("{input}.{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature));
                    let reply = json!({ "access_token": "at", "token_type": "Bearer", "id_token": id_token });
                    (StatusCode::OK, axum::Json(reply))
                }
            }
        };
        let remote = Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(move || async move { axum::Json(discovery) }),
            )
            .route("/jwks", axum::routing::get(move || async move { axum::Json(jwks) }))
            .route("/token", axum::routing::post(token));
        tokio::spawn(async move { axum::serve(listener, remote).await });
        Self { issuer, codes }
    }

    /// Plays the user at the service for the login the server sent the
    /// browser to (`location`): answers with a code for an ID token about
    /// `sub` with `extra` claims. Returns the callback's query.
    fn answer(&self, location: &str, sub: &str, extra: Value) -> String {
        let url = url::Url::parse(location).unwrap();
        assert!(location.starts_with(&format!("{}/authorize?", self.issuer)), "{location}");
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "rustcloud");
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(query["scope"].split(' ').any(|s| s == "openid"));
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({
            "iss": self.issuer, "aud": "rustcloud", "sub": sub, "nonce": query["nonce"],
            "iat": now, "exp": now + 300,
        });
        claims.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let code = uuid::Uuid::new_v4().to_string();
        self.codes.lock().unwrap().insert(code.clone(), (claims, query["code_challenge"].clone()));
        format!("code={code}&state={}", query["state"])
    }
}

impl Reply {
    /// A cookie (`name=value`) set by this response.
    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_owned())
            .find(|pair| pair.starts_with(&format!("{name}=")) && pair.len() > name.len() + 1)
    }
}

/// Logs in at the service of the test as `sub` with `claims`; the reply of
/// the callback (a redirect).
async fn oidc_login(app: &TestApp, idp: &TestIdp, sub: &str, claims: Value) -> Reply {
    let start = app.get("/api/v1/auth/oidc/corp/login", As::Anonymous).await;
    assert_eq!(start.status, StatusCode::SEE_OTHER, "{}", start.text());
    let flow = start.cookie("rustcloud_oidc").expect("the flow cookie");
    let query = idp.answer(start.header(header::LOCATION), sub, claims);
    app.send(
        Method::GET,
        &format!("/api/v1/auth/oidc/corp/callback?{query}"),
        As::Anonymous,
        Body::empty(),
        &[(header::COOKIE, &flow)],
    )
    .await
}

#[tokio::test]
async fn users_log_in_through_an_openid_connect_service() {
    let idp = TestIdp::start().await;
    let app = TestApp::new().await;
    let admin = app.register("admin").await;

    // The administrator adds the service; the secret never comes back.
    let provider = json!({
        "id": "corp", "name": "Corp SSO", "issuer": idp.issuer, "client_id": "rustcloud",
        "client_secret": "s3cret", "user_groups": ["staff"], "admin_groups": ["admins"],
    });
    let added = app.json(Method::POST, "/api/v1/admin/oidc", As::Browser(&admin), provider.clone()).await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text());
    let added = added.json();
    assert_eq!(added["has_secret"], true);
    assert!(added.get("client_secret").is_none());
    assert_eq!(added["scopes"], "openid profile email");
    assert_eq!(added["redirect_url"], "http://localhost/api/v1/auth/oidc/corp/callback");
    let again = app.json(Method::POST, "/api/v1/admin/oidc", As::Browser(&admin), provider).await;
    assert_eq!(again.json()["error"]["code"], "OIDC_PROVIDER_EXISTS");
    let broken = json!({ "id": "nowhere", "name": "X", "issuer": "http://127.0.0.1:1", "client_id": "x" });
    let broken = app.json(Method::POST, "/api/v1/admin/oidc", As::Browser(&admin), broken).await;
    assert_eq!(broken.json()["error"]["code"], "OIDC_DISCOVERY_FAILED", "a service that can't be reached isn't added");
    assert_eq!(app.get("/api/v1/auth/oidc", As::Anonymous).await.json(), json!([{ "id": "corp", "name": "Corp SSO" }]));

    // A new member of staff gets an account on the first login, the same one later.
    let alice = json!({ "preferred_username": "Alice", "name": "Alice Smith", "groups": ["staff"] });
    let reply = oidc_login(&app, &idp, "id-alice", alice.clone()).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), "/");
    let session = reply.cookie("rustcloud_session").expect("a session");
    let me = app.get("/api/v1/auth/me", As::Browser(&session)).await.json();
    assert_eq!((me["username"].as_str(), me["display_name"].as_str()), (Some("alice"), Some("Alice Smith")));
    assert_eq!(me["is_admin"], false);
    let reply = oidc_login(&app, &idp, "id-alice", alice).await;
    let session = reply.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&session)).await.json()["id"], me["id"]);

    // Groups decide who may log in and who is an administrator, at every login.
    let reply = oidc_login(&app, &idp, "id-guest", json!({ "groups": ["guests"] })).await;
    assert_eq!(reply.header(header::LOCATION), "/#/login?oidc_error=NOT_ALLOWED");
    let reply = oidc_login(&app, &idp, "id-alice", json!({ "groups": ["staff", "admins"] })).await;
    let session = reply.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&session)).await.json()["is_admin"], true);
    let reply = oidc_login(&app, &idp, "id-alice", json!({ "groups": ["staff"] })).await;
    let session = reply.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&session)).await.json()["is_admin"], false);

    // Someone else's code or an old login logs nobody in; nor does a token for another login.
    let start = app.get("/api/v1/auth/oidc/corp/login", As::Anonymous).await;
    let query = idp.answer(start.header(header::LOCATION), "id-alice", json!({ "groups": ["staff"] }));
    let stolen = app.get(&format!("/api/v1/auth/oidc/corp/callback?{query}"), As::Anonymous).await;
    assert_eq!(stolen.header(header::LOCATION), "/#/login?oidc_error=OIDC_EXPIRED");
    assert!(stolen.cookie("rustcloud_session").is_none());
    let reply = oidc_login(&app, &idp, "id-alice", json!({ "groups": ["staff"], "nonce": "other" })).await;
    assert_eq!(reply.header(header::LOCATION), "/#/login?oidc_error=OIDC_FAILED");
    let cancelled = app.get("/api/v1/auth/oidc/corp/login", As::Anonymous).await;
    let flow = cancelled.cookie("rustcloud_oidc").unwrap();
    let state = url::Url::parse(cancelled.header(header::LOCATION))
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let uri = format!("/api/v1/auth/oidc/corp/callback?error=access_denied&state={state}");
    let reply = app.send(Method::GET, &uri, As::Anonymous, Body::empty(), &[(header::COOKIE, &flow)]).await;
    assert_eq!(reply.header(header::LOCATION), "/#/login?oidc_error=OIDC_CANCELLED");

    // With linking by email, a verified email finds the existing account.
    let bob = json!({ "username": "bob", "password": "password123", "email": "Bob@corp.example" });
    app.json(Method::POST, "/api/v1/admin/users", As::Browser(&admin), bob).await;
    let mut change = added.clone();
    change["link_by_email"] = json!(true);
    change["auto_create"] = json!(false);
    let changed = app.json(Method::PUT, "/api/v1/admin/oidc/corp", As::Browser(&admin), change).await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.text());
    assert_eq!(changed.json()["has_secret"], true, "an empty secret keeps the old one");
    let unverified = json!({ "email": "bob@corp.example", "groups": ["staff"] });
    let reply = oidc_login(&app, &idp, "id-bob", unverified).await;
    assert_eq!(reply.header(header::LOCATION), "/#/login?oidc_error=NO_ACCOUNT", "unverified emails don't count");
    let verified = json!({ "email": "bob@corp.example", "email_verified": true, "groups": ["staff"] });
    let session = oidc_login(&app, &idp, "id-bob", verified).await.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&session)).await.json()["username"], "bob");

    // Users add the service to their account from the settings and remove it again.
    let link = app.json(Method::POST, "/api/v1/auth/oidc/corp/link", As::Browser(&admin), json!({})).await;
    assert_eq!(link.status, StatusCode::OK, "{}", link.text());
    let flow = link.cookie("rustcloud_oidc").unwrap();
    let query = idp.answer(link.json()["url"].as_str().unwrap(), "id-boss", json!({ "groups": ["admins"] }));
    let uri = format!("/api/v1/auth/oidc/corp/callback?{query}");
    let reply = app.send(Method::GET, &uri, As::Anonymous, Body::empty(), &[(header::COOKIE, &flow)]).await;
    assert_eq!(reply.header(header::LOCATION), "/#/settings?oidc_linked=corp");
    let session = oidc_login(&app, &idp, "id-boss", json!({ "groups": ["admins"] })).await;
    let session = session.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/auth/me", As::Browser(&session)).await.json()["username"], "admin");
    let logins = app.get("/api/v1/auth/logins", As::Browser(&admin)).await.json();
    let providers: Vec<&str> = logins.as_array().unwrap().iter().map(|l| l["provider"].as_str().unwrap()).collect();
    assert_eq!(providers, ["password", "oidc:corp"]);
    let password_id = logins[0]["id"].as_str().unwrap();
    let removed = app
        .send(Method::DELETE, &format!("/api/v1/auth/logins/{password_id}"), As::Browser(&admin), Body::empty(), &[])
        .await;
    assert_eq!(removed.json()["error"]["code"], "CANNOT_UNLINK_PASSWORD");
    let oidc_id = logins[1]["id"].as_str().unwrap();
    let removed = app
        .send(Method::DELETE, &format!("/api/v1/auth/logins/{oidc_id}"), As::Browser(&admin), Body::empty(), &[])
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);

    // Only administrators manage services; removing one forgets its logins.
    let alice = oidc_login(&app, &idp, "id-alice", json!({ "groups": ["staff"] })).await;
    let alice = alice.cookie("rustcloud_session").unwrap();
    assert_eq!(app.get("/api/v1/admin/oidc", As::Browser(&alice)).await.status, StatusCode::FORBIDDEN);
    let listed = app.get("/api/v1/admin/oidc", As::Browser(&admin)).await.json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let gone = app.send(Method::DELETE, "/api/v1/admin/oidc/corp", As::Browser(&admin), Body::empty(), &[]).await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/api/v1/auth/oidc", As::Anonymous).await.json(), json!([]));
    let start = app.get("/api/v1/auth/oidc/corp/login", As::Anonymous).await;
    assert_eq!(start.header(header::LOCATION), "/#/login?oidc_error=OIDC_UNAVAILABLE");
}

#[tokio::test]
async fn sync_clients_read_the_change_feed_and_hear_of_new_changes() {
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;
    let start = app.get("/api/v1/files/changes", As::Browser(&cookie)).await.json();
    let cursor = start["cursor"].as_str().unwrap().to_owned();

    // The connection that waits for hints is open before the change.
    let request = Request::get("/api/v1/files/changes/watch")
        .header(header::COOKIE, &cookie)
        .header("x-requested-with", "rustcloud")
        .body(Body::empty())
        .unwrap();
    let watch = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(watch.status(), StatusCode::OK);
    assert_eq!(watch.headers()[header::CONTENT_TYPE], "text/event-stream");
    let file = app.upload(&cookie, &root, "a.txt", "hello").await;

    let mut events = watch.into_body();
    let frame =
        tokio::time::timeout(std::time::Duration::from_secs(10), events.frame()).await.unwrap().unwrap().unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(text.contains("event: changes"), "{text}");

    let page = app.get(&format!("/api/v1/files/changes?cursor={cursor}"), As::Browser(&cookie)).await.json();
    assert_eq!(page["changes"], json!([{ "id": file["id"], "kind": "created" }]));
    assert_eq!(page["more"], false);

    let stale = app.get("/api/v1/files/changes?cursor=999999", As::Browser(&cookie)).await;
    assert_eq!(stale.status, StatusCode::GONE);
    assert_eq!(stale.json()["error"]["code"], "CURSOR_EXPIRED");
}

#[tokio::test]
async fn large_files_are_uploaded_in_parts() {
    use sha2::Digest;
    let app = TestApp::new().await;
    let cookie = app.register("admin").await;
    let root = app.root_id(&cookie).await;

    let start = json!({ "name": "video.mp4", "size": 10, "mtime": "1700000000" });
    let reply = app.json(Method::POST, &format!("/api/v1/files/{root}/uploads"), As::Browser(&cookie), start).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/files/uploads/{id}");

    let (app, cookie) = (&app, cookie.as_str());
    let part = |offset: u32, bytes: &'static str| {
        let uri = format!("{uri}?offset={offset}");
        async move { app.send(Method::PUT, &uri, As::Browser(cookie), Body::from(bytes), &[]).await }
    };
    assert_eq!(part(0, "01234").await.json()["received"], 5);
    let again = part(0, "01234").await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.json()["error"]["code"], "WRONG_OFFSET");
    // A client that restarted finds where it stopped.
    let open = app.get("/api/v1/files/uploads", As::Browser(cookie)).await.json();
    assert_eq!(open[0]["received"], 5);
    part(5, "56789").await;

    let sha256 = format!("{:x}", sha2::Sha256::digest("0123456789"));
    let reply =
        app.json(Method::POST, &format!("{uri}/finish"), As::Browser(cookie), json!({ "sha256": sha256 })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let file = reply.json();
    assert_eq!(file["etag"], sha256.as_str());
    assert_eq!(file["modified_at"], "2023-11-14T22:13:20Z");
    let content =
        app.get(&format!("/api/v1/files/{}/content", file["id"].as_str().unwrap()), As::Browser(cookie)).await;
    assert_eq!(content.text(), "0123456789");
    assert_eq!(app.get(&uri, As::Browser(cookie)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn files_are_reached_over_webdav() {
    let app = TestApp::new().await;
    let alice = app.register("alice").await;
    let mut passwords = Vec::new();
    for access in ["write", "read"] {
        let body = json!({ "name": "Laptop", "scopes": [{ "app": "files", "access": access }] });
        let reply = app.json(Method::POST, "/api/v1/app-passwords", As::Browser(&alice), body).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        passwords.push(reply.json()["password"].as_str().unwrap().to_owned());
    }
    let (laptop, reader) = (passwords[0].as_str(), passwords[1].as_str());

    // Clients ask what the server can, then for a password.
    let options = app.send(Method::OPTIONS, "/dav/", As::Anonymous, Body::empty(), &[]).await;
    assert_eq!(options.header(header::HeaderName::from_static("dav")), "1, 2");
    let propfind = Method::from_bytes(b"PROPFIND").unwrap();
    let asked = app.send(propfind, "/dav/", As::Anonymous, Body::empty(), &[]).await;
    assert_eq!(asked.status, StatusCode::UNAUTHORIZED);
    assert!(asked.header(header::WWW_AUTHENTICATE).starts_with("Basic"));

    assert_eq!(dav(&app, laptop, "MKCOL", "/dav/Docs", "", &[]).await.status, StatusCode::CREATED);
    assert_eq!(dav(&app, laptop, "MKCOL", "/dav/Docs", "", &[]).await.status, StatusCode::METHOD_NOT_ALLOWED);
    let put = dav(&app, laptop, "PUT", "/dav/Docs/a%20b.txt", "hello", &[("x-oc-mtime", "1730985600")]).await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", put.text());
    assert_eq!(put.header(header::HeaderName::from_static("x-oc-mtime")), "accepted");
    let etag = put.header(header::ETAG).to_owned();
    assert_eq!(dav(&app, laptop, "PUT", "/dav/Nope/x.txt", "x", &[]).await.status, StatusCode::CONFLICT);

    // A folder lists itself and what is in it, with what the client asked for.
    let props = r#"<d:propfind xmlns:d="DAV:" xmlns:z="urn:z"><d:prop>
        <d:getcontentlength/><d:getlastmodified/><d:resourcetype/><z:color/></d:prop></d:propfind>"#;
    let listed = dav(&app, laptop, "PROPFIND", "/dav/Docs/", props, &[("depth", "1")]).await;
    assert_eq!(listed.status.as_u16(), 207);
    let text = listed.text();
    assert!(text.contains("<D:href>/dav/Docs/</D:href>"), "{text}");
    assert!(text.contains("<D:href>/dav/Docs/a%20b.txt</D:href>"), "{text}");
    assert!(text.contains("<D:getcontentlength>5</D:getcontentlength>"), "{text}");
    assert!(text.contains("Thu, 07 Nov 2024 13:20:00 GMT"), "the mtime the client gave: {text}");
    assert!(text.contains("<D:collection/>"), "{text}");
    assert!(text.contains("<x:color xmlns:x=\"urn:z\"/>") && text.contains("404 Not Found"), "{text}");
    let all = dav(&app, laptop, "PROPFIND", "/dav/", "", &[("depth", "1")]).await.text();
    assert!(all.contains("<D:href>/dav/Docs/</D:href>"), "{all}");

    let got = dav(&app, laptop, "GET", "/dav/Docs/a%20b.txt", "", &[]).await;
    assert_eq!(got.text(), "hello");
    assert_eq!(got.header(header::LAST_MODIFIED), "Thu, 07 Nov 2024 13:20:00 GMT");

    // Overwriting checks the ETag, if the client gives one.
    let stale = dav(&app, laptop, "PUT", "/dav/Docs/a%20b.txt", "lost", &[("if-match", "\"0000\"")]).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let fresh = dav(&app, laptop, "PUT", "/dav/Docs/a%20b.txt", "hello!", &[("if-match", &etag)]).await;
    assert_eq!(fresh.status, StatusCode::NO_CONTENT);

    // Copy, move, and moving over something only when allowed.
    let to_copy = [("destination", "http://localhost/dav/Copy.txt")];
    assert_eq!(dav(&app, laptop, "COPY", "/dav/Docs/a%20b.txt", "", &to_copy).await.status, StatusCode::CREATED);
    let over = [("destination", "/dav/Docs/a%20b.txt"), ("overwrite", "F")];
    assert_eq!(dav(&app, laptop, "MOVE", "/dav/Copy.txt", "", &over).await.status, StatusCode::PRECONDITION_FAILED);
    let to_move = [("destination", "/dav/Docs/Moved.txt")];
    assert_eq!(dav(&app, laptop, "MOVE", "/dav/Copy.txt", "", &to_move).await.status, StatusCode::CREATED);
    assert_eq!(dav(&app, laptop, "GET", "/dav/Docs/Moved.txt", "", &[]).await.text(), "hello!");
    assert_eq!(dav(&app, laptop, "GET", "/dav/Copy.txt", "", &[]).await.status, StatusCode::NOT_FOUND);
    let into_itself = [("destination", "/dav/Docs/Inner/")];
    assert_eq!(dav(&app, laptop, "MOVE", "/dav/Docs/", "", &into_itself).await.status, StatusCode::FORBIDDEN);

    // Windows sets the time of a file it copied; other properties are refused.
    let patch = r#"<d:propertyupdate xmlns:d="DAV:" xmlns:w="urn:schemas-microsoft-com:"><d:set><d:prop>
        <w:Win32LastModifiedTime>Wed, 20 Feb 2008 17:44:48 GMT</w:Win32LastModifiedTime>
        <w:Win32FileAttributes>00000020</w:Win32FileAttributes></d:prop></d:set></d:propertyupdate>"#;
    let patched = dav(&app, laptop, "PROPPATCH", "/dav/Docs/Moved.txt", patch, &[]).await;
    assert_eq!(patched.status.as_u16(), 207);
    assert!(patched.text().contains("200 OK") && !patched.text().contains("403"), "{}", patched.text());
    let moved = dav(&app, laptop, "GET", "/dav/Docs/Moved.txt", "", &[]).await;
    assert_eq!(moved.header(header::LAST_MODIFIED), "Wed, 20 Feb 2008 17:44:48 GMT");
    let foreign = r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>x</d:displayname>
        </d:prop></d:set></d:propertyupdate>"#;
    let refused = dav(&app, laptop, "PROPPATCH", "/dav/Docs/Moved.txt", foreign, &[]).await.text();
    assert!(refused.contains("403 Forbidden"), "{refused}");

    // Locks are granted (Finder and Windows need them); locking a new name makes the file.
    let lockinfo = r#"<d:lockinfo xmlns:d="DAV:"><d:lockscope><d:exclusive/></d:lockscope>
        <d:locktype><d:write/></d:locktype></d:lockinfo>"#;
    let locked = dav(&app, laptop, "LOCK", "/dav/new.txt", lockinfo, &[]).await;
    assert_eq!(locked.status, StatusCode::CREATED, "{}", locked.text());
    assert!(locked.header(header::HeaderName::from_static("lock-token")).starts_with("<opaquelocktoken:"));
    assert_eq!(dav(&app, laptop, "UNLOCK", "/dav/new.txt", "", &[]).await.status, StatusCode::NO_CONTENT);

    // Deleting goes to the trash, which WebDAV doesn't show.
    assert_eq!(dav(&app, laptop, "DELETE", "/dav/Docs/Moved.txt", "", &[]).await.status, StatusCode::NO_CONTENT);
    assert_eq!(dav(&app, laptop, "GET", "/dav/Docs/Moved.txt", "", &[]).await.status, StatusCode::NOT_FOUND);
    let trash = app.get("/api/v1/files/trash", As::Browser(&alice)).await.json();
    let in_trash = format!("/api/v1/files/{}/children", trash["id"].as_str().unwrap());
    assert_eq!(app.get(&in_trash, As::Browser(&alice)).await.json()[0]["name"], "Moved.txt");
    assert!(!dav(&app, laptop, "PROPFIND", "/dav/", "", &[("depth", "1")]).await.text().contains(".trash"));
    assert_eq!(dav(&app, laptop, "PROPFIND", "/dav/.trash/", "", &[]).await.status, StatusCode::NOT_FOUND);
    assert_eq!(dav(&app, laptop, "MKCOL", "/dav/.trash", "", &[]).await.status, StatusCode::FORBIDDEN);

    // A password for reading only reads.
    let read = dav_as(&app, "alice", reader, "PROPFIND", "/dav/", "", &[("depth", "0")]).await;
    assert_eq!(read.status.as_u16(), 207);
    let write = dav_as(&app, "alice", reader, "PUT", "/dav/x.txt", "x", &[]).await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);
    let wrong = dav_as(&app, "alice", "wrong", "PROPFIND", "/dav/", "", &[]).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn folders_are_downloaded_as_zip_archives() {
    let app = TestApp::with_config("[accounts]\nopen_registration = true\n").await;
    let cookie = app.register("alice").await;
    let root = app.root_id(&cookie).await;
    let folder = |parent: String, name: &'static str| {
        let app = &app;
        let cookie = cookie.clone();
        async move {
            let uri = format!("/api/v1/files/{parent}/folders");
            let reply = app.json(Method::POST, &uri, As::Browser(&cookie), json!({ "name": name })).await;
            reply.json()["id"].as_str().unwrap().to_owned()
        }
    };
    let trip = folder(root.clone(), "Trip").await;
    let day = folder(trip.clone(), "Day 1").await;
    folder(trip.clone(), "Empty").await;
    app.upload(&cookie, &trip, "notes.txt", "packing list").await;
    app.upload(&cookie, &day, "photo.jpg", "not really a photo").await;
    let gone = app.upload(&cookie, &root, "old.txt", "deleted").await;
    let uri = format!("/api/v1/files/{}", gone["id"].as_str().unwrap());
    app.send(Method::DELETE, &uri, As::Browser(&cookie), Body::empty(), &[]).await;

    let read = |bytes: &[u8]| {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut entries = Vec::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let mut content = String::new();
            std::io::Read::read_to_string(&mut file, &mut content).unwrap();
            entries.push((file.name().unwrap().to_string(), content));
        }
        entries
    };
    let reply = app.get(&format!("/api/v1/files/{trip}/zip"), As::Browser(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::CONTENT_TYPE), "application/zip");
    assert!(reply.header(header::CONTENT_DISPOSITION).contains("Trip.zip"));
    let entries = read(&reply.body);
    let expected =
        [("Day 1/", ""), ("Day 1/photo.jpg", "not really a photo"), ("Empty/", ""), ("notes.txt", "packing list")];
    let expected: Vec<(String, String)> = expected.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    assert_eq!(entries, expected);

    // Everything, but not the trash.
    let all = read(&app.get(&format!("/api/v1/files/{root}/zip"), As::Browser(&cookie)).await.body);
    assert!(all.iter().any(|(name, _)| name == "Trip/Day 1/photo.jpg"));
    assert!(all.iter().all(|(name, _)| !name.contains("trash") && !name.contains("old.txt")), "{all:?}");

    // Through a public link, a subfolder too; files are not archives.
    let share = app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), json!({ "node_id": trip })).await.json();
    let token = share["token"].as_str().unwrap();
    let public = app.get(&format!("/api/v1/public/{token}/zip?node={day}"), As::Anonymous).await;
    assert_eq!(read(&public.body), [("photo.jpg".to_owned(), "not really a photo".to_owned())]);
    let file = app.upload(&cookie, &trip, "x.txt", "x").await;
    let uri = format!("/api/v1/files/{}/zip", file["id"].as_str().unwrap());
    assert_eq!(app.get(&uri, As::Browser(&cookie)).await.json()["error"]["code"], "NOT_A_FOLDER");
    let bob = app.register("bob").await;
    assert_eq!(app.get(&format!("/api/v1/files/{trip}/zip"), As::Browser(&bob)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admins_delete_accounts_with_all_their_files() {
    let app = TestApp::new().await;
    let admin = app.register("admin").await;
    let account = json!({ "username": "bob", "password": "password123" });
    let created = app.json(Method::POST, "/api/v1/admin/users", As::Browser(&admin), account.clone()).await.json();
    let bob_id = created["id"].as_str().unwrap().to_owned();
    let login = json!({ "username": "bob", "password": "password123" });
    let bob = app.json(Method::POST, "/api/v1/auth/login", As::Anonymous, login.clone()).await.session_cookie();

    // Files, the trash and half an upload in parts: all blobs. (Saves this
    // close together keep no earlier version.)
    let blobs = || walk_files(&app.dir.path().join("blobs"));
    let root = app.root_id(&bob).await;
    app.upload(&bob, &root, "a.txt", "one").await;
    let uri = format!("/api/v1/files/{root}/upload?name=a.txt&overwrite=true");
    app.send(Method::POST, &uri, As::Browser(&bob), Body::from("two"), &[]).await;
    let gone = app.upload(&bob, &root, "b.txt", "trash").await;
    let uri = format!("/api/v1/files/{}", gone["id"].as_str().unwrap());
    app.send(Method::DELETE, &uri, As::Browser(&bob), Body::empty(), &[]).await;
    let uri = format!("/api/v1/files/{root}/uploads");
    let upload = app.json(Method::POST, &uri, As::Browser(&bob), json!({ "name": "big.bin", "size": 10 })).await.json();
    let uri = format!("/api/v1/files/uploads/{}?offset=0", upload["id"].as_str().unwrap());
    let part = app.send(Method::PUT, &uri, As::Browser(&bob), Body::from("12345"), &[]).await;
    assert_eq!(part.status, StatusCode::OK, "{}", part.text());
    assert_eq!(blobs(), 3);

    let user_uri = format!("/api/v1/admin/users/{bob_id}");
    let by_bob = app.send(Method::DELETE, &user_uri, As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(by_bob.status, StatusCode::FORBIDDEN);
    let me = app.get("/api/v1/auth/me", As::Browser(&admin)).await.json();
    let self_uri = format!("/api/v1/admin/users/{}", me["id"].as_str().unwrap());
    let own = app.send(Method::DELETE, &self_uri, As::Browser(&admin), Body::empty(), &[]).await;
    assert_eq!(own.json()["error"]["code"], "CANNOT_CHANGE_SELF");

    let deleted = app.send(Method::DELETE, &user_uri, As::Browser(&admin), Body::empty(), &[]).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text());
    assert_eq!(blobs(), 0, "nothing of bob's is left in storage");
    assert_eq!(app.get("/api/v1/files/root", As::Browser(&bob)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/api/v1/admin/users", As::Browser(&admin)).await.json().as_array().unwrap().len(), 1);
    let again = app.send(Method::DELETE, &user_uri, As::Browser(&admin), Body::empty(), &[]).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    // The name is free again, for a new account with nothing of the old one.
    let created = app.json(Method::POST, "/api/v1/admin/users", As::Browser(&admin), account).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let bob = app.json(Method::POST, "/api/v1/auth/login", As::Anonymous, login).await.session_cookie();
    let root = app.root_id(&bob).await;
    let children = app.get(&format!("/api/v1/files/{root}/children"), As::Browser(&bob)).await.json();
    assert_eq!(children, json!([]));
}

/// Files below `dir`, at any depth.
fn walk_files(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.flatten().map(|entry| if entry.path().is_dir() { walk_files(&entry.path()) } else { 1 }).sum()
}

#[tokio::test]
async fn public_links_may_have_a_password_and_take_uploads() {
    let app = TestApp::new().await;
    let cookie = app.register("alice").await;
    let root = app.root_id(&cookie).await;
    let uri = format!("/api/v1/files/{root}/folders");
    let inbox = app.json(Method::POST, &uri, As::Browser(&cookie), json!({ "name": "Inbox" })).await.json();
    let inbox = inbox["id"].as_str().unwrap().to_owned();
    let file = app.upload(&cookie, &inbox, "rules.txt", "be nice").await;
    let file_id = file["id"].as_str().unwrap();

    let options = json!({ "node_id": inbox, "password": "open sesame", "allow_upload": true });
    let share = app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), options).await.json();
    assert_eq!((share["has_password"].as_bool(), share["allow_upload"].as_bool()), (Some(true), Some(true)));
    let token = share["token"].as_str().unwrap().to_owned();
    let weak = json!({ "node_id": inbox, "password": "123" });
    assert_eq!(
        app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), weak).await.json()["error"]["code"],
        "WEAK_LINK_PASSWORD"
    );
    let file_upload = json!({ "node_id": file_id, "allow_upload": true });
    let refused = app.json(Method::POST, "/api/v1/shares", As::Browser(&cookie), file_upload).await;
    assert_eq!(refused.json()["error"]["code"], "NOT_A_FOLDER");

    // Without the password: nothing, not even the file.
    let view = format!("/api/v1/public/{token}");
    assert_eq!(app.get(&view, As::Anonymous).await.json()["error"]["code"], "PASSWORD_REQUIRED");
    let content = format!("/api/v1/public/{token}/content?node={file_id}");
    assert_eq!(app.get(&content, As::Anonymous).await.status, StatusCode::FORBIDDEN);
    let unlock = format!("/api/v1/public/{token}/unlock");
    let wrong = app.json(Method::POST, &unlock, As::Anonymous, json!({ "password": "guess" })).await;
    assert_eq!(wrong.json()["error"]["code"], "WRONG_LINK_PASSWORD");

    let right = app.json(Method::POST, &unlock, As::Anonymous, json!({ "password": "open sesame" })).await;
    assert_eq!(right.status, StatusCode::NO_CONTENT);
    let set_cookie = right.header(header::SET_COOKIE).to_owned();
    assert!(
        set_cookie.contains(&format!("Path=/api/v1/public/{token}")) && set_cookie.contains("HttpOnly"),
        "{set_cookie}"
    );
    let key = set_cookie.split(';').next().unwrap().to_owned();
    let with_key = [(header::COOKIE, key.as_str())];
    let opened = app.send(Method::GET, &view, As::Anonymous, Body::empty(), &with_key).await.json();
    assert_eq!(opened["children"][0]["name"], "rules.txt");
    assert_eq!(opened["can_upload"], true);
    assert_eq!(app.send(Method::GET, &content, As::Anonymous, Body::empty(), &with_key).await.text(), "be nice");

    // Visitors upload; a taken name gets a number, nothing is overwritten.
    let upload = format!("/api/v1/public/{token}/upload?name=rules.txt");
    let uploaded = app.send(Method::POST, &upload, As::Anonymous, Body::from("mine"), &with_key).await;
    assert_eq!(uploaded.status, StatusCode::CREATED, "{}", uploaded.text());
    assert_eq!(uploaded.json()["name"], "rules (2).txt");
    let locked = app.send(Method::POST, &upload, As::Anonymous, Body::from("x"), &[]).await;
    assert_eq!(locked.json()["error"]["code"], "PASSWORD_REQUIRED");
    let children = app.get(&format!("/api/v1/files/{inbox}/children"), As::Browser(&cookie)).await.json();
    assert_eq!(children.as_array().unwrap().len(), 2);

    // A new password shuts out those who had the old one; uploads can be turned off.
    let share_uri = format!("/api/v1/shares/{}", share["id"].as_str().unwrap());
    let changed = app
        .json(Method::PATCH, &share_uri, As::Browser(&cookie), json!({ "password": "new one", "allow_upload": false }))
        .await;
    assert_eq!(changed.json()["allow_upload"], false);
    let shut = app.send(Method::GET, &view, As::Anonymous, Body::empty(), &with_key).await;
    assert_eq!(shut.json()["error"]["code"], "PASSWORD_REQUIRED");
    let open = app.json(Method::PATCH, &share_uri, As::Browser(&cookie), json!({ "password": "" })).await.json();
    assert_eq!(open["has_password"], false);
    let view_now = app.get(&view, As::Anonymous).await.json();
    assert_eq!(view_now["can_upload"], false);
    let no_upload = app.send(Method::POST, &upload, As::Anonymous, Body::from("x"), &[]).await;
    assert_eq!(no_upload.json()["error"]["code"], "UPLOAD_NOT_ALLOWED");
}

#[tokio::test]
async fn without_the_media_engine_there_are_no_thumbnails() {
    let app = TestApp::new().await;
    let cookie = app.register("alice").await;
    let root = app.root_id(&cookie).await;
    let photo = app.upload(&cookie, &root, "cat.jpg", "not really a jpeg").await;
    assert_eq!(app.get("/api/v1/info", As::Anonymous).await.json()["thumbnails"], false);
    let uri = format!("/api/v1/files/{}/thumbnail?size=small", photo["id"].as_str().unwrap());
    let reply = app.get(&uri, As::Browser(&cookie)).await;
    assert_eq!(reply.json()["error"]["code"], "NO_THUMBNAIL");
    assert_eq!(app.get(&uri, As::Anonymous).await.status, StatusCode::UNAUTHORIZED);
    let wrong_size = format!("{uri}x");
    assert_eq!(app.get(&wrong_size, As::Browser(&cookie)).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn folders_are_shared_with_users_and_groups() {
    let app = TestApp::with_config("[accounts]\nopen_registration = true\n").await;
    let alice = app.register("alice").await; // the first account: an administrator
    let bob = app.register("bob").await;
    let carol = app.register("carol").await;
    let id_of = |reply: Reply| reply.json()["id"].as_str().unwrap().to_owned();
    let user_id = |cookie: String| {
        let app = &app;
        async move { id_of(app.get("/api/v1/auth/me", As::Browser(&cookie)).await) }
    };
    let (alice_id, bob_id, carol_id) =
        (user_id(alice.clone()).await, user_id(bob.clone()).await, user_id(carol.clone()).await);
    let root = app.root_id(&alice).await;
    let mkdir = |parent: String, name: &'static str| {
        let (app, alice) = (&app, alice.clone());
        async move {
            let uri = format!("/api/v1/files/{parent}/folders");
            id_of(app.json(Method::POST, &uri, As::Browser(&alice), json!({ "name": name })).await)
        }
    };
    let trip = mkdir(root.clone(), "Trip").await;
    let day = mkdir(trip.clone(), "Day 1").await;
    let photo = app.upload(&alice, &day, "photo.jpg", "sunset").await;
    let photo = photo["id"].as_str().unwrap().to_owned();

    // Shared for reading: bob sees it, reads it, but changes nothing.
    let access = format!("/api/v1/files/{trip}/access");
    let given = app.json(Method::POST, &access, As::Browser(&alice), json!({ "username": "Bob" })).await;
    assert_eq!(given.status, StatusCode::CREATED, "{}", given.text());
    assert_eq!(given.json()["user"]["username"], "bob");
    let given_id = id_of(given);
    let shared = app.get("/api/v1/files/shared", As::Browser(&bob)).await.json();
    assert_eq!(shared[0]["node"]["name"], "Trip");
    assert_eq!((shared[0]["owner"]["username"].as_str(), shared[0]["write"].as_bool()), (Some("alice"), Some(false)));
    let details = app.get(&format!("/api/v1/files/{day}"), As::Browser(&bob)).await.json();
    let path: Vec<&str> = details["path"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(path, ["Trip"], "alice's folders above the share stay unseen");
    assert_eq!(app.get(&format!("/api/v1/files/{photo}/content"), As::Browser(&bob)).await.text(), "sunset");
    let zip = app.get(&format!("/api/v1/files/{trip}/zip"), As::Browser(&bob)).await;
    assert_eq!(zip.status, StatusCode::OK);
    let upload = format!("/api/v1/files/{day}/upload?name=mine.txt");
    let refused = app.send(Method::POST, &upload, As::Browser(&bob), Body::from("hi"), &[]).await;
    assert_eq!(refused.json()["error"]["code"], "READ_ONLY_SHARE");
    assert_eq!(app.get(&format!("/api/v1/files/{trip}"), As::Browser(&carol)).await.status, StatusCode::NOT_FOUND);
    let not_owner = app.get(&access, As::Browser(&bob)).await;
    assert_eq!(not_owner.status, StatusCode::NOT_FOUND, "only the owner sees whom it is shared with");

    // For changes: bob's writes are alice's files, in her quota.
    let uri = format!("/api/v1/access/{given_id}");
    let changed = app.json(Method::PATCH, &uri, As::Browser(&alice), json!({ "write": true })).await;
    assert_eq!(changed.json()["write"], true);
    let added = app.send(Method::POST, &upload, As::Browser(&bob), Body::from("hello"), &[]).await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text());
    let added_id = id_of(added);
    let alice_files = app.get(&format!("/api/v1/files/{day}/children"), As::Browser(&alice)).await.json();
    assert!(alice_files.as_array().unwrap().iter().any(|n| n["name"] == "mine.txt"));
    assert_eq!(app.get("/api/v1/usage", As::Browser(&bob)).await.json()["used_bytes"], 0);
    assert_eq!(app.get("/api/v1/usage", As::Browser(&alice)).await.json()["used_bytes"], 11);
    let rename = format!("/api/v1/files/{added_id}");
    let renamed = app.json(Method::PATCH, &rename, As::Browser(&bob), json!({ "name": "notes.txt" })).await;
    assert_eq!(renamed.json()["name"], "notes.txt");
    // The shared folder itself is alice's to rename, move or delete.
    let top = format!("/api/v1/files/{trip}");
    let rename_top = app.json(Method::PATCH, &top, As::Browser(&bob), json!({ "name": "Mine" })).await;
    assert_eq!(rename_top.json()["error"]["code"], "SHARED_ROOT");
    let delete_top = app.send(Method::DELETE, &top, As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(delete_top.json()["error"]["code"], "SHARED_ROOT");
    // Bob copies a photo out for himself; his own files don't move into alice's.
    let bob_root = app.root_id(&bob).await;
    let copy = format!("/api/v1/files/{photo}/copy");
    let copied = app.json(Method::POST, &copy, As::Browser(&bob), json!({ "parent_id": bob_root })).await;
    assert_eq!(copied.status, StatusCode::CREATED, "{}", copied.text());
    let own = app.upload(&bob, &bob_root, "own.txt", "x").await;
    let move_in = format!("/api/v1/files/{}", own["id"].as_str().unwrap());
    let moved = app.json(Method::PATCH, &move_in, As::Browser(&bob), json!({ "parent_id": day })).await;
    assert_eq!(moved.json()["error"]["code"], "CROSS_OWNER_MOVE");
    // Deleting goes to alice's trash.
    let deleted = app.send(Method::DELETE, &rename, As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let trash = app.get("/api/v1/files/trash", As::Browser(&alice)).await.json();
    let in_trash = format!("/api/v1/files/{}/children", trash["id"].as_str().unwrap());
    assert_eq!(app.get(&in_trash, As::Browser(&alice)).await.json()[0]["name"], "notes.txt");

    // Groups: the administrator keeps them; alice shares with hers.
    let group = app.json(Method::POST, "/api/v1/admin/groups", As::Browser(&alice), json!({ "name": "Family" })).await;
    let group_id = id_of(group);
    for member in [&alice_id, &carol_id] {
        let uri = format!("/api/v1/admin/groups/{group_id}/members/{member}");
        assert_eq!(
            app.send(Method::PUT, &uri, As::Browser(&alice), Body::empty(), &[]).await.status,
            StatusCode::NO_CONTENT
        );
    }
    let not_admin = app.json(Method::POST, "/api/v1/admin/groups", As::Browser(&bob), json!({ "name": "X" })).await;
    assert_eq!(not_admin.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get("/api/v1/groups", As::Browser(&carol)).await.json()[0]["name"], "Family");
    assert_eq!(app.get("/api/v1/groups", As::Browser(&bob)).await.json(), json!([]));
    let by_group = app.json(Method::POST, &access, As::Browser(&alice), json!({ "group_id": group_id })).await;
    assert_eq!(by_group.json()["group"]["name"], "Family");
    let carols = app.get("/api/v1/files/shared", As::Browser(&carol)).await.json();
    assert_eq!(carols[0]["group"]["name"], "Family");
    let not_in_group = app
        .json(Method::POST, &format!("/api/v1/files/{day}/access"), As::Browser(&bob), json!({ "group_id": group_id }))
        .await;
    assert_eq!(not_in_group.status, StatusCode::NOT_FOUND, "bob shares nothing of alice's");
    let uri = format!("/api/v1/admin/groups/{group_id}/members/{carol_id}");
    app.send(Method::DELETE, &uri, As::Browser(&alice), Body::empty(), &[]).await;
    assert_eq!(app.get(&format!("/api/v1/files/{trip}"), As::Browser(&carol)).await.status, StatusCode::NOT_FOUND);

    // A name nobody has is listed like a person; one's own name is refused.
    let nobody = app.json(Method::POST, &access, As::Browser(&alice), json!({ "username": "nobody" })).await;
    assert_eq!(nobody.status, StatusCode::CREATED);
    let listed = app.get(&access, As::Browser(&alice)).await.json();
    assert_eq!(listed.as_array().unwrap().len(), 3);
    let own_name = app.json(Method::POST, &access, As::Browser(&alice), json!({ "username": "alice" })).await;
    assert_eq!(own_name.json()["error"]["code"], "SHARE_WITH_SELF");
    let root_share = app
        .json(Method::POST, &format!("/api/v1/files/{root}/access"), As::Browser(&alice), json!({ "username": "bob" }))
        .await;
    assert_eq!(root_share.json()["error"]["code"], "ROOT_FOLDER");

    // Bob gives it up.
    let left =
        app.send(Method::DELETE, &format!("/api/v1/access/{given_id}"), As::Browser(&bob), Body::empty(), &[]).await;
    assert_eq!(left.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/api/v1/files/shared", As::Browser(&bob)).await.json(), json!([]));
    assert_eq!(
        app.get(&format!("/api/v1/files/{photo}/content"), As::Browser(&bob)).await.status,
        StatusCode::NOT_FOUND
    );
    let _ = bob_id;
}

#[tokio::test]
async fn the_text_editor_edits_text_files_it_was_opened_with() {
    let app = TestApp::with_wasm_apps(|apps| install_shipped(apps, "editor")).await;
    let alice = app.register("alice").await;
    let root = app.root_id(&alice).await;
    let file = app.upload(&alice, &root, "notes.env", "A=1\r\nB=2\r\n").await;
    let file_id = file["id"].as_str().unwrap().to_owned();
    let api = format!("/app/editor/api/file?id={file_id}");

    // Its manifest offers it for text by type and by extension; no side panel entry.
    let apps = app.get("/api/v1/apps", As::Browser(&alice)).await.json();
    let editor = apps["apps"].as_array().unwrap().iter().find(|a| a["id"] == "editor").unwrap().clone();
    assert!(editor["ui"].is_null(), "{editor}");
    assert!(editor["open_with"][0]["extensions"].as_array().unwrap().iter().any(|e| e == "env"));
    let page = app.get("/app/editor/edit", As::Browser(&alice)).await;
    assert!(page.text().contains("highlight.js"));

    // Only the file the user opened with it.
    assert_eq!(app.get(&api, As::Browser(&alice)).await.status, StatusCode::NOT_FOUND);
    let grant = json!({ "app_id": "editor", "node_id": file_id, "write": true, "kind": "session" });
    let granted = app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    assert_eq!(granted.status, StatusCode::CREATED, "{}", granted.text());
    let opened = app.get(&api, As::Browser(&alice)).await.json();
    assert_eq!((opened["name"].as_str(), opened["content"].as_str()), (Some("notes.env"), Some("A=1\r\nB=2\r\n")));
    let etag = opened["etag"].as_str().unwrap().to_owned();

    let save = |content: &str, etag: &str| json!({ "content": content, "etag": etag });
    let saved = app.json(Method::PUT, &api, As::Browser(&alice), save("A=3\r\n", &etag)).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    let content = format!("/api/v1/files/{file_id}/content");
    assert_eq!(app.get(&content, As::Browser(&alice)).await.text(), "A=3\r\n");
    let stale = app.json(Method::PUT, &api, As::Browser(&alice), save("lost", &etag)).await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "saved over someone else's version");

    // Not text: never opened, so never saved back broken.
    let binary = format!("/api/v1/files/{root}/upload?name=blob.txt");
    let blob = app.send(Method::POST, &binary, As::Browser(&alice), Body::from(vec![0xff, 0xfe, 0x00]), &[]).await;
    let blob_id = blob.json()["id"].as_str().unwrap().to_owned();
    let grant = json!({ "app_id": "editor", "node_id": blob_id, "write": true, "kind": "session" });
    app.json(Method::POST, "/api/v1/grants", As::Browser(&alice), grant).await;
    let refused = app.get(&format!("/app/editor/api/file?id={blob_id}"), As::Browser(&alice)).await;
    assert_eq!(refused.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let preview =
        app.send(Method::POST, "/app/editor/api/preview", As::Browser(&alice), Body::from("# Hi <script>"), &[]).await;
    assert!(preview.text().contains("<h1>Hi &lt;script&gt;</h1>"), "{}", preview.text());
}
