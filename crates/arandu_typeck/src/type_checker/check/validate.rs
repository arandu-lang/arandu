use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind, TypeExprId};
use arandu_parser::{
    Block, CatchHandler, Condition, DeferBody, ForClause, LambdaBody, MatchArmBody, Program,
    ResultType, SimpleStmt, Stmt, TopLevelDecl, TypeExpr,
};

use super::super::TypeChecker;
use super::super::types::ArType;
use arandu_lexer::Span;

const ANY_ERROR_MESSAGE: &str =
    "any is only allowed in variadic parameters, extern declarations, and compiler builtins";

pub(crate) fn contains_any(pool: &AstPool, ty: TypeExprId) -> Option<Span> {
    match pool.type_expr(ty) {
        TypeExpr::Const { .. } => None,
        TypeExpr::Primitive { span, name } => {
            if name == "any" {
                Some(*span)
            } else {
                None
            }
        }
        TypeExpr::Named { args, .. } => {
            for arg in pool.type_expr_list(*args) {
                if let Some(span) = contains_any(pool, *arg) {
                    return Some(span);
                }
            }
            None
        }
        TypeExpr::Nullable { inner, .. }
        | TypeExpr::Pointer { inner, .. }
        | TypeExpr::Ref { inner, .. }
        | TypeExpr::RefMut { inner, .. }
        | TypeExpr::Slice { inner, .. }
        | TypeExpr::Array { elem: inner, .. }
        | TypeExpr::Group { inner, .. } => contains_any(pool, *inner),
        TypeExpr::Func { params, result, .. } => {
            for param in pool.type_expr_list(*params) {
                if let Some(span) = contains_any(pool, *param) {
                    return Some(span);
                }
            }
            if let Some(res) = result {
                match res {
                    ResultType::Single { ty: res_ty, .. } => {
                        if let Some(span) = contains_any(pool, *res_ty) {
                            return Some(span);
                        }
                    }
                    ResultType::Multi { types, .. } => {
                        for res_ty in pool.type_expr_list(*types) {
                            if let Some(span) = contains_any(pool, *res_ty) {
                                return Some(span);
                            }
                        }
                    }
                }
            }
            None
        }
    }
}

#[cold]
#[inline(never)]
fn report_any_error(checker: &mut TypeChecker<'_>, span: Span) {
    checker.diagnostics.push(crate::Diagnostic::error(
        crate::DiagCode::T014InvalidVariadicType,
        ANY_ERROR_MESSAGE,
        span,
    ));
}

fn validate_type_no_any(checker: &mut TypeChecker<'_>, ty: TypeExprId) {
    if let Some(span) = contains_any(checker.pool, ty) {
        report_any_error(checker, span);
    }
}

fn validate_result_type_no_any(checker: &mut TypeChecker<'_>, result: &ResultType) {
    match result {
        ResultType::Single { ty, .. } => validate_type_no_any(checker, *ty),
        ResultType::Multi { types, .. } => {
            for ty in checker.pool.type_expr_list(*types) {
                validate_type_no_any(checker, *ty);
            }
        }
    }
}

fn validate_expr(checker: &mut TypeChecker<'_>, expr: ExprId) {
    match checker.pool.expr(expr) {
        ExprKind::Generic { callee, args } => {
            validate_expr(checker, *callee);
            let arg_ids = checker.pool.type_expr_list(*args).to_vec();
            for arg_id in arg_ids {
                validate_type_no_any(checker, arg_id);
            }
        }
        ExprKind::Field { base, .. }
        | ExprKind::SafeField { base, .. }
        | ExprKind::Alloc { expr: base, .. }
        | ExprKind::Try { expr: base, .. }
        | ExprKind::Group { expr: base, .. }
        | ExprKind::Unary { expr: base, .. } => {
            validate_expr(checker, *base);
        }
        ExprKind::Cast { expr: base, ty, .. } => {
            validate_expr(checker, *base);
            validate_type_no_any(checker, *ty);
        }
        ExprKind::Index { base, index, .. }
        | ExprKind::SafeIndex { base, index, .. }
        | ExprKind::NullCoalesce {
            left: base,
            right: index,
            ..
        }
        | ExprKind::Binary {
            left: base,
            right: index,
            ..
        } => {
            validate_expr(checker, *base);
            validate_expr(checker, *index);
        }
        ExprKind::Call {
            callee,
            args,
            trailing_block,
            ..
        } => {
            validate_expr(checker, *callee);
            let arg_ids = checker.pool.expr_list(*args).to_vec();
            for arg_id in arg_ids {
                validate_expr(checker, arg_id);
            }
            if let Some(block_id) = trailing_block {
                validate_block(checker, checker.pool, checker.pool.block(*block_id));
            }
        }
        ExprKind::StructLiteral { ty, fields, .. } => {
            validate_type_no_any(checker, *ty);
            let field_ids = checker.pool.field_init_list(*fields).to_vec();
            for field_id in field_ids {
                validate_expr(checker, checker.pool.field_init(field_id).value);
            }
        }
        ExprKind::Array { items, .. } => {
            let item_ids = checker.pool.expr_list(*items).to_vec();
            for item_id in item_ids {
                validate_expr(checker, item_id);
            }
        }
        ExprKind::Lambda { params, body, .. } => {
            let param_ids = checker.pool.lambda_param_list(*params).to_vec();
            for param_id in param_ids {
                let param = checker.pool.lambda_param(param_id);
                if let Some(ty) = &param.ty {
                    validate_type_no_any(checker, *ty);
                }
            }
            match body {
                LambdaBody::Expr {
                    expr: inner_expr, ..
                } => validate_expr(checker, *inner_expr),
                LambdaBody::Block { block, .. } => validate_block(checker, checker.pool, block),
            }
        }
        ExprKind::AsyncBlock { block, .. } | ExprKind::UnsafeBlock { block, .. } => {
            validate_block(checker, checker.pool, checker.pool.block(*block));
        }
        ExprKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            validate_condition(checker, condition);
            validate_block(checker, checker.pool, checker.pool.block(*then_block));
            validate_block(checker, checker.pool, checker.pool.block(*else_block));
        }
        ExprKind::Match { value, arms, .. } => {
            validate_expr(checker, *value);
            let arm_ids = checker.pool.match_arm_list(*arms).to_vec();
            for arm_id in arm_ids {
                let arm = checker.pool.match_arm(arm_id);
                if let Some(guard) = &arm.guard {
                    validate_expr(checker, *guard);
                }
                match &arm.body {
                    MatchArmBody::Expr {
                        expr: inner_expr, ..
                    } => validate_expr(checker, *inner_expr),
                    MatchArmBody::Block { block, .. } => {
                        validate_block(checker, checker.pool, block)
                    }
                }
            }
        }
        ExprKind::Catch {
            expr: base,
            handler,
            ..
        } => {
            validate_expr(checker, *base);
            match checker.pool.catch_handler(*handler) {
                CatchHandler::Expr {
                    expr: inner_expr, ..
                } => validate_expr(checker, *inner_expr),
                CatchHandler::Block { block, .. } => validate_block(checker, checker.pool, block),
            }
        }
        ExprKind::InterpolatedString { parts, .. } => {
            let part_ids = checker.pool.string_part_list(*parts).to_vec();
            for part_id in part_ids {
                if let arandu_parser::StringPart::Expr {
                    expr: inner_expr, ..
                } = checker.pool.string_part(part_id)
                {
                    validate_expr(checker, *inner_expr);
                }
            }
        }
        _ => {}
    }
}

fn validate_condition(checker: &mut TypeChecker<'_>, cond: &Condition) {
    match cond {
        Condition::Expr { expr, .. } => validate_expr(checker, *expr),
        Condition::Is { expr, .. } => validate_expr(checker, *expr),
        Condition::And { conditions, .. } => {
            for condition in conditions {
                validate_condition(checker, condition);
            }
        }
    }
}

fn validate_simple_stmt(checker: &mut TypeChecker<'_>, stmt: &SimpleStmt) {
    match stmt {
        SimpleStmt::VarDecl {
            bindings, value, ..
        } => {
            for binding in bindings {
                if let Some(ty) = &binding.ty {
                    validate_type_no_any(checker, *ty);
                }
            }
            validate_expr(checker, *value);
        }
        SimpleStmt::Set { value, .. } => {
            validate_expr(checker, *value);
        }
        SimpleStmt::Expr { expr, .. } => {
            validate_expr(checker, *expr);
        }
    }
}

fn validate_block(checker: &mut TypeChecker<'_>, pool: &AstPool, block: &Block) {
    for &stmt in pool.stmt_list(block.statements) {
        let stmt = pool.stmt(stmt);
        match stmt {
            Stmt::VarDecl {
                bindings, value, ..
            } => {
                for binding in bindings {
                    if let Some(ty) = &binding.ty {
                        validate_type_no_any(checker, *ty);
                    }
                }
                validate_expr(checker, *value);
            }
            Stmt::Set { value, .. } => {
                validate_expr(checker, *value);
            }
            Stmt::Return { values, .. } => {
                for val in values {
                    validate_expr(checker, *val);
                }
            }
            Stmt::Free { expr, .. } => {
                validate_expr(checker, *expr);
            }
            Stmt::Expr { expr, .. } => {
                validate_expr(checker, *expr);
            }
            Stmt::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                validate_condition(checker, condition);
                validate_block(checker, pool, then_block);
                if let Some(block) = else_block {
                    validate_block(checker, pool, block);
                }
            }
            Stmt::For { clause, body, .. } => {
                match clause {
                    ForClause::In { iterable, .. } => {
                        validate_expr(checker, *iterable);
                    }
                    ForClause::CStyle {
                        init,
                        condition,
                        step,
                        ..
                    } => {
                        if let Some(init_stmt) = init {
                            validate_simple_stmt(checker, init_stmt);
                        }
                        if let Some(cond_expr) = condition {
                            validate_expr(checker, *cond_expr);
                        }
                        if let Some(step_stmt) = step {
                            validate_simple_stmt(checker, step_stmt);
                        }
                    }
                }
                validate_block(checker, pool, body);
            }
            Stmt::While {
                condition, body, ..
            } => {
                validate_condition(checker, condition);
                validate_block(checker, pool, body);
            }
            Stmt::Match { expr, .. } => {
                validate_expr(checker, *expr);
            }
            Stmt::Defer { body, .. } | Stmt::ErrDefer { body, .. } => match body {
                DeferBody::Expr { expr, .. } => validate_expr(checker, *expr),
                DeferBody::Block { block, .. } => validate_block(checker, pool, block),
            },
            Stmt::Unsafe { block, .. } => {
                validate_block(checker, pool, block);
            }
            _ => {}
        }
    }
}

pub(crate) fn validate_top_level_any(checker: &mut TypeChecker<'_>, decl: &TopLevelDecl) {
    match decl {
        TopLevelDecl::Struct(struct_decl) => {
            for field in &struct_decl.fields {
                validate_type_no_any(checker, field.ty);
            }
        }
        TopLevelDecl::Enum(enum_decl) => {
            for variant in &enum_decl.variants {
                if let Some(payload) = &variant.payload {
                    match payload {
                        arandu_parser::EnumPayload::Tuple { types, .. } => {
                            for ty in checker.pool.type_expr_list(*types) {
                                validate_type_no_any(checker, *ty);
                            }
                        }
                        arandu_parser::EnumPayload::Struct { fields, .. } => {
                            for field in fields {
                                validate_type_no_any(checker, field.ty);
                            }
                        }
                    }
                }
            }
        }
        TopLevelDecl::TypeAlias(alias_decl) => {
            validate_type_no_any(checker, alias_decl.ty);
        }
        TopLevelDecl::Func(func_decl) => {
            for param in &func_decl.params {
                if !param.is_variadic {
                    validate_type_no_any(checker, param.ty);
                }
            }
            if let Some(result) = &func_decl.result {
                validate_result_type_no_any(checker, result);
            }
            validate_block(checker, checker.pool, &func_decl.body);
        }
        _ => {}
    }
}

pub(crate) fn validate_type_constraints_in_program(
    checker: &mut TypeChecker<'_>,
    program: &Program,
) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        validate_decl_type_constraints(checker, decl);
    }
}

fn validate_decl_type_constraints(checker: &mut TypeChecker<'_>, decl: &TopLevelDecl) {
    let global = checker.symbols.global_scope();
    match decl {
        TopLevelDecl::Struct(d) => {
            let struct_key = crate::NodeKey::from(d.span);
            if let Some(struct_id) = checker.resolved.definitions.get(&struct_key).copied() {
                let mut visited = rustc_hash::FxHashSet::default();
                let mut has_infinite_cycle = false;
                if let Some(fields) = checker.type_info.struct_fields.get(&struct_id).cloned() {
                    for f in fields.iter() {
                        let field_ty = checker.resolve(f.ty);
                        if type_contains_named_without_indirection(
                            &field_ty,
                            struct_id,
                            &checker.type_info.type_interner,
                            &checker.type_info,
                            &mut visited,
                        ) {
                            has_infinite_cycle = true;
                            break;
                        }
                    }
                }
                if has_infinite_cycle {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T029RecursiveStructInfiniteSize,
                        format!(
                            "struct '{}' has recursive/infinite size without indirection",
                            d.name
                        ),
                        d.span,
                    ));
                }
            }
            for field in &d.fields {
                validate_type_expr_constraints(checker, field.ty, global);
            }
        }
        TopLevelDecl::TypeAlias(d) => {
            validate_type_expr_constraints(checker, d.ty, global);
        }
        TopLevelDecl::Func(d) => {
            for param in &d.params {
                validate_type_expr_constraints(checker, param.ty, global);
            }
            if let Some(result) = &d.result {
                match result {
                    ResultType::Single { ty, .. } => {
                        validate_type_expr_constraints(checker, *ty, global);
                    }
                    ResultType::Multi { types, .. } => {
                        for &ty in checker.pool.type_expr_list(*types) {
                            validate_type_expr_constraints(checker, ty, global);
                        }
                    }
                }
            }
        }
        TopLevelDecl::Extern(d) => {
            for member in &d.members {
                for param in &member.params {
                    validate_type_expr_constraints(checker, param.ty, global);
                }
                if let Some(result) = &member.result {
                    match result {
                        ResultType::Single { ty, .. } => {
                            validate_type_expr_constraints(checker, *ty, global);
                        }
                        ResultType::Multi { types, .. } => {
                            for &ty in checker.pool.type_expr_list(*types) {
                                validate_type_expr_constraints(checker, ty, global);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn validate_type_expr_constraints(
    checker: &mut TypeChecker<'_>,
    type_expr_id: TypeExprId,
    scope: crate::ScopeId,
) {
    let expr = checker.pool.type_expr(type_expr_id);
    match expr {
        TypeExpr::Named { args, span, .. } => {
            // First, validate recursively for the arguments:
            for &arg in checker.pool.type_expr_list(*args) {
                validate_type_expr_constraints(checker, arg, scope);
            }
            // Now, lower this specific named type:
            let ty = checker.lower_type_expr(type_expr_id, scope);
            if let ArType::Named(struct_or_enum_id, arg_ids) = ty
                && let Some(params) = checker
                    .type_info
                    .generic_params
                    .get(&struct_or_enum_id)
                    .cloned()
            {
                let interner = &checker.type_info.type_interner;
                let arg_tys: Vec<ArType> = interner
                    .type_args(arg_ids)
                    .iter()
                    .map(|&id| checker.resolve(id))
                    .collect();
                crate::type_checker::types::interfaces::check_instantiation_constraints(
                    checker, &params, &arg_tys, *span,
                );
            }
        }
        TypeExpr::Const { .. } | TypeExpr::Primitive { .. } => {}
        TypeExpr::Nullable { inner, .. }
        | TypeExpr::Pointer { inner, .. }
        | TypeExpr::Ref { inner, .. }
        | TypeExpr::RefMut { inner, .. }
        | TypeExpr::Slice { inner, .. }
        | TypeExpr::Array { elem: inner, .. }
        | TypeExpr::Group { inner, .. } => {
            validate_type_expr_constraints(checker, *inner, scope);
        }
        TypeExpr::Func { params, result, .. } => {
            for &param in checker.pool.type_expr_list(*params) {
                validate_type_expr_constraints(checker, param, scope);
            }
            if let Some(res) = result {
                match res {
                    ResultType::Single { ty, .. } => {
                        validate_type_expr_constraints(checker, *ty, scope);
                    }
                    ResultType::Multi { types, .. } => {
                        for &ty in checker.pool.type_expr_list(*types) {
                            validate_type_expr_constraints(checker, ty, scope);
                        }
                    }
                }
            }
        }
    }
}

fn type_contains_named_without_indirection(
    ty: &ArType,
    target_id: crate::SymbolId,
    interner: &crate::type_checker::types::TypeInterner,
    provider: &dyn arandu_middle::layout::StructLayoutProvider,
    visited: &mut rustc_hash::FxHashSet<crate::SymbolId>,
) -> bool {
    match ty {
        ArType::Named(id, _) => {
            if *id == target_id {
                return true;
            }
            if visited.insert(*id) {
                if let Some(fields) = provider.get_struct_fields(*id) {
                    for f in fields.iter() {
                        let field_ty = interner.resolve(f.ty);
                        if type_contains_named_without_indirection(
                            &field_ty, target_id, interner, provider, visited,
                        ) {
                            return true;
                        }
                    }
                }
                visited.remove(id);
            }
            false
        }
        ArType::Tuple(tys) => {
            for &tid in interner.type_args(*tys).iter() {
                let inner = interner.resolve(tid);
                if type_contains_named_without_indirection(
                    &inner, target_id, interner, provider, visited,
                ) {
                    return true;
                }
            }
            false
        }
        ArType::Array(_, inner) | ArType::ConstArray(_, inner) => {
            let inner_ty = interner.resolve(*inner);
            type_contains_named_without_indirection(
                &inner_ty, target_id, interner, provider, visited,
            )
        }
        ArType::Result(ok, err) => {
            let ok_ty = interner.resolve(*ok);
            let err_ty = interner.resolve(*err);
            type_contains_named_without_indirection(&ok_ty, target_id, interner, provider, visited)
                || type_contains_named_without_indirection(
                    &err_ty, target_id, interner, provider, visited,
                )
        }
        ArType::Option(inner) | ArType::Poll(inner) => {
            let inner_ty = interner.resolve(*inner);
            type_contains_named_without_indirection(
                &inner_ty, target_id, interner, provider, visited,
            )
        }
        ArType::Range(inner) => {
            let inner_ty = interner.resolve(*inner);
            type_contains_named_without_indirection(
                &inner_ty, target_id, interner, provider, visited,
            )
        }
        _ => false,
    }
}
