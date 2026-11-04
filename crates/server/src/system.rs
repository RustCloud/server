//! Administration → Server: what the administrator needs to know about the
//! machine and the process: version, system, processor, memory, database.
//! Read from `/proc` and `/sys` on Linux; elsewhere those parts are `null`.

use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use rustcloud_auth::AdminUser;
use rustcloud_db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Serialize;

use crate::AppState;

/// When the server started, as near as it can tell: the first use of this module.
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn router() -> Router<AppState> {
    LazyLock::force(&STARTED);
    Router::new().route("/api/v1/admin/system", get(system))
}

#[derive(Serialize)]
struct SystemInfo {
    version: &'static str,
    /// The commit the server was built from, if the build said (`RUSTCLOUD_REVISION`).
    revision: Option<&'static str>,
    uptime_seconds: u64,
    /// E.g. "Alpine Linux v3.22".
    os: Option<String>,
    kernel: Option<String>,
    arch: &'static str,
    /// Whether the server runs in a container (Docker, Podman).
    container: bool,
    /// Processors the server may use.
    cpus: usize,
    /// Average number of processes running or waiting over 1, 5 and 15 minutes.
    load: Option<[f64; 3]>,
    memory: Option<Memory>,
    /// The container's memory limit and use, if it has a limit (cgroup v2).
    container_memory: Option<ContainerMemory>,
    process: Process,
    database: Database,
}

#[derive(Serialize, PartialEq, Debug)]
struct Memory {
    total_bytes: u64,
    available_bytes: u64,
    swap_total_bytes: u64,
    swap_free_bytes: u64,
}

#[derive(Serialize)]
struct ContainerMemory {
    limit_bytes: u64,
    used_bytes: u64,
}

#[derive(Serialize)]
struct Process {
    /// Memory the server process holds (resident set).
    memory_bytes: Option<u64>,
    threads: Option<u64>,
}

#[derive(Serialize)]
struct Database {
    /// `sqlite` or `postgres`.
    kind: &'static str,
    version: Option<String>,
    size_bytes: Option<u64>,
}

/// `GET /api/v1/admin/system`: the state of the server's machine and process.
async fn system(State(db): State<DatabaseConnection>, _admin: AdminUser) -> Json<SystemInfo> {
    let database = database(&db).await;
    let (process_memory, threads) = read("/proc/self/status").map(|text| process_status(&text)).unwrap_or_default();
    Json(SystemInfo {
        version: env!("CARGO_PKG_VERSION"),
        revision: option_env!("RUSTCLOUD_REVISION").filter(|r| !r.is_empty()),
        uptime_seconds: STARTED.elapsed().as_secs(),
        os: read("/etc/os-release").and_then(|text| os_name(&text)),
        kernel: read("/proc/sys/kernel/osrelease").map(|text| text.trim().to_owned()),
        arch: std::env::consts::ARCH,
        container: std::path::Path::new("/.dockerenv").exists() || std::path::Path::new("/run/.containerenv").exists(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
        load: read("/proc/loadavg").and_then(|text| load(&text)),
        memory: read("/proc/meminfo").and_then(|text| meminfo(&text)),
        container_memory: container_memory(),
        process: Process { memory_bytes: process_memory, threads },
        database,
    })
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `PRETTY_NAME` of `/etc/os-release`.
fn os_name(text: &str) -> Option<String> {
    let value = text.lines().find_map(|line| line.strip_prefix("PRETTY_NAME="))?;
    Some(value.trim().trim_matches('"').to_owned()).filter(|name| !name.is_empty())
}

/// The three averages of `/proc/loadavg`.
fn load(text: &str) -> Option<[f64; 3]> {
    let mut numbers = text.split_whitespace().map(|n| n.parse().ok());
    Some([numbers.next()??, numbers.next()??, numbers.next()??])
}

/// Total and available memory and swap from `/proc/meminfo` (in kB there).
fn meminfo(text: &str) -> Option<Memory> {
    let field = |name: &str| kilobytes(text, name);
    Some(Memory {
        total_bytes: field("MemTotal:")?,
        available_bytes: field("MemAvailable:")?,
        swap_total_bytes: field("SwapTotal:").unwrap_or(0),
        swap_free_bytes: field("SwapFree:").unwrap_or(0),
    })
}

/// The value of a `Name:   123 kB` line, in bytes.
fn kilobytes(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find_map(|line| line.strip_prefix(name))?;
    let kb: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// Resident memory and threads from `/proc/self/status`.
fn process_status(text: &str) -> (Option<u64>, Option<u64>) {
    let threads = text.lines().find_map(|line| line.strip_prefix("Threads:")).and_then(|n| n.trim().parse().ok());
    (kilobytes(text, "VmRSS:"), threads)
}

/// The memory limit of the server's cgroup (v2), when it has one.
fn container_memory() -> Option<ContainerMemory> {
    let limit = read("/sys/fs/cgroup/memory.max")?.trim().parse().ok()?;
    let used = read("/sys/fs/cgroup/memory.current")?.trim().parse().ok()?;
    Some(ContainerMemory { limit_bytes: limit, used_bytes: used })
}

async fn database(db: &DatabaseConnection) -> Database {
    let backend = db.get_database_backend();
    let (kind, version, size) = match backend {
        DbBackend::Postgres => {
            ("postgres", "SHOW server_version", "SELECT pg_database_size(current_database())::bigint")
        }
        _ => (
            "sqlite",
            "SELECT sqlite_version()",
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        ),
    };
    let one = |sql: &str| db.query_one_raw(Statement::from_string(backend, sql.to_owned()));
    let version = one(version).await.ok().flatten().and_then(|row| row.try_get_by_index::<String>(0).ok());
    let size = one(size).await.ok().flatten().and_then(|row| row.try_get_by_index::<i64>(0).ok());
    Database { kind, version, size_bytes: size.and_then(|bytes| u64::try_from(bytes).ok()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files_are_read() {
        let os = "NAME=\"Alpine Linux\"\nPRETTY_NAME=\"Alpine Linux v3.22\"\n";
        assert_eq!(os_name(os).as_deref(), Some("Alpine Linux v3.22"));
        assert_eq!(load("0.52 0.41 0.30 2/345 6789\n"), Some([0.52, 0.41, 0.30]));
        assert_eq!(load("garbage"), None);
        let meminfo_text = "MemTotal:        8000000 kB\nMemFree:  100 kB\nMemAvailable:    5000000 kB\n\
                            SwapTotal:  0 kB\nSwapFree: 0 kB\n";
        assert_eq!(
            meminfo(meminfo_text),
            Some(Memory {
                total_bytes: 8_000_000 * 1024,
                available_bytes: 5_000_000 * 1024,
                swap_total_bytes: 0,
                swap_free_bytes: 0
            })
        );
        let status = "Name:\trustcloud\nVmRSS:\t   51200 kB\nThreads:\t17\n";
        assert_eq!(process_status(status), (Some(51200 * 1024), Some(17)));
    }

    #[tokio::test]
    async fn the_database_tells_its_version_and_size() {
        let db = rustcloud_db::testing::test_db().await;
        let info = database(&db.conn).await;
        let postgres = db.conn.get_database_backend() == DbBackend::Postgres;
        assert_eq!(info.kind, if postgres { "postgres" } else { "sqlite" });
        assert!(info.version.is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())));
        assert!(info.size_bytes.is_some_and(|size| size > 0));
    }
}
