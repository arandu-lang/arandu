//! Per-item body typeck (P1 functions + P2 all top-level body items).

use super::super::TypeCheckResult;
use super::super::TypeChecker;
use super::super::synth::synth_expr;
use super::func::check_func_body;
use super::validate::validate_top_level_any;
use crate::type_checker::TargetInfo;
use crate::{NodeKey, ResolvedNames, SymbolId};
use arandu_parser::{FuncDecl, Program, TopLevelDecl};
use std::sync::Arc;

fn func_name_key(decl: &FuncDecl) -> NodeKey {
    let name_span = match &decl.name {
        arandu_parser::FuncName::Free { span, .. } => *span,
        arandu_parser::FuncName::Method { span, .. } => *span,
    };
    NodeKey::from(name_span)
}

/// Primary definition key used by resolve for this top-level decl (if any).
#[must_use]
pub fn primary_def_key(decl: &TopLevelDecl) -> Option<NodeKey> {
    match decl {
        TopLevelDecl::Const(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::TypeAlias(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::Func(d) => Some(func_name_key(d)),
        TopLevelDecl::Struct(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::Enum(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::Interface(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::Submodule(d) => Some(NodeKey::from(d.span)),
        TopLevelDecl::Extern(d) => d.members.first().map(|m| NodeKey::from(m.span)),
        TopLevelDecl::Error(_) => None,
    }
}

/// Full item span used for source fingerprinting.
#[must_use]
pub fn item_source_span(decl: &TopLevelDecl) -> arandu_lexer::Span {
    decl.span()
}

fn find_decl_for_symbol<'a>(
    program: &'a Program,
    resolved: &ResolvedNames,
    item_sym: SymbolId,
) -> Option<&'a TopLevelDecl> {
    let mut found = None;
    program.for_each_decl_recursive(|_decl_id, decl| {
        if found.is_some() {
            return;
        }
        if let Some(key) = primary_def_key(decl)
            && resolved.definitions.get(&key) == Some(&item_sym)
        {
            found = Some(decl);
            return;
        }
        // Extern: any member symbol maps to the whole extern block.
        if let TopLevelDecl::Extern(ext) = decl {
            for member in &ext.members {
                let mkey = NodeKey::from(member.span);
                if resolved.definitions.get(&mkey) == Some(&item_sym) {
                    found = Some(decl);
                    return;
                }
            }
        }
    });
    found
}

/// Free + method function symbols (P1 helper; subset of [`body_item_symbols`]).
#[must_use]
pub fn free_func_symbols(program: &Program, resolved: &ResolvedNames) -> Vec<SymbolId> {
    let mut out = Vec::new();
    program.for_each_decl_recursive(|_decl_id, decl| {
        if let TopLevelDecl::Func(func_decl) = decl {
            let key = func_name_key(func_decl);
            if let Some(&id) = resolved.definitions.get(&key) {
                out.push(id);
            }
        }
    });
    out.sort_by_key(|s| (s.file_id, s.local_id.0));
    out.dedup();
    out
}

/// All top-level items that participate in the body typeck phase (P2).
///
/// Includes funcs, consts, structs, enums, type aliases, interfaces, and the
/// primary symbol of each extern block.
#[must_use]
pub fn body_item_symbols(program: &Program, resolved: &ResolvedNames) -> Vec<SymbolId> {
    let mut out = Vec::new();
    program.for_each_decl_recursive(|_decl_id, decl| {
        let Some(key) = primary_def_key(decl) else {
            return;
        };
        if let Some(&id) = resolved.definitions.get(&key) {
            out.push(id);
        }
    });
    out.sort_by_key(|s| (s.file_id, s.local_id.0));
    out.dedup();
    out
}

/// Type-check **one** free/method function body (P1).
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_typeck",
    skip(signatures, program),
    fields(func = ?func_sym)
)]
pub fn check_func_body_only(
    signatures: &TypeCheckResult,
    program: &Program,
    func_sym: SymbolId,
    target_info: TargetInfo,
) -> TypeCheckResult {
    check_item_body_only(signatures, program, func_sym, target_info)
}

/// Type-check **one** top-level item body/validate phase (P2).
///
/// Diagnostics are only those produced for this item. `TypeInfo` starts from
/// signatures and records this item's contributions (merge via `merge_from`).
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_typeck",
    skip(signatures, program),
    fields(item = ?item_sym)
)]
pub fn check_item_body_only(
    signatures: &TypeCheckResult,
    program: &Program,
    item_sym: SymbolId,
    target_info: TargetInfo,
) -> TypeCheckResult {
    check_item_body_with_substitution(
        signatures,
        program,
        item_sym,
        target_info,
        &arandu_middle::types::GenericSubst::new(),
    )
}

/// Canonical body checker with arguments supplied by the pre-body instance
/// staging edge. No query or VM callback is executed by this pure API.
#[must_use]
pub fn check_item_body_with_substitution(
    signatures: &TypeCheckResult,
    program: &Program,
    item_sym: SymbolId,
    target_info: TargetInfo,
    substitution: &arandu_middle::types::GenericSubst,
) -> TypeCheckResult {
    let mut checker = TypeChecker::new(
        Arc::clone(&signatures.symbols),
        Arc::clone(&signatures.resolved),
        Vec::new(),
        &program.pool,
        target_info,
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

    if let Some(decl) = find_decl_for_symbol(program, &checker.resolved, item_sym) {
        check_one_item_body(&mut checker, program, decl);
    }

    checker.finish()
}

/// Check one residual static iteration using the owner's return contract and
/// ordinary locals. Staged expressions remain isolated obligations.
#[must_use]
pub fn check_residual_loop_body(
    initial: &TypeCheckResult,
    program: &Program,
    owner: SymbolId,
    block: &arandu_parser::Block,
    iteration: (SymbolId, arandu_middle::types::TypeId),
    target: TargetInfo,
    substitution: &arandu_middle::types::GenericSubst,
) -> TypeCheckResult {
    let (binding, ty) = iteration;
    let mut checker = TypeChecker::new(
        Arc::clone(&initial.symbols),
        Arc::clone(&initial.resolved),
        Vec::new(),
        &program.pool,
        target,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&initial.type_info));
    checker.type_info.header_requests.clear();
    checker.generic_substitution = substitution.clone();
    for (&symbol, &ty) in &checker.type_info.decl_types {
        checker.ctx.bind(symbol, ty);
    }

    checker.ctx.bind(binding, ty);
    checker.record_decl_type(binding, ty);
    if let Some(TopLevelDecl::Func(function)) =
        find_decl_for_symbol(program, &checker.resolved, owner)
    {
        let scope = super::func::func_type_scope(&checker, function);
        checker.type_scope_id = Some(scope);
        let ret = function
            .result
            .as_ref()
            .map(|result| checker.lower_result_type(result, scope))
            .unwrap_or(arandu_middle::types::ArType::Void);
        let ret = checker.intern(ret);
        checker.ctx.push_return(ret, function.span);
    }
    checker.ctx.enter_loop();
    super::block::check_block(&mut checker, &program.pool, block);
    checker.finalize_literal_vars();
    checker.finish()
}

fn check_one_item_body(checker: &mut TypeChecker<'_>, program: &Program, decl: &TopLevelDecl) {
    match decl {
        TopLevelDecl::Func(func_decl) => {
            validate_top_level_any(checker, decl);
            check_func_body(checker, func_decl);
        }
        TopLevelDecl::Const(const_decl) => {
            validate_top_level_any(checker, decl);
            let key = NodeKey::from(const_decl.span);
            if let Some(&symbol) = checker.resolved.definitions.get(&key)
                && checker.type_info.ctfe_global_values.contains_key(&symbol)
                && let Some(ty) = checker.type_info.decl_type_id(symbol)
            {
                checker.type_info.record_expr_type(const_decl.value, ty);
                return;
            }
            let val_ty = synth_expr(checker, const_decl.value);
            let const_key = NodeKey::from(const_decl.span);
            if let Some(&symbol_id) = checker.resolved.definitions.get(&const_key) {
                checker.record_decl_type(symbol_id, val_ty);
                if let Some(var_id) = checker.literal_table.var_for_expr(const_decl.value) {
                    checker.literal_table.bind_symbol(symbol_id, var_id);
                }
            }
            checker.finalize_literal_vars();
        }
        TopLevelDecl::Extern(extern_decl) => {
            validate_top_level_any(checker, decl);
            if arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                == arandu_parser::AbiKind::AranduIntrinsic
            {
                let module_name = program
                    .module
                    .as_ref()
                    .map(|m| m.path.join("."))
                    .unwrap_or_default();
                if !module_name.starts_with("std.core") {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::U001FeatureNotSupported,
                        "the 'arandu-intrinsic' ABI is restricted to the std.core module"
                            .to_string(),
                        extern_decl.span,
                    ));
                }
            }
        }
        TopLevelDecl::Struct(_)
        | TopLevelDecl::Enum(_)
        | TopLevelDecl::TypeAlias(_)
        | TopLevelDecl::Interface(_)
        | TopLevelDecl::Submodule(_) => {
            validate_top_level_any(checker, decl);
        }
        TopLevelDecl::Error(_) => {}
    }
}

/// Residual body work for decls without a primary definition key (errors only).
/// Kept for parity; normally empty when all items go through [`check_item_body_only`].
#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(signatures, program))]
pub fn check_non_func_bodies_only(
    signatures: &TypeCheckResult,
    program: &Program,
    target_info: TargetInfo,
) -> TypeCheckResult {
    let mut checker = TypeChecker::new(
        Arc::clone(&signatures.symbols),
        Arc::clone(&signatures.resolved),
        Vec::new(),
        &program.pool,
        target_info,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&signatures.type_info));

    // Residuals without a primary symbol are already covered by items; keep empty shell.
    let _ = program;
    checker.finish()
}
