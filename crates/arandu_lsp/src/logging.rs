//! Operator diagnostics go to stderr, never the LSP protocol stream.

use lsp_types::TraceValue;
use std::sync::atomic::{AtomicU8, Ordering};

const TRACE_OFF: u8 = 0;
const TRACE_MESSAGES: u8 = 1;
const TRACE_VERBOSE: u8 = 2;

static TRACE_LEVEL: AtomicU8 = AtomicU8::new(TRACE_OFF);

pub(crate) fn set_trace(value: TraceValue) {
    let encoded = match value {
        TraceValue::Off => TRACE_OFF,
        TraceValue::Messages => TRACE_MESSAGES,
        TraceValue::Verbose => TRACE_VERBOSE,
    };
    TRACE_LEVEL.store(encoded, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn current_trace() -> TraceValue {
    match TRACE_LEVEL.load(Ordering::Relaxed) {
        TRACE_MESSAGES => TraceValue::Messages,
        TRACE_VERBOSE => TraceValue::Verbose,
        _ => TraceValue::Off,
    }
}

pub(crate) fn log_panic(context: &str, payload: &(dyn std::any::Any + Send)) {
    let message = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    };
    eprintln!("arandu-lsp: {context} panicked: {message}");
}
