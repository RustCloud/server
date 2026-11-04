//! Brute-force protection for logins.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Longest wait between attempts on one account.
pub const MAX_DELAY: Duration = Duration::from_secs(60);

/// Counts failed attempts per key (an account, an IP address) in a fixed
/// time window that starts with the first failure. A successful login
/// clears the key. State is kept in memory: it resets on restart and is per
/// server process.
///
/// Attempts are counted as failures when they start (so a burst of them at
/// once is held back too); one that succeeds is taken back.
pub struct AttemptLimiter {
    window: Duration,
    failures: Mutex<HashMap<String, Failures>>,
}

struct Failures {
    count: u32,
    window_start: Instant,
    last: Instant,
}

impl AttemptLimiter {
    pub fn new(window: Duration) -> Self {
        Self { window, failures: Mutex::new(HashMap::new()) }
    }

    /// Starts an attempt of `key` if it has made fewer than `max_failures`
    /// failed ones in this window. Used where a hard limit is fine: per IP
    /// address, or for someone who is already logged in.
    pub fn try_start(&self, key: &str, max_failures: u32) -> bool {
        self.start(key, |failures| failures.count < max_failures).is_ok()
    }

    /// Starts an attempt of `key` unless it has to wait. The first `free`
    /// failures cost nothing; after that every attempt waits for 1 s, 2 s,
    /// 4 s … since the last one, at most [`MAX_DELAY`]. So an account is
    /// never locked: guessing is slowed down, while its owner can still log in
    /// after a short wait. `Err` tells how long is left.
    pub fn try_start_delayed(&self, key: &str, free: u32) -> Result<(), Duration> {
        let mut left = Duration::ZERO;
        let started = self.start(key, |failures| {
            let Some(over) = failures.count.checked_sub(free) else {
                return true;
            };
            left = delay_after(over).saturating_sub(failures.last.elapsed());
            left.is_zero()
        });
        started.map_err(|()| left)
    }

    /// Records a failure of `key` if `allowed` says so, in one step.
    fn start(&self, key: &str, allowed: impl FnOnce(&Failures) -> bool) -> Result<(), ()> {
        let mut failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Forget finished windows so the map does not grow forever.
        failures.retain(|_, entry| entry.window_start.elapsed() <= self.window);
        let now = Instant::now();
        let entry = failures.entry(key.to_owned()).or_insert(Failures { count: 0, window_start: now, last: now });
        if !allowed(entry) {
            return Err(());
        }
        entry.count += 1;
        entry.last = now;
        Ok(())
    }

    /// Whether `key` has made fewer than `max_failures` failed attempts in
    /// this window, without starting one. For cheap checks that record
    /// their failures afterwards with [`Self::record_failure`].
    pub fn is_allowed(&self, key: &str, max_failures: u32) -> bool {
        let failures = self.failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        failures.get(key).is_none_or(|entry| entry.window_start.elapsed() > self.window || entry.count < max_failures)
    }

    /// Records a failed attempt that wasn't started with `try_start`.
    pub fn record_failure(&self, key: &str) {
        let _ = self.start(key, |_| true);
    }

    /// Takes back one failure of `key`: the attempt started but wasn't a
    /// failure after all.
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

/// The wait after `over` failures beyond the free ones: 1 s, 2 s, 4 s …
fn delay_after(over: u32) -> Duration {
    Duration::from_secs(1u64 << over.min(16)).min(MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_too_many_failures() {
        let limiter = AttemptLimiter::new(Duration::from_secs(60));
        for _ in 0..3 {
            assert!(limiter.try_start("alice", 3));
        }
        assert!(!limiter.try_start("alice", 3));
        assert!(limiter.try_start("bob", 3), "other keys are not affected");
    }

    #[test]
    fn success_clears_failures() {
        let limiter = AttemptLimiter::new(Duration::from_secs(60));
        assert!(limiter.try_start("alice", 2));
        limiter.record_success("alice");
        assert!(limiter.try_start("alice", 2));
        assert!(limiter.try_start("alice", 2));
    }

    #[test]
    fn block_ends_with_the_window() {
        let limiter = AttemptLimiter::new(Duration::from_millis(20));
        assert!(limiter.try_start("alice", 1));
        assert!(!limiter.try_start("alice", 1));
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.try_start("alice", 1));
    }

    #[test]
    fn free_attempts_then_growing_waits() {
        let limiter = AttemptLimiter::new(Duration::from_secs(600));
        for _ in 0..3 {
            assert_eq!(limiter.try_start_delayed("alice", 3), Ok(()));
        }
        let left = limiter.try_start_delayed("alice", 3).unwrap_err();
        assert!(left > Duration::from_millis(900) && left <= Duration::from_secs(1), "{left:?}");
        assert_eq!(limiter.try_start_delayed("bob", 3), Ok(()), "other keys are not affected");
    }

    #[test]
    fn waits_double_up_to_the_limit() {
        assert_eq!(delay_after(0), Duration::from_secs(1));
        assert_eq!(delay_after(3), Duration::from_secs(8));
        assert_eq!(delay_after(6), MAX_DELAY);
        assert_eq!(delay_after(u32::MAX), MAX_DELAY);
    }
}
