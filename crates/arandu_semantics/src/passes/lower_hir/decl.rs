use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{
    HirConst, HirDecl, HirEnum, HirEnumVariant, HirExtern, HirFunc, HirFuncSignature, HirInterface,
    HirParam, HirStruct, HirStructField, HirTypeAlias, ReceiverKind,
};
use crate::passes::lowering::require_def_symbol;
use crate::passes::type_checker::types::ArType;
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_parser::ast_pool::AstPool;
use arandu_parser::{FuncName, TopLevelDecl};

fn error_ty() -> TypeId {
    TypeInterner::preinterned_error_id()
}

pub(crate) fn lower_decl(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    decl: &TopLevelDecl,
) -> Result<Option<HirDecl>, Diagnostic> {
    lower_decl_with_body(type_check, pool, hir_pool, decl, true)
}

pub(crate) fn lower_declaration(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    decl: &TopLevelDecl,
) -> Result<Option<HirDecl>, Diagnostic> {
    lower_decl_with_body(type_check, pool, hir_pool, decl, false)
}

fn lower_decl_with_body(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    decl: &TopLevelDecl,
    include_body: bool,
) -> Result<Option<HirDecl>, Diagnostic> {
    match decl {
        TopLevelDecl::Const(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let ty = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            let value_vid = super::expr::lower_expr(type_check, pool, hir_pool, d.value)?;
            Ok(Some(HirDecl::Const(HirConst {
                symbol,
                ty,
                value: value_vid,
                span: d.span,
            })))
        }
        TopLevelDecl::TypeAlias(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let target = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            Ok(Some(HirDecl::TypeAlias(HirTypeAlias {
                symbol,
                target,
                span: d.span,
            })))
        }
        TopLevelDecl::Func(d) => {
            let name_span = match &d.name {
                arandu_parser::FuncName::Free { span, .. } => *span,
                arandu_parser::FuncName::Method { span, .. } => *span,
            };
            let symbol = require_def_symbol(&type_check.resolved, name_span)?;
            let decl_ty_id = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            let (formal_types, return_type) =
                match type_check.type_info.type_interner.resolve(decl_ty_id) {
                    ArType::Func(formals, ret) => {
                        (type_check.type_info.type_interner.type_args(formals), ret)
                    }
                    _ => (Vec::new(), decl_ty_id),
                };
            let mut params = Vec::new();
            for (index, p) in d.params.iter().enumerate() {
                let p_symbol = require_def_symbol(&type_check.resolved, p.span)?;
                let p_ty = type_check
                    .type_info
                    .decl_type_id(p_symbol)
                    // A declaration-only checker has no body-local parameter
                    // entries. The function signature is the canonical source
                    // of its formal types and borrow modes in that context.
                    .or_else(|| formal_types.get(index).copied())
                    .unwrap_or_else(error_ty);
                params.push(HirParam {
                    symbol: p_symbol,
                    ty: p_ty,
                    span: p.span,
                    is_receiver: p.is_receiver,
                    receiver_kind: receiver_kind(type_check, p.is_receiver, p.ownership, p_ty),
                });
            }
            let params = hir_pool.alloc_param_list(&params);
            let annotation_target = if matches!(d.name, FuncName::Method { .. }) {
                crate::attributes::AnnotationTarget::Method
            } else {
                crate::attributes::AnnotationTarget::Function
            };
            let annotations =
                crate::attributes::validate_attributes(&d.attrs, annotation_target, pool);
            let no_fallback = annotations.contains(crate::attributes::AnnotationId::NoFallback);
            if annotations.contains(crate::attributes::AnnotationId::Destructor) {
                let valid_shape = matches!(d.name, FuncName::Method { .. })
                    && d.params.len() == 1
                    && d.params[0].is_receiver
                    && hir_pool.params_list(params)[0].receiver_kind == Some(ReceiverKind::Own)
                    && matches!(
                        type_check.type_info.type_interner.resolve(return_type),
                        ArType::Void
                    )
                    && !d.is_async;
                if !valid_shape {
                    return Err(Diagnostic::error(
                        crate::DiagCode::T035InvalidDestructor,
                        "@Destructor requires a synchronous consuming method `func Type.name(self: own Type): void` with no additional parameters",
                        d.span,
                    ));
                }
                let receiver_ty = d.params.first().and_then(|_| {
                    match type_check.type_info.type_interner.resolve(decl_ty_id) {
                        ArType::Func(params, _) => type_check
                            .type_info
                            .type_interner
                            .type_args(params)
                            .first()
                            .copied(),
                        _ => None,
                    }
                });
                let Some(receiver_ty) = receiver_ty else {
                    return Err(Diagnostic::error(
                        crate::DiagCode::T035InvalidDestructor,
                        "@Destructor receiver must be a nominal struct or enum type",
                        d.params[0].span,
                    ));
                };
                let Some(receiver_symbol) =
                    (match type_check.type_info.type_interner.resolve(receiver_ty) {
                        ArType::Named(symbol, _) => Some(symbol),
                        _ => None,
                    })
                else {
                    return Err(Diagnostic::error(
                        crate::DiagCode::T035InvalidDestructor,
                        "@Destructor receiver must be a nominal struct or enum type",
                        d.params[0].span,
                    ));
                };
                if let Some(previous) = type_check
                    .type_info
                    .destructors
                    .get(&receiver_symbol)
                    .copied()
                    && previous != symbol
                {
                    return Err(Diagnostic::error(
                        crate::DiagCode::T035InvalidDestructor,
                        "a type may declare only one @Destructor method",
                        d.span,
                    ));
                }
                type_check
                    .type_info_mut()
                    .destructors
                    .insert(receiver_symbol, symbol);
                if !type_check.type_info.generic_params.contains_key(&symbol) {
                    type_check
                        .type_info_mut()
                        .destructor_instances
                        .insert(receiver_ty, symbol);
                }
            }
            Ok(Some(HirDecl::Func(HirFunc {
                symbol,
                params,
                return_type,
                body: if include_body {
                    Some(super::stmt::lower_block(
                        type_check, pool, hir_pool, &d.body,
                    )?)
                } else {
                    None
                },
                span: d.span,
                is_async: d.is_async,
                no_fallback,
            })))
        }
        TopLevelDecl::Struct(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let mut fields = Vec::new();
            if let Some(struct_fields_map) = type_check.type_info.struct_fields.get(&symbol) {
                for f in &d.fields {
                    let field_symbol = require_def_symbol(&type_check.resolved, f.span)?;
                    let field_ty = struct_fields_map
                        .get(f.name.as_str())
                        .map(|f| f.ty)
                        .unwrap_or_else(error_ty);
                    fields.push(HirStructField {
                        symbol: field_symbol,
                        ty: field_ty,
                        span: f.span,
                    });
                }
            }
            let fields = hir_pool.alloc_struct_field_list(&fields);
            Ok(Some(HirDecl::Struct(HirStruct {
                symbol,
                fields,
                span: d.span,
            })))
        }
        TopLevelDecl::Enum(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let mut variants = Vec::new();
            for v in &d.variants {
                let v_symbol = require_def_symbol(&type_check.resolved, v.span)?;
                let payload =
                    type_check
                        .type_info
                        .enum_variants
                        .get(&v_symbol)
                        .and_then(|(_, shape)| match shape {
                            crate::passes::type_checker::EnumPayloadShape::Unit => None,
                            crate::passes::type_checker::EnumPayloadShape::Tuple(tids) => {
                                if tids.is_empty() {
                                    None
                                } else if tids.len() == 1 {
                                    Some(tids[0])
                                } else {
                                    let interner = &type_check.type_info.type_interner;
                                    Some(interner.intern(ArType::tuple(tids, interner)))
                                }
                            }
                        });
                variants.push(HirEnumVariant {
                    symbol: v_symbol,
                    payload,
                    span: v.span,
                });
            }
            let variants = hir_pool.alloc_enum_variant_list(&variants);
            Ok(Some(HirDecl::Enum(HirEnum {
                symbol,
                variants,
                span: d.span,
            })))
        }
        TopLevelDecl::Interface(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            Ok(Some(HirDecl::Interface(HirInterface {
                symbol,
                span: d.span,
            })))
        }
        TopLevelDecl::Extern(d) => {
            let mut members = Vec::new();
            for m in &d.members {
                let symbol = require_def_symbol(&type_check.resolved, m.span)?;
                let m_ty_id = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or_else(error_ty);
                let return_type = match type_check.type_info.type_interner.resolve(m_ty_id) {
                    ArType::Func(_, ret) => ret,
                    _ => m_ty_id,
                };
                let mut params = Vec::new();
                for p in &m.params {
                    let p_symbol = require_def_symbol(&type_check.resolved, p.span)?;
                    let p_ty = type_check
                        .type_info
                        .decl_type_id(p_symbol)
                        .unwrap_or_else(error_ty);
                    params.push(HirParam {
                        symbol: p_symbol,
                        ty: p_ty,
                        span: p.span,
                        is_receiver: p.is_receiver,
                        receiver_kind: receiver_kind(type_check, p.is_receiver, p.ownership, p_ty),
                    });
                }
                let params = hir_pool.alloc_param_list(&params);
                members.push(HirFuncSignature {
                    symbol,
                    params,
                    return_type,
                    span: m.span,
                });
            }
            let members = hir_pool.alloc_func_signature_list(&members);
            Ok(Some(HirDecl::Extern(HirExtern {
                abi: arandu_parser::AbiKind::from_abi_str(&d.abi),
                members,
                span: d.span,
            })))
        }
        TopLevelDecl::Submodule(_) | TopLevelDecl::Error(_) => Ok(None),
    }
}

fn receiver_kind(
    type_check: &TypeCheckResult,
    is_receiver: bool,
    ownership: Option<arandu_parser::Ownership>,
    ty: arandu_middle::types::TypeId,
) -> Option<ReceiverKind> {
    if !is_receiver {
        return None;
    }
    ownership
        .map(super::stmt::ownership_to_receiver_kind)
        .or_else(|| {
            Some(match type_check.type_info.type_interner.resolve(ty) {
                ArType::Ref(_) => ReceiverKind::Shared,
                ArType::RefMut(_) => ReceiverKind::Mut,
                _ => ReceiverKind::Own,
            })
        })
}
