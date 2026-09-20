use arandu_parser::FuncDecl;

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::ArType;
use super::collect::apply_receiver_ownership;

fn func_name_key(decl: &FuncDecl) -> crate::NodeKey {
    let name_span = match &decl.name {
        arandu_parser::FuncName::Free { span, .. } => *span,
        arandu_parser::FuncName::Method { span, .. } => *span,
    };
    crate::NodeKey::from(name_span)
}

fn validate_method_receiver(checker: &mut TypeChecker<'_>, decl: &FuncDecl) {
    let arandu_parser::FuncName::Method { receiver, .. } = &decl.name else {
        return;
    };
    // `Type.name` is the namespace representation for both instance methods
    // and associated functions. Only a member that explicitly declares
    // `self` participates in receiver validation; members without `self` are
    // associated functions such as `Point.new(...)`.
    if !decl.params.first().is_some_and(|param| param.is_receiver) {
        return;
    }
    let mut recv_ty =
        checker.lower_named_type(receiver.span, receiver, &[], checker.symbols.global_scope());
    // Only generic *structs* parameterize `self` (e.g. List<T>.push). Method type
    // params (e.g. Holder.map<U>) must NOT become struct type arguments.
    if let ArType::Named(struct_id, ref args) = recv_ty
        && args.is_empty()
        && let Some(struct_params) = checker.type_info.generic_params.get(&struct_id).cloned()
        && !struct_params.is_empty()
    {
        let mut new_args = Vec::new();
        for &param_sym in struct_params.iter() {
            let arg_ty =
                if checker.symbols.get(param_sym).kind == arandu_middle::SymbolKind::ConstParam {
                    ArType::ConstParam(param_sym)
                } else {
                    ArType::named(param_sym, &[], &checker.type_info.type_interner)
                };
            new_args.push(checker.intern(arg_ty));
        }
        recv_ty = ArType::named(struct_id, &new_args, &checker.type_info.type_interner);
    }
    let first = &decl.params[0];
    let mut self_ty = checker.lower_type_expr(first.ty, checker.symbols.global_scope());
    // Canonical receiver syntax carries ownership in the type (`self: ref T`
    // or `self: mut ref T`). Legacy prefix ownership is applied later. Compare
    // the associated nominal type against the unwrapped receiver here.
    self_ty = match self_ty {
        ArType::Ref(inner) | ArType::RefMut(inner) => checker.resolve(inner),
        other => other,
    };
    if let ArType::Named(struct_id, ref args) = self_ty
        && args.is_empty()
        && let Some(struct_params) = checker.type_info.generic_params.get(&struct_id).cloned()
        && !struct_params.is_empty()
    {
        let mut new_args = Vec::new();
        for &param_sym in struct_params.iter() {
            let arg_ty =
                if checker.symbols.get(param_sym).kind == arandu_middle::SymbolKind::ConstParam {
                    ArType::ConstParam(param_sym)
                } else {
                    ArType::named(param_sym, &[], &checker.type_info.type_interner)
                };
            new_args.push(checker.intern(arg_ty));
        }
        self_ty = ArType::named(struct_id, &new_args, &checker.type_info.type_interner);
    }
    if !super::super::types::unify(&recv_ty, &self_ty, &checker.type_info.type_interner) {
        // `lhs_span` points at the declared `self: T` type and `rhs_span` at
        // the receiver name in `Type.method`, so `self_ty` is the declared
        // (expected) type and `recv_ty` the found one. Passing them the other
        // way round rendered a misleading T002 (labels swapped vs types).
        checker.add_constraint(
            self_ty,
            recv_ty,
            ConstraintOrigin::Assignment {
                lhs_span: first.span,
                rhs_span: receiver.span,
            },
        );
    }
}

fn func_type_scope(checker: &TypeChecker<'_>, decl: &FuncDecl) -> crate::ScopeId {
    if let Some(param) = decl.params.first() {
        let param_key = crate::NodeKey::from(param.span);
        if let Some(symbol_id) = checker.resolved.definitions.get(&param_key) {
            return checker.symbols.get(*symbol_id).scope;
        }
    }
    let func_key = func_name_key(decl);
    if let Some(symbol_id) = checker.resolved.definitions.get(&func_key) {
        return checker.symbols.get(*symbol_id).scope;
    }
    checker.symbols.global_scope()
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, decl))]
pub fn check_func_body(checker: &mut TypeChecker<'_>, decl: &FuncDecl) {
    if matches!(decl.name, arandu_parser::FuncName::Method { .. }) {
        validate_method_receiver(checker, decl);
    }

    let type_scope = func_type_scope(checker, decl);
    checker.type_scope_id = Some(type_scope);

    let (ret_ty, return_decl_span) = if let Some(result) = &decl.result {
        (
            checker.lower_result_type(result, type_scope),
            super::super::types::result_type_decl_span(result),
        )
    } else {
        (ArType::Void, decl.span)
    };

    for param in &decl.params {
        let mut param_ty = checker.lower_type_expr(param.ty, type_scope);

        if param.is_receiver
            && let ArType::Named(struct_id, ref args) = param_ty
            && args.is_empty()
            && let Some(struct_params) = checker.type_info.generic_params.get(&struct_id).cloned()
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
            param_ty = ArType::named(struct_id, &new_args, &checker.type_info.type_interner);
        }

        let mut param_ty_id = checker.intern(param_ty);
        // `shared`/`mut` formals (receivers and free-function params) bind as
        // `&T` / `&mut T` so body field access and call-site auto-ref match.
        param_ty_id = apply_receiver_ownership(checker, param_ty_id, param.ownership);
        let param_key = crate::NodeKey::from(param.span);
        if let Some(&symbol_id) = checker.resolved.definitions.get(&param_key) {
            checker.ctx.bind(symbol_id, param_ty_id);
            checker.record_decl_type(symbol_id, param_ty_id);
        }
    }

    let ret_id = checker.intern(ret_ty);
    checker.ctx.push_return(ret_id, return_decl_span);
    let func_key = func_name_key(decl);
    let func_symbol = checker.resolved.definitions.get(&func_key).copied();
    let old_effects = checker.current_observed_effects;
    checker.current_observed_effects = arandu_middle::EffectFlags::NONE;

    // SYN.1: last expression in the function body is an implicit return.
    super::block::check_block_tail(checker, checker.pool, &decl.body, Some(ret_id));
    checker.ctx.pop_return();
    checker.type_scope_id = None;
    checker.finalize_literal_vars();

    if let Some(symbol_id) = func_symbol {
        let declared = checker.type_info.function_effects.get(&symbol_id).copied();
        if let Some(declared_flags) = declared {
            let missing = checker.current_observed_effects.difference(declared_flags);
            if !missing.is_empty() {
                let names = missing.to_names().join(", ");
                let name_span = match &decl.name {
                    arandu_parser::FuncName::Free { span, .. }
                    | arandu_parser::FuncName::Method { span, .. } => *span,
                };
                checker.diagnostics.push(
                    arandu_middle::Diagnostic::error(
                        arandu_middle::DiagCode::T039UnsatisfiedEffect,
                        format!("function performs undeclared effect '{names}'"),
                        decl.span,
                    )
                    .with_label(
                        name_span,
                        format!("declared effects do not permit '{names}'"),
                    )
                    .with_hint(format!(
                        "declare '@Effects({})' or eliminate operations causing '{names}'",
                        checker.current_observed_effects.to_names().join(", ")
                    )),
                );
            }
        }
        let total = declared
            .unwrap_or(arandu_middle::EffectFlags::NONE)
            .union(checker.current_observed_effects);
        checker.type_info.function_effects.insert(symbol_id, total);
    }
    checker.current_observed_effects = old_effects.union(checker.current_observed_effects);
}
