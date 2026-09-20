use std::fmt::Write;

use super::super::ast_pool::AstPool;
use super::super::{
    ConstDecl, EnumDecl, EnumPayload, ExternDecl, FuncDecl, FuncName, FuncSignature, ImportDecl,
    InterfaceDecl, Ownership, Param, ResultType, StructDecl, TopLevelDecl, TypeAliasDecl, TypeExpr,
    TypeName, Visibility,
};
use super::expr::dump_expr;
use super::stmt::dump_block_body;
use super::{dump_attrs, dump_generic_params, dump_span, dump_visibility, dump_where_clause};

pub(super) fn dump_top_level_decl(pool: &AstPool, decl: &TopLevelDecl, out: &mut Vec<String>) {
    match decl {
        TopLevelDecl::Const(decl) => dump_const(pool, decl, out),
        TopLevelDecl::TypeAlias(decl) => dump_type_alias(pool, decl, out),
        TopLevelDecl::Func(func) => dump_func(pool, func, out),
        TopLevelDecl::Struct(decl) => dump_struct(pool, decl, out),
        TopLevelDecl::Enum(decl) => dump_enum(pool, decl, out),
        TopLevelDecl::Interface(decl) => dump_interface(pool, decl, out),
        TopLevelDecl::Extern(decl) => dump_extern(pool, decl, out),
        TopLevelDecl::Error(span) => out.push(format!("  DeclError {}", dump_span(*span))),
    }
}

fn dump_const(pool: &AstPool, decl: &ConstDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    let ty = decl
        .ty
        .map(|ty| format!(" {}", dump_type(pool.type_expr(ty), pool)))
        .unwrap_or_default();
    out.push(format!(
        "  Const {} {}{}{ty} = {}",
        dump_span(decl.span),
        dump_visibility(decl.visibility),
        decl.name,
        dump_expr(pool, decl.value)
    ));
}

fn dump_type_alias(pool: &AstPool, decl: &TypeAliasDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    out.push(format!(
        "  Type {} {}{}{} = {}",
        dump_span(decl.span),
        dump_visibility(decl.visibility),
        decl.name,
        dump_generic_params(pool, &decl.generic_params),
        dump_type(pool.type_expr(decl.ty), pool)
    ));
}

fn dump_func(pool: &AstPool, func: &FuncDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &func.attrs, out, 2);
    let params = func
        .params
        .iter()
        .map(|param| dump_param(pool, param))
        .collect::<Vec<_>>()
        .join(", ");
    let result = func
        .result
        .as_ref()
        .map_or_else(|| "void".to_string(), |r| dump_result_type(r, pool));
    let mut modifiers = Vec::new();
    if func.visibility == Visibility::Public {
        modifiers.push("public");
    }
    if func.is_async {
        modifiers.push("async");
    }
    let modifiers = if modifiers.is_empty() {
        String::new()
    } else {
        format!("{} ", modifiers.join(" "))
    };
    out.push(format!(
        "  Func {} {modifiers}{}{}({}) -> {}{}",
        dump_span(func.span),
        dump_func_name(&func.name),
        dump_generic_params(pool, &func.generic_params),
        params,
        result,
        dump_where_clause(pool, &func.where_clause)
    ));
    dump_block_body(pool, &func.body, out, 4);
}

fn dump_struct(pool: &AstPool, decl: &StructDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    out.push(format!(
        "  Struct {} {}{}{}{}",
        dump_span(decl.span),
        dump_visibility(decl.visibility),
        decl.name,
        dump_generic_params(pool, &decl.generic_params),
        dump_where_clause(pool, &decl.where_clause)
    ));
    for field in &decl.fields {
        dump_attrs(pool, &field.attrs, out, 4);
        out.push(format!(
            "    Field {} {}{} {}",
            dump_span(field.span),
            dump_visibility(field.visibility),
            field.name,
            dump_type(pool.type_expr(field.ty), pool)
        ));
    }
}

fn dump_enum(pool: &AstPool, decl: &EnumDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    out.push(format!(
        "  Enum {} {}{}{}{}",
        dump_span(decl.span),
        dump_visibility(decl.visibility),
        decl.name,
        dump_generic_params(pool, &decl.generic_params),
        dump_where_clause(pool, &decl.where_clause)
    ));
    for variant in &decl.variants {
        dump_attrs(pool, &variant.attrs, out, 4);
        let payload = match &variant.payload {
            None => String::new(),
            Some(EnumPayload::Tuple { types, .. }) => {
                let list = pool.type_expr_list(*types);
                let types_str = list
                    .iter()
                    .map(|&ty| dump_type(pool.type_expr(ty), pool))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({types_str})")
            }
            Some(EnumPayload::Struct { fields, .. }) => {
                let fields = fields
                    .iter()
                    .map(|field| {
                        format!(
                            "{} {}",
                            field.name,
                            dump_type(pool.type_expr(field.ty), pool)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" {{ {fields} }}")
            }
        };
        out.push(format!(
            "    Variant {} {}{payload}",
            dump_span(variant.span),
            variant.name
        ));
    }
}

fn dump_interface(pool: &AstPool, decl: &InterfaceDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    out.push(format!(
        "  Interface {} {}{}{}{}",
        dump_span(decl.span),
        dump_visibility(decl.visibility),
        decl.name,
        dump_generic_params(pool, &decl.generic_params),
        dump_where_clause(pool, &decl.where_clause)
    ));
    for member in &decl.members {
        dump_signature(pool, member, out, 4);
    }
}

fn dump_extern(pool: &AstPool, decl: &ExternDecl, out: &mut Vec<String>) {
    dump_attrs(pool, &decl.attrs, out, 2);
    out.push(format!(
        "  Extern {} \"{}\"",
        dump_span(decl.span),
        decl.abi
    ));
    for member in &decl.members {
        dump_signature(pool, member, out, 4);
    }
}

fn dump_signature(pool: &AstPool, signature: &FuncSignature, out: &mut Vec<String>, indent: usize) {
    dump_attrs(pool, &signature.attrs, out, indent);
    let pad = " ".repeat(indent);
    let params = signature
        .params
        .iter()
        .map(|param| dump_param(pool, param))
        .collect::<Vec<_>>()
        .join(", ");
    let result = signature
        .result
        .as_ref()
        .map_or_else(|| "void".to_string(), |r| dump_result_type(r, pool));
    out.push(format!(
        "{pad}Signature {} {}{}({}) -> {}{}",
        dump_span(signature.span),
        signature.name,
        dump_generic_params(pool, &signature.generic_params),
        params,
        result,
        dump_where_clause(pool, &signature.where_clause)
    ));
}

pub(super) fn dump_import(_pool: &AstPool, import: &ImportDecl) -> String {
    match import {
        ImportDecl::ModuleAlias { span, path, alias } => {
            format!("Import {} {} as {alias}", dump_span(*span), path.join("."))
        }
        ImportDecl::Named { span, items, path } => {
            let items = items
                .iter()
                .map(|item| match &item.alias {
                    Some(alias) => format!("{} {} as {alias}", dump_span(item.span), item.name),
                    None => format!("{} {}", dump_span(item.span), item.name),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "From {} Import {} {{ {items} }}",
                path.join("."),
                dump_span(*span)
            )
        }
        ImportDecl::ExternalNamed {
            span,
            items,
            source,
        } => {
            let items = items
                .iter()
                .map(|item| match &item.alias {
                    Some(alias) => format!("{} {} as {alias}", dump_span(item.span), item.name),
                    None => format!("{} {}", dump_span(item.span), item.name),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "From \"{}\" Import {} {{ {items} }}",
                source,
                dump_span(*span)
            )
        }
        ImportDecl::ExternalAlias {
            span,
            source,
            alias,
        } => {
            format!("Import {} \"{}\" as {alias}", dump_span(*span), source)
        }
    }
}

pub(super) fn dump_param(pool: &AstPool, param: &Param) -> String {
    let mut out = format!("{} ", dump_span(param.span));
    if let Some(ownership) = param.ownership {
        out.push_str(match ownership {
            Ownership::Own => "own ",
            Ownership::Mut => "mut ",
            Ownership::Shared => "shared ",
        });
    }
    out.push_str(&param.name);
    out.push(' ');
    out.push_str(&dump_type(pool.type_expr(param.ty), pool));
    if param.is_variadic {
        out.push_str("...");
    }
    out
}

fn dump_func_name(name: &FuncName) -> String {
    match name {
        FuncName::Free { name, .. } => name.to_string(),
        FuncName::Method { receiver, name, .. } => {
            format!("{}.{}", dump_type_name(receiver), name)
        }
    }
}

pub(super) fn dump_result_type(result: &ResultType, pool: &AstPool) -> String {
    match result {
        ResultType::Single { ty, .. } => dump_type(pool.type_expr(*ty), pool),
        ResultType::Multi { types, .. } => {
            let list = pool.type_expr_list(*types);
            let inner = list
                .iter()
                .map(|&t| dump_type(pool.type_expr(t), pool))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({inner})")
        }
    }
}

pub(super) fn dump_type(ty: &TypeExpr, pool: &AstPool) -> String {
    match ty {
        TypeExpr::Const { span, value } => {
            format!("Const {} {value}", dump_span(*span))
        }
        TypeExpr::Primitive { span, name } => format!("Type {} {name}", dump_span(*span)),
        TypeExpr::Named { span, name, args } => {
            let mut out = format!("Type {} {}", dump_span(*span), dump_type_name(name));
            let arg_list = pool.type_expr_list(*args);
            if !arg_list.is_empty() {
                let args_str = arg_list
                    .iter()
                    .map(|&arg| dump_type(pool.type_expr(arg), pool))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = write!(out, "<{args_str}>");
            }
            out
        }
        TypeExpr::Nullable { span, inner } => {
            format!(
                "Nullable {} {}",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
        TypeExpr::Pointer { span, inner } => {
            format!(
                "Ptr {} [{}]",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
        TypeExpr::Ref { span, inner } => {
            format!(
                "Ref {} {}",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
        TypeExpr::RefMut { span, inner } => {
            format!(
                "RefMut {} {}",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
        TypeExpr::Slice { span, inner } => {
            format!(
                "Slice {} {}",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
        TypeExpr::Array {
            span, size, elem, ..
        } => {
            format!(
                "ArrayType {} [{size}]{}",
                dump_span(*span),
                dump_type(pool.type_expr(*elem), pool)
            )
        }
        TypeExpr::Func {
            span,
            params,
            result,
        } => {
            let param_list = pool.type_expr_list(*params);
            let params_str = param_list
                .iter()
                .map(|&p| dump_type(pool.type_expr(p), pool))
                .collect::<Vec<_>>()
                .join(", ");
            match result {
                Some(result) => {
                    format!(
                        "FuncType {} ({params_str}) {}",
                        dump_span(*span),
                        dump_result_type(result, pool)
                    )
                }
                None => format!("FuncType {} ({params_str})", dump_span(*span)),
            }
        }
        TypeExpr::Group { span, inner } => {
            format!(
                "GroupType {} ({})",
                dump_span(*span),
                dump_type(pool.type_expr(*inner), pool)
            )
        }
    }
}

pub(super) fn dump_type_name(name: &TypeName) -> String {
    format!("{} {}", dump_span(name.span), name.path.join("."))
}
