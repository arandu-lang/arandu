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
            if annotations.contains(crate::attributes::AnnotationId::Export) {
                lower_export(type_check, pool, d, symbol, &formal_types, return_type)?;
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

/// Applies `@Export` to a free function: validates that the signature crosses
/// the C ABI without an implicit transformation, then records the flat symbol
/// name used by every native backend.
fn lower_export(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    declaration: &arandu_parser::FuncDecl,
    symbol: arandu_middle::SymbolId,
    formal_types: &[TypeId],
    return_type: TypeId,
) -> Result<(), Diagnostic> {
    if matches!(declaration.name, FuncName::Method { .. }) {
        return Err(Diagnostic::error(
            crate::DiagCode::T049InvalidFfiExport,
            "@Export is only valid on free functions; methods have no C ABI",
            declaration.span,
        ));
    }
    if declaration.is_async {
        return Err(Diagnostic::error(
            crate::DiagCode::T049InvalidFfiExport,
            "@Export cannot be applied to an async function; the C ABI has no suspension point",
            declaration.span,
        ));
    }
    if type_check.type_info.generic_params.contains_key(&symbol) {
        return Err(Diagnostic::error(
            crate::DiagCode::T049InvalidFfiExport,
            "@Export requires a monomorphic function; add a concrete wrapper instead of exporting a generic",
            declaration.span,
        ));
    }

    let interner = &type_check.type_info.type_interner;
    let mut has_aggregate = false;
    let mut resolved_params = Vec::with_capacity(formal_types.len());
    for ty in formal_types {
        let resolved = interner.resolve(*ty);
        has_aggregate |= arandu_middle::layout::is_export_aggregate(&resolved);
        resolved_params.push(resolved);
    }
    let resolved_return = interner.resolve(return_type);
    has_aggregate |= arandu_middle::layout::is_export_aggregate(&resolved_return);
    if has_aggregate {
        let target = &type_check.type_info.target_identity;
        // An empty identity means the driver did not configure a target (unit
        // tests and the pure semantic API). The backend enforces the same rule
        // with the real triple before emitting.
        if !target.arch.is_empty()
            && arandu_middle::layout::ExportAbi::for_target(
                &target.arch,
                &target.os,
                &target.triple,
            )
            .is_none()
        {
            return Err(Diagnostic::error(
                crate::DiagCode::T049InvalidFfiExport,
                format!(
                    "aggregates by value are not classified for target '{}'; pass scalars or pointers",
                    if target.triple.is_empty() {
                        &target.arch
                    } else {
                        &target.triple
                    }
                ),
                declaration.span,
            ));
        }
    }
    for (index, ty) in resolved_params.into_iter().enumerate() {
        if let Some(reason) = c_export_type_error(type_check, ty, false, 0) {
            let span = declaration
                .params
                .get(index)
                .map_or(declaration.span, |param| param.span);
            return Err(Diagnostic::error(
                crate::DiagCode::T049InvalidFfiExport,
                format!("cannot export parameter {}: {reason}", index + 1),
                span,
            ));
        }
    }
    if let Some(reason) = c_export_type_error(type_check, resolved_return, true, 0) {
        return Err(Diagnostic::error(
            crate::DiagCode::T049InvalidFfiExport,
            format!("cannot export return type: {reason}"),
            declaration.span,
        ));
    }

    let export_name = declaration
        .attrs
        .iter()
        .filter_map(|attr| {
            crate::attributes::annotation_spec(&attr.name)
                .filter(|(spec, _)| spec.id == crate::attributes::AnnotationId::Export)
                .map(|(_, _)| attr)
        })
        .find_map(|attr| crate::attributes::attribute_string_arg(attr, pool))
        .unwrap_or_else(|| type_check.symbols.get(symbol).name.to_string());

    if type_check
        .symbols
        .host_function_names
        .iter()
        .any(|(other, name)| *other != symbol && name == export_name.as_str())
    {
        return Err(Diagnostic::error(
            crate::DiagCode::T049InvalidFfiExport,
            format!("exported symbol '{export_name}' collides with another host symbol"),
            declaration.span,
        ));
    }

    let symbols = type_check.symbols_mut();
    symbols
        .host_function_names
        .insert(symbol, crate::SmolStr::new(export_name));
    symbols.exported_functions.insert(symbol);
    Ok(())
}

/// Rejects every type whose C representation depends on an ABI transformation
/// the backend does not implement yet. Rejecting is deliberate: exporting a
/// slice or a by-value struct today would silently emit a mismatched ABI.
fn c_export_type_error(
    type_check: &TypeCheckResult,
    ty: ArType,
    allow_void: bool,
    depth: usize,
) -> Option<String> {
    use arandu_middle::layout::StructLayoutProvider as _;
    use arandu_middle::types::Primitive;
    if depth > 8 {
        return Some("aggregate nesting is too deep for a C boundary".to_owned());
    }
    let interner = &type_check.type_info.type_interner;
    match ty {
        ArType::Primitive(
            Primitive::I8
            | Primitive::I16
            | Primitive::I32
            | Primitive::I64
            | Primitive::U8
            | Primitive::U16
            | Primitive::U32
            | Primitive::U64
            | Primitive::F32
            | Primitive::F64
            | Primitive::Bool,
        ) => None,
        ArType::Primitive(Primitive::Byte) => None,
        ArType::Void if allow_void => None,
        ArType::Ptr(inner) => match interner.resolve(inner) {
            ArType::Void | ArType::Named(..) => None,
            ArType::Primitive(
                Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::F32
                | Primitive::F64
                | Primitive::Bool
                | Primitive::Byte,
            ) => None,
            other => Some(format!(
                "pointer to {other:?} has no stable C representation; point at a scalar or a nominal type"
            )),
        },
        ArType::Slice(element) | ArType::MutSlice(element) => match interner.resolve(element) {
            ArType::Primitive(
                Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::F32
                | Primitive::F64
                | Primitive::Byte,
            ) => None,
            other => Some(format!(
                "a slice of {other:?} has no C representation; use fixed-width scalars or a pointer"
            )),
        },
        ArType::Named(symbol, _) => {
            if !type_check.type_info.struct_repr_c.contains(&symbol) {
                return Some(
                    "nominal types by value need `@Repr(\"C\")` so their layout is the C layout; add it or pass `ptr T`"
                        .to_owned(),
                );
            }
            let Some(fields) = type_check
                .type_info
                .get_struct_fields_for_type(&ty, interner)
            else {
                return Some(
                    "this nominal type has no concrete field table; instantiate it or pass `ptr T`"
                        .to_owned(),
                );
            };
            if fields.is_empty() {
                return Some("an empty struct has no C representation".to_owned());
            }
            let fields = fields.clone();
            for field in fields.iter() {
                let field_ty = interner.resolve(field.ty);
                if let Some(reason) = c_export_type_error(type_check, field_ty, false, depth + 1) {
                    return Some(format!("field `{}`: {reason}", field.name));
                }
            }
            None
        }
        ArType::Primitive(Primitive::Str) => Some(
            "`str` is a fat pointer with an encoding contract; pass `ptr u8` plus a length"
                .to_owned(),
        ),
        ArType::Primitive(Primitive::Char) => {
            Some("`char` is a Unicode scalar value, not a C character; use u32 or u8".to_owned())
        }
        ArType::Primitive(other) => Some(format!(
            "`{other:?}` has a target-dependent width; use a fixed-width type (i8..i64, u8..u64, f32, f64, bool)"
        )),
        ArType::Void => Some("`void` is only valid as the return type".to_owned()),
        other => Some(format!("{other:?} has no C representation")),
    }
}
