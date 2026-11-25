use arandu_parser::{Ownership, Program, TopLevelDecl};

use super::apply_receiver_ownership;
use crate::type_checker::TypeChecker;
use crate::type_checker::types::ArType;

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, program))]
pub(crate) fn collect_signature_types(checker: &mut TypeChecker<'_>, program: &Program) {
    program.for_each_decl_recursive(|_decl_id, decl| {
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
                    if func_decl.params.last().is_some_and(|p| p.is_variadic) {
                        checker.type_info.variadic_functions.insert(symbol_id);
                    }
                    let method_params = super::super::super::types::extract_generic_param_symbols(
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
                                let arg_ty = if checker.symbols.get(param_sym).kind
                                    == arandu_middle::SymbolKind::ConstParam
                                {
                                    ArType::ConstParam(param_sym)
                                } else {
                                    ArType::named(param_sym, &[], &checker.type_info.type_interner)
                                };
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
                        } else if let ArType::Named(struct_id, _) = lowered_first_ty
                            && let Some(struct_params) =
                                checker.type_info.generic_params.get(&struct_id).cloned()
                        {
                            // `self: ref Pair<T>` names the struct's own `T`, which
                            // is in scope for every method of `Pair` (as in a Swift
                            // extension) whether or not the method restates it.
                            // Parameters the method restates keep its declared order.
                            let mentioned: Vec<_> = struct_params
                                .iter()
                                .copied()
                                .filter(|param| !method_params.contains(param))
                                .filter(|&param| {
                                    crate::type_checker::synth::method::contains_generic_params(
                                        &lowered_first_ty,
                                        &[param],
                                        &checker.type_info.type_interner,
                                    )
                                })
                                .collect();
                            if !mentioned.is_empty() {
                                struct_params_for_mono = Some(std::sync::Arc::new(mentioned));
                            }
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
                    all_params.extend(super::callback::implicit_callback_params(
                        checker,
                        symbol_id,
                        &func_decl.params,
                        &mut param_types,
                    ));
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

                    if func_decl
                        .attrs
                        .iter()
                        .any(|attr| attr.name == "Unsafe")
                    {
                        checker.type_info.unsafe_functions.insert(symbol_id);
                    }
                    if func_decl.attrs.iter().any(|attr| attr.name == "Export") {
                        checker.type_info.exported_functions.insert(symbol_id);
                    }
                    if func_decl.attrs.iter().any(|attr| attr.name == "Cold") {
                        checker.type_info.cold_functions.insert(symbol_id);
                    }


                    // Drop Elaboration: Check for @Destructor attribute
                    let has_destructor_attr = func_decl
                        .attrs
                        .iter()
                        .any(|attr| attr.name == "Destructor");
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
                    // `@NoSuspend` is `@Effects(NoSuspend)`, added to whatever
                    // the function already declares.
                    if func_decl
                        .attrs
                        .iter()
                        .any(|attr| attr.name == "NoSuspend" || attr.name == "nosuspend")
                    {
                        let declared = checker
                            .type_info
                            .function_effects
                            .get(&symbol_id)
                            .copied()
                            .unwrap_or(arandu_middle::EffectFlags::NONE);
                        checker.type_info.function_effects.insert(
                            symbol_id,
                            declared.union(arandu_middle::EffectFlags::NO_SUSPEND),
                        );
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
                        reject_foreign_owning_closure(checker, bare, param.span);
                        param_types.push(apply_receiver_ownership(checker, bare, param.ownership));
                    }
                    let ret_id = checker.intern(ret_ty.clone());
                    reject_foreign_owning_closure(checker, ret_id, member.span);

                    let name_key = crate::NodeKey::from(member.span);
                    if let Some(symbol_id) = checker.resolved.definitions.get(&name_key).copied() {
                        if member.params.last().is_some_and(|p| p.is_variadic) {
                            checker.type_info.variadic_functions.insert(symbol_id);
                        }
                        let member_name = member.name.as_str();
                        let pure_bits = arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && arandu_middle::IntrinsicKind::from_name(member_name)
                                .is_some_and(|kind| kind.is_pure_integer_bit_operation());
                        if pure_bits
                            && let Some(kind) = arandu_middle::IntrinsicKind::from_name(member_name)
                        {
                            use arandu_middle::types::Primitive;
                            let input = ArType::Primitive(if member_name.ends_with("64") {
                                Primitive::U64
                            } else {
                                Primitive::U32
                            });
                            let output = if kind.bit_result_is_input_width() {
                                input.clone()
                            } else {
                                ArType::Primitive(Primitive::U32)
                            };
                            // Rotations take a trailing `u32` amount.
                            let mut expected_params = vec![input.clone()];
                            if kind.bit_operand_count() == Some(2) {
                                expected_params.push(ArType::Primitive(Primitive::U32));
                            }
                            let input_name = if member_name.ends_with("64") { "u64" } else { "u32" };
                            let output_name = if output == input { input_name } else { "u32" };
                            let params_text = if expected_params.len() == 2 {
                                format!("{input_name} and u32 parameters")
                            } else {
                                format!("one {input_name} parameter")
                            };
                            let actual_params: Vec<ArType> = param_types
                                .iter()
                                .map(|&ty| checker.type_info.type_interner.resolve(ty))
                                .collect();
                            if !member.generic_params.is_empty()
                                || actual_params != expected_params
                                || ret_ty != output
                            {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for bit intrinsic '{member_name}': expected {params_text} and {output_name} result without generics"),
                                    member.span,
                                ));
                            }
                        }
                        let float_bitcast = arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && arandu_middle::IntrinsicKind::from_name(member_name)
                                .is_some_and(|kind| matches!(kind, arandu_middle::IntrinsicKind::FloatToBits(..) | arandu_middle::IntrinsicKind::FloatFromBits(..)));
                        if float_bitcast
                            && let Some(kind) = arandu_middle::IntrinsicKind::from_name(member_name)
                        {
                            use arandu_middle::types::Primitive;
                            let (expected_param, expected_ret, param_name, ret_name) = match kind {
                                arandu_middle::IntrinsicKind::FloatToBits(arandu_middle::intrinsics::FloatWidth::F64) => {
                                    (ArType::Primitive(Primitive::F64), ArType::Primitive(Primitive::U64), "f64", "u64")
                                }
                                arandu_middle::IntrinsicKind::FloatToBits(arandu_middle::intrinsics::FloatWidth::F32) => {
                                    (ArType::Primitive(Primitive::F32), ArType::Primitive(Primitive::U32), "f32", "u32")
                                }
                                arandu_middle::IntrinsicKind::FloatFromBits(arandu_middle::intrinsics::FloatWidth::F64) => {
                                    (ArType::Primitive(Primitive::U64), ArType::Primitive(Primitive::F64), "u64", "f64")
                                }
                                arandu_middle::IntrinsicKind::FloatFromBits(arandu_middle::intrinsics::FloatWidth::F32) => {
                                    (ArType::Primitive(Primitive::U32), ArType::Primitive(Primitive::F32), "u32", "f32")
                                }
                                _ => unreachable!(),
                            };
                            let actual_params: Vec<ArType> = param_types
                                .iter()
                                .map(|&ty| checker.type_info.type_interner.resolve(ty))
                                .collect();
                            if !member.generic_params.is_empty()
                                || actual_params != vec![expected_param]
                                || ret_ty != expected_ret
                            {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for float bitcast intrinsic '{member_name}': expected one {param_name} parameter and {ret_name} result without generics"),
                                    member.span,
                                ));
                            }
                        }
                        let simd = (arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic)
                            .then(|| arandu_middle::intrinsics::SimdIntrinsic::from_name(member_name))
                            .flatten();
                        if let Some(simd) = simd {
                            let (parameters, result) =
                                simd.signature(&checker.type_info.type_interner);
                            let actual: Vec<_> = param_types.iter().map(|&ty| checker.resolve(ty)).collect();
                            if !member.generic_params.is_empty() || actual != parameters || ret_ty != result {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for SIMD intrinsic '{member_name}'"),
                                    member.span,
                                ));
                            }
                        }
                        let is_fma = arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && arandu_middle::IntrinsicKind::from_name(member_name)
                                == Some(arandu_middle::IntrinsicKind::Fma);
                        if is_fma {
                            use arandu_middle::types::Primitive;
                            let (float_ty, float_name) = if member_name == "fma" {
                                (ArType::Primitive(Primitive::Float), "float")
                            } else if member_name.ends_with("64") {
                                (ArType::Primitive(Primitive::F64), "f64")
                            } else {
                                (ArType::Primitive(Primitive::F32), "f32")
                            };
                            let expected_params = vec![float_ty.clone(), float_ty.clone(), float_ty.clone()];
                            let actual_params: Vec<ArType> = param_types
                                .iter()
                                .map(|&ty| checker.type_info.type_interner.resolve(ty))
                                .collect();
                            if !member.generic_params.is_empty()
                                || actual_params != expected_params
                                || ret_ty != float_ty
                            {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for FMA intrinsic '{member_name}': expected three {float_name} parameters and {float_name} result without generics"),
                                    member.span,
                                ));
                            }
                        }
                        let is_spin_hint = arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && arandu_middle::IntrinsicKind::from_name(member_name)
                                == Some(arandu_middle::IntrinsicKind::SpinLoopHint);
                        if is_spin_hint
                            && (!member.generic_params.is_empty()
                                || !param_types.is_empty()
                                || ret_ty != ArType::Void)
                        {
                            checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                arandu_middle::DiagCode::U001FeatureNotSupported,
                                format!("unsupported signature for spin-loop hint intrinsic '{member_name}': expected no parameters and void result without generics"),
                                member.span,
                            ));
                        }
                        let is_branch_hint = arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && matches!(
                                arandu_middle::IntrinsicKind::from_name(member_name),
                                Some(
                                    arandu_middle::IntrinsicKind::Likely
                                        | arandu_middle::IntrinsicKind::Unlikely
                                )
                            );
                        if is_branch_hint {
                            use arandu_middle::types::Primitive;
                            let bool_ty = ArType::Primitive(Primitive::Bool);
                            let actual_params: Vec<ArType> = param_types
                                .iter()
                                .map(|&ty| checker.type_info.type_interner.resolve(ty))
                                .collect();
                            if !member.generic_params.is_empty()
                                || actual_params != [bool_ty.clone()]
                                || ret_ty != bool_ty
                            {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for branch-hint intrinsic '{member_name}': expected one bool parameter and bool result without generics"),
                                    member.span,
                                ));
                            }
                        }
                        let pure_arith = (arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic)
                            .then(|| arandu_middle::IntrinsicKind::from_name(member_name))
                            .flatten()
                            .filter(|kind| {
                                matches!(
                                    kind,
                                    arandu_middle::intrinsics::IntrinsicKind::OverflowArithmetic(..)
                                        | arandu_middle::intrinsics::IntrinsicKind::SaturatingArithmetic(..)
                                )
                            });
                        if let Some(kind) = pure_arith {
                            use arandu_middle::intrinsics::IntrinsicKind;
                            let arith_int = match kind {
                                IntrinsicKind::OverflowArithmetic(_, arith)
                                | IntrinsicKind::SaturatingArithmetic(_, arith) => arith,
                                _ => unreachable!(),
                            };
                            let int_prim = arith_int.primitive();
                            let int_ty = ArType::Primitive(int_prim);
                            let int_id = checker.intern(int_ty.clone());
                            let bool_id = checker.intern(ArType::Primitive(arandu_middle::types::Primitive::Bool));
                            let expected_ret = match kind {
                                IntrinsicKind::OverflowArithmetic(..) => {
                                    ArType::tuple(&[int_id, bool_id], &checker.type_info.type_interner)
                                }
                                _ => int_ty.clone(),
                            };
                            let expected_params = vec![int_ty.clone(), int_ty];
                            let actual_params: Vec<ArType> = param_types
                                .iter()
                                .map(|&ty| checker.type_info.type_interner.resolve(ty))
                                .collect();
                            if !member.generic_params.is_empty()
                                || actual_params != expected_params
                                || ret_ty != expected_ret
                            {
                                let int_name = arith_int.ar_type_name();
                                let expected_ret_text = if matches!(kind, IntrinsicKind::OverflowArithmetic(..)) {
                                    format!("({int_name}, bool)")
                                } else {
                                    int_name.to_owned()
                                };
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for arithmetic intrinsic '{member_name}': expected two {int_name} parameters and {expected_ret_text} result without generics"),
                                    member.span,
                                ));
                            }
                        }
                        let mut flags = if pure_bits
                            || float_bitcast
                            || simd.is_some()
                            || is_fma
                            || is_spin_hint
                            || is_branch_hint
                            || pure_arith.is_some()
                        {
                            arandu_middle::EffectFlags::NONE
                        } else {
                            arandu_middle::EffectFlags::FOREIGN
                        };
                        if arandu_parser::AbiKind::from_abi_str(&extern_decl.abi)
                            == arandu_parser::AbiKind::AranduIntrinsic
                            && let Some(kind) = arandu_middle::IntrinsicKind::from_name(member_name)
                        {
                            use arandu_middle::intrinsics::{AtomicOp, IntrinsicKind};
                            let expected = match kind {
                                IntrinsicKind::Atomic(op, width, _) => {
                                    let scalar_ty = if width == arandu_middle::intrinsics::AtomicWidth::Pointer {
                                        let byte = checker.intern(ArType::Primitive(arandu_middle::types::Primitive::U8));
                                        ArType::Ptr(byte)
                                    } else { ArType::Primitive(width.primitive()) };
                                    let scalar = checker.intern(scalar_ty.clone());
                                    let reference = if matches!(op, AtomicOp::Load | AtomicOp::Store) { ArType::Ref(scalar) } else { ArType::RefMut(scalar) };
                                    let mut parameters = vec![checker.intern(reference)];
                                    parameters.resize(op.argument_count(), scalar);
                                    Some((parameters, if op == AtomicOp::Store { ArType::Void } else { scalar_ty }))
                                }
                                IntrinsicKind::AtomicFence(..) | IntrinsicKind::AtomicCompilerFence(..) => Some((Vec::new(), ArType::Void)),
                                _ => None,
                            };
                            if let Some((parameters, result)) = expected
                                && (!member.generic_params.is_empty() || param_types != parameters || ret_ty != result
                                    || matches!(kind, IntrinsicKind::Atomic(op, arandu_middle::intrinsics::AtomicWidth::Pointer, _)
                                        if !matches!(op, AtomicOp::Load | AtomicOp::Store | AtomicOp::Swap | AtomicOp::CompareExchange)))
                            {
                                checker.diagnostics.push(arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::U001FeatureNotSupported,
                                    format!("unsupported signature for atomic intrinsic '{member_name}'"),
                                    member.span,
                                ));
                            }
                        }
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
                        let params = super::super::super::types::extract_generic_param_symbols(
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
    });
}

/// An owning closure carries code with the internal calling convention and
/// an environment only Arandu can destroy, so it never crosses a foreign
/// boundary (RFC 0032, L3).
fn reject_foreign_owning_closure(
    checker: &mut TypeChecker<'_>,
    ty: crate::type_checker::types::TypeId,
    span: arandu_lexer::Span,
) {
    let mut found = false;
    if let Ok(shape) =
        arandu_middle::types::TypeShape::from_id(ty, &checker.type_info.type_interner)
    {
        let _ = shape.for_each_symbol(|symbol| {
            found |= checker.symbols.try_get(symbol).is_some_and(|entry| {
                entry
                    .lang_item
                    .is_some_and(|item| item.owning_closure().is_some())
            });
        });
    }
    if found {
        checker.diagnostics.push(
            arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T049InvalidFfiExport,
                "an owning closure cannot cross a foreign function boundary",
                span,
            )
            .with_primary_label("`own (A) -> R` uses Arandu's internal calling convention")
            .with_hint("pass a plain function value `(A) -> R` and a context pointer instead"),
        );
    }
}
