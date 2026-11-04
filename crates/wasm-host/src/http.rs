//! The `http` import: requests to the addresses the app's manifest names in
//! `permissions.network`, and nowhere else — redirects included.

use std::io;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, COOKIE, HOST, LOCATION};
use reqwest::{Client, Method, StatusCode};
use rustcloud_core::ByteStream;
use rustcloud_plugin_api::NetworkRule;
use tokio::time::{timeout_at, Instant};
use url::Url;

use crate::bindings::rustcloud::app::http::{Error, Request, Response};

/// The largest answer an app gets, as when it reads a file.
/// Redirects followed for one request.
const MAX_REDIRECTS: usize = 5;
/// Longest a download may wait for an answer or for more data.
const IDLE_LIMIT: Duration = Duration::from_secs(60);

/// The client all apps share. It never follows redirects itself: each one
/// is checked against the app's addresses first.
pub(crate) fn client() -> reqwest::Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("RustCloud/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Sends `request` if its address is among `rules`, giving up after
/// `time_left` or on an answer larger than `limit` bytes.
pub(crate) async fn fetch(
    client: &Client,
    rules: &[NetworkRule],
    request: Request,
    time_left: Duration,
    limit: u64,
) -> Result<Response, Error> {
    let deadline = Instant::now() + time_left;
    let url = Url::parse(&request.url).map_err(|_| Error::InvalidRequest)?;
    let method = Method::from_bytes(request.method.as_bytes()).map_err(|_| Error::InvalidRequest)?;
    let mut headers = HeaderMap::new();
    for (name, value) in request.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::InvalidRequest)?;
        let value = HeaderValue::from_str(&value).map_err(|_| Error::InvalidRequest)?;
        if name == HOST {
            return Err(Error::InvalidRequest);
        }
        headers.append(name, value);
    }
    let response = send(client, rules, method, url, headers, Bytes::from(request.body), deadline).await?;
    read(response, deadline, limit).await
}

/// Downloads `url` if its address is among `rules`: its size, if known, and
/// its content. The content stops with an error if no data comes for
/// [`IDLE_LIMIT`]; a large file may take as long as it needs otherwise.
pub(crate) async fn download(
    client: &Client,
    rules: &[NetworkRule],
    url: &str,
) -> Result<(Option<u64>, ByteStream), String> {
    let url = Url::parse(url).map_err(|err| format!("{url}: {err}"))?;
    let deadline = Instant::now() + IDLE_LIMIT;
    let response = send(client, rules, Method::GET, url, HeaderMap::new(), Bytes::new(), deadline).await.map_err(
        |err| match err {
            Error::NotAllowed => "the address is not among the plugin's network addresses".to_owned(),
            Error::Timeout => "no answer".to_owned(),
            Error::Failed(message) => message,
            other => format!("{other:?}"),
        },
    )?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    let size = response.content_length();
    let chunks = futures_util::stream::unfold(Some(response), |response| async move {
        let mut response = response?;
        match tokio::time::timeout(IDLE_LIMIT, response.chunk()).await {
            Ok(Ok(Some(chunk))) => Some((Ok(chunk), Some(response))),
            Ok(Ok(None)) => None,
            Ok(Err(err)) => Some((Err(io::Error::other(err)), None)),
            Err(_) => Some((Err(io::Error::new(io::ErrorKind::TimedOut, "the download stalled")), None)),
        }
    });
    Ok((size, Box::pin(chunks)))
}

/// Sends a request, following redirects as long as they stay among `rules`.
async fn send(
    client: &Client,
    rules: &[NetworkRule],
    mut method: Method,
    mut url: Url,
    mut headers: HeaderMap,
    mut body: Bytes,
    deadline: Instant,
) -> Result<reqwest::Response, Error> {
    for _ in 0..=MAX_REDIRECTS {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::InvalidRequest);
        }
        if !allowed(rules, &url) {
            return Err(Error::NotAllowed);
        }
        let send = client.request(method.clone(), url.clone()).headers(headers.clone()).body(body.clone()).send();
        let response = timeout_at(deadline, send).await.map_err(|_| Error::Timeout)?.map_err(failed)?;
        let status = response.status();
        let next = response.headers().get(LOCATION).and_then(|l| l.to_str().ok()).and_then(|l| url.join(l).ok());
        let Some(next) = next.filter(|_| status.is_redirection() && status != StatusCode::NOT_MODIFIED) else {
            return Ok(response);
        };
        // As browsers do: 303, and 301/302 after a POST, go on with a GET.
        if status == StatusCode::SEE_OTHER
            || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND) && method == Method::POST)
        {
            method = Method::GET;
            body = Bytes::new();
        }
        // Credentials stay with the host they were meant for.
        if next.host_str() != url.host_str() {
            headers.remove(AUTHORIZATION);
            headers.remove(COOKIE);
        }
        url = next;
    }
    Err(Error::Failed(format!("more than {MAX_REDIRECTS} redirects")))
}

/// Whether `url` is one of the addresses in `rules`.
fn allowed(rules: &[NetworkRule], url: &Url) -> bool {
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return false;
    };
    rules.iter().any(|rule| rule.allows(url.scheme(), host, port))
}

/// The answer for the app, as long as it isn't larger than `limit`.
async fn read(mut response: reqwest::Response, deadline: Instant, limit: u64) -> Result<Response, Error> {
    if response.content_length().is_some_and(|length| length > limit) {
        return Err(Error::TooLarge);
    }
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let mut body = Vec::new();
    while let Some(chunk) = timeout_at(deadline, response.chunk()).await.map_err(|_| Error::Timeout)?.map_err(failed)? {
        if (body.len() + chunk.len()) as u64 > limit {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Response { status, headers, body })
}

/// The WIT error for a failed request, with the causes ("connection refused").
fn failed(err: reqwest::Error) -> Error {
    if err.is_timeout() {
        return Error::Timeout;
    }
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    Error::Failed(message)
}
