use super::super::{ParseError, Parser, TokenKind, span_between};
use crate::ast::ast_pool::{ExprId, ExprKind};

impl<'a> Parser<'a> {
    pub(super) fn finish_call(&mut self, callee: ExprId) -> Result<ExprId, ParseError> {
        let span_start = self.pool.expr_span(callee);
        self.expect_name("LPAREN")?;
        let mut args = self.parse_arguments()?;
        self.expect_name("RPAREN")?;
        let mut end = self.previous().span(self.file_id);
        // Trailing-block call `f(args) { ... }` only when allowed.
        // Critical: `match f() { arms }` uses parse_expr_without_block_calls so the
        // match's `{` must not be swallowed as a trailing block (else arms parse as
        // statements → "expected type-qualified..." on `Some(x)` / `Ok(x)`).
        if self.allow_block_calls && self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            end = block.span;
            args.push(self.pool.alloc_trailing_lambda(block));
        }
        let range = self.pool.alloc_expr_list(&args);
        Ok(self.pool.alloc_expr(
            ExprKind::Call {
                callee,
                args: range,
            },
            span_between(span_start, end),
        ))
    }

    pub(super) fn finish_trailing_block_call(
        &mut self,
        callee: ExprId,
    ) -> Result<ExprId, ParseError> {
        let span_start = self.pool.expr_span(callee);
        let block = self.parse_block()?;
        let block_span = block.span;
        let lambda = self.pool.alloc_trailing_lambda(block);
        let range = self.pool.alloc_expr_list(&[lambda]);
        Ok(self.pool.alloc_expr(
            ExprKind::Call {
                callee,
                args: range,
            },
            span_between(span_start, block_span),
        ))
    }

    pub(super) fn parse_arguments(&mut self) -> Result<Vec<ExprId>, ParseError> {
        let mut args = Vec::new();
        if self.at_kind_name("RPAREN") {
            return Ok(args);
        }
        loop {
            if matches!(self.current().kind, TokenKind::IdentValue)
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Colon))
            {
                let start = self.mark();
                let name = self.expect_ident_value()?;
                self.expect_name("COLON")?;
                if self.at_kind_name("COMMA") || self.at_kind_name("RPAREN") {
                    let span = self.span_from_mark(start);
                    args.push(self.pool.alloc_expr(
                        ExprKind::Path {
                            path: smallvec::smallvec![name],
                        },
                        span,
                    ));
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    if self.at_kind_name("RPAREN") {
                        break;
                    }
                    continue;
                }
            }
            args.push(self.parse_expr(0)?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name("RPAREN") {
                break;
            }
        }
        Ok(args)
    }
}
