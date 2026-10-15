use arandu_lexer::Span;

use super::super::ast_pool::AstPool;
use super::super::{
    Block, CatchHandler, ExprId, ExprKind, LambdaBody, MatchArm, MatchArmBody, Pattern, StringPart,
};
use super::decl::{dump_type, dump_type_name};
use super::stmt::{dump_block_inline, dump_condition, dump_inline_block};
use super::{dump_binary, dump_span, dump_unary};

pub(super) fn dump_expr(pool: &AstPool, expr: ExprId) -> String {
    let span = pool.expr_span(expr);
    match pool.expr(expr) {
        ExprKind::Path { path } => format!("Path {}({})", dump_span(span), path.join(".")),
        ExprKind::TypePath { type_name, member } => {
            format!(
                "TypePath {}({}.{})",
                dump_span(span),
                dump_type_name(type_name),
                member
            )
        }
        ExprKind::VariantSugar { name, args } => {
            let arg_ids = pool.expr_list(*args);
            if arg_ids.is_empty() {
                format!("VariantSugar {}(.{})", dump_span(span), name)
            } else {
                let args_str = arg_ids
                    .iter()
                    .map(|id| dump_expr(pool, *id))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("VariantSugar {}(.{}({args_str}))", dump_span(span), name)
            }
        }
        ExprKind::Generic { callee, args } => {
            let type_expr_ids = pool.type_expr_list(*args);
            let args_str = type_expr_ids
                .iter()
                .map(|id| dump_type(pool.type_expr(*id), pool))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Generic {}({}, <{args_str}>)",
                dump_span(span),
                dump_expr(pool, *callee)
            )
        }
        ExprKind::Field { base, field } => {
            format!(
                "Field {}({}, {field})",
                dump_span(span),
                dump_expr(pool, *base)
            )
        }
        ExprKind::SafeField { base, field } => {
            format!(
                "SafeField {}({}, {field})",
                dump_span(span),
                dump_expr(pool, *base)
            )
        }
        ExprKind::Index { base, index } => {
            format!(
                "Index {}({}, {})",
                dump_span(span),
                dump_expr(pool, *base),
                dump_expr(pool, *index)
            )
        }
        ExprKind::SafeIndex { base, index } => {
            format!(
                "SafeIndex {}({}, {})",
                dump_span(span),
                dump_expr(pool, *base),
                dump_expr(pool, *index)
            )
        }
        ExprKind::Try { expr } => format!("Try {}({})", dump_span(span), dump_expr(pool, *expr)),
        ExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
            let arg_ids = pool.expr_list(*args);
            let block = trailing_block.map(|block_id| pool.block(block_id));
            dump_call(pool, span, *callee, arg_ids, block)
        }
        ExprKind::StructLiteral { ty, fields } => {
            let field_init_ids = pool.field_init_list(*fields);
            let fields_str = field_init_ids
                .iter()
                .map(|id| {
                    let field = pool.field_init(*id);
                    format!(
                        "{} {}: {}",
                        dump_span(field.span),
                        field.name,
                        dump_expr(pool, field.value)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "StructLiteral {}({}, [{fields_str}])",
                dump_span(span),
                dump_type(pool.type_expr(*ty), pool)
            )
        }
        ExprKind::Array { items } => {
            let item_ids = pool.expr_list(*items);
            let items_str = item_ids
                .iter()
                .map(|item| dump_expr(pool, *item))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Array {}([{items_str}])", dump_span(span))
        }
        ExprKind::ArrayRepeat { value, count } => format!(
            "ArrayRepeat {}({}, {})",
            dump_span(span),
            dump_expr(pool, *value),
            dump_type(pool.type_expr(*count), pool)
        ),
        ExprKind::Lambda { params, body } => {
            let param_ids = pool.lambda_param_list(*params);
            let params_str = param_ids
                .iter()
                .map(|id| {
                    let param = pool.lambda_param(*id);
                    match &param.ty {
                        Some(ty) => {
                            format!(
                                "{} {} {}",
                                dump_span(param.span),
                                param.name,
                                dump_type(pool.type_expr(*ty), pool)
                            )
                        }
                        None => format!("{} {}", dump_span(param.span), param.name),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Lambda {}([{params_str}], {})",
                dump_span(span),
                dump_lambda_body(pool, body)
            )
        }
        ExprKind::Alloc { expr } => {
            format!("Alloc {}({})", dump_span(span), dump_expr(pool, *expr))
        }
        ExprKind::AsyncBlock { block } => {
            dump_inline_block(pool, "AsyncBlock", span, pool.block(*block))
        }
        ExprKind::Comptime { body } => match body {
            crate::ast::ast_pool::ComptimeBody::Expression(expr) => {
                format!("Comptime {}({})", dump_span(span), dump_expr(pool, *expr))
            }
            crate::ast::ast_pool::ComptimeBody::Block(block) => {
                dump_inline_block(pool, "Comptime", span, pool.block(*block))
            }
        },
        ExprKind::UnsafeBlock { block } => {
            dump_inline_block(pool, "UnsafeBlock", span, pool.block(*block))
        }
        ExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            format!(
                "IfExpr {}({}, {}, {})",
                dump_span(span),
                dump_condition(pool, condition),
                dump_block_inline(pool, pool.block(*then_block)),
                dump_block_inline(pool, pool.block(*else_block))
            )
        }
        ExprKind::Match { value, arms } => {
            let arm_ids = pool.match_arm_list(*arms);
            let arms_str = arm_ids
                .iter()
                .map(|id| dump_match_arm(pool, pool.match_arm(*id)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Match {}({}, [{arms_str}])",
                dump_span(span),
                dump_expr(pool, *value)
            )
        }
        ExprKind::Catch { expr, handler } => {
            format!(
                "Catch {}({}, {})",
                dump_span(span),
                dump_expr(pool, *expr),
                dump_catch_handler(pool, pool.catch_handler(*handler))
            )
        }
        ExprKind::NullCoalesce { left, right } => {
            format!(
                "NullCoalesce {}({}, {})",
                dump_span(span),
                dump_expr(pool, *left),
                dump_expr(pool, *right)
            )
        }
        ExprKind::Layout { query, ty } => {
            format!(
                "@{} {}({})",
                query.name(),
                dump_span(span),
                ty.map(|ty| dump_type(pool.type_expr(ty), pool))
                    .unwrap_or_default()
            )
        }
        ExprKind::Cast { expr, ty } => {
            format!(
                "Cast {}({}, {})",
                dump_span(span),
                dump_expr(pool, *expr),
                dump_type(pool.type_expr(*ty), pool)
            )
        }
        ExprKind::Group { expr } => {
            format!("Group {}({})", dump_span(span), dump_expr(pool, *expr))
        }
        ExprKind::Unary { op, expr } => {
            format!(
                "Unary {}({}, {})",
                dump_span(span),
                dump_unary(*op),
                dump_expr(pool, *expr)
            )
        }
        ExprKind::Binary { op, left, right } => {
            format!(
                "Binary {}({}, {}, {})",
                dump_span(span),
                dump_binary(*op),
                dump_expr(pool, *left),
                dump_expr(pool, *right)
            )
        }
        ExprKind::Int { value } => format!("Int {}({value})", dump_span(span)),
        ExprKind::Float { value } => format!("Float {}({value})", dump_span(span)),
        ExprKind::Bool { value } => format!("Bool {}({value})", dump_span(span)),
        ExprKind::Char { value } => format!("Char {}('{value}')", dump_span(span)),
        ExprKind::Byte { value } => format!("Byte {}(b'{value}')", dump_span(span)),
        ExprKind::InterpolatedString { parts } => {
            let part_ids = pool.string_part_list(*parts);
            let parts_resolved: Vec<StringPart> = part_ids
                .iter()
                .map(|id| pool.string_part(*id).clone())
                .collect();
            dump_interpolated_string(pool, span, &parts_resolved)
        }
        ExprKind::Nil => format!("Nil {}", dump_span(span)),
        ExprKind::Error => format!("ExprError {}", dump_span(span)),
    }
}

fn dump_call(
    pool: &AstPool,
    span: Span,
    callee: ExprId,
    args: &[ExprId],
    trailing_block: Option<&Block>,
) -> String {
    let args_str = args
        .iter()
        .map(|arg| dump_expr(pool, *arg))
        .collect::<Vec<_>>()
        .join(", ");
    match trailing_block {
        Some(block) => format!(
            "Call {}({}, [{args_str}], {})",
            dump_span(span),
            dump_expr(pool, callee),
            dump_block_inline(pool, block)
        ),
        None => format!(
            "Call {}({}, [{args_str}])",
            dump_span(span),
            dump_expr(pool, callee)
        ),
    }
}

fn dump_lambda_body(pool: &AstPool, body: &LambdaBody) -> String {
    match body {
        LambdaBody::Expr { expr, .. } => format!("Expr({})", dump_expr(pool, *expr)),
        LambdaBody::Block { block, .. } => dump_block_inline(pool, block),
    }
}

fn dump_catch_handler(pool: &AstPool, handler: &CatchHandler) -> String {
    match handler {
        CatchHandler::Expr { expr, .. } => format!("Expr({})", dump_expr(pool, *expr)),
        CatchHandler::Block { error, block, .. } => {
            format!("Handler({error}, {})", dump_block_inline(pool, block))
        }
    }
}

fn dump_match_arm(pool: &AstPool, arm: &MatchArm) -> String {
    format!(
        "Arm {} {}{} => {}",
        dump_span(arm.span),
        dump_pattern(pool, pool.pattern(arm.pattern)),
        arm.guard
            .as_ref()
            .map(|guard| format!(" if {}", dump_expr(pool, *guard)))
            .unwrap_or_default(),
        dump_match_arm_body(pool, &arm.body)
    )
}

fn dump_match_arm_body(pool: &AstPool, body: &MatchArmBody) -> String {
    match body {
        MatchArmBody::Expr { expr, .. } => dump_expr(pool, *expr),
        MatchArmBody::Block { block, .. } => dump_block_inline(pool, block),
    }
}

pub(super) fn dump_pattern(pool: &AstPool, pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard { span } => format!("Wildcard {}", dump_span(*span)),
        Pattern::Bind {
            span,
            name,
            mutable,
        } => format!(
            "Bind {}({}{name})",
            dump_span(*span),
            if *mutable { "mut " } else { "" }
        ),
        Pattern::Literal { span, expr } => {
            format!("Literal {}({})", dump_span(*span), dump_expr(pool, *expr))
        }
        Pattern::Enum {
            span,
            type_name,
            variant,
            payload,
        } => {
            let payload_str = pool
                .pattern_list(*payload)
                .iter()
                .map(|&p| dump_pattern(pool, pool.pattern(p)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "EnumPattern {}({}.{}, [{payload_str}])",
                dump_span(*span),
                dump_type_name(type_name),
                variant
            )
        }
        Pattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            let payload_str = pool
                .pattern_list(*payload)
                .iter()
                .map(|&p| dump_pattern(pool, pool.pattern(p)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("TypePattern {}({name}, [{payload_str}])", dump_span(*span))
        }
        Pattern::Struct {
            span,
            type_name,
            fields,
        } => {
            let fields_str = pool
                .field_pattern_list(*fields)
                .iter()
                .map(|&field_id| {
                    let field = pool.field_pattern(field_id);
                    match &field.pattern {
                        Some(pat_id) => {
                            format!(
                                "{} {}: {}",
                                dump_span(field.span),
                                field.name,
                                dump_pattern(pool, pool.pattern(*pat_id))
                            )
                        }
                        None => format!("{} {}", dump_span(field.span), field.name),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "StructPattern {}({}, [{fields_str}])",
                dump_span(*span),
                dump_type_name(type_name)
            )
        }
        Pattern::Tuple { span, items } => {
            let items_str = pool
                .pattern_list(*items)
                .iter()
                .map(|&item| dump_pattern(pool, pool.pattern(item)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("TuplePattern {}([{items_str}])", dump_span(*span))
        }
        Pattern::Range {
            span,
            start,
            inclusive,
            end,
        } => {
            let op = if *inclusive { "..=" } else { ".." };
            format!(
                "RangePattern {}({}, {op}, {})",
                dump_span(*span),
                dump_expr(pool, *start),
                dump_expr(pool, *end)
            )
        }
        Pattern::Or { span, alts } => {
            let alts_str = pool
                .pattern_list(*alts)
                .iter()
                .map(|&a| dump_pattern(pool, pool.pattern(a)))
                .collect::<Vec<_>>()
                .join(" | ");
            format!("OrPattern {}([{alts_str}])", dump_span(*span))
        }
    }
}

fn dump_interpolated_string(pool: &AstPool, span: Span, parts: &[StringPart]) -> String {
    if let [StringPart::Text { text, .. }] = parts {
        return format!("String {}(\"{text}\")", dump_span(span));
    }

    let parts_str = parts
        .iter()
        .map(|part| match part {
            StringPart::Text { span, text } => format!("Text {}(\"{text}\")", dump_span(*span)),
            StringPart::Expr { span, expr } => {
                format!("Expr {}({})", dump_span(*span), dump_expr(pool, *expr))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("InterpolatedString {}([{parts_str}])", dump_span(span))
}
