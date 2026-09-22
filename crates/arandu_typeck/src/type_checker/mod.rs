use arandu_parser::Program;
use arandu_parser::ast_pool::{AstPool, ExprId};
use std::sync::Arc;

use crate::{Diagnostic, ResolutionResult, ResolvedNames, ScopeId, SymbolId, SymbolTable};

pub mod check;
pub mod constraints;
pub mod context;
pub mod errors;
pub mod info;
pub mod provenance;
pub mod solver;
pub mod synth;
pub mod types;

pub use info::{EnumPayloadShape, TypeInfo, translate_type};

use constraints::{Constraint, ConstraintOrigin};
use context::TyCtx;
use types::{ArType, TypeId, TypeInterner};

// ── Results ─────────────────────────────────────────────────────────

/// Type-check output shared across Salsa queries and lowerings.
///
/// PERF.5: heavy tables live behind [`Arc`] so cloning a result (keystroke /
/// query fan-out in the future LSP) is O(1) atomic refcount ops. Diagnostics
/// stay owned — small and frequently extended.
///
/// Mutating `type_info` (e.g. HIR lower interning) uses [`Self::type_info_mut`]
/// (`Arc::make_mut`) so shared snapshots stay intact until uniquely owned.
#[derive(Debug, Clone)]
pub struct TypeCheckResult {
    pub symbols: Arc<SymbolTable>,
    pub resolved: Arc<ResolvedNames>,
    pub type_info: Arc<TypeInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

impl TypeCheckResult {
    /// Empty shell used by cycle recovery and parse-error fallbacks.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            symbols: Arc::new(SymbolTable::default()),
            resolved: Arc::new(ResolvedNames::default()),
            type_info: Arc::new(TypeInfo::default()),
            diagnostics: Vec::new(),
        }
    }

    /// Unique mutable access to [`TypeInfo`] (clone-on-write via [`Arc::make_mut`]).
    pub fn type_info_mut(&mut self) -> &mut TypeInfo {
        Arc::make_mut(&mut self.type_info)
    }

    /// Unique mutable access to the symbol table.
    pub fn symbols_mut(&mut self) -> &mut SymbolTable {
        Arc::make_mut(&mut self.symbols)
    }
}

// ── Entry point ─────────────────────────────────────────────────────

/// Standalone type-check for tests or isolated modules.
/// Runs both signature and body checks.
#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(resolution, program))]
pub fn type_check(
    resolution: ResolutionResult,
    program: &Program,
    target_info: TargetInfo,
) -> TypeCheckResult {
    let mut checker = TypeChecker::new(
        resolution.symbols,
        resolution.resolved,
        resolution.diagnostics,
        &program.pool,
        target_info,
    );
    check::check_signatures(&mut checker, program);
    check::check_bodies(&mut checker, program);
    checker.finish()
}

/// Runs ONLY the signature check phase, producing an initial TypeCheckResult.
#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(resolution, program))]
pub fn check_signatures_only(
    resolution: ResolutionResult,
    program: &Program,
    target_info: TargetInfo,
) -> TypeCheckResult {
    let mut checker = TypeChecker::new(
        resolution.symbols,
        resolution.resolved,
        resolution.diagnostics,
        &program.pool,
        target_info,
    );
    check::check_signatures(&mut checker, program);
    checker.finish()
}

/// Runs ONLY the bodies check phase, given a TypeCheckResult from check_signatures_only.
///
/// Takes `signatures` by reference to avoid a full clone at the call site — only
/// the three fields consumed by `TypeChecker` are cloned individually.
#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(signatures, program))]
pub fn check_bodies_only(
    signatures: &TypeCheckResult,
    program: &Program,
    target_info: TargetInfo,
) -> TypeCheckResult {
    // PERF.5: Arc clone is O(1); unwrap_or_clone only deep-copies when this
    // result is still shared with other Salsa consumers.
    let mut checker = TypeChecker::new(
        Arc::unwrap_or_clone(Arc::clone(&signatures.symbols)),
        Arc::unwrap_or_clone(Arc::clone(&signatures.resolved)),
        signatures.diagnostics.clone(),
        &program.pool,
        target_info,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&signatures.type_info));

    tracing::debug!(
        target: "arandu_typeck",
        before = ?checker.diagnostics,
        "check_bodies_only: entering body check"
    );
    check::check_bodies(&mut checker, program);
    tracing::debug!(
        target: "arandu_typeck",
        after = ?checker.diagnostics,
        "check_bodies_only: finished body check"
    );
    checker.finish()
}

pub use check::program_items::{
    body_item_symbols, check_func_body_only, check_item_body_only, check_non_func_bodies_only,
    free_func_symbols, item_source_span, primary_def_key,
};

// ── TypeChecker state ───────────────────────────────────────────────

pub struct TypeChecker<'a> {
    pub symbols: SymbolTable,
    pub resolved: ResolvedNames,
    pub ctx: TyCtx,
    pub type_info: TypeInfo,
    pub diagnostics: Vec<Diagnostic>,
    solved_constraints: Vec<solver::SolvedConstraint>,
    /// Scope for lowering type expressions inside the current function body.
    type_scope_id: Option<ScopeId>,
    pub pool: &'a AstPool,
    pub target_info: TargetInfo,
    pub current_observed_effects: arandu_middle::EffectFlags,
    pub literal_table: solver::LiteralTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetInfo {
    pub pointer_width: u8,
}

impl TargetInfo {
    /// Maximum value representable by `uint` (native-unsigned-width).
    #[must_use]
    pub const fn uint_max(&self) -> u128 {
        match self.pointer_width {
            32 => u32::MAX as u128,
            64 => u64::MAX as u128,
            // Unknown future width: keep the smallest tested range (32-bit) so
            // the compiler never admits a value the target might reject.
            _ => u32::MAX as u128,
        }
    }

    /// Minimum value representable by `int` (native-signed-width).
    #[must_use]
    pub const fn int_min(&self) -> i128 {
        match self.pointer_width {
            32 => i32::MIN as i128,
            64 => i64::MIN as i128,
            _ => i32::MIN as i128,
        }
    }

    /// Maximum value representable by `int` (native-signed-width).
    #[must_use]
    pub const fn int_max(&self) -> i128 {
        match self.pointer_width {
            32 => i32::MAX as i128,
            64 => i64::MAX as i128,
            _ => i32::MAX as i128,
        }
    }
}

impl<'a> TypeChecker<'a> {
    #[must_use]
    pub fn new(
        symbols: SymbolTable,
        resolved: ResolvedNames,
        diagnostics: Vec<Diagnostic>,
        pool: &'a AstPool,
        target_info: TargetInfo,
    ) -> Self {
        Self::new_with_interner(
            symbols,
            resolved,
            diagnostics,
            pool,
            TypeInterner::new(),
            target_info,
        )
    }

    #[must_use]
    pub fn new_with_interner(
        symbols: SymbolTable,
        resolved: ResolvedNames,
        diagnostics: Vec<Diagnostic>,
        pool: &'a AstPool,
        type_interner: TypeInterner,
        target_info: TargetInfo,
    ) -> Self {
        Self {
            symbols,
            resolved,
            ctx: TyCtx::new(),
            type_info: TypeInfo::with_interner(type_interner),
            diagnostics,
            solved_constraints: Vec::new(),
            type_scope_id: None,
            pool,
            target_info,
            current_observed_effects: arandu_middle::EffectFlags::NONE,
            literal_table: solver::LiteralTable::new(),
        }
    }

    pub fn intern(&mut self, ty: ArType) -> TypeId {
        self.type_info.type_interner.intern(ty)
    }

    #[must_use]
    pub fn resolve(&self, id: TypeId) -> ArType {
        self.type_info.type_interner.resolve(id)
    }

    #[must_use]
    pub fn is_result_type(&self, ty: &ArType) -> bool {
        types::is_result_type(ty, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn result_ok_err(&self, ty: &ArType) -> Option<(ArType, ArType)> {
        types::result_ok_err(ty, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn result_ok_err_ids(&self, id: TypeId) -> Option<(TypeId, TypeId)> {
        types::result_ok_err_id_fast(id, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn try_ok_type(&self, ty: &ArType) -> Option<ArType> {
        types::try_ok_type(ty, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn is_err_type(&self, ty: &ArType) -> bool {
        types::is_err_type(ty, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn unify_return_type(&self, expected: &ArType, actual: &ArType) -> bool {
        types::unify_return_type(expected, actual, &self.type_info.type_interner)
    }

    pub fn lower_type_expr(
        &mut self,
        expr_id: arandu_parser::TypeExprId,
        scope: ScopeId,
    ) -> ArType {
        let ctx = types::LowerCtx {
            pool: self.pool,
            symbols: &self.symbols,
            scope,
            resolved: &self.resolved,
        };
        let ty = arandu_middle::types::lower::lower_type_expr_ctx(
            expr_id,
            &ctx,
            &mut self.type_info.type_interner,
        );
        // T2.1: `Vec<int>` expands to `Vec<int, GlobalAllocator>` when A has a default.
        let ty = types::expand_named_with_defaults(self, ty);
        types::expand_aliases(self, ty)
    }

    pub fn lower_result_type(
        &mut self,
        result: &arandu_parser::ResultType,
        scope: ScopeId,
    ) -> ArType {
        let ctx = types::LowerCtx {
            pool: self.pool,
            symbols: &self.symbols,
            scope,
            resolved: &self.resolved,
        };
        let ty = arandu_middle::types::lower::lower_result_type_ctx(
            result,
            &ctx,
            &mut self.type_info.type_interner,
        );
        // T2.1: expand trailing defaults on Named return types (`Vec<T>` → `Vec<T, Adef>`).
        let ty = types::expand_named_with_defaults(self, ty);
        types::expand_aliases(self, ty)
    }

    pub fn lower_named_type(
        &mut self,
        span: arandu_lexer::Span,
        name: &arandu_parser::TypeName,
        args: &[arandu_parser::TypeExprId],
        scope: ScopeId,
    ) -> ArType {
        let ctx = types::LowerCtx {
            pool: self.pool,
            symbols: &self.symbols,
            scope,
            resolved: &self.resolved,
        };
        let ty = types::lower_named_type(span, name, args, &ctx, &mut self.type_info.type_interner);
        let ty = types::expand_named_with_defaults(self, ty);
        types::expand_aliases(self, ty)
    }

    /// Scope used when lowering type expressions in the current context.
    #[must_use]
    pub(crate) fn type_scope(&self) -> ScopeId {
        self.type_scope_id
            .unwrap_or_else(|| self.symbols.global_scope())
    }
}

pub enum ArTypeOrId {
    Type(ArType),
    Id(TypeId),
}

impl From<ArType> for ArTypeOrId {
    fn from(t: ArType) -> Self {
        ArTypeOrId::Type(t)
    }
}

impl From<TypeId> for ArTypeOrId {
    fn from(id: TypeId) -> Self {
        ArTypeOrId::Id(id)
    }
}

impl TypeChecker<'_> {
    /// Add a type constraint failure. The solver records the structured
    /// constraint, renders the flow-based error message and pushes it to
    /// the diagnostics list.
    pub fn add_constraint(
        &mut self,
        expected: impl Into<ArTypeOrId>,
        found: impl Into<ArTypeOrId>,
        origin: ConstraintOrigin,
    ) {
        let expected = match expected.into() {
            ArTypeOrId::Type(t) => t,
            ArTypeOrId::Id(id) => self.resolve(id),
        };
        let found = match found.into() {
            ArTypeOrId::Type(t) => t,
            ArTypeOrId::Id(id) => self.resolve(id),
        };
        solver::fail_constraint(
            self,
            Constraint {
                expected,
                found,
                is_subtype: false,
                origin,
            },
        );
    }

    pub fn add_subtype_constraint(
        &mut self,
        expected: impl Into<ArTypeOrId>,
        found: impl Into<ArTypeOrId>,
        origin: ConstraintOrigin,
    ) {
        let expected = match expected.into() {
            ArTypeOrId::Type(t) => t,
            ArTypeOrId::Id(id) => self.resolve(id),
        };
        let found = match found.into() {
            ArTypeOrId::Type(t) => t,
            ArTypeOrId::Id(id) => self.resolve(id),
        };
        solver::fail_constraint(
            self,
            Constraint {
                expected,
                found,
                is_subtype: true,
                origin,
            },
        );
    }

    pub(crate) fn record_expr_type(&mut self, expr: ExprId, id: TypeId) {
        self.type_info.record_expr_type(expr, id);
    }

    pub(crate) fn record_decl_type(&mut self, symbol: SymbolId, id: TypeId) {
        self.type_info.record_decl_type(symbol, id);
    }

    #[must_use]
    pub(crate) fn expr_type_id(&self, expr: ExprId) -> Option<TypeId> {
        self.type_info.expr_type_id(expr)
    }

    #[must_use]
    pub(crate) fn decl_type(&self, symbol: SymbolId) -> Option<ArType> {
        self.type_info.decl_type(symbol)
    }

    #[must_use]
    pub(crate) fn decl_type_id(&self, symbol: SymbolId) -> Option<TypeId> {
        self.type_info.decl_type_id(symbol)
    }

    pub(crate) fn unify_ids(&self, a: TypeId, b: TypeId) -> bool {
        if a == b {
            return true;
        }
        let a_ty = self.type_info.resolve_type_id(a);
        let b_ty = self.type_info.resolve_type_id(b);
        types::unify(&a_ty, &b_ty, &self.type_info.type_interner)
    }

    pub(crate) fn is_assignable(&self, actual: TypeId, expected: TypeId) -> bool {
        if actual == expected {
            return true;
        }
        let actual_ty = self.type_info.resolve_type_id(actual);
        let expected_ty = self.type_info.resolve_type_id(expected);
        types::is_assignable(&actual_ty, &expected_ty, &self.type_info.type_interner)
    }

    pub(crate) fn is_assignable_return_type(&self, expected: &ArType, actual: &ArType) -> bool {
        types::is_assignable_return_type(expected, actual, &self.type_info.type_interner)
    }

    #[must_use]
    pub fn finish(self) -> TypeCheckResult {
        TypeCheckResult {
            symbols: Arc::new(self.symbols),
            resolved: Arc::new(self.resolved),
            type_info: Arc::new(self.type_info),
            diagnostics: self.diagnostics,
        }
    }
}

#[cfg(test)]
mod tests;
