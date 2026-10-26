//! Pretty printer implementation for High-level Intermediate Representation (HIR).

pub mod decl;
pub mod expr;
pub mod pattern;
pub mod stmt;
pub mod types;

pub use decl::print_program;
pub use types::HirPrettyCtx;

impl super::HirExprId {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        ctx.pool.expr(*self).pretty_print_to(out, indent, ctx);
    }
    pub(super) fn pretty_print_inline(&self, ctx: &HirPrettyCtx<'_>) -> String {
        ctx.pool.expr(*self).pretty_print_inline(ctx)
    }
}

impl super::HirDeclId {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        ctx.pool.decl(*self).pretty_print_to(out, indent, ctx);
    }
}

impl super::HirPatternId {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        ctx.pool.pattern(*self).pretty_print_to(out, indent, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;
    use crate::hir::pool::HirPool;
    use crate::hir::{
        HirBlock, HirConst, HirDecl, HirExpr, HirExprKind, HirFunc, HirProgram, HirStmt,
        HirStmtKind, IndexRange,
    };
    use crate::types::{Primitive, TypeInterner};
    use crate::{ScopeId, SymbolKind, SymbolTable};

    fn make_ctx<'a>(pool: &'a HirPool, symbols: &'a SymbolTable) -> HirPrettyCtx<'a> {
        HirPrettyCtx {
            pool,
            symbols,
            show_spans: false,
            type_interner: None,
        }
    }

    #[test]
    fn pretty_print_empty_program() {
        let pool = HirPool::new();
        let symbols = SymbolTable::new(0);
        let program = HirProgram {
            span: Span::new(0, 0, 0),
            module: None,
            decls: Vec::new(),
            pool,
        };
        let ctx = make_ctx(&program.pool, &symbols);
        let out = program.pretty_print(&ctx);
        assert_eq!(out, "Program\n");
    }

    #[test]
    fn pretty_print_func_with_body() {
        let mut symbols = SymbolTable::new(0);
        let main_sym = symbols
            .define(ScopeId(0), "main", SymbolKind::Func, Span::new(0, 0, 0))
            .unwrap();
        let _x_sym = symbols
            .define(ScopeId(0), "x", SymbolKind::Param, Span::new(0, 0, 0))
            .unwrap();

        let mut pool = HirPool::new();
        let int_expr = pool.alloc_expr(HirExpr {
            kind: HirExprKind::Int("42".into()),
            ty: TypeInterner::preinterned_primitive(Primitive::Int),
            span: Span::new(0, 0, 0),
        });
        let values = pool.alloc_expr_list(&[int_expr]);
        let ret_stmt = pool.alloc_stmt(HirStmt {
            kind: HirStmtKind::Return { values },
            span: Span::new(0, 0, 0),
        });
        let stmts = pool.alloc_stmt_list(&[ret_stmt]);
        let body = pool.alloc_block(HirBlock {
            statements: stmts,
            span: Span::new(0, 0, 0),
        });
        let func_decl = pool.alloc_decl(HirDecl::Func(HirFunc {
            symbol: main_sym,
            params: IndexRange::empty(),
            return_type: TypeInterner::preinterned_primitive(Primitive::Int),
            body: Some(body),
            span: Span::new(0, 0, 0),
            is_async: false,
            no_fallback: false,
        }));

        let program = HirProgram {
            span: Span::new(0, 0, 0),
            module: None,
            decls: vec![func_decl],
            pool,
        };
        let ctx = make_ctx(&program.pool, &symbols);
        let out = program.pretty_print(&ctx);
        assert!(out.contains("Func main"));
        assert!(out.contains("Return"));
        assert!(out.contains("Int(42)"));
    }

    #[test]
    fn pretty_print_with_module() {
        let pool = HirPool::new();
        let symbols = SymbolTable::new(0);
        let program = HirProgram {
            span: Span::new(0, 0, 0),
            module: Some("mymod".into()),
            decls: Vec::new(),
            pool,
        };
        let ctx = make_ctx(&program.pool, &symbols);
        let out = program.pretty_print(&ctx);
        assert!(out.contains("Module mymod"));
    }

    #[test]
    fn pretty_print_multiple_decls() {
        let mut symbols = SymbolTable::new(0);
        let _a = symbols
            .define(ScopeId(0), "A", SymbolKind::Const, Span::new(0, 0, 0))
            .unwrap();
        let mut pool = HirPool::new();
        let val = pool.alloc_expr(HirExpr {
            kind: HirExprKind::Bool(true),
            ty: TypeInterner::preinterned_primitive(Primitive::Bool),
            span: Span::new(0, 0, 0),
        });
        let decl_a = pool.alloc_decl(HirDecl::Const(HirConst {
            symbol: _a,
            ty: TypeInterner::preinterned_primitive(Primitive::Bool),
            value: val,
            span: Span::new(0, 0, 0),
        }));
        let program = HirProgram {
            span: Span::new(0, 0, 0),
            module: None,
            decls: vec![decl_a],
            pool,
        };
        let ctx = make_ctx(&program.pool, &symbols);
        let out = program.pretty_print(&ctx);
        assert!(out.contains("Const A"));
        assert!(out.contains("Bool(true)"));
    }
}
