mod decl;
mod expr;
mod stmt;

use std::cell::RefCell;

use arandu_lexer::Span;

use super::ast_pool::AstPool;
use super::{Attribute, BinaryOp, GenericParam, Program, SetOp, UnaryOp, Visibility, WhereItem};
use decl::{dump_import, dump_top_level_decl};

thread_local! {
    static CURRENT_LINE_INDEX: RefCell<Option<arandu_base::line_index::LineIndex>> = const { RefCell::new(None) };
}

pub fn dump_program(program: &Program, line_index: &arandu_base::line_index::LineIndex) -> String {
    CURRENT_LINE_INDEX.with(|cell| {
        *cell.borrow_mut() = Some(line_index.clone());
    });

    let mut out = Vec::new();
    out.push(format!("Program {}", dump_span(program.span)));
    if let Some(module) = &program.module {
        out.push(format!(
            "  Module {} {}",
            dump_span(module.span),
            module.path.join(".")
        ));
    }
    for import in &program.imports {
        out.push(format!("  {}", dump_import(&program.pool, import)));
    }
    for decl in &program.decls {
        dump_top_level_decl(&program.pool, program.pool.decl(*decl), &mut out);
    }

    CURRENT_LINE_INDEX.with(|cell| {
        *cell.borrow_mut() = None;
    });

    out.join("\n")
}

pub(super) fn dump_span(span: Span) -> String {
    CURRENT_LINE_INDEX.with(|cell| {
        if let Some(line_index) = &*cell.borrow() {
            let (start_line, start_col) = line_index.line_col(span.start);
            let (end_line, end_col) = line_index.line_col(span.end);
            format!("@{}:{}-{}:{}", start_line, start_col, end_line, end_col)
        } else {
            format!("@{}:{}-{}:{}", 1, span.start, 1, span.end)
        }
    })
}

pub(super) fn dump_attrs(
    pool: &AstPool,
    attrs: &[Attribute],
    out: &mut Vec<String>,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    for attr in attrs {
        if attr.args.is_empty() {
            out.push(format!("{pad}Attr {} {}", dump_span(attr.span), attr.name));
        } else {
            let args = attr
                .args
                .iter()
                .map(|arg| expr::dump_expr(pool, *arg))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!(
                "{pad}Attr {} {}({args})",
                dump_span(attr.span),
                attr.name
            ));
        }
    }
}

pub(super) fn dump_generic_params(pool: &AstPool, params: &[GenericParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let params_str = params
        .iter()
        .map(|param| {
            let mut s = if let Some(const_ty) = param.const_ty {
                format!(
                    "{} const {}: {}",
                    dump_span(param.span),
                    param.name,
                    decl::dump_type(pool.type_expr(const_ty), pool)
                )
            } else if param.constraints.is_empty() {
                format!("{} {}", dump_span(param.span), param.name)
            } else {
                let constraints = param
                    .constraints
                    .iter()
                    .map(|c| dump_constraint(pool, *c))
                    .collect::<Vec<_>>()
                    .join(" + ");
                format!("{} {}: {constraints}", dump_span(param.span), param.name)
            };
            // T2.1: default type argument
            if let Some(def) = param.default {
                s.push_str(" = ");
                s.push_str(&decl::dump_type(pool.type_expr(def), pool));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{params_str}>")
}

fn dump_constraint(pool: &AstPool, c: crate::TypeExprId) -> String {
    match pool.type_expr(c) {
        crate::TypeExpr::Named { name, args, .. } if pool.type_expr_list(*args).is_empty() => {
            decl::dump_type_name(name)
        }
        _ => decl::dump_type(pool.type_expr(c), pool),
    }
}

pub(super) fn dump_where_clause(pool: &AstPool, where_clause: &[WhereItem]) -> String {
    if where_clause.is_empty() {
        return String::new();
    }
    let items = where_clause
        .iter()
        .map(|item| {
            let constraints = item
                .constraints
                .iter()
                .map(|c| dump_constraint(pool, *c))
                .collect::<Vec<_>>()
                .join(" + ");
            format!("{} {}: {constraints}", dump_span(item.span), item.name)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" where {items}")
}

pub(super) fn dump_visibility(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "",
        Visibility::Public => "public ",
    }
}

pub(super) fn dump_set_op(op: &SetOp) -> &'static str {
    match op {
        SetOp::Assign => "=",
        SetOp::AddAssign => "+=",
        SetOp::SubAssign => "-=",
        SetOp::MulAssign => "*=",
        SetOp::DivAssign => "/=",
        SetOp::ModAssign => "%=",
        SetOp::BitAndAssign => "&=",
        SetOp::BitOrAssign => "|=",
        SetOp::BitXorAssign => "^=",
        SetOp::ShiftLeftAssign => "<<=",
        SetOp::ShiftRightAssign => ">>=",
    }
}

pub(super) fn dump_unary(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
        UnaryOp::Await => "await",
        UnaryOp::Ref => "ref ",
        UnaryOp::RefMut => "mut ref ",
        UnaryOp::Deref => "*",
    }
}

pub(super) fn dump_binary(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Equal => "==",
        BinaryOp::NotEqual => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::LtEqual => "<=",
        BinaryOp::GtEqual => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::BitAnd => "&",
        BinaryOp::ShiftLeft => "<<",
        BinaryOp::ShiftRight => ">>",
        BinaryOp::NullCoalesce => "??",
        BinaryOp::RangeExclusive => "..",
        BinaryOp::RangeInclusive => "..=",
    }
}
