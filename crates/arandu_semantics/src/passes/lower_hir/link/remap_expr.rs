use super::{PoolOffsets, map_type};
use arandu_middle::hir::{
    HirCatchHandler, HirCondition, HirExpr, HirExprKind, HirLambdaBody, HirStringPart,
};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(super) fn remap_condition(cond: &HirCondition, offs: &PoolOffsets) -> HirCondition {
    match cond {
        HirCondition::Expr(e) => HirCondition::Expr(offs.expr_id(*e)),
        HirCondition::Is { expr, pattern } => HirCondition::Is {
            expr: offs.expr_id(*expr),
            pattern: offs.pattern_id(*pattern),
        },
        HirCondition::And(conditions) => HirCondition::And(
            conditions
                .iter()
                .map(|condition| remap_condition(condition, offs))
                .collect(),
        ),
    }
}

pub(super) fn remap_expr(
    expr: &HirExpr,
    offs: &PoolOffsets,
    from: &TypeInterner,
    dest_tc: &mut TypeCheckResult,
    ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirExpr {
    let ty = map_type(
        expr.ty,
        from,
        &mut dest_tc.type_info_mut().type_interner,
        ty_cache,
    );
    let kind = match &expr.kind {
        HirExprKind::Path { symbol } => HirExprKind::Path { symbol: *symbol },
        HirExprKind::TypePath {
            type_symbol,
            member_symbol,
        } => HirExprKind::TypePath {
            type_symbol: *type_symbol,
            member_symbol: *member_symbol,
        },
        HirExprKind::Generic { callee, args } => HirExprKind::Generic {
            callee: offs.expr_id(*callee),
            args: args
                .iter()
                .map(|&a| {
                    map_type(
                        a,
                        from,
                        &mut dest_tc.type_info_mut().type_interner,
                        ty_cache,
                    )
                })
                .collect(),
        },
        HirExprKind::Field { base, field } => HirExprKind::Field {
            base: offs.expr_id(*base),
            field: field.clone(),
        },
        HirExprKind::SafeField { base, field } => HirExprKind::SafeField {
            base: offs.expr_id(*base),
            field: field.clone(),
        },
        HirExprKind::Index { base, index } => HirExprKind::Index {
            base: offs.expr_id(*base),
            index: offs.expr_id(*index),
        },
        HirExprKind::SafeIndex { base, index } => HirExprKind::SafeIndex {
            base: offs.expr_id(*base),
            index: offs.expr_id(*index),
        },
        HirExprKind::Try { expr: inner } => HirExprKind::Try {
            expr: offs.expr_id(*inner),
        },
        HirExprKind::Call {
            callee,
            args,
            trailing_block,
        } => HirExprKind::Call {
            callee: offs.expr_id(*callee),
            args: offs.range(*args, offs.expr_ids),
            trailing_block: trailing_block.map(|b| offs.block_id(b)),
        },
        HirExprKind::ResultCtor { variant, value } => HirExprKind::ResultCtor {
            variant: *variant,
            value: offs.expr_id(*value),
        },
        HirExprKind::StructLiteral {
            struct_symbol,
            fields,
        } => HirExprKind::StructLiteral {
            struct_symbol: *struct_symbol,
            fields: offs.range(*fields, offs.field_inits),
        },
        HirExprKind::Array { items } => HirExprKind::Array {
            items: offs.range(*items, offs.expr_ids),
        },
        HirExprKind::ArrayRepeat { value } => HirExprKind::ArrayRepeat {
            value: offs.expr_id(*value),
        },
        HirExprKind::Tuple { items } => HirExprKind::Tuple {
            items: offs.range(*items, offs.expr_ids),
        },
        HirExprKind::Lambda { params, body } => HirExprKind::Lambda {
            params: offs.range(*params, offs.lambda_params),
            body: match body {
                HirLambdaBody::Expr(e) => HirLambdaBody::Expr(offs.expr_id(*e)),
                HirLambdaBody::Block(b) => HirLambdaBody::Block(offs.block_id(*b)),
            },
        },
        HirExprKind::Alloc { expr: inner } => HirExprKind::Alloc {
            expr: offs.expr_id(*inner),
        },
        HirExprKind::AsyncBlock { block } => HirExprKind::AsyncBlock {
            block: offs.block_id(*block),
        },
        HirExprKind::ValueBlock { block } => HirExprKind::ValueBlock {
            block: offs.block_id(*block),
        },
        HirExprKind::UnsafeBlock { block } => HirExprKind::UnsafeBlock {
            block: offs.block_id(*block),
        },
        HirExprKind::If {
            condition,
            then_block,
            else_block,
        } => HirExprKind::If {
            condition: remap_condition(condition, offs),
            then_block: offs.block_id(*then_block),
            else_block: offs.block_id(*else_block),
        },
        HirExprKind::Match { value, arms } => HirExprKind::Match {
            value: offs.expr_id(*value),
            arms: offs.range(*arms, offs.match_arms),
        },
        HirExprKind::Catch {
            expr: inner,
            handler,
        } => HirExprKind::Catch {
            expr: offs.expr_id(*inner),
            handler: match handler {
                HirCatchHandler::Expr(e) => HirCatchHandler::Expr(offs.expr_id(*e)),
                HirCatchHandler::Block {
                    error_symbol,
                    error_name,
                    block,
                } => HirCatchHandler::Block {
                    error_symbol: *error_symbol,
                    error_name: error_name.clone(),
                    block: offs.block_id(*block),
                },
            },
        },
        HirExprKind::NullCoalesce { left, right } => HirExprKind::NullCoalesce {
            left: offs.expr_id(*left),
            right: offs.expr_id(*right),
        },
        HirExprKind::Layout { query, operand_ty } => HirExprKind::Layout {
            query: *query,
            operand_ty: map_type(
                *operand_ty,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            ),
        },
        HirExprKind::Cast {
            expr: inner,
            target_ty,
        } => HirExprKind::Cast {
            expr: offs.expr_id(*inner),
            target_ty: map_type(
                *target_ty,
                from,
                &mut dest_tc.type_info_mut().type_interner,
                ty_cache,
            ),
        },
        HirExprKind::Unary { op, expr: inner } => HirExprKind::Unary {
            op: *op,
            expr: offs.expr_id(*inner),
        },
        HirExprKind::Binary { op, left, right } => HirExprKind::Binary {
            op: *op,
            left: offs.expr_id(*left),
            right: offs.expr_id(*right),
        },
        HirExprKind::Int(s) => HirExprKind::Int(s.clone()),
        HirExprKind::Float(s) => HirExprKind::Float(s.clone()),
        HirExprKind::FloatBits(value) => HirExprKind::FloatBits(*value),
        HirExprKind::FrozenBytes(value) => HirExprKind::FrozenBytes(value.clone()),
        HirExprKind::Bool(b) => HirExprKind::Bool(*b),
        HirExprKind::Char(s) => HirExprKind::Char(s.clone()),
        HirExprKind::Str(s) => HirExprKind::Str(s.clone()),
        HirExprKind::StringInterp { parts } => HirExprKind::StringInterp {
            parts: parts
                .iter()
                .map(|p| match p {
                    HirStringPart::Text(t) => HirStringPart::Text(t.clone()),
                    HirStringPart::Expr(e) => HirStringPart::Expr(offs.expr_id(*e)),
                })
                .collect(),
        },
        HirExprKind::ToStr { value } => HirExprKind::ToStr {
            value: offs.expr_id(*value),
        },
        HirExprKind::Nil => HirExprKind::Nil,
        HirExprKind::Error => HirExprKind::Error,
    };
    HirExpr {
        kind,
        ty,
        span: expr.span,
    }
}
