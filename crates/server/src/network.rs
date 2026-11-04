//! The `[server]` settings section and what it controls on every request:
//! the addresses the server answers on, client addresses behind reverse
//! proxies, and which other web sites may call the API (CORS).

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::uri::Authority;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use rustcloud_core::{ClientInfo, Error, Field, Live, Result, Settings};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::Instrument;
use url::Url;

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// Addresses, reverse proxies and CORS.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    /// The address users open, e.g. `https://cloud.example.com`. Public links
    /// are built from it; when empty, from the address the browser is on.
    pub public_url: String,
    /// Other addresses the server is reached at, e.g. a LAN address.
    pub other_urls: Vec<String>,
    /// Refuse requests addressed to any host not listed above (`localhost`
    /// always works). Protects against DNS rebinding.
    pub only_listed_urls: bool,
    /// Other web sites allowed to call the API from a browser, e.g.
    /// `https://app.example.com`. They must log in with a token: cookies are
    /// never sent to them.
    pub cors_origins: Vec<String>,
    /// Reverse proxies, as IP addresses or networks (`10.0.0.0/8`), whose
    /// `X-Forwarded-For` and `X-Forwarded-Proto` headers are trusted.
    pub trusted_proxies: Vec<String>,
}

impl Settings for ServerSettings {
    const SECTION: &'static str = "server";

    fn fields() -> Vec<Field> {
        vec![
            Field::text("public_url"),
            Field::list("other_urls"),
            Field::bool("only_listed_urls"),
            Field::list("cors_origins"),
            Field::list("trusted_proxies"),
        ]
    }

    fn validate(&self) -> Result<()> {
        for url in self.listed_urls() {
            parse_site_url(url)?;
        }
        if self.only_listed_urls && self.listed_urls().next().is_none() {
            return Err(invalid("Answering only on listed addresses needs at least one address".into()));
        }
        for origin in &self.cors_origins {
            let url = parse_site_url(origin)?;
            if url.origin().ascii_serialization() != *origin {
                let expected = url.origin().ascii_serialization();
                return Err(invalid(format!("CORS origin `{origin}` must be written as `{expected}`")));
            }
        }
        for proxy in &self.trusted_proxies {
            parse_network(proxy)?;
        }
        Ok(())
    }
}

/// An `http(s)://host[:port]` address without path, query or credentials.
fn parse_site_url(text: &str) -> Result<Url> {
    let url = Url::parse(text).map_err(|err| invalid(format!("`{text}` is not a URL: {err}")))?;
    let plain = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.username().is_empty();
    if !plain {
        return Err(invalid(format!("`{text}` must look like https://cloud.example.com")));
    }
    Ok(url)
}

/// An IP address or a network in CIDR notation.
fn parse_network(text: &str) -> Result<IpNet> {
    text.parse::<IpNet>()
        .or_else(|_| text.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| invalid(format!("`{text}` is not an IP address or network")))
}

fn invalid(message: String) -> Error {
    Error::bad_request("INVALID_SETTINGS", message)
}

impl ServerSettings {
    /// The public URL (if set) and the other URLs.
    fn listed_urls(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.public_url).filter(|url| !url.is_empty()).chain(&self.other_urls)
    }

    /// Whether a request addressed to `host` (the `Host` header) is accepted.
    fn accepts_host(&self, host: &Authority) -> bool {
        let name = host.host().trim_start_matches('[').trim_end_matches(']');
        if !self.only_listed_urls || name.eq_ignore_ascii_case("localhost") {
            return true;
        }
        if name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return true;
        }
        self.listed_urls().filter_map(|url| Url::parse(url).ok()).any(|url| {
            let same_name = url
                .host_str()
                .is_some_and(|h| h.trim_start_matches('[').trim_end_matches(']').eq_ignore_ascii_case(name));
            let same_port = match host.port_u16() {
                Some(port) => url.port_or_known_default() == Some(port),
                None => url.port().is_none(),
            };
            same_name && same_port
        })
    }

    fn is_trusted_proxy(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().filter_map(|proxy| parse_network(proxy).ok()).any(|net| net.contains(&ip))
    }

    /// Works out the client of a request that came from `peer`.
    ///
    /// Forwarded headers count only when `peer` is a trusted proxy. The client
    /// IP is the right-most `X-Forwarded-For` entry that is not a trusted proxy
    /// itself (entries further left can be forged by the client).
    pub fn client_info(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> ClientInfo {
        let via_proxy = peer.is_some_and(|ip| self.is_trusted_proxy(ip));
        let public_https = self.public_url.starts_with("https://");
        if !via_proxy {
            return ClientInfo { ip: peer, https: public_https };
        }

        let mut ip = peer;
        let forwarded = headers.get_all(X_FORWARDED_FOR).iter().filter_map(|value| value.to_str().ok());
        let hops: Vec<&str> = forwarded.flat_map(|value| value.split(',')).map(str::trim).collect();
        for hop in hops.iter().rev() {
            let Ok(hop) = hop.parse::<IpAddr>() else { break };
            ip = Some(hop);
            if !self.is_trusted_proxy(hop) {
                break;
            }
        }
        let https = match headers.get(X_FORWARDED_PROTO).and_then(|value| value.to_str().ok()) {
            Some(proto) => proto.eq_ignore_ascii_case("https"),
            None => public_https,
        };
        ClientInfo { ip, https }
    }
}

/// Addresses the services of plugins (`[[proxy]]`) call this server by,
/// e.g. `172.19.0.3:8080` inside a Docker network: not listed in the
/// settings, but accepted from the service's own IP address for the routes
/// of its plugin. Found by `ServiceProxy::self_url`.
#[derive(Debug, Default)]
pub struct InnerHosts(RwLock<Vec<InnerHost>>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct InnerHost {
    app_id: String,
    /// The service's IP address.
    service: IpAddr,
    /// `172.19.0.3:8080`.
    host: String,
}

impl InnerHosts {
    /// The service of `app_id` at `service` calls this server as `host`.
    pub fn set(&self, app_id: &str, service: IpAddr, host: String) {
        let mut hosts = self.0.write().unwrap_or_else(|e| e.into_inner());
        hosts.retain(|known| known.app_id != app_id);
        hosts.push(InnerHost { app_id: app_id.to_owned(), service, host });
    }

    /// Whether a request from `peer` to `host` for `path` is a service
    /// calling its plugin back.
    fn allow(&self, peer: Option<IpAddr>, host: &Authority, path: &str) -> bool {
        let hosts = self.0.read().unwrap_or_else(|e| e.into_inner());
        hosts.iter().any(|known| {
            Some(known.service) == peer
                && known.host.eq_ignore_ascii_case(host.as_str())
                && path.starts_with(&format!("/app/{}/", known.app_id))
        })
    }
}

/// What [`guard`] works with.
#[derive(Clone)]
pub struct GuardState {
    pub settings: Arc<Live<ServerSettings>>,
    pub inner_hosts: Arc<InnerHosts>,
}

/// Middleware: refuses unlisted hosts (when enabled) and attaches
/// [`ClientInfo`] to the request.
pub async fn guard(State(state): State<GuardState>, mut request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok())
        .or_else(|| request.uri().authority().cloned());
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip());

    let (listed, client) = state.settings.read(|settings| {
        let listed = host.as_ref().is_none_or(|host| settings.accepts_host(host));
        (listed, settings.client_info(peer, request.headers()))
    });
    // A service of a plugin calling it back by the server's inner address.
    let inner = host.as_ref().is_some_and(|host| state.inner_hosts.allow(peer, host, request.uri().path()));
    if !listed && !inner {
        return Error::bad_request("HOST_NOT_ALLOWED", "This address is not in the server settings").into_response();
    }
    request.extensions_mut().insert(client);

    // Events during the request (activity log entries, errors) get the client
    // address from this span, and the user once `CurrentUser` records it.
    let ip = client.ip.map(|ip| ip.to_string()).unwrap_or_default();
    let span = tracing::info_span!("client", ip, user = tracing::field::Empty);
    next.run(request).instrument(span).await
}

/// CORS for the origins in the settings, read on every request. Credentials
/// (cookies) are never allowed cross-origin; the `X-Requested-With` header
/// isn't either, so cookie sessions stay protected against CSRF.
///
/// Links of apps' pages (`/api/v1/app-links/…`) are open to any origin: the
/// pages run in sandboxed frames of their own origin, and a link needs no
/// cookie — its signed token says whose it is.
pub fn cors(settings: Arc<Live<ServerSettings>>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, request| {
            request.uri.path().starts_with("/api/v1/app-links/")
                || settings.read(|s| s.cors_origins.iter().any(|allowed| allowed.as_bytes() == origin.as_bytes()))
        }))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::RANGE, header::IF_NONE_MATCH])
        .expose_headers([header::CONTENT_RANGE, header::CONTENT_DISPOSITION, header::ETAG])
        .max_age(Duration::from_secs(3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ServerSettings {
        ServerSettings {
            public_url: "https://cloud.example.com".into(),
            other_urls: vec!["http://192.168.1.10:8080".into()],
            only_listed_urls: true,
            trusted_proxies: vec!["10.0.0.0/8".into()],
            ..Default::default()
        }
    }

    fn accepts(settings: &ServerSettings, host: &str) -> bool {
        settings.accepts_host(&host.parse().expect("valid host"))
    }

    #[test]
    fn only_listed_hosts_are_accepted_when_enabled() {
        let s = settings();
        assert!(accepts(&s, "cloud.example.com"));
        assert!(accepts(&s, "CLOUD.example.com:443"));
        assert!(accepts(&s, "192.168.1.10:8080"));
        assert!(accepts(&s, "localhost:8080"), "health checks and local access");
        assert!(accepts(&s, "127.0.0.1:8080"));
        assert!(accepts(&s, "[::1]:8080"));
        assert!(!accepts(&s, "evil.example.com"));
        assert!(!accepts(&s, "192.168.1.10:9999"), "wrong port");
        assert!(!accepts(&s, "cloud.example.com:8443"));

        let open = ServerSettings { only_listed_urls: false, ..settings() };
        assert!(accepts(&open, "evil.example.com"), "off by default");
    }

    #[test]
    fn a_service_calls_its_plugin_back_by_its_inner_address() {
        let hosts = InnerHosts::default();
        let service = Some("172.19.0.5".parse().unwrap());
        hosts.set("office", "172.19.0.5".parse().unwrap(), "172.19.0.3:8080".into());
        let allow = |peer, host: &str, path| hosts.allow(peer, &host.parse().unwrap(), path);
        assert!(allow(service, "172.19.0.3:8080", "/app/office/wopi/files/1"));
        assert!(!allow(service, "172.19.0.3:8080", "/api/v1/files"), "only its plugin's routes");
        assert!(!allow(service, "172.19.0.3:8080", "/app/other/x"));
        assert!(
            !allow(Some("172.19.0.6".parse().unwrap()), "172.19.0.3:8080", "/app/office/x"),
            "from the service only"
        );
        assert!(!allow(service, "evil.example.com", "/app/office/x"));
        hosts.set("office", "172.19.0.9".parse().unwrap(), "172.19.0.3:8080".into());
        assert!(!allow(service, "172.19.0.3:8080", "/app/office/x"), "the service moved");
    }

    #[test]
    fn forwarded_headers_count_only_from_trusted_proxies() {
        let s = settings();
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, "6.6.6.6, 203.0.113.7, 10.0.0.2".parse().unwrap());
        headers.insert(X_FORWARDED_PROTO, "http".parse().unwrap());

        let proxy = Some("10.0.0.1".parse().unwrap());
        let client = s.client_info(proxy, &headers);
        assert_eq!(client.ip, Some("203.0.113.7".parse().unwrap()), "right-most untrusted hop; 6.6.6.6 is forged");
        assert!(!client.https, "the proxy says the client used plain HTTP");

        let stranger = Some("198.51.100.1".parse().unwrap());
        let client = s.client_info(stranger, &headers);
        assert_eq!(client.ip, stranger, "headers from anyone else are ignored");
        assert!(client.https, "falls back to the public URL's scheme");
    }

    #[test]
    fn validation_rejects_malformed_values() {
        assert!(settings().validate().is_ok());
        let with = |change: fn(&mut ServerSettings)| {
            let mut s = settings();
            change(&mut s);
            s.validate()
        };
        assert!(with(|s| s.public_url = "cloud.example.com".into()).is_err(), "no scheme");
        assert!(with(|s| s.public_url = "https://cloud.example.com/sub".into()).is_err(), "path");
        assert!(with(|s| s.other_urls = vec!["ftp://x".into()]).is_err());
        assert!(with(|s| s.cors_origins = vec!["https://app.example.com/".into()]).is_err(), "not an origin");
        assert!(with(|s| s.cors_origins = vec!["https://app.example.com".into()]).is_ok());
        assert!(with(|s| s.trusted_proxies = vec!["10.0.0.0/33".into()]).is_err());
        assert!(with(|s| s.trusted_proxies = vec!["fd00::1".into()]).is_ok());
        assert!(ServerSettings::default().validate().is_ok(), "an empty public URL means the browser's address");
        let nothing_listed = ServerSettings { only_listed_urls: true, ..Default::default() };
        assert!(nothing_listed.validate().is_err(), "would lock everyone out");
    }
}
