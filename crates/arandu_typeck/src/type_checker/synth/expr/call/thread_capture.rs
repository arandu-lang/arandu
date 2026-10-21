//! Synthesize the `ThreadJob` behind an accepted `move ||` argument.
//!
//! The struct and `run` are span-derived symbols. They are not dense locals,
//! so parallel item typeck of the same signature table cannot reuse one id.

use arandu_lexer::Span;
use arandu_middle::layout::{StructFieldInfo, StructFields};
use arandu_middle::{Symbol, SymbolId, SymbolKind, Visibility};
use arandu_parser::ast_pool::ExprId;
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use std::sync::Arc;

use crate::type_checker::TypeChecker;
use crate::type_checker::info::{ThreadCaptureField, ThreadCaptureRecipe};
use crate::type_checker::types::ArType;
use crate::type_checker::types::TypeId;

pub(super) struct OwnedCapture {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub ty: TypeId,
    pub span: Span,
}

pub(super) struct CaptureInstall<'a> {
    pub call_expr: ExprId,
    pub callee_id: ExprId,
    pub arg_id: ExprId,
    pub arg_span: Span,
    pub captures: &'a [OwnedCapture],
    pub ret_ty: TypeId,
    pub res_ty: TypeId,
}

pub(super) fn install_thread_capture(
    checker: &mut TypeChecker<'_>,
    install: CaptureInstall<'_>,
) -> TypeId {
    let CaptureInstall {
        call_expr,
        callee_id,
        arg_id,
        arg_span,
        captures,
        ret_ty: ret_ty_id,
        res_ty,
    } = install;
    let file_id = checker.symbols.file_id;
    let mut reserved = FxHashSet::default();
    let struct_symbol = mint(checker, file_id, arg_span, 0, &mut reserved);
    let run_symbol = mint(checker, file_id, arg_span, 1, &mut reserved);
    let self_param = mint(checker, file_id, arg_span, 2, &mut reserved);
    let arg_param = mint(checker, file_id, arg_span, 3, &mut reserved);
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
            arg_span,
            4 + u32::try_from(index).unwrap_or(u32::MAX),
            &mut reserved,
        );
        fields.push(ThreadCaptureField {
            name,
            symbol,
            source: capture.symbol,
            ty: capture.ty,
            span: capture.span,
        });
    }

    let struct_name = format!("__ThreadCapture_{}_{}", arg_span.start, arg_span.end);
    let host_name = SmolStr::new(format!(
        "ar_thread_capture_{}_{}_{}_run",
        file_id, arg_span.start, arg_span.end
    ));
    let scope = checker.symbols.global_scope();
    let symbols = Arc::make_mut(&mut checker.symbols);
    register(
        symbols,
        struct_symbol,
        &struct_name,
        SymbolKind::Struct,
        arg_span,
        scope,
    );
    register(
        symbols,
        run_symbol,
        &format!("{struct_name}.run"),
        SymbolKind::AssociatedFunc,
        arg_span,
        scope,
    );
    register(
        symbols,
        self_param,
        "self",
        SymbolKind::Param,
        arg_span,
        scope,
    );
    register(
        symbols,
        arg_param,
        "argument",
        SymbolKind::Param,
        arg_span,
        scope,
    );
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
        .associated_members
        .insert((struct_symbol, "run".into()), run_symbol);
    symbols
        .host_function_names
        .insert(run_symbol, host_name.clone());

    let struct_named = ArType::named(struct_symbol, &[], &checker.type_info.type_interner);
    let struct_ty = checker.intern(struct_named);
    let void_ty = checker.intern(ArType::Void);
    let run_named = ArType::func(
        &[struct_ty, void_ty],
        ret_ty_id,
        &checker.type_info.type_interner,
    );
    let run_ty = checker.intern(run_named);
    checker.record_decl_type(struct_symbol, struct_ty);
    checker.record_decl_type(run_symbol, run_ty);
    checker.record_decl_type(self_param, struct_ty);
    checker.record_decl_type(arg_param, void_ty);
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
    checker.type_info.thread_captures.insert(
        arg_span,
        ThreadCaptureRecipe {
            lambda_span: arg_span,
            struct_symbol,
            run_symbol,
            self_param,
            arg_param,
            fields,
            return_type: ret_ty_id,
            host_name,
        },
    );

    checker.record_expr_type(arg_id, struct_ty);
    let call_func_ty = ArType::func(&[struct_ty], res_ty, &checker.type_info.type_interner);
    let call_func_ty_id = checker.intern(call_func_ty);
    checker.record_expr_type(callee_id, call_func_ty_id);
    checker.record_expr_type(call_expr, res_ty);
    res_ty
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
