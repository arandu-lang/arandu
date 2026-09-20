use arandu_lexer::Span;
use arandu_parser::{ResultType, TypeExpr, TypeExprId, TypeName, ast_pool::AstPool};

use super::ar_type::ArType;
use super::primitive::Primitive;
use super::result_option::{lower_builtin_generic, type_name_base};
use super::type_interner::TypeInterner;
use crate::{ResolvedNames, ScopeId, SymbolTable};

// ── Lowering context ─────────────────────────────────────────────────

/// Read-only context shared across all type-lowering helpers.
///
/// Groups `pool`, `symbols`, `scope` and `resolved` so that functions
/// that would otherwise need 4-5 extra parameters can accept a single
/// `&LowerCtx<'_>` instead, keeping signatures under the `too_many_arguments`
/// threshold.
pub struct LowerCtx<'a> {
    pub pool: &'a AstPool,
    pub symbols: &'a SymbolTable,
    pub scope: ScopeId,
    pub resolved: &'a ResolvedNames,
}

// ── Lowering from AST TypeExpr to ArType ────────────────────────────

/// Convert an AST `TypeExpr` into an internal `ArType`.
///
/// Uses the symbol table and resolved names to resolve named types to
/// their `SymbolId`. Returns `ArType::Error` for types that cannot be
/// resolved (the name resolver already reported the error).
#[must_use]
pub fn lower_type_expr(
    expr_id: TypeExprId,
    pool: &AstPool,
    symbols: &SymbolTable,
    scope: ScopeId,
    resolved: &ResolvedNames,
    interner: &mut TypeInterner,
) -> ArType {
    let ctx = LowerCtx {
        pool,
        symbols,
        scope,
        resolved,
    };
    lower_type_expr_ctx(expr_id, &ctx, interner)
}

/// Internal implementation that takes a [`LowerCtx`].
pub fn lower_type_expr_ctx(
    expr_id: TypeExprId,
    ctx: &LowerCtx<'_>,
    interner: &mut TypeInterner,
) -> ArType {
    let expr = ctx.pool.type_expr(expr_id);
    match expr {
        TypeExpr::Const { value, .. } => parse_const_u64(value)
            .map(ArType::Const)
            .unwrap_or(ArType::Error),
        TypeExpr::Primitive { name, .. } => {
            if name == "Err" {
                let ty = ArType::Err;
                interner.intern(ty.clone());
                return ty;
            }
            match Primitive::from_name(name) {
                Some(p) => {
                    let ty = ArType::Primitive(p);
                    interner.intern(ty.clone());
                    ty
                }
                None => ArType::Error,
            }
        }
        TypeExpr::Named { span, name, args } => {
            let arg_ids = ctx.pool.type_expr_list(*args);
            lower_named_type(*span, name, arg_ids, ctx, interner)
        }
        TypeExpr::Nullable { inner, .. } => {
            let inner_ty = lower_type_expr_ctx(*inner, ctx, interner);
            let id = interner.intern(inner_ty);
            ArType::Nullable(id)
        }
        TypeExpr::Pointer { inner, .. } => {
            let inner_ty = lower_type_expr_ctx(*inner, ctx, interner);
            let id = interner.intern(inner_ty);
            ArType::Ptr(id)
        }
        TypeExpr::Ref { inner, .. } => {
            let inner_ty = lower_type_expr_ctx(*inner, ctx, interner);
            let id = interner.intern(inner_ty);
            ArType::Ref(id)
        }
        TypeExpr::RefMut { inner, .. } => {
            let inner_ty = lower_type_expr_ctx(*inner, ctx, interner);
            let id = interner.intern(inner_ty);
            ArType::RefMut(id)
        }
        TypeExpr::Slice { inner, .. } => {
            let inner_ty = lower_type_expr_ctx(*inner, ctx, interner);
            let id = interner.intern(inner_ty);
            ArType::Slice(id)
        }
        TypeExpr::Array {
            size,
            size_span,
            elem,
            ..
        } => {
            let elem_ty = lower_type_expr_ctx(*elem, ctx, interner);
            let id = interner.intern(elem_ty);
            if let Some(n) = parse_const_u64(size) {
                ArType::Array(n, id)
            } else if let Some(symbol) = ctx
                .resolved
                .type_refs
                .get(&crate::NodeKey::from(*size_span))
                .copied()
                .filter(|symbol| ctx.symbols.get(*symbol).kind == crate::SymbolKind::ConstParam)
            {
                ArType::ConstArray(symbol, id)
            } else {
                ArType::Error
            }
        }
        TypeExpr::Func { params, result, .. } => {
            let param_ids = ctx.pool.type_expr_list(*params);
            let param_types: Vec<super::type_interner::TypeId> = param_ids
                .iter()
                .map(|&p| {
                    let ty = lower_type_expr_ctx(p, ctx, interner);
                    interner.intern(ty)
                })
                .collect();
            let ret = match result {
                Some(r) => lower_result_type_ctx(r, ctx, interner),
                None => ArType::Void,
            };
            let ret_id = interner.intern(ret);
            ArType::func(&param_types, ret_id, interner)
        }
        TypeExpr::Group { inner, .. } => lower_type_expr_ctx(*inner, ctx, interner),
    }
}

/// Convert an AST `ResultType` into an `ArType`.
#[must_use]
pub fn lower_result_type(
    result: &ResultType,
    pool: &AstPool,
    symbols: &SymbolTable,
    scope: ScopeId,
    resolved: &ResolvedNames,
    interner: &mut TypeInterner,
) -> ArType {
    let ctx = LowerCtx {
        pool,
        symbols,
        scope,
        resolved,
    };
    lower_result_type_ctx(result, &ctx, interner)
}

pub fn lower_result_type_ctx(
    result: &ResultType,
    ctx: &LowerCtx<'_>,
    interner: &mut TypeInterner,
) -> ArType {
    match result {
        ResultType::Single { ty, .. } => lower_type_expr_ctx(*ty, ctx, interner),
        ResultType::Multi { types, .. } => {
            let list = ctx.pool.type_expr_list(*types);
            let tys: Vec<super::type_interner::TypeId> = list
                .iter()
                .map(|&t| {
                    let ty = lower_type_expr_ctx(t, ctx, interner);
                    interner.intern(ty)
                })
                .collect();
            ArType::tuple(&tys, interner)
        }
    }
}

pub fn lower_named_type(
    _span: Span,
    name: &TypeName,
    args: &[TypeExprId],
    ctx: &LowerCtx<'_>,
    interner: &mut TypeInterner,
) -> ArType {
    if type_name_base(name) == "void" && args.is_empty() {
        return ArType::Void;
    }
    if type_name_base(name) == "Err" && args.is_empty() {
        return ArType::Err;
    }
    if let Some(builtin) = lower_builtin_generic(name, args, ctx, interner) {
        return builtin;
    }

    // The name resolver already resolved this name — look up the symbol ID.
    let key = crate::NodeKey::from(name.span);
    if let Some(&symbol_id) = ctx.resolved.type_refs.get(&key) {
        if ctx.symbols.get(symbol_id).kind == crate::SymbolKind::ConstParam && args.is_empty() {
            return ArType::ConstParam(symbol_id);
        }
        let generic_args: Vec<super::type_interner::TypeId> = args
            .iter()
            .map(|&a| {
                let ty = lower_type_expr_ctx(a, ctx, interner);
                interner.intern(ty)
            })
            .collect();
        ArType::named(symbol_id, &generic_args, interner)
    } else {
        // Name was not resolved — name resolver already emitted an error.
        ArType::Error
    }
}

fn parse_const_u64(text: &str) -> Option<u64> {
    let mut value = 0u64;
    let mut saw_digit = false;
    for byte in text.bytes() {
        if byte == b'_' {
            continue;
        }
        let digit = byte.checked_sub(b'0')?;
        if digit > 9 {
            return None;
        }
        saw_digit = true;
        value = value.checked_mul(10)?.checked_add(u64::from(digit))?;
    }
    saw_digit.then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Primitive;
    use arandu_parser::ast_pool::IndexRange;

    fn new_interner() -> TypeInterner {
        TypeInterner::new()
    }

    fn new_pool() -> AstPool {
        AstPool::new()
    }

    fn default_symbols() -> SymbolTable {
        SymbolTable::new(0)
    }

    fn default_resolved() -> ResolvedNames {
        ResolvedNames::default()
    }

    // ── Primitive ──

    #[test]
    fn lowers_int_primitive() {
        let mut pool = new_pool();
        let id = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 1, 0),
            name: "int".into(),
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(
            lower_type_expr_ctx(id, &ctx, &mut i),
            ArType::Primitive(Primitive::Int)
        );
    }

    #[test]
    fn lowers_err_primitive() {
        let mut pool = new_pool();
        let id = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "Err".into(),
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(lower_type_expr_ctx(id, &ctx, &mut i), ArType::Err);
    }

    #[test]
    fn lowers_unknown_primitive_to_error() {
        let mut pool = new_pool();
        let id = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 5, 0),
            name: "unknown".into(),
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(lower_type_expr_ctx(id, &ctx, &mut i), ArType::Error);
    }

    // ── Nullable ──

    #[test]
    fn lowers_nullable_int() {
        let mut pool = new_pool();
        let inner = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Nullable {
            span: Span::new(0, 4, 0),
            inner,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let expected_inner = i.intern(ArType::Primitive(Primitive::Int));
        assert_eq!(result, ArType::Nullable(expected_inner));
    }

    // ── Pointer ──

    #[test]
    fn lowers_pointer() {
        let mut pool = new_pool();
        let inner = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Pointer {
            span: Span::new(0, 8, 0),
            inner,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let expected_inner = i.intern(ArType::Primitive(Primitive::Int));
        assert_eq!(result, ArType::Ptr(expected_inner));
    }

    // ── Slice ──

    #[test]
    fn lowers_slice() {
        let mut pool = new_pool();
        let inner = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Slice {
            span: Span::new(0, 5, 0),
            inner,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let expected_inner = i.intern(ArType::Primitive(Primitive::Int));
        assert_eq!(result, ArType::Slice(expected_inner));
    }

    // ── Array ──

    #[test]
    fn lowers_array() {
        let mut pool = new_pool();
        let elem = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "float".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Array {
            span: Span::new(0, 8, 0),
            size: "10".into(),
            size_span: Span::new(0, 1, 0),
            elem,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let expected_elem = i.intern(ArType::Primitive(Primitive::Float));
        assert_eq!(result, ArType::Array(10, expected_elem));
    }

    #[test]
    fn lowers_array_invalid_size_to_error() {
        let mut pool = new_pool();
        let elem = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Array {
            span: Span::new(0, 8, 0),
            size: "abc".into(),
            size_span: Span::new(0, 3, 0),
            elem,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        assert_eq!(result, ArType::Error);
    }

    // ── Group ──

    #[test]
    fn lowers_group_unwraps() {
        let mut pool = new_pool();
        let inner = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "bool".into(),
        });
        let id = pool.alloc_type_expr(TypeExpr::Group {
            span: Span::new(0, 5, 0),
            inner,
        });
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(
            lower_type_expr_ctx(id, &ctx, &mut i),
            ArType::Primitive(Primitive::Bool)
        );
    }

    // ── Named with resolved type refs ──

    #[test]
    fn lowers_named_type_via_resolved_refs() {
        let mut pool = new_pool();
        let mut symbols = SymbolTable::new(0);
        let mut resolved = ResolvedNames::default();

        let span = Span::new(0, 4, 0);
        let sym = symbols
            .define(ScopeId(0), "User", crate::SymbolKind::Struct, span)
            .unwrap();
        resolved.type_refs.insert(crate::NodeKey::from(span), sym);

        let name = TypeName {
            span,
            path: vec!["User".into()].into(),
        };
        let id = pool.alloc_type_expr(TypeExpr::Named {
            span,
            name,
            args: IndexRange::empty(),
        });

        let mut i = new_interner();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(
            lower_type_expr_ctx(id, &ctx, &mut i),
            ArType::named(sym, &[], &i)
        );
    }

    #[test]
    fn lowers_unresolved_named_returns_error() {
        let mut pool = new_pool();
        let resolved = ResolvedNames::default();

        let name = TypeName {
            span: Span::new(0, 4, 0),
            path: vec!["Unknown".into()].into(),
        };
        let id = pool.alloc_type_expr(TypeExpr::Named {
            span: Span::new(0, 4, 0),
            name,
            args: IndexRange::empty(),
        });

        let mut i = new_interner();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &default_symbols(),
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(lower_type_expr_ctx(id, &ctx, &mut i), ArType::Error);
    }

    // ── Func type ──

    #[test]
    fn lowers_func_type_no_return() {
        let mut pool = new_pool();
        let param = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let params_range = pool.alloc_type_expr_list(&[param]);
        let id = pool.alloc_type_expr(TypeExpr::Func {
            span: Span::new(0, 12, 0),
            params: params_range,
            result: None,
        });

        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let int_tid = i.intern(ArType::Primitive(Primitive::Int));
        let void_tid = i.intern(ArType::Void);
        assert_eq!(result, ArType::func(&[int_tid], void_tid, &i));
    }

    #[test]
    fn lowers_func_type_with_return() {
        let mut pool = new_pool();
        let param = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let ret_expr = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 4, 0),
            name: "bool".into(),
        });
        let params_range = pool.alloc_type_expr_list(&[param]);
        let id = pool.alloc_type_expr(TypeExpr::Func {
            span: Span::new(0, 18, 0),
            params: params_range,
            result: Some(ResultType::Single {
                span: Span::new(10, 18, 0),
                ty: ret_expr,
            }),
        });

        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        let result = lower_type_expr_ctx(id, &ctx, &mut i);
        let int_tid = i.intern(ArType::Primitive(Primitive::Int));
        let bool_tid = i.intern(ArType::Primitive(Primitive::Bool));
        assert_eq!(result, ArType::func(&[int_tid], bool_tid, &i));
    }

    // ── lower_result_type ──

    #[test]
    fn lowers_single_result_type() {
        let mut pool = new_pool();
        let inner = pool.alloc_type_expr(TypeExpr::Primitive {
            span: Span::new(0, 3, 0),
            name: "int".into(),
        });
        let result = ResultType::Single {
            span: Span::new(0, 3, 0),
            ty: inner,
        };

        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(
            lower_result_type_ctx(&result, &ctx, &mut i),
            ArType::Primitive(Primitive::Int)
        );
    }

    // ── lower_named_type with void ──

    #[test]
    fn lowers_void_type_name() {
        let pool = new_pool();
        let name = TypeName {
            span: Span::new(0, 4, 0),
            path: vec!["void".into()].into(),
        };
        let mut i = new_interner();
        let symbols = default_symbols();
        let resolved = default_resolved();
        let ctx = LowerCtx {
            pool: &pool,
            symbols: &symbols,
            scope: ScopeId(0),
            resolved: &resolved,
        };
        assert_eq!(
            lower_named_type(Span::new(0, 4, 0), &name, &[], &ctx, &mut i),
            ArType::Void
        );
    }
}
