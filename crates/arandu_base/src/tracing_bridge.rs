//! Tracing and self-profiling bridge for the Arandu compiler.
//!
//! Controlled by `-Zdebug-*` and `-Zself-profile=<path>` flags.
//!
//! Design:
//! - `init_tracing()` composes a `tracing_subscriber::fmt` layer (filtered by
//!   `EnvFilter` built from the `-Zdebug-*` flags) with an optional
//!   `SelfProfileLayer` that emits Trace Event Format JSON (compatible with
//!   `chrome://tracing` and Perfetto).
//! - The two layers have independent filters: the debug layers respect category
//!   flags; the self-profile layer captures *all* spans for the flamegraph.
//! - `RUST_LOG` still works as an escape hatch for ad-hoc debugging.

use std::path::PathBuf;
#[cfg(all(debug_assertions, feature = "self-profile"))]
use std::{
    collections::HashMap,
    fmt,
    sync::{Mutex, OnceLock},
    time::Instant,
};
#[cfg(all(debug_assertions, feature = "self-profile"))]
use tracing::{
    Event, Id, Subscriber,
    field::{Field, Visit},
    span::Attributes,
};
#[cfg(all(debug_assertions, feature = "self-profile"))]
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::{EnvFilter, Registry, fmt::format::FmtSpan, prelude::*};

// ── Static helpers (lazy-init via OnceLock) ───────────────────────────

#[cfg(all(debug_assertions, feature = "self-profile"))]
fn base_time() -> &'static Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now)
}

#[cfg(all(debug_assertions, feature = "self-profile"))]
fn span_map() -> &'static Mutex<HashMap<Id, SpanMeta>> {
    static MAP: OnceLock<Mutex<HashMap<Id, SpanMeta>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(all(debug_assertions, feature = "self-profile"))]
fn entry_map() -> &'static Mutex<HashMap<Id, Instant>> {
    static MAP: OnceLock<Mutex<HashMap<Id, Instant>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(all(debug_assertions, feature = "self-profile"))]
struct SpanMeta {
    name: &'static str,
    target: &'static str,
}

// ── Self-profile buffer ───────────────────────────────────────────────
//
// Events are buffered in memory and flushed once via `finalize_self_profile()`
// (called from `print_perf_summary()`).  This avoids the Drop-on-global-leak
// problem that would leave the JSON array unclosed.

#[cfg(all(debug_assertions, feature = "self-profile"))]
static SELF_PROFILE_PATH: OnceLock<PathBuf> = OnceLock::new();
#[cfg(all(debug_assertions, feature = "self-profile"))]
static SELF_PROFILE_EVENTS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[cfg(all(debug_assertions, feature = "self-profile"))]
fn self_profile_events() -> &'static Mutex<Vec<String>> {
    SELF_PROFILE_EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Write all buffered trace events to the target file as a JSON array.
///
/// Should be called exactly once, at the end of compilation.
pub fn finalize_self_profile() {
    #[cfg(all(debug_assertions, feature = "self-profile"))]
    {
        let path = match SELF_PROFILE_PATH.get() {
            Some(p) => p,
            None => return,
        };
        let events = match self_profile_events().lock() {
            Ok(mut e) => std::mem::take(&mut *e),
            Err(_) => return,
        };

        let mut json = String::with_capacity(events.len() * 120);
        json.push_str("[\n");
        for (i, line) in events.iter().enumerate() {
            if i > 0 {
                json.push_str(",\n");
            }
            json.push_str(line);
        }
        json.push_str("\n]\n");
        let _ = std::fs::write(path, json);
    }
}

/// Configuration parsed from `-Z` flags by [`super::perf::init_z_flags`].
#[derive(Debug, Default)]
pub struct TracingConfig {
    pub debug_parser: bool,
    pub debug_typeck: bool,
    pub debug_ossa: bool,
    pub debug_layout: bool,
    pub debug_backend: bool,
    pub debug_all: bool,
    /// If `Some(path)`, write a Trace Event JSON file at the given path.
    pub self_profile: Option<PathBuf>,
}

// ── Initialisation ────────────────────────────────────────────────────

/// Initialise the global tracing subscriber.
///
/// Must be called exactly once, before any compilation begins.
pub fn init_tracing(cfg: TracingConfig) {
    #[cfg(all(debug_assertions, feature = "self-profile"))]
    if let Some(ref path) = cfg.self_profile {
        let _ = SELF_PROFILE_PATH.set(path.clone());
    }

    let filter = flags_to_env_filter(&cfg);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_level(true)
        .with_span_events(FmtSpan::ACTIVE)
        .with_filter(filter);

    let subscriber = Registry::default().with(fmt_layer);

    #[cfg(all(debug_assertions, feature = "self-profile"))]
    {
        if cfg.self_profile.is_some() {
            subscriber.with(SelfProfileLayer).init();
        } else {
            subscriber.init();
        }
    }

    #[cfg(not(all(debug_assertions, feature = "self-profile")))]
    {
        if cfg.self_profile.is_some() {
            eprintln!(
                "Warning: self-profiling requires a debug/development build with the `self-profile` Cargo feature."
            );
        }
        subscriber.init();
    }
}

/// Build an `EnvFilter` from the `-Zdebug-*` flags.
///
/// When no debug flag is active the filter is set to `"off"` so that nothing
/// is printed to stderr by the fmt layer.  The self-profile layer (if
/// configured) still captures all spans regardless of this filter.
/// `RUST_LOG` can still override this at runtime.
fn flags_to_env_filter(cfg: &TracingConfig) -> EnvFilter {
    let has_debug = cfg.debug_all
        || cfg.debug_parser
        || cfg.debug_typeck
        || cfg.debug_ossa
        || cfg.debug_layout
        || cfg.debug_backend;

    if !has_debug {
        return EnvFilter::new("off");
    }

    let mut directives: Vec<&str> = Vec::new();
    if cfg.debug_all || cfg.debug_parser {
        directives.push("arandu_parser=trace");
    }
    if cfg.debug_all || cfg.debug_typeck {
        directives.push("arandu_middle::types::unify=trace");
        directives.push("arandu_typeck=trace");
    }
    if cfg.debug_all || cfg.debug_ossa {
        directives.push("arandu_mir::move_checker=trace");
        directives.push("arandu_semantics::passes::lower_amir=trace");
        directives.push("arandu_middle::amir=trace");
    }
    if cfg.debug_all || cfg.debug_layout {
        directives.push("arandu_middle::layout=trace");
    }
    if cfg.debug_all || cfg.debug_backend {
        directives.push("arandu_backend_cranelift=trace");
    }
    if cfg.debug_all {
        directives.push("arandu_base=trace");
        directives.push("arandu_resolve=trace");
        directives.push("arandu_semantics=trace");
    }
    directives.push("arandu_cli=info");
    EnvFilter::new(directives.join(","))
}

// ── SelfProfileLayer — Trace Event JSON output ────────────────────────
//
// Buffers one JSON line per completed span.  Flushed to disk via
// `finalize_self_profile()`.  Compatible with Perfetto UI and
// chrome://tracing (Trace Event Format v2, `ph="X"` complete events).
//
// The layer is excluded unless both debug assertions and the opt-in Cargo
// feature are active, so production builds pay no span bookkeeping cost.

#[cfg(all(debug_assertions, feature = "self-profile"))]
struct SelfProfileLayer;

#[cfg(all(debug_assertions, feature = "self-profile"))]
impl<S: Subscriber> Layer<S> for SelfProfileLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _ctx: Context<'_, S>) {
        let meta = attrs.metadata();
        if let Ok(mut spans) = span_map().lock() {
            spans.insert(
                id.clone(),
                SpanMeta {
                    name: meta.name(),
                    target: meta.target(),
                },
            );
        }
    }

    fn on_enter(&self, id: &Id, _ctx: Context<'_, S>) {
        if let Ok(mut entries) = entry_map().lock() {
            entries.entry(id.clone()).or_insert_with(Instant::now);
        }
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        struct MsgVisitor(String);
        impl Visit for MsgVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut visitor = MsgVisitor(String::new());
        event.record(&mut visitor);
        let name = if visitor.0.is_empty() {
            event.metadata().name().to_string()
        } else {
            // Strip surrounding quotes added by Debug format
            visitor.0.trim_matches('"').to_string()
        };
        let ts = Instant::now().duration_since(*base_time()).as_micros() as u64;
        if let Ok(mut events) = self_profile_events().lock() {
            events.push(format!(
                r#"{{"cat":"{}","name":"{}","ph":"i","ts":{},"s":"g","pid":0,"tid":0}}"#,
                event.metadata().target(),
                name,
                ts,
            ));
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let start = entry_map().lock().ok().and_then(|mut m| m.remove(&id));
        let meta = span_map().lock().ok().and_then(|mut m| m.remove(&id));

        if let (Some(start), Some(meta)) = (start, meta) {
            let ts = start.duration_since(*base_time()).as_micros() as u64;
            let dur_us = Instant::now().duration_since(start).as_micros() as u64;

            if let Ok(mut events) = self_profile_events().lock() {
                events.push(format!(
                    r#"{{"cat":"{}","name":"{}","ph":"X","ts":{},"dur":{},"pid":0,"tid":0}}"#,
                    meta.target, meta.name, ts, dur_us,
                ));
            }
        }
    }
}
