//! Structural and invariant validation for HIR trees.

use crate::Span;
use crate::SymbolTable;
use crate::hir::HirProgram;
use crate::hir::decl::HirDecl;
use crate::hir::pool::HirPool;

pub(crate) fn check_span(span: Span) -> Result<(), String> {
    if span.start == 0 && span.end == 0 {
        return Ok(());
    }
    if span.start > span.end {
        return Err(format!(
            "Invalid span: start {} is greater than end {}",
            span.start, span.end
        ));
    }
    Ok(())
}

impl HirProgram {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        for decl_id in &self.decls {
            let decl = pool.decl(*decl_id);
            check_span(decl.span())?;
            match decl {
                HirDecl::Const(c) => {
                    let _sym = symbols.get(c.symbol);
                    pool.expr(c.value).validate_invariants(pool, symbols)?;
                }
                HirDecl::TypeAlias(_) => {}
                HirDecl::Func(f) => {
                    check_span(f.span)?;
                    for param in pool.params_list(f.params) {
                        check_span(param.span)?;
                    }
                    if let Some(body_id) = f.body {
                        pool.block(body_id).validate_invariants(pool, symbols)?;
                    }
                }
                HirDecl::Struct(s) => {
                    check_span(s.span)?;
                    for field in pool.struct_fields_list(s.fields) {
                        check_span(field.span)?;
                    }
                }
                HirDecl::Enum(e) => {
                    check_span(e.span)?;
                    for var in pool.enum_variants_list(e.variants) {
                        check_span(var.span)?;
                    }
                }
                HirDecl::Interface(i) => {
                    check_span(i.span)?;
                }
                HirDecl::Extern(ex) => {
                    check_span(ex.span)?;
                    for m in pool.func_signatures_list(ex.members) {
                        check_span(m.span)?;
                    }
                }
            }
        }
        Ok(())
    }
}
