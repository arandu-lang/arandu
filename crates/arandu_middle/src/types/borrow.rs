//! Canonical compile-time contracts for values that contain safe borrows.
//!
//! These types deliberately contain no [`TypeId`](super::TypeId), span, hash
//! map, or runtime address. They cross module and incremental-query boundaries,
//! so their representation must be deterministic and independent from an
//! interner generation.

use smol_str::SmolStr;

/// Maximum nesting published in a borrow interface. This bound is part of
/// the compiler contract, not a recursion guard chosen by the host stack.
pub const MAX_BORROW_PATH_DEPTH: usize = 32;
/// Maximum borrowed leaves published for one type.
pub const MAX_BORROW_PATHS: usize = 256;

/// Deterministic reason why a structural borrow contract cannot be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowShapeError {
    DepthExceeded,
    PathCountExceeded,
}

/// Access permission carried by a safe reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BorrowKind {
    Shared,
    Exclusive,
}

/// One stable step from a returned value to a nested borrowed component.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BorrowPathSegment {
    Tuple(u32),
    Field(SmolStr),
    Variant(u32),
    Payload(u32),
    OptionSome,
    ResultOk,
    ResultErr,
    ArrayElement,
    NullableValue,
    CoroutinePayload,
    PollReady,
    RangeElement,
}

use smallvec::SmallVec;

/// A canonical path. The empty path denotes the value itself.
/// Inline capacity of 4 avoids heap allocation in typical paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BorrowPath(pub SmallVec<[BorrowPathSegment; 4]>);

impl BorrowPath {
    #[must_use]
    pub fn root() -> Self {
        Self(SmallVec::new())
    }
}

impl From<Vec<BorrowPathSegment>> for BorrowPath {
    fn from(v: Vec<BorrowPathSegment>) -> Self {
        Self(SmallVec::from_vec(v))
    }
}

impl std::ops::Deref for BorrowPath {
    type Target = [BorrowPathSegment];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One formal input from which a returned borrow may originate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BorrowSource {
    pub parameter_index: u32,
    /// Path inside the formal parameter; empty for a direct `ref T` formal.
    pub parameter_path: BorrowPath,
}

/// Dependency for one borrowed component of the returned value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReturnBorrowDependency {
    pub result_path: BorrowPath,
    pub sources: Vec<BorrowSource>,
    pub kind: BorrowKind,
}

/// Complete exported dependency contract for a function return.
///
/// `dependencies`, and every dependency's `sources`, are sorted and
/// deduplicated before publication. An empty summary means that the return
/// contains no safe borrow; a borrow-bearing return with no demonstrable source
/// is rejected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReturnBorrowSummary {
    pub dependencies: Vec<ReturnBorrowDependency>,
}

impl ReturnBorrowSummary {
    /// Convenience constructor for the common direct-reference passthrough.
    #[must_use]
    pub fn direct(parameter_index: u32, kind: BorrowKind) -> Self {
        Self {
            dependencies: vec![ReturnBorrowDependency {
                result_path: BorrowPath::root(),
                sources: vec![BorrowSource {
                    parameter_index,
                    parameter_path: BorrowPath::root(),
                }],
                kind,
            }],
        }
    }

    /// Restore the canonical representation after composition or substitution.
    pub fn canonicalize(&mut self) {
        for dependency in &mut self.dependencies {
            dependency.sources.sort();
            dependency.sources.dedup();
        }
        self.dependencies.sort();
        self.dependencies.dedup();
    }
}
