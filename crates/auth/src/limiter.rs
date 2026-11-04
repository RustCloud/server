//! Brute-force protection for logins.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Counts failed attempts per key (e.g. username) in a fixed time window.
///
/// After the given number of failures the key is blocked until the window that
/// started with the first failure ends. A successful login clears the key.
/// State is kept in memory: it resets on restart and is per server process.
pub struct AttemptLimiter {
    window: Duration,
    failures: Mutex<HashMap<String, Failures>>,
}

struct Failures {
    count: u32,
    window_start: Instant,
}

impl AttemptLimiter {
    pub fn new(window: Duration) -> Self {
        Self { window, failures: Mutex::new(HashMap::new()) }
    }

    /// Whether `key` may make another attempt right now, given that
    /// `max_failures` are allowed per window.
    pub fn is_allowed(&self, key: &str, max_failures: u32) -> bool {
        let failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match failures.get(key) {
            Some(entry) => entry.window_start.elapsed() > self.window || entry.count < max_failures,
            None => true,
        }
    }

    /// Records a failed attempt.
    pub fn record_failure(&self, key: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Forget finished windows so the map does not grow forever.
        failures.retain(|_, entry| entry.window_start.elapsed() <= self.window);

        let entry = failures.entry(key.to_owned()).or_insert(Failures { count: 0, window_start: Instant::now() });
        entry.count += 1;
    }

    /// Takes back one failure of `key`: attempts are counted as failures
    /// before they are checked (so a burst of them at once is held back
    /// too), and one that succeeded wasn't.
    pub fn forgive(&self, key: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = failures.get_mut(key) {
            entry.count = entry.count.saturating_sub(1);
        }
    }

    /// Clears the failures of `key` after a successful attempt.
    pub fn record_success(&self, key: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        failures.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_too_many_failures() {
        let limiter = AttemptLimiter::new(Duration::from_secs(60));
        for _ in 0..3 {
            assert!(limiter.is_allowed("alice", 3));
            limiter.record_failure("alice");
        }
        assert!(!limiter.is_allowed("alice", 3));
        assert!(limiter.is_allowed("bob", 3), "other keys are not affected");
    }

    #[test]
    fn success_clears_failures() {
        let limiter = AttemptLimiter::new(Duration::from_secs(60));
        limiter.record_failure("alice");
        limiter.record_success("alice");
        limiter.record_failure("alice");
        assert!(limiter.is_allowed("alice", 2));
    }

    #[test]
    fn block_ends_with_the_window() {
        let limiter = AttemptLimiter::new(Duration::from_millis(20));
        limiter.record_failure("alice");
        assert!(!limiter.is_allowed("alice", 1));
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.is_allowed("alice", 1));
    }
}
