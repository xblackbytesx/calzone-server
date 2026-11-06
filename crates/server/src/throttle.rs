//! Login failure budgets, in memory: per client address for the DAV door and
//! the web login, and per account for the web login.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const WINDOW: Duration = Duration::from_secs(15 * 60);
/// Bound on remembered keys; expired windows go first.
const MAX_TRACKED: usize = 10_000;

#[derive(Default)]
pub struct Throttle {
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl Throttle {
    /// Whether `key` used up `limit` failures in the current window.
    pub fn blocked(&self, key: &str, limit: u32, now: Instant) -> bool {
        let map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).is_some_and(|(n, since)| *n >= limit && now.duration_since(*since) < WINDOW)
    }

    pub fn fail(&self, key: &str, now: Instant) {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED {
            map.retain(|_, (_, since)| now.duration_since(*since) < WINDOW);
        }
        let entry = map.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) >= WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    /// Forget a key (an account after a good login; never an address, so
    /// one right password does not reset a spray across many accounts).
    pub fn clear(&self, key: &str) {
        self.failures.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_budget_then_forgets() {
        let t = Throttle::default();
        let start = Instant::now();
        for _ in 0..10 {
            assert!(!t.blocked("a", 10, start));
            t.fail("a", start);
        }
        assert!(t.blocked("a", 10, start));
        assert!(!t.blocked("b", 10, start), "per key");
        assert!(!t.blocked("a", 10, start + WINDOW), "the window ends");
        t.fail("a", start + WINDOW);
        assert!(!t.blocked("a", 10, start + WINDOW), "a new window starts from zero");
        t.clear("a");
        assert!(!t.blocked("a", 1, start));
    }
}
