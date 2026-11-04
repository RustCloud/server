//! `rustcloud` — runs the server.

use std::path::PathBuf;

use clap::Parser;
use rustcloud_server::{app, journal, listen, AppState, Config, StartupResult};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// RustCloud: a small, fast and secure self-hosted cloud.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// TOML configuration file; it may not exist yet (the admin page creates it
    /// when settings are saved). Settings can also come from RUSTCLOUD_* environment variables.
    #[arg(short, long, env = "RUSTCLOUD_CONFIG", default_value = "rustcloud.toml")]
    config: PathBuf,
}

fn main() -> StartupResult<()> {
    // `rustcloud sandbox …` confines this fresh process and becomes a tool
    // (ffmpeg) for the media engine; `rustcloud onnx-worker …` confines it and
    // runs a neural network for use.onnx. Both before any thread starts.
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        Some(first) if first == "sandbox" => rustcloud_sandbox::main(args.collect()),
        Some(first) if first == "onnx-worker" => rustcloud_ml::worker::main(args.collect()),
        _ => {}
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve())
}

async fn serve() -> StartupResult<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;

    // Console logs follow the log level; the journal layer picks its own
    // events (audit, warnings, errors) whatever the level.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.startup.log));
    let (journal_layer, journal_queue) = journal::channel();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .with(journal_layer)
        .init();

    let state = AppState::open(&config, journal_queue).await?;
    create_first_admin(&state).await?;
    rustcloud_core::audit!("server.started", version = env!("CARGO_PKG_VERSION"), "server started");
    let listener = tokio::net::TcpListener::bind(config.startup.listen).await?;
    let public_url = state.server.read(|s| s.public_url.clone());
    let public_url = if public_url.is_empty() { "public URL not set".to_owned() } else { public_url };
    tracing::info!(
        "RustCloud {} listening on {} ({public_url}), config file {}",
        env!("CARGO_PKG_VERSION"),
        config.startup.listen,
        config.file.display()
    );

    // Each request gets the peer address (see `network::guard`).
    listen::serve(listener, app(state), shutdown_signal()).await;
    tracing::info!("stopped");
    Ok(())
}

/// Creates the administrator from `RUSTCLOUD_ADMIN_USERNAME` and
/// `RUSTCLOUD_ADMIN_PASSWORD` when the server has no accounts yet. Without
/// them the first account is made on the setup page.
async fn create_first_admin(state: &AppState) -> StartupResult<()> {
    let (Ok(username), Ok(password)) =
        (std::env::var("RUSTCLOUD_ADMIN_USERNAME"), std::env::var("RUSTCLOUD_ADMIN_PASSWORD"))
    else {
        return Ok(());
    };
    let account = rustcloud_auth::NewAccount { username, password, display_name: None, email: None };
    match state.auth.create_first_admin(account).await? {
        Some(admin) => tracing::info!(
            "administrator {} created from RUSTCLOUD_ADMIN_USERNAME; RUSTCLOUD_ADMIN_PASSWORD can be removed now",
            admin.username
        ),
        None => tracing::debug!("accounts exist, RUSTCLOUD_ADMIN_* are not used"),
    }
    Ok(())
}

/// Resolves on Ctrl+C or SIGTERM (what `docker stop` sends), letting
/// in-flight requests finish.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
