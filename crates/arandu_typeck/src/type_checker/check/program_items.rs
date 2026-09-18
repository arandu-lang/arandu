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
    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        let Some(key) = primary_def_key(decl) else {
            continue;
        };
        if resolved.definitions.get(&key) == Some(&item_sym) {
            return Some(decl);
        }
        // Extern: any member symbol maps to the whole extern block.
        if let TopLevelDecl::Extern(ext) = decl {
            for member in &ext.members {
                let mkey = NodeKey::from(member.span);
                if resolved.definitions.get(&mkey) == Some(&item_sym) {
                    return Some(decl);
                }
            }
        }
    }
    None
}

/// Free + method function symbols (P1 helper; subset of [`body_item_symbols`]).
#[must_use]
pub fn free_func_symbols(program: &Program, resolved: &ResolvedNames) -> Vec<SymbolId> {
    let mut out = Vec::new();
    for decl_id in &program.decls {
        if let TopLevelDecl::Func(func_decl) = program.pool.decl(*decl_id) {
            let key = func_name_key(func_decl);
            if let Some(&id) = resolved.definitions.get(&key) {
                out.push(id);
            }
        }
    }
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
    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        let Some(key) = primary_def_key(decl) else {
            continue;
        };
        if let Some(&id) = resolved.definitions.get(&key) {
            out.push(id);
        }
    }
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
    let mut checker = TypeChecker::new(
        Arc::unwrap_or_clone(Arc::clone(&signatures.symbols)),
        Arc::unwrap_or_clone(Arc::clone(&signatures.resolved)),
        Vec::new(),
        &program.pool,
        target_info,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&signatures.type_info));

    if let Some(decl) = find_decl_for_symbol(program, &checker.resolved, item_sym) {
        check_one_item_body(&mut checker, program, decl);
    }

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
        | TopLevelDecl::Interface(_) => {
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
        Arc::unwrap_or_clone(Arc::clone(&signatures.symbols)),
        Arc::unwrap_or_clone(Arc::clone(&signatures.resolved)),
        Vec::new(),
        &program.pool,
        target_info,
    );
    checker.type_info = Arc::unwrap_or_clone(Arc::clone(&signatures.type_info));

    // Residuals without a primary symbol are already covered by items; keep empty shell.
    let _ = program;
    checker.finish()
}
