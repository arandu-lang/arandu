//! C runtime helpers for coroutine polling, heap creation, and typed awaits.

use super::super::CEmitter;

impl CEmitter<'_> {
    pub(super) fn emit_co_poll_runtime(&mut self) {
        self.output.push_str(include_str!("coroutine.c"));
    }
}
