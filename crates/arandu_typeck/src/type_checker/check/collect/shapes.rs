use arandu_parser::{Program, TopLevelDecl};

use crate::type_checker::TypeChecker;

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub(crate) fn collect_type_shapes(checker: &mut TypeChecker<'_>, program: &Program) {
    program.for_each_decl_recursive(|_decl_id, decl| {
        match decl {
            TopLevelDecl::Struct(struct_decl) => {
                let mut field_entries: Vec<arandu_middle::layout::StructFieldInfo> = Vec::new();
                for (idx, field) in struct_decl.fields.iter().enumerate() {
                    let field_ty =
                        checker.lower_type_expr(field.ty, checker.symbols.global_scope());
                    let field_tid = checker.intern(field_ty);
                    let field_key = crate::NodeKey::from(field.span);
                    let field_symbol = checker.resolved.definitions.get(&field_key).copied();
                    if field.visibility == arandu_parser::Visibility::Private
                        && let Some(field_symbol) = field_symbol
                    {
                        checker.type_info.private_fields.insert(field_symbol);
                    }
                    field_entries.push(arandu_middle::layout::StructFieldInfo {
                        name: field.name.clone(),
                        symbol: field_symbol,
                        ty: field_tid,
                        index: idx,
                    });
                }
                let struct_key = crate::NodeKey::from(struct_decl.span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&struct_key).copied() {
                    if checker
                        .symbols
                        .get(symbol_id)
                        .lang_item
                        .and_then(arandu_middle::symbol_table::LangItem::vector_shape)
                        .is_some()
                        && (!struct_decl.fields.is_empty()
                            || !struct_decl.generic_params.is_empty())
                    {
                        checker.diagnostics.push(arandu_middle::Diagnostic::error(
                            arandu_middle::DiagCode::U001FeatureNotSupported,
                            "native SIMD declarations must be opaque and non-generic",
                            struct_decl.span,
                        ));
                    }
                    if matches!(
                        checker.symbols.get(symbol_id).lang_item,
                        Some(
                            arandu_middle::symbol_table::LangItem::UnsafeCell
                                | arandu_middle::symbol_table::LangItem::RwLock
                                | arandu_middle::symbol_table::LangItem::Mutex
                                | arandu_middle::symbol_table::LangItem::Condvar
                                | arandu_middle::symbol_table::LangItem::RwLockReadGuard
                                | arandu_middle::symbol_table::LangItem::RwLockWriteGuard
                                | arandu_middle::symbol_table::LangItem::MutexGuard
                        )
                    ) {
                        checker.type_info.interior_mutable_types.insert(symbol_id);
                    }
                    let is_repr_c = struct_decl.attrs.iter().any(|attr| {
                        if attr.name == "Repr" || attr.name == "repr" {
                            attr.args
                                .first()
                                .is_some_and(|arg| match checker.pool.expr(*arg) {
                                    arandu_parser::ExprKind::Path { path } => {
                                        path.first().is_some_and(|s| s.eq_ignore_ascii_case("c"))
                                    }
                                    arandu_parser::ExprKind::InterpolatedString { parts } => {
                                        let part_ids = checker.pool.string_part_list(*parts);
                                        part_ids.first().is_some_and(|&id| {
                                            match checker.pool.string_part(id) {
                                                arandu_parser::StringPart::Text {
                                                    text, ..
                                                } => text.eq_ignore_ascii_case("c"),
                                                _ => false,
                                            }
                                        })
                                    }
                                    _ => false,
                                })
                        } else {
                            false
                        }
                    });
                    if is_repr_c {
                        checker.type_info.struct_repr_c.insert(symbol_id);
                    }
                    checker.type_info.struct_fields.insert(
                        symbol_id,
                        std::sync::Arc::new(arandu_middle::layout::StructFields::from_entries(
                            field_entries,
                        )),
                    );
                    let params = super::super::super::types::extract_generic_param_symbols(
                        checker,
                        &struct_decl.generic_params,
                    );
                    if !params.is_empty() {
                        checker
                            .type_info
                            .generic_params
                            .insert(symbol_id, std::sync::Arc::new(params));
                    }
                }
            }
            TopLevelDecl::Enum(enum_decl) => {
                let enum_key = crate::NodeKey::from(enum_decl.span);
                let Some(enum_symbol_id) = checker.resolved.definitions.get(&enum_key).copied()
                else {
                    return;
                };
                let params = super::super::super::types::extract_generic_param_symbols(
                    checker,
                    &enum_decl.generic_params,
                );
                let params_arc = if !params.is_empty() {
                    let arc = std::sync::Arc::new(params);
                    checker
                        .type_info
                        .generic_params
                        .insert(enum_symbol_id, arc.clone());
                    Some(arc)
                } else {
                    None
                };

                for (tag, variant) in enum_decl.variants.iter().enumerate() {
                    let mut struct_field_entries: Option<
                        Vec<arandu_middle::layout::StructFieldInfo>,
                    > = None;
                    let shape = match &variant.payload {
                        None => super::super::super::EnumPayloadShape::Unit,
                        Some(arandu_parser::EnumPayload::Tuple { types, .. }) => {
                            let type_list = checker.pool.type_expr_list(*types).to_vec();
                            let tids: Vec<_> = type_list
                                .iter()
                                .map(|&ty_expr| {
                                    let ty = checker
                                        .lower_type_expr(ty_expr, checker.symbols.global_scope());
                                    checker.intern(ty)
                                })
                                .collect();
                            if tids.len() > 1 {
                                checker.intern(super::super::super::ArType::tuple(
                                    &tids,
                                    &checker.type_info.type_interner,
                                ));
                            }
                            super::super::super::EnumPayloadShape::Tuple(tids)
                        }
                        Some(arandu_parser::EnumPayload::Struct { fields, .. }) => {
                            let mut field_entries: Vec<arandu_middle::layout::StructFieldInfo> =
                                Vec::with_capacity(fields.len());
                            let mut tids: Vec<arandu_middle::types::TypeId> =
                                Vec::with_capacity(fields.len());
                            for (idx, field) in fields.iter().enumerate() {
                                let field_ty = checker
                                    .lower_type_expr(field.ty, checker.symbols.global_scope());
                                let field_tid = checker.intern(field_ty);
                                let field_key = crate::NodeKey::from(field.span);
                                let field_symbol =
                                    checker.resolved.definitions.get(&field_key).copied();
                                if field.visibility == arandu_parser::Visibility::Private
                                    && let Some(field_symbol) = field_symbol
                                {
                                    checker.type_info.private_fields.insert(field_symbol);
                                }
                                field_entries.push(arandu_middle::layout::StructFieldInfo {
                                    name: field.name.clone(),
                                    symbol: field_symbol,
                                    ty: field_tid,
                                    index: idx,
                                });
                                tids.push(field_tid);
                            }
                            struct_field_entries = Some(field_entries);
                            if tids.is_empty() {
                                super::super::super::EnumPayloadShape::Unit
                            } else {
                                if tids.len() > 1 {
                                    checker.intern(super::super::super::ArType::tuple(
                                        &tids,
                                        &checker.type_info.type_interner,
                                    ));
                                }
                                super::super::super::EnumPayloadShape::Tuple(tids)
                            }
                        }
                    };
                    let variant_key = crate::NodeKey::from(variant.span);
                    if let Some(variant_symbol_id) =
                        checker.resolved.definitions.get(&variant_key).copied()
                    {
                        // Build constructor signature type for the variant
                        let mut enum_args = Vec::new();
                        let gp = checker
                            .type_info
                            .generic_params
                            .get(&enum_symbol_id)
                            .cloned();
                        if let Some(gp) = gp {
                            for &p_sym in gp.iter() {
                                let arg_ty = super::super::super::ArType::named(
                                    p_sym,
                                    &[],
                                    &checker.type_info.type_interner,
                                );
                                enum_args.push(checker.intern(arg_ty));
                            }
                        }
                        let ret_ty_id = checker.intern(super::super::super::ArType::named(
                            enum_symbol_id,
                            &enum_args,
                            &checker.type_info.type_interner,
                        ));
                        let variant_ty = match &shape {
                            super::super::super::EnumPayloadShape::Tuple(tids) => {
                                super::super::super::ArType::func(
                                    tids,
                                    ret_ty_id,
                                    &checker.type_info.type_interner,
                                )
                            }
                            super::super::super::EnumPayloadShape::Unit => {
                                super::super::super::ArType::named(
                                    enum_symbol_id,
                                    &[],
                                    &checker.type_info.type_interner,
                                )
                            }
                        };
                        let variant_ty_id = checker.intern(variant_ty);
                        checker.record_decl_type(variant_symbol_id, variant_ty_id);

                        checker
                            .type_info
                            .enum_variants
                            .insert(variant_symbol_id, (enum_symbol_id, shape.clone()));
                        checker
                            .type_info
                            .record_enum_variant_tag(variant_symbol_id, tag);

                        let fields_arc = struct_field_entries.map(|entries| {
                            std::sync::Arc::new(arandu_middle::layout::StructFields::from_entries(
                                entries,
                            ))
                        });
                        if let Some(ref f_arc) = fields_arc {
                            checker
                                .type_info
                                .struct_fields
                                .insert(variant_symbol_id, f_arc.clone());
                            if let Some(ref p_arc) = params_arc {
                                checker
                                    .type_info
                                    .generic_params
                                    .insert(variant_symbol_id, p_arc.clone());
                            }
                        }

                        // Also register the *associated-member* SymbolId that the resolver
                        // creates for qualified uses like `Color.Red`.
                        // `define_associated_member` stores that symbol under the enum's
                        // SymbolId and the resolver records it as the expr-ref for any
                        // `TypePath { Color, Red }` node.
                        // Without this second registration a direct
                        // `enum_variant_tags.get(color_red_sym)` silently misses.
                        if let Some(assoc_id) = checker
                            .symbols
                            .lookup_associated_member(enum_symbol_id, &variant.name)
                            && assoc_id != variant_symbol_id
                        {
                            checker.record_decl_type(assoc_id, variant_ty_id);
                            checker
                                .type_info
                                .enum_variants
                                .insert(assoc_id, (enum_symbol_id, shape.clone()));
                            checker.type_info.record_enum_variant_tag(assoc_id, tag);
                            if let Some(f_arc) = fields_arc {
                                checker.type_info.struct_fields.insert(assoc_id, f_arc);
                                if let Some(ref p_arc) = params_arc {
                                    checker
                                        .type_info
                                        .generic_params
                                        .insert(assoc_id, p_arc.clone());
                                }
                            }
                        }
                    }
                }
            }
            TopLevelDecl::Const(const_decl) => {
                // A cast gives an unannotated constant a statically knowable
                // type. Publish it with signatures so sibling function bodies
                // can type-check references before the constant's body shard is
                // merged (e.g. a `uint` capacity bound used in a condition).
                let declared_ty =
                    const_decl
                        .ty
                        .or_else(|| match checker.pool.expr(const_decl.value) {
                            arandu_parser::ExprKind::Cast { ty, .. } => Some(*ty),
                            _ => None,
                        });
                if let Some(ty_expr) = declared_ty {
                    let const_ty = checker.lower_type_expr(ty_expr, checker.symbols.global_scope());
                    let const_key = crate::NodeKey::from(const_decl.span);
                    if let Some(symbol_id) = checker.resolved.definitions.get(&const_key).copied() {
                        let const_id = checker.intern(const_ty);
                        checker.record_decl_type(symbol_id, const_id);
                    }
                }
            }
            TopLevelDecl::TypeAlias(alias_decl) => {
                let alias_ty =
                    checker.lower_type_expr(alias_decl.ty, checker.symbols.global_scope());
                let alias_key = crate::NodeKey::from(alias_decl.span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&alias_key).copied() {
                    let alias_id = checker.intern(alias_ty);
                    checker.record_decl_type(symbol_id, alias_id);
                    let params = super::super::super::types::extract_generic_param_symbols(
                        checker,
                        &alias_decl.generic_params,
                    );
                    if !params.is_empty() {
                        checker
                            .type_info
                            .generic_params
                            .insert(symbol_id, std::sync::Arc::new(params));
                    }
                }
            }
            TopLevelDecl::Func(func_decl) => {
                let name_span = match func_decl.name {
                    arandu_parser::FuncName::Free { span, .. } => span,
                    arandu_parser::FuncName::Method { span, .. } => span,
                };
                let name_key = crate::NodeKey::from(name_span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&name_key).copied() {
                    let params = super::super::super::types::extract_generic_param_symbols(
                        checker,
                        &func_decl.generic_params,
                    );
                    if !params.is_empty() {
                        checker
                            .type_info
                            .generic_params
                            .insert(symbol_id, std::sync::Arc::new(params));
                    }
                }
            }
            _ => {}
        }
    });
}
