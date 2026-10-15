pub mod bitset;
pub mod index_vec;
pub mod line_index;
pub mod perf;
pub mod source_registry;
pub mod span;
pub mod tracing_bridge;

// Re-export core items for ease of use
pub use bitset::{BitMatrix, BitSet};
pub use index_vec::IndexVec;
pub use line_index::LineIndex;
pub use perf::{
    DUMP_MIR, EXPLAIN_REBUILD, NO_GENERATIONAL_FALLBACK, PROFILE_QUERIES, any_debug_flag_active,
    build_tracing_config, init_z_flags, print_perf_summary, track_alloc, track_query_hit,
    track_query_miss,
};
pub use source_registry::{SourceFile, SourceRegistry};
pub use span::Span;
pub use tracing_bridge::{TracingConfig, finalize_self_profile, init_tracing};
