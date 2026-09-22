//! Shared debounce buffer for LSP text edits and CLI filesystem watch.
//!
//! Both consumers face the same problem: raw events arrive in bursts
//! (editor save = write-temp + rename; `cp -r` / git checkout = dozens of files).
//! Materializing each event as a Salsa revision cancels in-flight queries.
//!
//! [`DebouncedMap`] accumulates updates and only yields them after a quiet
//! window ([`DEFAULT_DEBOUNCE`]), so one logical edit becomes one commit.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Quiet period before committing pending changes (100ms matches LSP gold).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(100);

/// Debounced key → value map. Later pushes for the same key replace earlier ones.
#[derive(Debug)]
pub struct DebouncedMap<K, V> {
    pending: HashMap<K, Pending<V>>,
    debounce: Duration,
}

#[derive(Debug, Clone)]
struct Pending<V> {
    value: V,
    changed_at: Instant,
}

impl<K, V> Default for DebouncedMap<K, V>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> DebouncedMap<K, V>
where
    K: Eq + Hash + Clone,
{
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            debounce: DEFAULT_DEBOUNCE,
        }
    }

    #[must_use]
    pub fn with_debounce(debounce: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            debounce,
        }
    }

    #[must_use]
    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Queue or replace the value for `key` (resets quiet timer for that key).
    pub fn push(&mut self, key: K, value: V) {
        self.pending.insert(
            key,
            Pending {
                value,
                changed_at: Instant::now(),
            },
        );
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.pending.get(key).map(|pending| &pending.value)
    }

    /// Time until the soonest pending entry becomes due.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        self.pending
            .values()
            .map(|p| {
                let due = p.changed_at + self.debounce;
                if due <= now {
                    Duration::ZERO
                } else {
                    due.saturating_duration_since(now)
                }
            })
            .min()
    }

    /// Drain entries whose quiet window has elapsed.
    pub fn take_due(&mut self) -> Vec<(K, V)> {
        let now = Instant::now();
        let mut due = Vec::new();
        let mut keep = HashMap::new();
        for (k, p) in self.pending.drain() {
            if p.changed_at + self.debounce <= now {
                due.push((k, p.value));
            } else {
                keep.insert(k, p);
            }
        }
        self.pending = keep;
        due
    }

    /// Drain **all** pending entries immediately (save / shutdown / force flush).
    pub fn take_all(&mut self) -> Vec<(K, V)> {
        self.pending.drain().map(|(k, p)| (k, p.value)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn batches_multiple_pushes_for_same_key() {
        let mut m = DebouncedMap::with_debounce(Duration::from_millis(40));
        m.push("a", 1);
        m.push("a", 2);
        m.push("a", 3);
        assert_eq!(m.pending_count(), 1);
        assert!(m.take_due().is_empty());
        thread::sleep(Duration::from_millis(50));
        let due = m.take_due();
        assert_eq!(due, vec![("a", 3)]);
    }

    #[test]
    fn take_all_immediate() {
        let mut m = DebouncedMap::with_debounce(Duration::from_secs(10));
        m.push("x", 9);
        assert_eq!(m.take_all(), vec![("x", 9)]);
    }
}
