//! Serving HTTP: connections with time limits.
//!
//! `axum::serve` sets no timers, so a client could hold a connection forever
//! by sending its headers byte by byte, and a body could stall for good.
//! Here every connection must send its request headers within
//! [`HEADER_TIMEOUT`], a request body may pause at most [`BODY_IDLE`], and
//! at most [`MAX_CONNECTIONS`] connections are open at once.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::Router;
use futures_util::StreamExt;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Time for the headers of a request, also while an idle connection waits
/// for its next request.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request body may pause before the request fails.
pub const BODY_IDLE: Duration = Duration::from_secs(60);
/// Connections open at once; more are closed right away.
pub const MAX_CONNECTIONS: usize = 4096;
/// How long open requests may still run once the server stops.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// Serves `router` on `listener` until `shutdown` resolves, then lets the
/// requests under way finish (for [`SHUTDOWN_GRACE`] at most). Handlers get
/// the peer address as `ConnectInfo<SocketAddr>`.
pub async fn serve(listener: TcpListener, router: Router, shutdown: impl Future<Output = ()>) {
    let places = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let mut connections = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    // Out of file descriptors and the like: wait a moment.
                    tracing::warn!("could not accept a connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        while connections.try_join_next().is_some() {}
        let Ok(place) = places.clone().try_acquire_owned() else {
            tracing::warn!("too many connections: one from {peer} is closed");
            continue;
        };
        let _ = stream.set_nodelay(true);
        let router = router.clone();
        let mut stopping = stopping.clone();
        connections.spawn(async move {
            let _place = place;
            let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                let mut request = request.map(idle_limited);
                request.extensions_mut().insert(ConnectInfo(peer));
                router.clone().oneshot(request)
            });
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            tokio::pin!(connection);
            let result = tokio::select! {
                result = connection.as_mut() => result,
                _ = stopping.changed() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(err) = result {
                tracing::debug!("connection from {peer}: {err}");
            }
        });
    }
    let _ = stop.send(true);
    let all_done = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(SHUTDOWN_GRACE, all_done).await.is_err() {
        tracing::warn!("some requests were still running when the server stopped");
    }
}

/// A request body that fails once no data has come for [`BODY_IDLE`].
fn idle_limited(body: hyper::body::Incoming) -> Body {
    let chunks = Body::new(body).into_data_stream();
    let stream = futures_util::stream::unfold(Some(chunks), |chunks| async move {
        let mut chunks = chunks?;
        match tokio::time::timeout(BODY_IDLE, chunks.next()).await {
            Ok(Some(chunk)) => Some((chunk.map_err(std::io::Error::other), Some(chunks))),
            Ok(None) => None,
            // One error, then the body ends.
            Err(_) => Some((Err(std::io::Error::other("the request body stopped coming")), None)),
        }
    });
    Body::from_stream(stream)
}
