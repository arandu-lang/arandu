use arandu_parser::{Ownership, Program, TopLevelDecl};

use crate::type_checker::TypeChecker;
use crate::type_checker::types::ArType;

/// Wrap a receiver's bare type with the ownership qualifier.
///
/// Legacy receiver prefixes are lowered to the canonical `ref T` / `mut ref T`
/// types. Canonical type-qualified parameters already arrive wrapped and pass
/// through unchanged.
#[inline]
pub(crate) fn apply_receiver_ownership(
    checker: &mut TypeChecker<'_>,
    bare_ty_id: arandu_middle::types::TypeId,
    ownership: Option<Ownership>,
) -> arandu_middle::types::TypeId {
    match ownership {
        Some(Ownership::Shared) => checker.intern(ArType::Ref(bare_ty_id)),
        Some(Ownership::Mut) => checker.intern(ArType::RefMut(bare_ty_id)),
        Some(Ownership::Own) | None => bare_ty_id,
    }
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub(crate) fn collect_type_shapes(checker: &mut TypeChecker<'_>, program: &Program) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        match decl {
            TopLevelDecl::Struct(struct_decl) => {
                let mut field_entries: Vec<arandu_middle::layout::StructFieldInfo> = Vec::new();
                for (idx, field) in struct_decl.fields.iter().enumerate() {
                    let field_ty =
                        checker.lower_type_expr(field.ty, checker.symbols.global_scope());
                    let field_tid = checker.intern(field_ty);
                    let field_key = crate::NodeKey::from(field.span);
                    let field_symbol = checker.resolved.definitions.get(&field_key).copied();
                    field_entries.push(arandu_middle::layout::StructFieldInfo {
                        name: field.name.clone(),
                        symbol: field_symbol,
                        ty: field_tid,
                        index: idx,
                    });
                }
                let struct_key = crate::NodeKey::from(struct_decl.span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&struct_key).copied() {
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
                    let params = super::super::types::extract_generic_param_symbols(
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
                    continue;
                };
                let params = super::super::types::extract_generic_param_symbols(
                    checker,
                    &enum_decl.generic_params,
                );
                if !params.is_empty() {
                    checker
                        .type_info
                        .generic_params
                        .insert(enum_symbol_id, std::sync::Arc::new(params));
                }

                for (tag, variant) in enum_decl.variants.iter().enumerate() {
                    let shape = match &variant.payload {
                        None => super::super::EnumPayloadShape::Unit,
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
                                checker.intern(super::super::ArType::tuple(
                                    &tids,
                                    &checker.type_info.type_interner,
                                ));
                            }
                            super::super::EnumPayloadShape::Tuple(tids)
                        }
                        _ => super::super::EnumPayloadShape::Unit,
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
                                let arg_ty = super::super::ArType::named(
                                    p_sym,
                                    &[],
                                    &checker.type_info.type_interner,
                                );
                                enum_args.push(checker.intern(arg_ty));
                            }
                        }
                        let ret_ty_id = checker.intern(super::super::ArType::named(
                            enum_symbol_id,
                            &enum_args,
                            &checker.type_info.type_interner,
                        ));
                        let variant_ty = match &shape {
                            super::super::EnumPayloadShape::Tuple(tids) => {
                                super::super::ArType::func(
                                    tids,
                                    ret_ty_id,
                                    &checker.type_info.type_interner,
                                )
                            }
                            super::super::EnumPayloadShape::Unit => super::super::ArType::named(
                                enum_symbol_id,
                                &[],
                                &checker.type_info.type_interner,
                            ),
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
                        }
                    }
                }
            }
            TopLevelDecl::Const(const_decl) => {
                if let Some(ty_expr) = const_decl.ty {
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
                    let params = super::super::types::extract_generic_param_symbols(
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
                    let params = super::super::types::extract_generic_param_symbols(
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
    }
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub(crate) fn collect_signature_types(checker: &mut TypeChecker<'_>, program: &Program) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        match decl {
            TopLevelDecl::Func(func_decl) => {
                let mut ret_ty = if let Some(result) = &func_decl.result {
                    checker.lower_result_type(result, checker.symbols.global_scope())
                } else {
                    ArType::Void
                };
                // A3: `async func f(): T` ≡ `func f(): Coroutine[T]` (type sugar).
                // Body still typechecks against bare `T` (see check_func_body).
                if func_decl.is_async && !matches!(ret_ty, ArType::Coroutine(_)) {
                    let inner = checker.intern(ret_ty);
                    ret_ty = ArType::Coroutine(inner);
                }

                let mut param_types = Vec::new();
                for param in &func_decl.params {
                    let param_ty =
                        checker.lower_type_expr(param.ty, checker.symbols.global_scope());
                    // All params: `shared`/`mut` → `&T` / `&mut T` (not only `self`).
                    // Free functions like `spawn_i64(shared ex: SyncExecutor, …)` must
                    // reborrow, not move, so the executor can be reused.
                    let bare = checker.intern(param_ty);
                    param_types.push(apply_receiver_ownership(checker, bare, param.ownership));
                }

                let name_span = match func_decl.name {
                    arandu_parser::FuncName::Free { span, .. } => span,
                    arandu_parser::FuncName::Method { span, .. } => span,
                };
                let name_key = crate::NodeKey::from(name_span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&name_key).copied() {
                    let method_params = super::super::types::extract_generic_param_symbols(
                        checker,
                        &func_decl.generic_params,
                    );
                    // Generic struct receivers: `self: List` → `List<T>` using the
                    // *struct*'s type parameters — never the method's own params.
                    // Ownership wrap already applied above for every param.
                    let mut struct_params_for_mono: Option<std::sync::Arc<Vec<crate::SymbolId>>> =
                        None;
                    if let arandu_parser::FuncName::Method { .. } = &func_decl.name
                        && let Some(first_param) = func_decl.params.first()
                        && first_param.is_receiver
                        && let Some(first_ty_id) = param_types.first_mut()
                    {
                        // Peel the receiver for generic expansion, then restore
                        // either the canonical type wrapper or a legacy prefix.
                        let (bare_id, effective_ownership) = match checker.resolve(*first_ty_id) {
                            ArType::Ref(inner) => (inner, Some(Ownership::Shared)),
                            ArType::RefMut(inner) => (inner, Some(Ownership::Mut)),
                            _ => (*first_ty_id, first_param.ownership),
                        };
                        let lowered_first_ty = checker.resolve(bare_id);
                        if let ArType::Named(struct_id, ref args) = lowered_first_ty
                            && args.is_empty()
                            && let Some(struct_params) =
                                checker.type_info.generic_params.get(&struct_id).cloned()
                            && !struct_params.is_empty()
                        {
                            let mut new_args = Vec::new();
                            for &param_sym in struct_params.iter() {
                                let arg_ty =
                                    ArType::named(param_sym, &[], &checker.type_info.type_interner);
                                new_args.push(checker.intern(arg_ty));
                            }
                            let new_first_ty = ArType::named(
                                struct_id,
                                &new_args,
                                &checker.type_info.type_interner,
                            );
                            let bare_inst = checker.intern(new_first_ty);
                            *first_ty_id =
                                apply_receiver_ownership(checker, bare_inst, effective_ownership);
                            struct_params_for_mono = Some(struct_params);
                        }
                    }
                    // Mono key params = struct type params (if method) ++ method-only
                    // type params. Methods restate receiver params (`BoxG.get<T>`) with
                    // the **same** SymbolIds as the struct — must not double-count or
                    // receiver-driven mono sees params=[T,T] vs recv_args=[int] and skips.
                    let mut all_params = Vec::new();
                    if let Some(sp) = struct_params_for_mono {
                        all_params.extend(sp.iter().copied());
                    }
                    for p in method_params {
                        if !all_params.contains(&p) {
                            all_params.push(p);
                        }
                    }
                    if !all_params.is_empty() {
                        checker
                            .type_info
                            .generic_params
                            .insert(symbol_id, std::sync::Arc::new(all_params));
                    }
                    let ret_id = checker.intern(ret_ty);
                    let return_kind = match checker.resolve(ret_id) {
                        ArType::Ref(_) => Some(arandu_middle::types::BorrowKind::Shared),
                        ArType::RefMut(_) => Some(arandu_middle::types::BorrowKind::Exclusive),
                        _ => None,
                    };
                    if let Some(kind) = return_kind {
                        let candidates = param_types
                            .iter()
                            .enumerate()
                            .filter(|(_, parameter)| {
                                matches!(
                                    (kind, checker.resolve(**parameter)),
                                    (
                                        arandu_middle::types::BorrowKind::Shared,
                                        ArType::Ref(_) | ArType::RefMut(_),
                                    ) | (
                                        arandu_middle::types::BorrowKind::Exclusive,
                                        ArType::RefMut(_),
                                    )
                                )
                            })
                            .map(|(index, _)| index)
                            .collect::<Vec<_>>();
                        if let [parameter_index] = candidates.as_slice()
                            && let Ok(parameter_index) = u32::try_from(*parameter_index)
                        {
                            checker.type_info.return_borrow_summaries.insert(
                                symbol_id,
                                arandu_middle::types::ReturnBorrowSummary::direct(
                                    parameter_index,
                                    kind,
                                ),
                            );
                        }
                    }
                    let func_ty =
                        ArType::func(&param_types, ret_id, &checker.type_info.type_interner);
                    let func_id = checker.intern(func_ty);
                    checker.record_decl_type(symbol_id, func_id);

                    // Drop Elaboration: Check for @Destructor attribute
                    let has_destructor_attr = func_decl
                        .attrs
                        .iter()
                        .any(|attr| attr.name == "Destructor" || attr.name == "destructor");
                    if has_destructor_attr
                        && let arandu_parser::FuncName::Method { .. } = &func_decl.name
                        && let Some(first_param) = func_decl.params.first()
                        && first_param.is_receiver
                    {
                        let param_ty =
                            checker.lower_type_expr(first_param.ty, checker.symbols.global_scope());
                        if let ArType::Named(struct_id, _) = param_ty {
                            checker.type_info.destructors.insert(struct_id, symbol_id);
                        }
                    }

                    // Effect System (A2): Process @Effects(...) attribute
                    for attr in &func_decl.attrs {
                        if attr.name == "Effects" || attr.name == "effects" {
                            let mut flags = arandu_middle::EffectFlags::NONE;
                            for arg in &attr.args {
                                let name = match checker.pool.expr(*arg) {
                                    arandu_parser::ExprKind::Path { path } => {
                                        path.first().map(|s| s.as_str())
                                    }
                                    arandu_parser::ExprKind::InterpolatedString { parts } => {
                                        let part_ids = checker.pool.string_part_list(*parts);
                                        part_ids.first().and_then(|&id| {
                                            match checker.pool.string_part(id) {
                                                arandu_parser::StringPart::Text {
                                                    text, ..
                                                } => Some(text.as_str()),
                                                _ => None,
                                            }
                                        })
                                    }
                                    _ => None,
                                };
                                if let Some(eff_name) = name {
                                    if let Some(flag) =
                                        arandu_middle::EffectFlags::from_name(eff_name)
                                    {
                                        flags = flags.union(flag);
                                    } else {
                                        checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                            arandu_middle::DiagCode::N012UnknownAnnotation,
                                            format!("unknown effect '{eff_name}' in @Effects"),
                                            attr.span,
                                        ));
                                    }
                                }
                            }
                            checker.type_info.function_effects.insert(symbol_id, flags);
                        }
                    }
                }
            }
            TopLevelDecl::Extern(extern_decl) => {
                for member in &extern_decl.members {
                    let ret_ty = if let Some(result) = &member.result {
                        checker.lower_result_type(result, checker.symbols.global_scope())
                    } else {
                        ArType::Void
                    };

                    let mut param_types = Vec::new();
                    for param in &member.params {
                        let bare_ty =
                            checker.lower_type_expr(param.ty, checker.symbols.global_scope());
                        // Reflect `shared`/`mut` ownership (`&T`/`&mut T`) the
                        // same way defined-function formals do, so W3.3
                        // auto-ref lower (Borrow/BorrowMut) also applies to
                        // calls through extern declarations.
                        let bare = checker.intern(bare_ty);
                        param_types.push(apply_receiver_ownership(checker, bare, param.ownership));
                    }

                    let name_key = crate::NodeKey::from(member.span);
                    if let Some(symbol_id) = checker.resolved.definitions.get(&name_key).copied() {
                        let member_name = member.name.as_str();
                        let mut flags = arandu_middle::EffectFlags::FOREIGN;
                        if member_name.starts_with("ar_rt_tcp_")
                            || member_name.starts_with("ar_net_")
                        {
                            flags = flags.union(arandu_middle::EffectFlags::NET);
                        } else if member_name.starts_with("ar_io_") {
                            flags = flags
                                .union(arandu_middle::EffectFlags::FILE_READ)
                                .union(arandu_middle::EffectFlags::FILE_WRITE);
                        } else if member_name.starts_with("ar_fs_") {
                            flags = flags.union(arandu_middle::EffectFlags::FILE_READ);
                            // read-only hosts (`ar_fs_read*`) need just FileRead;
                            // every other FS host may write.
                            if !member_name.starts_with("ar_fs_read") {
                                flags = flags.union(arandu_middle::EffectFlags::FILE_WRITE);
                            }
                        } else if member_name.starts_with("ar_rt_supervisor_")
                            || member_name.starts_with("ar_process_")
                        {
                            flags = flags.union(arandu_middle::EffectFlags::PROCESS);
                        }
                        checker.type_info.function_effects.insert(symbol_id, flags);

                        let ret_id = checker.intern(ret_ty);
                        // Signature-only intrinsics cannot be inspected by the
                        // AMIR solver. Publish a borrow interface only when the
                        // signature has one unambiguous borrow-bearing input.
                        // This covers stdlib view constructors without adding
                        // names, addresses, or lifetimes to the stable contract.
                        if let Ok(result_paths) = checker.type_info.borrow_paths(ret_id)
                            && !result_paths.is_empty()
                        {
                            let candidates = param_types
                                .iter()
                                .enumerate()
                                .filter_map(|(index, parameter)| {
                                    let paths = checker.type_info.borrow_paths(*parameter).ok()?;
                                    (!paths.is_empty()).then_some((index, paths))
                                })
                                .collect::<Vec<_>>();
                            if let [(parameter_index, parameter_paths)] = candidates.as_slice()
                                && let Ok(parameter_index) = u32::try_from(*parameter_index)
                            {
                                let mut summary =
                                    arandu_middle::types::ReturnBorrowSummary::default();
                                for (result_path, kind) in result_paths {
                                    let sources = parameter_paths
                                        .iter()
                                        .filter(|(_, source_kind)| {
                                            kind == arandu_middle::types::BorrowKind::Shared
                                                || *source_kind
                                                    == arandu_middle::types::BorrowKind::Exclusive
                                        })
                                        .map(|(parameter_path, _)| {
                                            arandu_middle::types::BorrowSource {
                                                parameter_index,
                                                parameter_path: parameter_path.clone(),
                                            }
                                        })
                                        .collect::<Vec<_>>();
                                    if !sources.is_empty() {
                                        summary.dependencies.push(
                                            arandu_middle::types::ReturnBorrowDependency {
                                                result_path,
                                                sources,
                                                kind,
                                            },
                                        );
                                    }
                                }
                                summary.canonicalize();
                                if !summary.dependencies.is_empty() {
                                    checker
                                        .type_info
                                        .return_borrow_summaries
                                        .insert(symbol_id, summary);
                                }
                            }
                        }
                        let func_ty =
                            ArType::func(&param_types, ret_id, &checker.type_info.type_interner);
                        let func_id = checker.intern(func_ty);
                        checker.record_decl_type(symbol_id, func_id);
                        let params = super::super::types::extract_generic_param_symbols(
                            checker,
                            &member.generic_params,
                        );
                        if !params.is_empty() {
                            checker
                                .type_info
                                .generic_params
                                .insert(symbol_id, std::sync::Arc::new(params));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
