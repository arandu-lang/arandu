//! Pure initial typing/capture checks for isolated CTFE roots. Query selection,
//! AMIR interpretation and residual materialization belong to later stages.

use std::sync::Arc;

use arandu_middle::{Span, SymbolId, SymbolKind};
use arandu_parser::{
    Block,
    ast_pool::{AstPool, ExprId},
};

use super::super::{
    TargetInfo, TypeCheckResult, TypeChecker, constraints::ConstraintOrigin, types::ArType,
};
use super::{CtfeBlockType, check_ctfe_block};

#[derive(Debug, Clone, Copy)]
pub enum CtfeInitialRoot<'a> {
    Block(&'a Block),
    Expression(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtfeCapture {
    pub symbol: SymbolId,
    pub use_span: Span,
    pub declaration_span: Span,
}

/// Inspect resolved reads and assignment places deterministically. Enclosing
/// `let`s remain runtime values even if their initializer is a literal; only
/// declarations contained in this root belong to its frame.
#[must_use]
pub fn find_ctfe_runtime_capture(
    signatures: &TypeCheckResult,
    pool: &AstPool,
    root_span: Span,
) -> Option<CtfeCapture> {
    find_ctfe_runtime_capture_except(signatures, pool, root_span, &[])
}

#[must_use]
pub fn find_ctfe_runtime_capture_except(
    signatures: &TypeCheckResult,
    pool: &AstPool,
    root_span: Span,
    exempt: &[SymbolId],
) -> Option<CtfeCapture> {
    let contains = |inner: Span| {
        root_span.file_id == inner.file_id
            && root_span.start <= inner.start
            && inner.end <= root_span.end
    };
    let reads = signatures
        .resolved
        .expr_symbols
        .iter()
        .zip(&pool.expr_spans)
        .filter_map(|(symbol, span)| symbol.map(|symbol| (*span, symbol)));
    let writes = signatures
        .resolved
        .value_refs
        .iter()
        .map(|(key, &symbol)| (Span::new(root_span.file_id, key.start, key.end), symbol));
    reads
        .chain(writes)
        .filter(|(span, _)| contains(*span))
        .filter_map(|(use_span, id)| {
            let symbol = signatures.symbols.try_get(id)?;
            (matches!(symbol.kind, SymbolKind::Local | SymbolKind::Param)
                && !contains(symbol.span)
                && !exempt.contains(&id))
            .then_some(CtfeCapture {
                symbol: id,
                use_span,
                declaration_span: symbol.span,
            })
        })
        .min_by_key(|capture| {
            (
                capture.use_span.start,
                capture.use_span.end,
                capture.symbol.file_id,
                capture.symbol.local_id.0,
            )
        })
}

/// Borrow the canonical AST/pool and a declaration context. No runtime parent
/// parameters or body are typed, and there is no Salsa/VM callback in this API.
/// The caller filters diagnostics to this root/import boundary beforehand.
#[must_use]
pub fn check_ctfe_root(
    signatures: &TypeCheckResult,
    pool: &AstPool,
    root: CtfeInitialRoot<'_>,
    expected: Option<ArType>,
    target: TargetInfo,
) -> (TypeCheckResult, CtfeBlockType) {
    check_ctfe_root_with_substitution(
        signatures,
        pool,
        root,
        expected,
        target,
        &arandu_middle::types::GenericSubst::new(),
    )
}

#[must_use]
pub fn check_ctfe_root_with_substitution(
    signatures: &TypeCheckResult,
    pool: &AstPool,
    root: CtfeInitialRoot<'_>,
    expected: Option<ArType>,
    target: TargetInfo,
    substitution: &arandu_middle::types::GenericSubst,
) -> (TypeCheckResult, CtfeBlockType) {
    let mut checker = TypeChecker::new(
        Arc::clone(&signatures.symbols),
        Arc::clone(&signatures.resolved),
        signatures.diagnostics.clone(),
        pool,
        target,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&signatures.type_info));
    checker.type_info.header_requests.clear();
    checker.generic_substitution = substitution.clone();
    if !substitution.is_empty() {
        for ty in checker.type_info.decl_types.values_mut() {
            *ty = arandu_middle::types::substitute_type_id(
                *ty,
                substitution,
                &checker.type_info.type_interner,
            );
        }
    }
    let expected = expected.map(|ty| checker.intern(ty));
    let mut typed = match root {
        CtfeInitialRoot::Block(block) => check_ctfe_block(&mut checker, block, expected),
        CtfeInitialRoot::Expression(expression) => {
            let ty = super::super::synth::synth_expr_expected(&mut checker, expression, expected);
            if let Some(expected) = expected
                && !checker.unify_ids(expected, ty)
            {
                let span = pool.expr_span(expression);
                checker.add_constraint(
                    expected,
                    ty,
                    ConstraintOrigin::CtfeResult {
                        value_span: span,
                        block_span: span,
                    },
                );
            }
            CtfeBlockType {
                return_type: ty,
                observed_effects: checker.current_observed_effects,
                value_tail: Some(expression),
            }
        }
    };
    checker.finalize_literal_vars();
    if expected.is_none() {
        let span = match root {
            CtfeInitialRoot::Expression(expression) => pool.expr_span(expression),
            CtfeInitialRoot::Block(block) => block.span,
        };
        // Literal occurrences are solved above. Compound expressions (notably
        // if/group tails) can still carry the inference-only literal type even
        // though their children are concrete. An unconstrained isolated root
        // must finish with ordinary defaults, not pass pseudo-types to AMIR.
        for (index, ty) in checker.type_info.expr_types.iter_mut().enumerate() {
            let Some(source) = pool.expr_spans.get(index) else {
                continue;
            };
            if source.file_id == span.file_id
                && span.start <= source.start
                && source.end <= span.end
                && let Some(id) = ty
            {
                let inferred = checker.type_info.type_interner.resolve(*id);
                if inferred.is_literal() {
                    *id = checker
                        .type_info
                        .type_interner
                        .intern(inferred.default_literal());
                }
            }
        }
    }
    if let CtfeInitialRoot::Expression(expression) = root {
        typed.return_type = checker
            .type_info
            .expr_type_id(expression)
            .unwrap_or(typed.return_type);
    }
    (checker.finish(), typed)
}
