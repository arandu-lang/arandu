//! DX.5 — causal-chain explain-rebuild from Salsa runtime events.
//!
//! Uses Salsa's own [`salsa::Event`] stream (no hand-rolled dependency graph).
//! Recording is opt-in via [`crate::db::DatabaseImpl::with_rebuild_log`] so the
//! hot path pays zero overhead when disabled.

use salsa::{Event, EventKind};
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};

/// Compact rebuild event (stringified key; allocated only while recording).
#[derive(Debug, Clone)]
pub enum RebuildEvent {
    /// Query body will run (cold or inputs dirty).
    Execute { key: String },
    /// Memo hit — inputs still valid.
    Validate { key: String },
}

impl fmt::Display for RebuildEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RebuildEvent::Execute { key } => write!(f, "execute {key}"),
            RebuildEvent::Validate { key } => write!(f, "validate {key}"),
        }
    }
}

/// Thread-safe ring of rebuild events shared with the Salsa event callback.
///
/// Prefer a single shared [`Arc`] across the database lifetime; clear between
/// measurement windows instead of reallocating the log.
#[derive(Default)]
pub struct RebuildLog {
    events: Mutex<Vec<RebuildEvent>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RebuildCounts {
    pub executed: usize,
    pub validated: usize,
}

impl RebuildLog {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.events.lock() {
            g.clear();
        }
    }

    pub fn push(&self, event: RebuildEvent) {
        if let Ok(mut g) = self.events.lock() {
            g.push(event);
        }
    }

    /// Snapshot for formatting / tests (clones only the recorded window).
    #[must_use]
    pub fn snapshot(&self) -> Vec<RebuildEvent> {
        self.events.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Allocation-free aggregate for performance/endurance measurement windows.
    #[must_use]
    pub fn counts(&self) -> RebuildCounts {
        let Ok(events) = self.events.lock() else {
            return RebuildCounts::default();
        };
        let mut counts = RebuildCounts::default();
        for event in events.iter() {
            match event {
                RebuildEvent::Execute { .. } => counts.executed += 1,
                RebuildEvent::Validate { .. } => counts.validated += 1,
            }
        }
        counts
    }

    /// Counts recorded executions whose query key contains `substring`.
    #[must_use]
    pub fn count_executions_matching(&self, substring: &str) -> usize {
        let Ok(events) = self.events.lock() else {
            return 0;
        };
        events
            .iter()
            .filter(|e| match e {
                RebuildEvent::Execute { key } => key.contains(substring),
                _ => false,
            })
            .count()
    }

    /// Human-readable causal chain of **executions** (validate hits omitted unless verbose).
    #[must_use]
    pub fn format_chain(&self, verbose: bool) -> String {
        let events = self.snapshot();
        let mut out = String::new();
        let mut n_exec = 0u32;
        let mut n_val = 0u32;
        for ev in &events {
            match ev {
                RebuildEvent::Execute { .. } => {
                    n_exec += 1;
                    let _ = writeln!(out, "  → {ev}");
                }
                RebuildEvent::Validate { .. } => {
                    n_val += 1;
                    if verbose {
                        let _ = writeln!(out, "  · {ev}");
                    }
                }
            }
        }
        let mut header = String::new();
        let _ = writeln!(header, "rebuild chain: {n_exec} execute, {n_val} validate");
        header.push_str(&out);
        header
    }

    /// Salsa event callback: maps runtime events into [`RebuildEvent`] and updates `-Zprofile-queries`.
    pub fn salsa_callback(this: Arc<Self>) -> Box<dyn Fn(Event) + Send + Sync + 'static> {
        Box::new(move |event| match event.kind {
            EventKind::WillExecute { database_key } => {
                arandu_base::perf::track_query_miss();
                this.push(RebuildEvent::Execute {
                    key: format!("{database_key:?}"),
                });
            }
            EventKind::DidValidateMemoizedValue { database_key } => {
                arandu_base::perf::track_query_hit();
                this.push(RebuildEvent::Validate {
                    key: format!("{database_key:?}"),
                });
            }
            _ => {}
        })
    }

    /// Lightweight Salsa callback for `-Zprofile-queries` when `-Zexplain-rebuild` is off.
    #[must_use]
    pub fn profile_queries_callback() -> Box<dyn Fn(Event) + Send + Sync + 'static> {
        Box::new(|event| match event.kind {
            EventKind::WillExecute { .. } => {
                arandu_base::perf::track_query_miss();
            }
            EventKind::DidValidateMemoizedValue { .. } => {
                arandu_base::perf::track_query_hit();
            }
            _ => {}
        })
    }
}

/// True if any `WillExecute` was recorded (something recomputed).
#[must_use]
pub fn any_execute(log: &RebuildLog) -> bool {
    log.snapshot()
        .iter()
        .any(|e| matches!(e, RebuildEvent::Execute { .. }))
}

impl RebuildLog {
    /// One-line status for `arandu run` / `build` (DX.5 surface).
    ///
    /// - `[cached]` — no query bodies ran (full memo hit window)
    /// - `[rebuilt: N queries]` — N `WillExecute` events in this window
    #[must_use]
    pub fn status_line(&self) -> String {
        let events = self.snapshot();
        let n_exec = events
            .iter()
            .filter(|e| matches!(e, RebuildEvent::Execute { .. }))
            .count();
        if n_exec == 0 {
            "[cached]".to_string()
        } else {
            format!("[rebuilt: {n_exec} queries]")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rebuild_log_operations() {
        let log = RebuildLog::new();
        assert!(log.snapshot().is_empty());
        assert!(!any_execute(&log));

        log.push(RebuildEvent::Validate {
            key: "query_a".to_string(),
        });
        assert_eq!(log.snapshot().len(), 1);
        assert!(!any_execute(&log));

        log.push(RebuildEvent::Execute {
            key: "query_b".to_string(),
        });
        assert_eq!(log.snapshot().len(), 2);
        assert_eq!(
            log.counts(),
            RebuildCounts {
                executed: 1,
                validated: 1
            }
        );
        assert!(any_execute(&log));

        log.clear();
        assert!(log.snapshot().is_empty());
        assert!(!any_execute(&log));
    }

    #[test]
    fn test_rebuild_log_format_chain() {
        let log = RebuildLog::new();
        log.push(RebuildEvent::Execute {
            key: "query_a".to_string(),
        });
        log.push(RebuildEvent::Validate {
            key: "query_b".to_string(),
        });
        log.push(RebuildEvent::Execute {
            key: "query_c".to_string(),
        });

        let chain_non_verbose = log.format_chain(false);
        assert!(chain_non_verbose.contains("rebuild chain: 2 execute, 1 validate"));
        assert!(chain_non_verbose.contains("→ execute query_a"));
        assert!(chain_non_verbose.contains("→ execute query_c"));
        assert!(!chain_non_verbose.contains("validate query_b"));

        let chain_verbose = log.format_chain(true);
        assert!(chain_verbose.contains("rebuild chain: 2 execute, 1 validate"));
        assert!(chain_verbose.contains("→ execute query_a"));
        assert!(chain_verbose.contains("· validate query_b"));
        assert!(chain_verbose.contains("→ execute query_c"));
    }

    #[test]
    fn test_rebuild_event_display() {
        let exec = RebuildEvent::Execute {
            key: "test_exec".to_string(),
        };
        let val = RebuildEvent::Validate {
            key: "test_val".to_string(),
        };
        assert_eq!(format!("{exec}"), "execute test_exec");
        assert_eq!(format!("{val}"), "validate test_val");
    }
}
