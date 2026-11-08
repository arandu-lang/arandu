//! Synthesize the capture struct and method behind an accepted lambda.
//!
//! A `move ||` job gets `run`; a callback argument gets `call`. The struct and
//! method are span-derived symbols. They are not dense locals, so parallel
//! item typeck of the same signature table cannot reuse one id.

use arandu_lexer::Span;
use arandu_middle::layout::{StructFieldInfo, StructFields};
use arandu_middle::{Symbol, SymbolId, SymbolKind, Visibility};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use std::sync::Arc;

use crate::type_checker::TypeChecker;
use crate::type_checker::info::{CaptureMode, ClosureCaptureField, ClosureParam, ClosureRecipe};
use crate::type_checker::types::ArType;
use crate::type_checker::types::TypeId;

pub(super) struct Capture {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub ty: TypeId,
    pub span: Span,
    pub mode: CaptureMode,
}

/// What the closure method looks like: its name, the struct and host name
/// prefixes, and its real parameters. `None` symbols are minted as `argument`.
pub(super) struct ClosureShape<'a> {
    pub method: &'static str,
    pub struct_prefix: &'static str,
    pub host_prefix: &'static str,
    pub params: &'a [(Option<SymbolId>, TypeId)],
    pub return_type: TypeId,
}

/// Registers the struct, its fields and the method; returns the struct type.
pub(super) fn install_closure(
    checker: &mut TypeChecker<'_>,
    lambda_span: Span,
    shape: &ClosureShape<'_>,
    captures: &[Capture],
) -> TypeId {
    let file_id = checker.symbols.file_id;
    let mut reserved = FxHashSet::default();
    let struct_symbol = mint(checker, file_id, lambda_span, 0, &mut reserved);
    let method_symbol = mint(checker, file_id, lambda_span, 1, &mut reserved);
    let self_param = mint(checker, file_id, lambda_span, 2, &mut reserved);
    let mut minted_params = Vec::new();
    let params = shape
        .params
        .iter()
        .enumerate()
        .map(|(index, &(symbol, ty))| {
            let symbol = symbol.unwrap_or_else(|| {
                let role = 3u32.wrapping_add(u32::try_from(index).unwrap_or(u32::MAX) << 8);
                let minted = mint(checker, file_id, lambda_span, role, &mut reserved);
                minted_params.push(minted);
                minted
            });
            ClosureParam { symbol, ty }
        })
        .collect::<Vec<_>>();
    let mut used_names = FxHashSet::default();
    let mut fields = Vec::with_capacity(captures.len());
    for (index, capture) in captures.iter().enumerate() {
        let mut name = capture.name.clone();
        if !used_names.insert(name.clone()) {
            name = SmolStr::new(format!("{name}_{}", capture.symbol.local_id.0));
            used_names.insert(name.clone());
        }
        let symbol = mint(
            checker,
            file_id,
            lambda_span,
            4 + u32::try_from(index).unwrap_or(u32::MAX),
            &mut reserved,
        );
        fields.push(ClosureCaptureField {
            name,
            symbol,
            source: capture.symbol,
            ty: capture.ty,
            span: capture.span,
            mode: capture.mode,
        });
    }

    let struct_name = SmolStr::new(format!(
        "{}_{}_{}",
        shape.struct_prefix, lambda_span.start, lambda_span.end
    ));
    let method_name = SmolStr::new(shape.method);
    let host_name = SmolStr::new(format!(
        "{}_{}_{}_{}_{}",
        shape.host_prefix, file_id, lambda_span.start, lambda_span.end, shape.method
    ));
    let scope = checker.symbols.global_scope();
    let symbols = Arc::make_mut(&mut checker.symbols);
    register(
        symbols,
        struct_symbol,
        &struct_name,
        SymbolKind::Struct,
        lambda_span,
        scope,
    );
    register(
        symbols,
        method_symbol,
        &format!("{struct_name}.{method_name}"),
        SymbolKind::AssociatedFunc,
        lambda_span,
        scope,
    );
    register(
        symbols,
        self_param,
        "self",
        SymbolKind::Param,
        lambda_span,
        scope,
    );
    for &param in &minted_params {
        register(
            symbols,
            param,
            "argument",
            SymbolKind::Param,
            lambda_span,
            scope,
        );
    }
    for field in &fields {
        register(
            symbols,
            field.symbol,
            &field.name,
            SymbolKind::Field,
            field.span,
            scope,
        );
    }
    symbols
        .associated_members_mut()
        .insert((struct_symbol, method_name.clone()), method_symbol);
    symbols
        .host_function_names
        .insert(method_symbol, host_name.clone());

    let struct_named = ArType::named(struct_symbol, &[], &checker.type_info.type_interner);
    let struct_ty = checker.intern(struct_named);
    let mut method_params = Vec::with_capacity(params.len() + 1);
    method_params.push(struct_ty);
    method_params.extend(params.iter().map(|param| param.ty));
    let method_named = ArType::func(
        &method_params,
        shape.return_type,
        &checker.type_info.type_interner,
    );
    let method_ty = checker.intern(method_named);
    checker.record_decl_type(struct_symbol, struct_ty);
    checker.record_decl_type(method_symbol, method_ty);
    checker.record_decl_type(self_param, struct_ty);
    for param in &params {
        checker.record_decl_type(param.symbol, param.ty);
    }
    for field in &fields {
        checker.record_decl_type(field.symbol, field.ty);
    }
    let table = StructFields::from_entries(fields.iter().enumerate().map(|(index, field)| {
        StructFieldInfo {
            name: field.name.clone(),
            symbol: Some(field.symbol),
            ty: field.ty,
            index,
        }
    }));
    checker
        .type_info
        .struct_fields
        .insert(struct_symbol, Arc::new(table));
    checker.type_info.closures.insert(
        lambda_span,
        ClosureRecipe {
            lambda_span,
            struct_symbol,
            struct_name,
            method_symbol,
            method_name,
            self_param,
            params,
            fields,
            return_type: shape.return_type,
            host_name,
        },
    );
    struct_ty
}

fn register(
    symbols: &mut arandu_middle::SymbolTable,
    id: SymbolId,
    name: &str,
    kind: SymbolKind,
    span: Span,
    scope: arandu_middle::symbol_table::ScopeId,
) {
    symbols.register_synthetic(Symbol {
        id,
        name: name.into(),
        kind,
        span,
        scope,
        visibility: Visibility::Public,
        lang_item: None,
    });
}

fn mint(
    checker: &TypeChecker<'_>,
    file_id: arandu_middle::db::FileId,
    span: Span,
    mut role: u32,
    reserved: &mut FxHashSet<u32>,
) -> SymbolId {
    loop {
        let mixed = span.start.wrapping_mul(0x9E37_79B1)
            ^ span.end.wrapping_mul(0x85EB_CA6B)
            ^ role.wrapping_mul(0xC2B2_AE35);
        let local = SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF);
        let id = SymbolId::new(file_id, local);
        if reserved.insert(local) && checker.symbols.try_get(id).is_none() {
            return id;
        }
        role = role.wrapping_add(0x1_0000);
        if role == 0 {
            return SymbolId::new(file_id, SymbolId::SYNTHETIC_LOCAL_BIT | 1);
        }
    }
}
