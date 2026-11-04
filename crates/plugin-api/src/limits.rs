//! What an app may use, as multiples of one base: the server's administrator
//! (and the app's manifest) choose only how much memory, how much time and
//! how many batches at once; everything else follows from them, so no
//! setting can leave an app that fails whatever it does.
//!
//! | ×1 (the base)                         | grows with |
//! |---------------------------------------|------------|
//! | 64 MiB of memory                      | `memory`   |
//! | 16 MiB of data in one call            | `memory`   |
//! | 10 s of the app's own work            | `time`     |
//! | 60 s in all, waiting for the server   | `time`     |
//!
//! Time spent in native tools (`use-ffmpeg`, `use-onnx`) counts in neither: they have
//! limits of their own.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Memory of an instance at ×1.
pub const BASE_MEMORY: u64 = 64 * 1024 * 1024;
/// Most data one call moves into or out of the app at ×1: a file read at
/// once, an HTTP answer, a request body. A quarter of the memory, which the
/// data passes through in a few copies.
pub const BASE_DATA: u64 = BASE_MEMORY / 4;
/// The app's own work in one request at ×1, without waiting for the server.
pub const BASE_TIME: Duration = Duration::from_secs(10);
/// One request in all at ×1, waiting for the server included.
pub const BASE_WALL: Duration = Duration::from_secs(60);

/// The steps `memory` and `time` may take.
pub const SCALES: [u32; 7] = [1, 2, 4, 8, 16, 32, 64];
/// Most batches of jobs one app runs at a time.
pub const MAX_WORKERS: u32 = 16;

/// `[limits]` of a manifest: what the app needs, as multiples of the base.
///
/// ```toml
/// [limits]
/// memory = 4    # 256 MiB, files up to 64 MiB at once
/// time = 2      # 20 s of its own work in a request
/// workers = 2   # batches of jobs at a time
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scale {
    pub memory: u32,
    pub time: u32,
    pub workers: u32,
}

impl Default for Scale {
    fn default() -> Self {
        Self { memory: 1, time: 1, workers: 1 }
    }
}

impl Scale {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether it asks for no more than `other` in anything.
    pub fn within(&self, other: &Scale) -> bool {
        self.memory <= other.memory && self.time <= other.time && self.workers <= other.workers
    }

    /// What is wrong with it, if anything.
    pub fn problem(&self) -> Option<String> {
        if !SCALES.contains(&self.memory) {
            Some(format!("memory must be one of {SCALES:?}"))
        } else if !SCALES.contains(&self.time) {
            Some(format!("time must be one of {SCALES:?}"))
        } else if !(1..=MAX_WORKERS).contains(&self.workers) {
            Some(format!("workers must be from 1 to {MAX_WORKERS}"))
        } else {
            None
        }
    }

    /// What one request of the app gets.
    pub fn limits(&self) -> Limits {
        Limits::scaled(self.memory, self.time)
    }
}

/// What one call of an app may use, worked out from a [`Scale`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of memory.
    pub memory: u64,
    /// Most bytes one call moves at once.
    pub data: u64,
    /// The app's own work.
    pub time: Duration,
    /// All of it, waiting for the server included.
    pub wall: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self::scaled(1, 1)
    }
}

impl Limits {
    pub fn scaled(memory: u32, time: u32) -> Self {
        Self {
            memory: BASE_MEMORY * u64::from(memory),
            data: BASE_DATA * u64::from(memory),
            time: BASE_TIME * time,
            wall: BASE_WALL * time,
        }
    }
}

/// The smallest step that holds `wanted` times the base, at most the largest.
pub fn step_up(wanted: u64) -> u32 {
    SCALES.into_iter().find(|step| u64::from(*step) >= wanted).unwrap_or(SCALES[SCALES.len() - 1])
}

/// Two multiples together (a kind of jobs within its app), at most the largest step.
pub fn combine(a: u32, b: u32) -> u32 {
    step_up(u64::from(a) * u64::from(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_grows_from_the_two_multiples() {
        let base = Limits::default();
        assert_eq!((base.memory, base.data), (64 << 20, 16 << 20));
        assert_eq!((base.time, base.wall), (Duration::from_secs(10), Duration::from_secs(60)));
        let big = Scale { memory: 4, time: 2, workers: 1 }.limits();
        assert_eq!((big.memory, big.data), (256 << 20, 64 << 20), "data stays a quarter of the memory");
        assert_eq!((big.time, big.wall), (Duration::from_secs(20), Duration::from_secs(120)));
    }

    #[test]
    fn steps_round_up_and_stop_at_the_largest() {
        assert_eq!([step_up(0), step_up(1), step_up(3), step_up(64), step_up(500)], [1, 1, 4, 64, 64]);
        assert_eq!(combine(8, 16), 64);
        assert!(Scale { memory: 3, ..Scale::default() }.problem().is_some());
        assert!(Scale { workers: 0, ..Scale::default() }.problem().is_some());
        assert!(Scale { memory: 2, ..Scale::default() }.within(&Scale { memory: 4, ..Scale::default() }));
        assert!(!Scale { workers: 2, ..Scale::default() }.within(&Scale::default()));
    }
}
