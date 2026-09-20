pub mod block;
pub mod cfg_viz;
pub mod dominators;
pub mod local;
pub mod pretty;
pub mod program;
pub mod reachability;
pub mod rpo;
pub mod stmt;
pub mod value;
pub mod visit;

pub use block::{AmirBasicBlock, BlockId, BlockParam};
pub use dominators::Dominators;
pub use local::{AmirLocal, AmirReceiver, AmirTemp, LocalId, TempId};
pub use program::{AmirDebugBinding, AmirFunc, AmirProgram};
pub use reachability::reachable_blocks_dense;
pub use rpo::{reverse_post_order, reverse_post_order_body_first};
pub use stmt::{
    AmirStmt, AmirStmtKind, AmirStmtTable, AmirTerminator, CallBorrowDependency, InstrId,
};
pub use value::{AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, GenArenaDomain};
pub use visit::{
    for_each_place_operand, for_each_rvalue_operand, for_each_rvalue_place,
    for_each_terminator_operand,
};

/// AMIR enum payload is always stored at field index 1 (0 is the tag/discriminant).
/// Used by `?`/`??`/`?.`/enum pattern extraction for both `Result.Ok`/`Err` and
/// `Option.Some`. Kept centralized so a layout change cannot silently diverge.
pub const ENUM_PAYLOAD_FIELD: usize = 1;

/// AMIR representation invariant for `Option<T>`: `None` is the tag-0 variant
/// and `Some` the tag-1 variant (payload at `ENUM_PAYLOAD_FIELD`).
/// `?`/`??`/`?.` lowering and `Option` branch reconstruction rely on this
/// layout, which is produced by the `Option`-specific construction path
/// (independent of the source variant order in `stdlib/core/option.aru`).
pub const OPTION_NONE_TAG: usize = 0;
pub const OPTION_SOME_TAG: usize = 1;
