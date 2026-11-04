//! What the server knows about the client of a request.

use std::convert::Infallible;
use std::net::IpAddr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// The client of a request, as worked out by the server's network layer
/// (which trusts `X-Forwarded-For` only from configured reverse proxies).
///
/// Handlers take it as an argument. Without the network layer (in tests that
/// call a module's router directly) it is [`ClientInfo::default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientInfo {
    /// The client's IP address, if known.
    pub ip: Option<IpAddr>,
    /// Whether the client reaches the server over HTTPS. Decides the `Secure`
    /// flag of cookies.
    pub https: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<ClientInfo>().copied().unwrap_or_default())
    }
}
