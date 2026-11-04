//! What administrators see about the file storage: its size, how full it is,
//! and how fast it is.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rustcloud_core::{audit, Capacity, Error, Result};
use rustcloud_db::node;
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use serde::Serialize;

use crate::service::Files;

/// From this share of the disk in use on, the storage counts as nearly full.
pub const NEARLY_FULL_PERCENT: f64 = 80.0;
/// Size of the data the speed test writes and reads.
pub const SPEED_TEST_BYTES: u64 = 64 * 1024 * 1024;

/// The storage at a glance.
#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    /// `local` for a directory on disk; later e.g. `s3`.
    pub kind: &'static str,
    pub location: String,
    /// `None` for storages without a fixed size.
    pub capacity: Option<Capacity>,
    /// Share of the disk in use (by anything, not only RustCloud), 0–100.
    pub used_percent: Option<f64>,
    pub nearly_full: bool,
    /// Size of all files of all users.
    pub stored_bytes: i64,
    pub file_count: i64,
    /// The latest speed test, if one ran since the server started.
    pub speed_test: Option<SpeedTestResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedTestResult {
    pub bytes: u64,
    /// MiB per second.
    pub write_speed: f64,
    pub read_speed: f64,
    pub tested_at: DateTime<Utc>,
}

impl Files {
    pub async fn storage_report(&self) -> Result<StorageReport> {
        let info = self.blobs.describe();
        let capacity = self.blobs.capacity().await?;
        let used_percent = capacity.filter(|c| c.total_bytes > 0).map(|c| {
            let used = c.total_bytes.saturating_sub(c.free_bytes);
            used as f64 * 100.0 / c.total_bytes as f64
        });
        let (stored_bytes, file_count) = self.stored_totals().await?;
        Ok(StorageReport {
            kind: info.kind,
            location: info.location,
            capacity,
            used_percent,
            nearly_full: used_percent.is_some_and(|percent| percent >= NEARLY_FULL_PERCENT),
            stored_bytes,
            file_count,
            speed_test: self.last_speed_test.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone(),
        })
    }

    /// Measures how fast the storage writes and reads (one test at a time).
    pub async fn speed_test(&self) -> Result<SpeedTestResult> {
        let _running = self.speed_test_running.lock().await;
        if let Some(capacity) = self.blobs.capacity().await? {
            if capacity.free_bytes < SPEED_TEST_BYTES * 2 {
                return Err(Error::conflict("NOT_ENOUGH_SPACE", "Not enough free space for a speed test"));
            }
        }
        let timings = self.blobs.speed_test(SPEED_TEST_BYTES).await?;
        let result = SpeedTestResult {
            bytes: timings.bytes,
            write_speed: mib_per_second(timings.bytes, timings.write),
            read_speed: mib_per_second(timings.bytes, timings.read),
            tested_at: Utc::now(),
        };
        audit!("storage.speed_test", write = result.write_speed, read = result.read_speed, "storage speed test, MiB/s");
        *self.last_speed_test.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result.clone());
        Ok(result)
    }

    /// Total size and number of files of all users.
    async fn stored_totals(&self) -> Result<(i64, i64)> {
        // SUM() of a BIGINT is NUMERIC in PostgreSQL, COUNT() may be too; cast both.
        let total = Func::cast_as(
            Func::coalesce([Expr::from(Func::sum(Expr::col(node::Column::Size))), Expr::val(0)]),
            "BIGINT",
        );
        let count = Func::cast_as(Func::count(Expr::col(node::Column::Id)), "BIGINT");
        let totals: Option<(i64, i64)> = node::Entity::find()
            .select_only()
            .column_as(Expr::from(total), "total")
            .column_as(Expr::from(count), "count")
            .filter(node::Column::IsDir.eq(false))
            .into_tuple()
            .one(&self.db)
            .await?;
        Ok(totals.unwrap_or_default())
    }
}

fn mib_per_second(bytes: u64, time: Duration) -> f64 {
    let seconds = time.as_secs_f64().max(1e-6);
    (bytes as f64 / (1024.0 * 1024.0) / seconds * 10.0).round() / 10.0
}
