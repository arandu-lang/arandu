use super::{
    Expr, MatchArm, MatchArmBody, ParseError, ParseErrorCode, Parser, Pattern, TokenKind, TypeName,
};
use crate::ast::{FieldPattern, IndexRange, PatternId};

impl<'a> Parser<'a> {
    pub(super) fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let start = self.mark();
        let pattern = self.parse_pattern()?;
        let guard = if self.eat_name("KW_IF") {
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        self.expect_name("FAT_ARROW")?;
        let body = if self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            MatchArmBody::Block {
                span: block.span,
                block,
            }
        } else {
            let body_start = self.mark();
            let expr = self.parse_expr(0)?;
            if self.at_kind_name("SEMICOLON") {
                self.expect_semicolon()?;
            }
            MatchArmBody::Expr {
                span: self.span_from_mark(body_start),
                expr,
            }
        };
        Ok(MatchArm {
            span: self.span_from_mark(start),
            pattern,
            guard,
            body,
        })
    }

    pub(super) fn parse_pattern(&mut self) -> Result<PatternId, ParseError> {
        // SYN.4: `p1 | p2 | …`
        let mark = self.mark();
        let first = self.parse_pattern_atom()?;
        if !matches!(self.current().kind, TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat_name("PIPE") {
            alts.push(self.parse_pattern_atom()?);
        }
        let range = self.pool.alloc_pattern_list(&alts);
        Ok(self.pool.alloc_pattern(Pattern::Or {
            span: self.span_from_mark(mark),
            alts: range,
        }))
    }

    pub(super) fn parse_pattern_atom(&mut self) -> Result<PatternId, ParseError> {
        let start = self.mark();
        if matches!(&self.current().kind, TokenKind::IdentValue) && self.current_text() == "_" {
            self.advance();
            return Ok(self.pool.alloc_pattern(Pattern::Wildcard {
                span: self.span_from_mark(start),
            }));
        }
        if self.at_kind_name("LPAREN") {
            self.advance();
            let mut items = vec![self.parse_pattern()?];
            self.expect_name("COMMA")?;
            items.push(self.parse_pattern()?);
            while self.eat_name("COMMA") {
                if self.at_kind_name("RPAREN") {
                    break;
                }
                items.push(self.parse_pattern()?);
            }
            self.expect_name("RPAREN")?;
            let range = self.pool.alloc_pattern_list(&items);
            return Ok(self.pool.alloc_pattern(Pattern::Tuple {
                span: self.span_from_mark(start),
                items: range,
            }));
        }
        if matches!(self.current().kind, TokenKind::IdentType) {
            let type_start_span = self.current().span(self.file_id);
            let name = self.expect_ident_type()?;
            if self.eat_name("DOT") {
                let variant = self.expect_member_name()?;
                let payload = if self.eat_name("LPAREN") {
                    let payload = self.parse_pattern_list_until("RPAREN")?;
                    self.expect_name("RPAREN")?;
                    self.pool.alloc_pattern_list(&payload)
                } else {
                    IndexRange::empty()
                };
                return Ok(self.pool.alloc_pattern(Pattern::Enum {
                    span: self.span_from_mark(start),
                    type_name: TypeName {
                        span: type_start_span,
                        path: vec![name].into(),
                    },
                    variant,
                    payload,
                }));
            }
            if self.eat_name("LBRACE") {
                let mut fields = Vec::new();
                if !self.at_kind_name("RBRACE") {
                    loop {
                        let field_start = self.mark();
                        let name = self.expect_ident_value()?;
                        let pattern = if self.eat_name("COLON") {
                            Some(self.parse_pattern()?)
                        } else {
                            None
                        };
                        let field_pat_id = self.pool.alloc_field_pattern(FieldPattern {
                            span: self.span_from_mark(field_start),
                            name,
                            pattern,
                        });
                        fields.push(field_pat_id);
                        if !self.eat_name("COMMA") {
                            break;
                        }
                        if self.at_kind_name("RBRACE") {
                            break;
                        }
                    }
                }
                self.expect_name("RBRACE")?;
                let range = self.pool.alloc_field_pattern_list(&fields);
                return Ok(self.pool.alloc_pattern(Pattern::Struct {
                    span: self.span_from_mark(start),
                    type_name: TypeName {
                        span: type_start_span,
                        path: vec![name].into(),
                    },
                    fields: range,
                }));
            }
            if self.eat_name("LPAREN") {
                let payload = self.parse_pattern_list_until("RPAREN")?;
                self.expect_name("RPAREN")?;
                let range = self.pool.alloc_pattern_list(&payload);
                return Ok(self.pool.alloc_pattern(Pattern::TypeTuple {
                    span: self.span_from_mark(start),
                    name,
                    payload: range,
                }));
            }
            return Ok(self.pool.alloc_pattern(Pattern::TypeTuple {
                span: self.span_from_mark(start),
                name,
                payload: IndexRange::empty(),
            }));
        }
        let mutable = self.eat_name("KW_MUT");
        if mutable || matches!(self.current().kind, TokenKind::IdentValue) {
            let name = self.expect_ident_value()?;
            return Ok(self.pool.alloc_pattern(Pattern::Bind {
                span: self.span_from_mark(start),
                name,
                mutable,
            }));
        }
        let literal = self.parse_literal_pattern_expr()?;
        if self.eat_name("RANGE_EXCLUSIVE") || self.eat_name("RANGE_INCLUSIVE") {
            let inclusive = self.previous().kind == TokenKind::RangeInclusive;
            let end = self.parse_literal_pattern_expr()?;
            return Ok(self.pool.alloc_pattern(Pattern::Range {
                span: self.span_from_mark(start),
                start: literal,
                inclusive,
                end,
            }));
        }
        Ok(self.pool.alloc_pattern(Pattern::Literal {
            span: self.span_from_mark(start),
            expr: literal,
        }))
    }

    pub(super) fn parse_pattern_list_until(
        &mut self,
        end: &str,
    ) -> Result<Vec<PatternId>, ParseError> {
        let mut patterns = Vec::new();
        if self.at_kind_name(end) {
            return Ok(patterns);
        }
        loop {
            patterns.push(self.parse_pattern()?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name(end) {
                break;
            }
        }
        Ok(patterns)
    }

    pub(super) fn parse_literal_pattern_expr(&mut self) -> Result<Expr, ParseError> {
        match &self.current().kind {
            TokenKind::IntDec
            | TokenKind::IntHex
            | TokenKind::IntBin
            | TokenKind::IntOct
            | TokenKind::Float
            | TokenKind::BoolTrue
            | TokenKind::BoolFalse
            | TokenKind::Char
            | TokenKind::StringStart
            | TokenKind::Nil => self.parse_prefix(),
            _ => Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                "expected pattern",
                self.current(),
                self.file_id,
                self.source,
            )),
        }
    }
}
