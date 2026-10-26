use super::super::{
    GenericParam, ParseError, Parser, TokenKind, TypeExpr, TypeName, WhereItem, is_type_token,
};
use crate::{IndexRange, TypeExprId};
use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_generic_params(
        &mut self,
    ) -> Result<SmallVec<[GenericParam; 2]>, ParseError> {
        if !self.eat_name("LT") {
            return Ok(SmallVec::new());
        }
        let params_vec = self.parse_generic_list(1, |parser| {
            let start = parser.mark();
            // Both spellings share the existing value-parameter AST contract;
            // comptime here is not an extra runtime argument or evaluator.
            let is_const = parser.eat_name("KW_CONST") || parser.eat_name("KW_COMPTIME");
            let name = parser.expect_ident_type()?;
            let const_ty = if is_const {
                parser.expect_name("COLON")?;
                Some(parser.parse_type()?)
            } else {
                None
            };
            let constraints = if !is_const && parser.eat_name("COLON") {
                parser.parse_constraint_list()?.into()
            } else {
                SmallVec::new()
            };
            // T2.1: `T = DefaultType` after optional constraints.
            let default = if !is_const && parser.eat_name("EQUAL") {
                Some(parser.parse_type()?)
            } else {
                None
            };
            Ok(GenericParam {
                span: parser.span_from_mark(start),
                name,
                const_ty,
                constraints,
                default,
            })
        })?;
        self.expect_gt()?;
        Ok(params_vec.into())
    }

    pub(in crate::parser) fn parse_generic_args(&mut self) -> Result<IndexRange, ParseError> {
        self.expect_name("LT")?;
        let args = self.parse_generic_list(1, |parser| parser.parse_generic_argument())?;
        self.expect_gt()?;
        let range = self.pool.alloc_type_expr_list(&args);
        Ok(range)
    }

    pub(in crate::parser) fn parse_generic_argument(&mut self) -> Result<TypeExprId, ParseError> {
        if matches!(self.current().kind, TokenKind::IntDec) {
            let start = self.mark();
            let value = SmolStr::new(self.current_text());
            self.advance();
            let span = self.span_from_mark(start);
            Ok(self.pool.alloc_type_expr(TypeExpr::Const { span, value }))
        } else if matches!(self.current().kind, TokenKind::KwComptime) {
            let start = self.mark();
            self.advance();
            self.expect_name("LPAREN")?;
            let expression = self.parse_expr(0)?;
            self.expect_name("RPAREN")?;
            let span = self.span_from_mark(start);
            Ok(self
                .pool
                .alloc_type_expr(TypeExpr::ConstExpression { span, expression }))
        } else {
            self.parse_type()
        }
    }

    pub(in crate::parser) fn parse_where_clause(
        &mut self,
        end_name: &str,
    ) -> Result<SmallVec<[WhereItem; 2]>, ParseError> {
        if !self.eat_name("KW_WHERE") {
            return Ok(SmallVec::new());
        }
        let mut items = SmallVec::new();
        loop {
            let start = self.mark();
            let name = self.expect_ident_type()?;
            self.expect_name("COLON")?;
            let constraints = self.parse_constraint_list()?.into();
            items.push(WhereItem {
                span: self.span_from_mark(start),
                name,
                constraints,
            });
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name(end_name) {
                break;
            }
        }
        Ok(items)
    }

    pub(in crate::parser) fn parse_constraint_list(
        &mut self,
    ) -> Result<Vec<TypeExprId>, ParseError> {
        let mut constraints = vec![self.parse_type()?];
        while self.eat_name("PLUS") {
            constraints.push(self.parse_type()?);
        }
        Ok(constraints)
    }

    pub(in crate::parser) fn parse_type_name(&mut self) -> Result<TypeName, ParseError> {
        let start = self.mark();
        let mut path = Vec::new();
        while matches!(self.current().kind, TokenKind::IdentValue)
            && !(self.current_text() == "slice"
                && !self
                    .tokens
                    .get(self.pos + 2)
                    .is_some_and(|token| matches!(token.kind, TokenKind::IdentType)))
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| matches!(token.kind, TokenKind::Dot))
        {
            path.push(self.expect_ident_value()?);
            self.expect_name("DOT")?;
        }
        let last = match &self.current().kind {
            TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            TokenKind::IdentValue if matches!(self.current_text(), "void" | "slice") => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            kind if kind.is_primitive_type() => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            _ => self.expect_ident_type()?,
        };
        path.push(last);
        Ok(TypeName {
            span: self.span_from_mark(start),
            path: path.into(),
        })
    }

    pub(in crate::parser) fn can_start_type(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::KwPtr | TokenKind::LBracket | TokenKind::KwFunc | TokenKind::LParen
        ) || is_type_token(&self.current().kind)
    }

    pub(in crate::parser) fn looks_like_generic_call_or_block_suffix(&self) -> bool {
        self.find_matching_gt(self.pos)
            .and_then(|gt| self.tokens.get(gt + 1))
            .is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::LParen | TokenKind::LBrace | TokenKind::KwAs
                )
            })
    }

    pub(in crate::parser) fn looks_like_bare_generic_args(&self) -> bool {
        self.find_matching_gt(self.pos).is_some()
    }

    pub(in crate::parser) fn looks_like_generic_args_boundary(&self) -> bool {
        self.find_matching_gt(self.pos)
            .and_then(|gt| self.tokens.get(gt + 1))
            .is_none_or(|token| {
                matches!(
                    token.kind,
                    TokenKind::Semicolon
                        | TokenKind::Comma
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::RBrace
                        | TokenKind::Eof
                )
            })
    }

    /// Find the `>` closing the generic argument list that opens at `start`.
    ///
    /// The scan is **delimiter-aware**: a `>` nested inside `()`, `[]` or `{}`
    /// groups opened after the `<` cannot close the generic list. Without this,
    /// `while a < b { x = c > (y) }` would match the `<` in `a < b` with the
    /// `>` of a later comparison (crossing a block boundary) and misparse the
    /// comparison as generic arguments.
    ///
    /// The search also **aborts at statement/expression boundaries** before the
    /// closing `>` is found: a `;` (explicit or ASI), a top-level `=` or a
    /// top-level closing delimiter (`)`, `]`, `}`) means the `<` is a
    /// comparison, not a generic argument list — the list could only span a
    /// statement if no closing `>` existed inside it.
    pub(in crate::parser) fn find_matching_gt(&self, start: usize) -> Option<usize> {
        if !matches!(self.tokens.get(start)?.kind, TokenKind::Lt) {
            return None;
        }

        // Record the delimiter level of every `<`. A `>` closes only an opener
        // at its own level: this distinguishes `F<(G<T>)>` from a comparison
        // inside a grouped expression without allocating on ordinary nesting.
        let mut generic_open_depths = SmallVec::<[usize; 8]>::new();
        generic_open_depths.push(0);
        let mut delimiter_depth = 0usize;
        for (index, token) in self.tokens.iter().enumerate().skip(start + 1) {
            match token.kind {
                TokenKind::Lt => generic_open_depths.push(delimiter_depth),
                TokenKind::Gt => {
                    if generic_open_depths.last() == Some(&delimiter_depth) {
                        generic_open_depths.pop();
                        if generic_open_depths.is_empty() {
                            return Some(index);
                        }
                    }
                }
                TokenKind::ShiftRight => {
                    for _ in 0..2 {
                        if generic_open_depths.last() == Some(&delimiter_depth) {
                            generic_open_depths.pop();
                            if generic_open_depths.is_empty() {
                                return Some(index);
                            }
                        }
                    }
                }
                TokenKind::LBrace if delimiter_depth == 0 => return None,
                TokenKind::LBrace | TokenKind::LBracket | TokenKind::LParen => {
                    delimiter_depth += 1;
                }
                TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen => {
                    if delimiter_depth == 0 {
                        // Closing a group that opened before the `<` — the
                        // generic list, if any, would have closed before this.
                        return None;
                    }
                    delimiter_depth -= 1;
                    // Comparisons inside a group are not generic openers in
                    // the enclosing list (`F<comptime (a < b)>`).
                    while generic_open_depths
                        .last()
                        .is_some_and(|depth| *depth > delimiter_depth)
                    {
                        generic_open_depths.pop();
                    }
                }
                TokenKind::Semicolon => {
                    // Top-level statement terminator (explicit or ASI): a
                    // generic list cannot span statements.
                    if delimiter_depth == 0 {
                        return None;
                    }
                }
                TokenKind::Equal => {
                    if delimiter_depth == 0 {
                        return None;
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
        }
        None
    }
}
