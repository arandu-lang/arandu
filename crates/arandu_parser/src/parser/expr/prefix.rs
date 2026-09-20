use super::super::{
    ParseError, ParseErrorCode, Parser, StringPart, TokenKind, UnaryOp, is_primitive_type_token,
};
use crate::ast::ast_pool::{ExprId, ExprKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_prefix(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        match &self.current().kind {
            TokenKind::Minus => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Bang => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Tilde => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::BitNot,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwAwait => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Await,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Amp => {
                self.advance();
                let is_mut = self.eat_name("KW_MUT");
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: if is_mut {
                            UnaryOp::RefMut
                        } else {
                            UnaryOp::Ref
                        },
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwRef => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Ref,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwMut
                if self.tokens.get(self.pos + 1).map(|token| token.kind)
                    == Some(TokenKind::KwRef) =>
            {
                self.advance();
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::RefMut,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Star => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwAsync => {
                self.advance();
                let block = self.parse_block()?;
                let block_id = self.pool.alloc_block(block);
                let span = self.span_from_mark(start);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::AsyncBlock { block: block_id }, span))
            }
            TokenKind::KwUnsafe => {
                self.advance();
                let block = self.parse_block()?;
                let block_id = self.pool.alloc_block(block);
                let span = self.span_from_mark(start);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::UnsafeBlock { block: block_id }, span))
            }
            TokenKind::KwIf => self.parse_if_expr(),
            TokenKind::KwMatch => self.parse_match_expr(),
            TokenKind::KwSelf => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Path {
                        path: vec![SmolStr::new("self")].into(),
                    },
                    span,
                ))
            }
            TokenKind::IdentValue => {
                if self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Dot))
                    && self
                        .tokens
                        .get(self.pos + 2)
                        .is_some_and(|t| matches!(t.kind, TokenKind::IdentType))
                {
                    self.parse_type_led_expr()
                } else {
                    let name = self.expect_ident_value()?;
                    let span = self.span_from_mark(start);
                    Ok(self.pool.alloc_expr(
                        ExprKind::Path {
                            path: vec![name].into(),
                        },
                        span,
                    ))
                }
            }
            TokenKind::IdentType if !self.ident_type_starts_type_led_expr() => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Path {
                        path: vec![name].into(),
                    },
                    span,
                ))
            }
            kind if matches!(kind, TokenKind::IdentType) || is_primitive_type_token(kind) => {
                self.parse_type_led_expr()
            }
            TokenKind::IntDec | TokenKind::IntHex | TokenKind::IntBin | TokenKind::IntOct => {
                let value = SmolStr::new(self.current_text());
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Int { value }, span))
            }
            TokenKind::Float => {
                let value = SmolStr::new(self.current_text());
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Float { value }, span))
            }
            TokenKind::BoolTrue => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Bool { value: true }, span))
            }
            TokenKind::BoolFalse => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Bool { value: false }, span))
            }
            TokenKind::Char => {
                let value = SmolStr::new(self.current().char_content(self.source));
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Char { value }, span))
            }
            TokenKind::Nil => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Nil, span))
            }
            TokenKind::StringStart => self.parse_string_like("STRING_START", "STRING_END"),
            TokenKind::MultilineStringStart => {
                self.parse_string_like("MULTILINE_STRING_START", "MULTILINE_STRING_END")
            }
            TokenKind::RawString => {
                let value = SmolStr::new(self.current().raw_string_content(self.source));
                self.advance();
                let span = self.span_from_mark(start);
                let text_part = StringPart::Text { span, text: value };
                let part_id = self.pool.alloc_string_part(text_part);
                let range = self.pool.alloc_string_part_list(&[part_id]);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::InterpolatedString { parts: range }, span))
            }
            TokenKind::LParen => {
                let mut is_lambda = false;
                {
                    let checkpoint = self.checkpoint();
                    if self.eat_name("LPAREN") {
                        let mut params_ok = true;
                        if !self.at_kind_name("RPAREN") {
                            loop {
                                if !matches!(self.current().kind, TokenKind::IdentValue) {
                                    params_ok = false;
                                    break;
                                }
                                self.advance();
                                if self.can_start_type() && self.parse_type().is_err() {
                                    params_ok = false;
                                    break;
                                }
                                if !self.eat_name("COMMA") {
                                    break;
                                }
                                if self.at_kind_name("RPAREN") {
                                    break;
                                }
                            }
                        }
                        if params_ok && self.eat_name("RPAREN") && self.at_kind_name("FAT_ARROW") {
                            is_lambda = true;
                        }
                    }
                    checkpoint.rollback(self);
                }

                if is_lambda {
                    self.parse_lambda()
                } else {
                    self.advance();
                    let expr = self.parse_expr(0)?;
                    self.expect_name("RPAREN")?;
                    let span = self.span_from_mark(start);
                    Ok(self.pool.alloc_expr(ExprKind::Group { expr }, span))
                }
            }
            TokenKind::Pipe => self.parse_pipe_lambda(),
            TokenKind::LBracket => self.parse_array(),
            _ => Err(ParseError::new(
                ParseErrorCode::ExpectedExpression,
                "expected expression",
                self.current(),
                self.file_id,
                self.source,
            )),
        }
    }

    /// Uppercase identifiers normally introduce type-led expressions, but scalar const
    /// parameters use the same lexical class and are valid values (`i < N`).  Keep the
    /// decision syntactic: only a following struct literal or associated member requires
    /// parsing the identifier as a type.
    fn ident_type_starts_type_led_expr(&self) -> bool {
        match self.tokens.get(self.pos + 1).map(|token| token.kind) {
            Some(TokenKind::Dot) => true,
            Some(TokenKind::LBrace) => {
                match self.tokens.get(self.pos + 2).map(|token| token.kind) {
                    Some(TokenKind::RBrace) => true,
                    Some(TokenKind::IdentValue | TokenKind::IdentType) => {
                        self.tokens.get(self.pos + 3).is_some_and(|token| {
                            matches!(
                                token.kind,
                                TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                            )
                        })
                    }
                    _ => false,
                }
            }
            Some(TokenKind::Lt) => {
                let mut depth = 0_u32;
                for (index, token) in self.tokens.iter().enumerate().skip(self.pos + 1) {
                    match token.kind {
                        TokenKind::Lt => depth += 1,
                        TokenKind::Gt => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                return self.tokens.get(index + 1).is_some_and(|next| {
                                    matches!(next.kind, TokenKind::LBrace | TokenKind::Dot)
                                });
                            }
                        }
                        TokenKind::ShiftRight => {
                            depth = depth.saturating_sub(2);
                            if depth == 0 {
                                return self.tokens.get(index + 1).is_some_and(|next| {
                                    matches!(next.kind, TokenKind::LBrace | TokenKind::Dot)
                                });
                            }
                        }
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }
}
