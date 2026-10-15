use super::super::{
    Block, LambdaBody, LambdaParam, ParseError, ParseErrorCode, Parser, Stmt, StringPart,
    TokenKind, merge_text_parts,
};
use crate::ast::ast_pool::{ExprId, ExprKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_if_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_IF")?;
        let condition = self.parse_condition()?;
        let then_block = self.parse_block()?;
        self.expect_name("KW_ELSE")?;
        let else_block = if self.at_kind_name("KW_IF") {
            let nested = self.parse_if_expr()?;
            let nested_span = self.pool.expr_span(nested);
            Block {
                span: nested_span,
                statements: {
                    let nested_id = self.pool.alloc_stmt(Stmt::Expr {
                        span: nested_span,
                        expr: nested,
                        has_semi: false,
                    });
                    self.pool.alloc_stmt_list(&[nested_id])
                },
            }
        } else {
            self.parse_block()?
        };
        let then_id = self.pool.alloc_block(then_block);
        let else_id = self.pool.alloc_block(else_block);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(
            ExprKind::If {
                condition,
                then_block: then_id,
                else_block: else_id,
            },
            span,
        ))
    }

    pub(super) fn parse_array(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("LBRACKET")?;
        let mut items = Vec::new();
        if !self.at_kind_name("RBRACKET") {
            let value = self.parse_expr(0)?;
            if self.eat_name("SEMICOLON") {
                let count = self.parse_generic_argument()?;
                self.expect_name("RBRACKET")?;
                let span = self.span_from_mark(start);
                return Ok(self
                    .pool
                    .alloc_expr(ExprKind::ArrayRepeat { value, count }, span));
            }
            items.push(value);
            if self.eat_name("COMMA") && !self.at_kind_name("RBRACKET") {
                loop {
                    items.push(self.parse_expr(0)?);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    if self.at_kind_name("RBRACKET") {
                        break;
                    }
                }
            }
        }
        self.expect_name("RBRACKET")?;
        let range = self.pool.alloc_expr_list(&items);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(ExprKind::Array { items: range }, span))
    }

    pub(super) fn parse_lambda(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("LPAREN")?;
        let mut params = Vec::new();
        if !self.at_kind_name("RPAREN") {
            loop {
                let param_start = self.mark();
                let name = self.expect_ident_value()?;
                let ty = if self.can_start_type() {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let param = LambdaParam {
                    span: self.span_from_mark(param_start),
                    name,
                    ty,
                };
                let param_id = self.pool.alloc_lambda_param(param);
                params.push(param_id);
                if !self.eat_name("COMMA") {
                    break;
                }
                if self.at_kind_name("RPAREN") {
                    break;
                }
            }
        }
        self.expect_name("RPAREN")?;
        self.expect_name("FAT_ARROW")?;
        let body = if self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            LambdaBody::Block {
                span: block.span,
                block,
            }
        } else {
            let body_start = self.mark();
            let expr = self.parse_expr(0)?;
            LambdaBody::Expr {
                span: self.span_from_mark(body_start),
                expr,
            }
        };
        let range = self.pool.alloc_lambda_param_list(&params);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(
            ExprKind::Lambda {
                params: range,
                body,
            },
            span,
        ))
    }

    pub(super) fn parse_pipe_lambda(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("PIPE")?;
        let mut params = Vec::new();
        if !self.at_kind_name("PIPE") {
            loop {
                let param_start = self.mark();
                let name = self.expect_ident_value()?;
                let ty = if self.can_start_type() {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let param = LambdaParam {
                    span: self.span_from_mark(param_start),
                    name,
                    ty,
                };
                let param_id = self.pool.alloc_lambda_param(param);
                params.push(param_id);
                if !self.eat_name("COMMA") {
                    break;
                }
                if self.at_kind_name("PIPE") {
                    break;
                }
            }
        }
        self.expect_name("PIPE")?;
        let body = if self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            LambdaBody::Block {
                span: block.span,
                block,
            }
        } else {
            let body_start = self.mark();
            let expr = self.parse_expr(0)?;
            LambdaBody::Expr {
                span: self.span_from_mark(body_start),
                expr,
            }
        };
        let range = self.pool.alloc_lambda_param_list(&params);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(
            ExprKind::Lambda {
                params: range,
                body,
            },
            span,
        ))
    }

    pub(super) fn parse_match_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_MATCH")?;
        let value = self.parse_expr_without_block_calls(0)?;
        self.expect_name("LBRACE")?;
        let mut arms = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            let arm = self.parse_match_arm()?;
            let arm_id = self.pool.alloc_match_arm(arm);
            arms.push(arm_id);
        }
        self.expect_name("RBRACE")?;
        let range = self.pool.alloc_match_arm_list(&arms);
        let span = self.span_from_mark(start);
        Ok(self
            .pool
            .alloc_expr(ExprKind::Match { value, arms: range }, span))
    }

    pub(super) fn parse_string_like(
        &mut self,
        start_name: &str,
        end_name: &str,
    ) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name(start_name)?;
        let mut parts = Vec::new();
        while !self.at_kind_name(end_name) {
            match &self.current().kind {
                TokenKind::StringText => {
                    let span = self.current().span(self.file_id);
                    let text = SmolStr::new(self.current_text());
                    self.advance();
                    parts.push(StringPart::Text { span, text });
                }
                TokenKind::StringEscape => {
                    let span = self.current().span(self.file_id);
                    let value = arandu_lexer::decode_char_content(self.current_text()).ok_or_else(
                        || {
                            ParseError::new(
                                ParseErrorCode::ExpectedExpression,
                                "expected a valid string escape",
                                self.current(),
                                self.file_id,
                                self.source,
                            )
                        },
                    )?;
                    let mut utf8 = [0; 4];
                    let text = SmolStr::new(value.encode_utf8(&mut utf8));
                    self.advance();
                    parts.push(StringPart::Text { span, text });
                }
                TokenKind::InterpStart => {
                    let part_start = self.mark();
                    self.advance();
                    let expr = self.parse_expr(0)?;
                    self.expect_name("INTERP_END")?;
                    parts.push(StringPart::Expr {
                        span: self.span_from_mark(part_start),
                        expr,
                    });
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedExpression,
                        "expected string part",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
        }
        self.expect_name(end_name)?;
        let merged = merge_text_parts(parts);
        let mut part_ids = Vec::new();
        for p in merged {
            part_ids.push(self.pool.alloc_string_part(p));
        }
        let range = self.pool.alloc_string_part_list(&part_ids);
        let span = self.span_from_mark(start);
        Ok(self
            .pool
            .alloc_expr(ExprKind::InterpolatedString { parts: range }, span))
    }
}
