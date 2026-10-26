//! Platform Abstraction Layer emitted as the only OS-ifdef region of generated C.
//!
//! `pal.c` covers filesystem, clocks, process arguments, and native threads.
//! `pal_net.c` is emitted only for `ar_net_*` so other programs do not take a
//! Winsock link dependency. When both are emitted, the net PAL comes first so
//! `winsock2.h` precedes `windows.h`.

use super::super::CEmitter;

impl CEmitter<'_> {
    pub(super) fn emit_pal_net_runtime(&mut self) {
        self.output.push_str(include_str!("pal_net.c"));
        self.output.push('\n');
    }

    pub(super) fn emit_pal_runtime(&mut self) {
        self.output.push_str(include_str!("pal.c"));
        self.output.push('\n');
    }
}
