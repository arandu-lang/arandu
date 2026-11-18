use super::super::{
    LambdaBody, LambdaParam, ParseError, ParseErrorCode, Parser, TokenKind, span_between,
};
use crate::ast::ast_pool::{ExprId, ExprKind, LambdaParamId};

/// Blocks that may nest inside one another (`if`, loops, `match` arms, trailing lambdas).
const MAX_BLOCK_NESTING: u32 = 64;

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
            let lambda = self.parse_trailing_lambda()?;
            end = self.pool.expr_span(lambda);
            args.push(lambda);
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
        let lambda = self.parse_trailing_lambda()?;
        let lambda_span = self.pool.expr_span(lambda);
        let range = self.pool.alloc_expr_list(&[lambda]);
        Ok(self.pool.alloc_expr(
            ExprKind::Call {
                callee,
                args: range,
            },
            span_between(span_start, lambda_span),
        ))
    }

    fn parse_trailing_lambda(&mut self) -> Result<ExprId, ParseError> {
        use crate::syntax::SyntaxKind;
        if self.block_depth >= MAX_BLOCK_NESTING {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedExpression,
                "block nesting exceeds the limit; move the inner code into a function",
                self.current(),
                self.file_id,
                self.source,
            ));
        }
        self.block_depth += 1;
        self.start_node(SyntaxKind::BLOCK);
        let result = self.parse_trailing_lambda_inner();
        self.finish_node();
        self.block_depth -= 1;
        result
    }

    fn parse_trailing_lambda_inner(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("LBRACE")?;
        let params = self.try_parse_trailing_lambda_params().unwrap_or_default();
        let block = self.parse_block_statements_until_rbrace(start)?;
        let span = block.span;
        let params_range = self.pool.alloc_lambda_param_list(&params);
        Ok(self.pool.alloc_expr(
            ExprKind::Lambda {
                params: params_range,
                body: LambdaBody::Block { span, block },
                is_move: false,
            },
            span,
        ))
    }

    fn try_parse_trailing_lambda_params(&mut self) -> Option<Vec<LambdaParamId>> {
        let has_parens = self.at_kind_name("LPAREN");
        if !has_parens && !matches!(self.current().kind, TokenKind::IdentValue) {
            return None;
        }
        let checkpoint = self.checkpoint();
        if has_parens {
            self.advance();
        }
        let mut params = Vec::new();
        if !(has_parens && self.at_kind_name("RPAREN")) {
            loop {
                let param_start = self.mark();
                let Ok(name) = self.expect_ident_value() else {
                    checkpoint.rollback(self);
                    return None;
                };
                let annotated = self.eat_name("COLON");
                let ty = if annotated || self.can_start_type() {
                    match self.parse_type() {
                        Ok(t) => Some(t),
                        Err(_) => {
                            checkpoint.rollback(self);
                            return None;
                        }
                    }
                } else {
                    None
                };
                let param = LambdaParam {
                    span: self.span_from_mark(param_start),
                    name,
                    ty,
                };
                params.push(self.pool.alloc_lambda_param(param));
                if !self.eat_name("COMMA") {
                    break;
                }
            }
        }
        if has_parens && !self.eat_name("RPAREN") {
            checkpoint.rollback(self);
            return None;
        }
        if (!has_parens && params.is_empty()) || !self.eat_name("FAT_ARROW") {
            checkpoint.rollback(self);
            return None;
        }
        Some(params)
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
