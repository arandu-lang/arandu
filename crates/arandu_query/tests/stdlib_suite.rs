//! Consolidated integration test suite for Arandu stdlib modules.
//!
//! Consolidating stdlib tests into a single test runner binary reduces
//! compiler/linker churn by eliminating 24 separate Cranelift/Salsa link invocations.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "common/mod.rs"]
mod common;

#[path = "stdlib_suite/arena.rs"]
mod arena;

#[path = "stdlib_suite/atomic.rs"]
mod atomic;

#[path = "stdlib_suite/bitset.rs"]
mod bitset;

#[path = "stdlib_suite/buf_io.rs"]
mod buf_io;

#[path = "stdlib_suite/cell_marker.rs"]
mod cell_marker;

#[path = "stdlib_suite/channel.rs"]
mod channel;

#[path = "stdlib_suite/cmp_hash.rs"]
mod cmp_hash;

#[path = "stdlib_suite/core_foundation.rs"]
mod core_foundation;

#[path = "stdlib_suite/fmt.rs"]
mod fmt;

#[path = "stdlib_suite/fs_ext.rs"]
mod fs_ext;

#[path = "stdlib_suite/full_coverage.rs"]
mod full_coverage;

#[path = "stdlib_suite/gen_arena.rs"]
mod gen_arena;

#[path = "stdlib_suite/hash_map.rs"]
mod hash_map;

#[path = "stdlib_suite/io.rs"]
mod io;

#[path = "stdlib_suite/iter.rs"]
mod iter;

#[path = "stdlib_suite/math.rs"]
mod math;

#[path = "stdlib_suite/mem.rs"]
mod mem;

#[path = "stdlib_suite/net.rs"]
mod net;

#[path = "stdlib_suite/parallel.rs"]
mod parallel;

#[path = "stdlib_suite/process_ext.rs"]
mod process_ext;

#[path = "stdlib_suite/smallvec.rs"]
mod smallvec;

#[path = "stdlib_suite/std_io.rs"]
mod std_io;

#[path = "stdlib_suite/string_ext.rs"]
mod string_ext;

#[path = "stdlib_suite/string_from.rs"]
mod string_from;

#[path = "stdlib_suite/time.rs"]
mod time;

#[path = "stdlib_suite/rc_arc.rs"]
mod rc_arc;

#[path = "stdlib_suite/deque_slab.rs"]
mod deque_slab;

#[path = "stdlib_suite/text.rs"]
mod text;
