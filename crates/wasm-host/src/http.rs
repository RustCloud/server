//! The `http` import: requests to the addresses the app's manifest names in
//! `permissions.network`, and nowhere else — redirects included.
//!
//! A host name of the manifest must lead into the internet: its DNS is in the
//! hands of whoever owns the name, and could point at this machine or the LAN
//! (the database, the cloud's metadata service, routers). Such requests go
//! through a client whose resolver drops private addresses, checked when it
//! connects (so DNS can't change its answer in between). An IP address in the
//! manifest, or an address the administrator set, may be private.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
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

/// The clients all apps share. They never follow redirects themselves: each
/// one is checked against the app's addresses first. Proxies of the
/// environment (`HTTP_PROXY`) aren't used: they would resolve the names.
#[derive(Clone)]
pub(crate) struct Clients {
    /// For the internet only: private addresses are dropped when resolving.
    public: Client,
    /// For any address.
    any: Client,
}

impl Clients {
    pub(crate) fn new() -> reqwest::Result<Self> {
        let builder = || {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .user_agent(concat!("RustCloud/", env!("CARGO_PKG_VERSION")))
        };
        Ok(Self { public: builder().dns_resolver(Arc::new(InternetOnly)).build()?, any: builder().build()? })
    }

    /// The client for a request that `rule` allows.
    fn for_rule(&self, rule: &NetworkRule) -> &Client {
        if rule.reaches_private() {
            &self.any
        } else {
            &self.public
        }
    }
}

/// Resolves names to their addresses in the internet; a name with none
/// there fails.
struct InternetOnly;

impl reqwest::dns::Resolve for InternetOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> = found.into_iter().filter(|addr| in_the_internet(addr.ip())).collect();
            if public.is_empty() {
                let message = format!("{host} has no address in the internet (private addresses are not reached)");
                return Err(message.into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(public.into_iter());
            Ok(addrs)
        })
    }
}

/// Whether an address is in the internet: not this machine, a private
/// network, link-local (cloud metadata), shared (CGNAT), multicast,
/// documentation, benchmarking or reserved.
pub(crate) fn in_the_internet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_in_the_internet(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => v4_in_the_internet(v4),
            None => v6_in_the_internet(ip),
        },
    }
}

fn v4_in_the_internet(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn v6_in_the_internet(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // NAT64 (64:ff9b::/96) leads to the IPv4 address in its last 32 bits.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., hi, lo] = segments;
        return v4_in_the_internet(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || ip.to_ipv4().is_some())
}

/// Sends `request` if its address is among `rules`, giving up after
/// `time_left` or on an answer larger than `limit` bytes.
pub(crate) async fn fetch(
    client: &Clients,
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
    client: &Clients,
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
    clients: &Clients,
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
        let Some(rule) = allowing(rules, &url) else {
            return Err(Error::NotAllowed);
        };
        let send = clients
            .for_rule(rule)
            .request(method.clone(), url.clone())
            .headers(headers.clone())
            .body(body.clone())
            .send();
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

/// The rule of `rules` that allows `url`, if one does. One that may reach
/// private addresses comes first.
fn allowing<'a>(rules: &'a [NetworkRule], url: &Url) -> Option<&'a NetworkRule> {
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return None;
    };
    let mut matching = rules.iter().filter(|rule| rule.allows(url.scheme(), host, port));
    let first = matching.next()?;
    Some(std::iter::once(first).chain(matching).find(|rule| rule.reaches_private()).unwrap_or(first))
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

#[cfg(test)]
mod address_tests {
    use super::*;

    #[test]
    fn only_internet_addresses_count_as_the_internet() {
        let check = |ip: &str| in_the_internet(ip.parse().unwrap());
        for ip in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(check(ip), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "64:ff9b::a9fe:a9fe",
            "2001:db8::1",
        ] {
            assert!(!check(ip), "{ip}");
        }
    }
}
