//! Isolated staged roots selected from canonical AST. Public roots use lexical
//! ordinals; internal selectors use statement paths. Initial typing never requests the runtime owner's
//! body, final AMIR or borrow contracts. These selectors are source-local paths,
//! not persistent IDs or an alternative source spelling of public `comptime`.

use arandu_middle::SymbolId;
use arandu_mir::ctfe::{Budget, EvalError};

use super::BuildFailure;
use crate::{ArandCompilerDb, SourceFile};

/// A source-local statement child. No offsets or arena IDs are part of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockBranch {
    IfThen,
    IfElse,
    LoopBody,
    Unsafe,
    Defer,
    ErrDefer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockStep {
    pub statement: u32,
    pub branch: BlockBranch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RootSelector {
    InIteration {
        ordinal: u32,
        value: super::FrozenConstant,
        domain_size: u64,
        selector: Box<RootSelector>,
    },
    InInstance {
        instance: arandu_middle::types::FunctionInstance,
        selector: Box<RootSelector>,
    },
    /// Source-local ordinal of an explicit public root in the owner's body.
    PublicComptime(u32),
    /// The initializer of a module/type constant, evaluated independently.
    GlobalInitializer,
    HeaderArgument(u32),
    /// Lexical ordinal of a public static-if statement in the owner.
    StaticIfCondition(u32),
    /// A frozen fixed-array domain, before runtime body typing.
    StaticForDomain(u32),
    /// A finite half-open static range endpoint, before runtime body typing.
    StaticForBound {
        ordinal: u32,
        upper: bool,
    },
    /// Lexical ordinal of a computed generic argument in the owner body.
    ConstArgument(u32),
    /// Empty path selects the owner's body with an independent return target.
    Block(Vec<BlockStep>),
    /// Select one initializer without typing the surrounding runtime statements.
    Initializer {
        block: Vec<BlockStep>,
        statement: u32,
    },
    /// Internal pre-body condition staging; validates neither branch. This is
    /// not public `comptime if` syntax or authorization to select nested parents.
    IfCondition {
        block: Vec<BlockStep>,
        statement: u32,
    },
}

/// Pool-independent expected type for the initial scalar staging slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RootExpectedType {
    Unit,
    Primitive(arandu_middle::types::Primitive),
    Structural(arandu_middle::types::TypeShape),
}

#[salsa::interned(constructor = new_in_context)]
pub struct CtfeRoot<'db> {
    pub file: SourceFile,
    pub owner: SymbolId,
    #[returns(ref)]
    pub selector: RootSelector,
    pub expected: Option<RootExpectedType>,
    #[returns(ref)]
    pub dependency: super::DependencyContext,
}

impl<'db> CtfeRoot<'db> {
    pub fn new(
        db: &'db dyn ArandCompilerDb,
        file: SourceFile,
        owner: SymbolId,
        selector: RootSelector,
        expected: Option<RootExpectedType>,
    ) -> Self {
        Self::new_in_context(
            db,
            file,
            owner,
            selector,
            expected,
            super::DependencyContext::default(),
        )
    }
}

#[salsa::interned]
pub struct CtfeRootRequest<'db> {
    pub root: CtfeRoot<'db>,
    pub budget: Budget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootEvalError {
    Build(BuildFailure),
    Evaluation(EvalError),
}

mod eval;
mod lower;
mod lower_staged;
mod select;

pub use eval::ctfe_eval_root;
pub use lower::ctfe_root_amir;
pub(crate) use select::{
    const_arguments, in_occurrence, instance_substitution, loop_requires_occurrences, public_roots,
    static_fors, static_ifs,
};
