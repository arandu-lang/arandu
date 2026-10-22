//! Standalone C mirror of ownership-aware `std.thread` native handles.

use super::super::CEmitter;

impl CEmitter<'_> {
    pub(super) fn emit_native_thread_runtime(&mut self) {
        self.output.push_str(include_str!("native_thread.c"));
    }
}
