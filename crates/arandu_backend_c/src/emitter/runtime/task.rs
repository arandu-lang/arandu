//! C runtime helpers for cooperative tasks, threads, mutexes, and dynamic parallel fold.

use super::super::CEmitter;

impl CEmitter<'_> {
    /// SL_R.0 cooperative task table mirror (JIT `ar_rt_*`) for the C backend.
    ///
    /// Mirrors `arandu_runtime::rt_runtime`: a growable table of slots with
    /// Pending/Running/Completed states. A Pending blob is owned by the row;
    /// a join claims it, drives it, releases it, then caches the result. A
    /// cancel releases a Pending blob or marks a Running row for retirement.
    /// Cooperative only: the joining thread drives the blob; standalone C has
    /// no OS-worker scheduling at this surface.
    pub(super) fn emit_task_runtime(&mut self) {
        self.output.push_str(include_str!("task.c"));
    }
}
