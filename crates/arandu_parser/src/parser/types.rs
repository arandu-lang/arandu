use super::{
    GenericParam, Ownership, Param, ParseError, ParseErrorCode, Parser, ResultType, TokenKind,
    TypeExpr, TypeName, WhereItem, is_type_token, primitive_type_name,
};
use crate::{IndexRange, TypeExprId};

fn type_expr_is_err_slot(ty_id: TypeExprId, pool: &crate::ast::ast_pool::AstPool) -> bool {
    match pool.type_expr(ty_id) {
        TypeExpr::Nullable { inner, .. } => type_expr_is_err_slot(*inner, pool),
        TypeExpr::Primitive { name, .. } => name == "Err",
        TypeExpr::Named { name, args, .. } => {
            args.is_empty() && name.path.len() == 1 && name.path[0] == "Err"
        }
        _ => false,
    }
}

fn result_type_must_use_result_generic(
    result: &ResultType,
    pool: &crate::ast::ast_pool::AstPool,
) -> bool {
    match result {
        ResultType::Single { ty, .. } => type_expr_is_err_slot(*ty, pool),
        ResultType::Multi { types, .. } => {
            let list = pool.type_expr_list(*types);
            list.len() == 2 && type_expr_is_err_slot(list[1], pool)
        }
    }
}

use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_generic_params(
        &mut self,
    ) -> Result<SmallVec<[GenericParam; 2]>, ParseError> {
        if !self.eat_name("LT") {
            return Ok(SmallVec::new());
        }
        let params_vec = self.parse_generic_list(1, |parser| {
            let start = parser.mark();
            let is_const = parser.eat_name("KW_CONST");
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

    pub(super) fn parse_generic_args(&mut self) -> Result<IndexRange, ParseError> {
        self.expect_name("LT")?;
        let args = self.parse_generic_list(1, |parser| {
            if matches!(parser.current().kind, TokenKind::IntDec) {
                let start = parser.mark();
                let value = SmolStr::new(parser.current_text());
                parser.advance();
                let span = parser.span_from_mark(start);
                Ok(parser.pool.alloc_type_expr(TypeExpr::Const { span, value }))
            } else {
                parser.parse_type()
            }
        })?;
        self.expect_gt()?;
        let range = self.pool.alloc_type_expr_list(&args);
        Ok(range)
    }

    pub(super) fn parse_where_clause(
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

    pub(super) fn parse_constraint_list(&mut self) -> Result<Vec<TypeExprId>, ParseError> {
        let mut constraints = vec![self.parse_type()?];
        while self.eat_name("PLUS") {
            constraints.push(self.parse_type()?);
        }
        Ok(constraints)
    }

    fn parse_ownership(&mut self) -> Option<Ownership> {
        if self.eat_name("KW_OWN") {
            Some(Ownership::Own)
        } else if self.eat_name("KW_MUT") {
            Some(Ownership::Mut)
        } else if self.eat_name("KW_SHARED") {
            Some(Ownership::Shared)
        } else {
            None
        }
    }

    pub(super) fn parse_params(
        &mut self,
        method_receiver: Option<&TypeName>,
    ) -> Result<Vec<Param>, ParseError> {
        let params = self.parse_comma_separated_list("RPAREN", 0, |parser| {
            let start = parser.mark();
            let attrs = parser.parse_attributes()?;
            let ownership = parser.parse_ownership();
            let name = if parser.eat_name("KW_SELF") {
                SmolStr::new("self")
            } else {
                parser.expect_ident_value()?
            };
            let is_receiver = name == "self";
            let ty = if is_receiver {
                if parser.eat_name("COLON") {
                    let type_start = parser.mark();
                    if parser.eat_name("KW_REF") {
                        if parser.can_start_type() {
                            let inner = parser.parse_type()?;
                            let span = parser.span_from_mark(type_start);
                            parser.pool.alloc_type_expr(TypeExpr::Ref { span, inner })
                        } else {
                            let receiver = method_receiver.ok_or_else(|| {
                                ParseError::new(
                                    ParseErrorCode::ExpectedType,
                                    "receiver parameter 'self' requires an explicit type here",
                                    parser.current(),
                                    parser.file_id,
                                    parser.source,
                                )
                            })?;
                            let empty_args = parser.pool.alloc_type_expr_list(&[]);
                            let named = parser.pool.alloc_type_expr(TypeExpr::Named {
                                span: receiver.span,
                                name: receiver.clone(),
                                args: empty_args,
                            });
                            let span = parser.span_from_mark(type_start);
                            parser
                                .pool
                                .alloc_type_expr(TypeExpr::Ref { span, inner: named })
                        }
                    } else if parser.eat_name("KW_MUT") {
                        if parser.eat_name("KW_REF") {
                            if parser.can_start_type() {
                                let inner = parser.parse_type()?;
                                let span = parser.span_from_mark(type_start);
                                parser
                                    .pool
                                    .alloc_type_expr(TypeExpr::RefMut { span, inner })
                            } else {
                                let receiver = method_receiver.ok_or_else(|| {
                                    ParseError::new(
                                        ParseErrorCode::ExpectedType,
                                        "receiver parameter 'self' requires an explicit type here",
                                        parser.current(),
                                        parser.file_id,
                                        parser.source,
                                    )
                                })?;
                                let empty_args = parser.pool.alloc_type_expr_list(&[]);
                                let named = parser.pool.alloc_type_expr(TypeExpr::Named {
                                    span: receiver.span,
                                    name: receiver.clone(),
                                    args: empty_args,
                                });
                                let span = parser.span_from_mark(type_start);
                                parser
                                    .pool
                                    .alloc_type_expr(TypeExpr::RefMut { span, inner: named })
                            }
                        } else {
                            return Err(ParseError::new(
                                ParseErrorCode::ExpectedToken,
                                "expected `ref` after `mut` in receiver type",
                                parser.current(),
                                parser.file_id,
                                parser.source,
                            ));
                        }
                    } else if parser.eat_name("KW_OWN") {
                        if parser.can_start_type() {
                            parser.parse_type()?
                        } else {
                            let receiver = method_receiver.ok_or_else(|| {
                                ParseError::new(
                                    ParseErrorCode::ExpectedType,
                                    "receiver parameter 'self' requires an explicit type here",
                                    parser.current(),
                                    parser.file_id,
                                    parser.source,
                                )
                            })?;
                            let empty_args = parser.pool.alloc_type_expr_list(&[]);
                            parser.pool.alloc_type_expr(TypeExpr::Named {
                                span: receiver.span,
                                name: receiver.clone(),
                                args: empty_args,
                            })
                        }
                    } else {
                        parser.parse_type()?
                    }
                } else {
                    let receiver = method_receiver.ok_or_else(|| {
                        ParseError::new(
                            ParseErrorCode::ExpectedType,
                            "receiver parameter 'self' requires an explicit type here",
                            parser.current(),
                            parser.file_id,
                            parser.source,
                        )
                    })?;
                    let empty_args = parser.pool.alloc_type_expr_list(&[]);
                    parser.pool.alloc_type_expr(TypeExpr::Named {
                        span: receiver.span,
                        name: receiver.clone(),
                        args: empty_args,
                    })
                }
            } else {
                parser.expect_name("COLON")?;
                parser.parse_type()?
            };
            let is_variadic = parser.eat_name("ELLIPSIS");
            Ok(Param {
                span: parser.span_from_mark(start),
                attrs: attrs.into(),
                ownership,
                name,
                ty,
                is_variadic,
                is_receiver,
            })
        })?;
        Ok(params)
    }

    pub(super) fn parse_result_type(&mut self) -> Result<ResultType, ParseError> {
        let start = self.mark();
        let result = if self.eat_name("LPAREN") {
            let types = self.parse_comma_separated_list("RPAREN", 2, super::Parser::parse_type)?;
            self.expect_name("RPAREN")?;
            let range = self.pool.alloc_type_expr_list(&types);
            ResultType::Multi {
                span: self.span_from_mark(start),
                types: range,
            }
        } else {
            let ty = self.parse_type()?;
            ResultType::Single {
                span: self.span_from_mark(start),
                ty,
            }
        };
        if result_type_must_use_result_generic(&result, &self.pool) {
            let token = *self.current();
            return Err(ParseError::new(
                ParseErrorCode::InvalidResultReturn,
                "function result must use `Result<T, E>` syntax",
                &token,
                self.file_id,
                self.source,
            ));
        }
        Ok(result)
    }

    pub(super) fn parse_type(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        let mut ty = self.parse_type_primary()?;
        if self.eat_name("QUESTION") {
            let span = self.span_from_mark(start);
            ty = self
                .pool
                .alloc_type_expr(TypeExpr::Nullable { span, inner: ty });
        }
        Ok(ty)
    }

    pub(super) fn parse_type_primary(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        // Canonical ownership spelling: `own T`, `ref T`, `mut ref T`.
        // `own` is semantically the default and therefore lowers directly to T.
        if self.eat_name("KW_OWN") {
            return self.parse_type();
        }
        if self.eat_name("KW_REF") {
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Ref { span, inner }));
        }
        if self.eat_name("KW_MUT") {
            self.expect_name("KW_REF")?;
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::RefMut { span, inner }));
        }
        // `&mut T` / `&T` (F2.0 safe references)
        if self.eat_name("AMP") {
            let is_mut = self.eat_name("KW_MUT");
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(if is_mut {
                TypeExpr::RefMut { span, inner }
            } else {
                TypeExpr::Ref { span, inner }
            }));
        }
        if self.eat_name("KW_PTR") {
            self.expect_name("LBRACKET")?;
            let inner = self.parse_type()?;
            self.expect_name("RBRACKET")?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Pointer { span, inner }));
        }
        if self.eat_name("LBRACKET") {
            if self.eat_name("RBRACKET") {
                let inner = self.parse_type_primary()?;
                let span = self.span_from_mark(start);
                return Ok(self.pool.alloc_type_expr(TypeExpr::Slice { span, inner }));
            }
            let size_span = self.current().span(self.file_id);
            let size = match &self.current().kind {
                TokenKind::IntDec | TokenKind::IdentValue | TokenKind::IdentType => {
                    let value = SmolStr::new(self.current_text());
                    self.advance();
                    value
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "expected array size",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            };
            self.expect_name("RBRACKET")?;
            let elem = self.parse_type_primary()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Array {
                span,
                size,
                size_span,
                elem,
            }));
        }
        if self.eat_name("KW_FUNC") {
            self.expect_name("LPAREN")?;
            let mut params = Vec::new();
            if !self.at_kind_name("RPAREN") {
                loop {
                    params.push(self.parse_type()?);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    if self.at_kind_name("RPAREN") {
                        break;
                    }
                }
            }
            self.expect_name("RPAREN")?;
            let result = if self.can_start_type() || self.at_kind_name("LPAREN") {
                Some(self.parse_result_type()?)
            } else {
                None
            };
            let span = self.span_from_mark(start);
            let params_range = self.pool.alloc_type_expr_list(&params);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Func {
                span,
                params: params_range,
                result,
            }));
        }
        if self.eat_name("LPAREN") {
            let mut params = Vec::new();
            let mut saw_comma = false;
            if !self.at_kind_name("RPAREN") {
                loop {
                    params.push(self.parse_type()?);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    saw_comma = true;
                    if self.at_kind_name("RPAREN") {
                        break;
                    }
                }
            }
            self.expect_name("RPAREN")?;
            if self.eat_name("ARROW") {
                let result_start = self.mark();
                let result_ty = self.parse_type()?;
                let result = ResultType::Single {
                    span: self.span_from_mark(result_start),
                    ty: result_ty,
                };
                let span = self.span_from_mark(start);
                let params = self.pool.alloc_type_expr_list(&params);
                return Ok(self.pool.alloc_type_expr(TypeExpr::Func {
                    span,
                    params,
                    result: Some(result),
                }));
            }
            if saw_comma || params.len() != 1 {
                return Err(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '->' after function type parameters",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
            }
            let ty = params[0];
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_type_expr(TypeExpr::Group { span, inner: ty }));
        }
        if let Some(name) = primitive_type_name(&self.current().kind) {
            self.advance();
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Primitive {
                span,
                name: name.into(),
            }));
        }
        if matches!(
            self.current().kind,
            TokenKind::IdentValue | TokenKind::IdentType
        ) {
            let name = self.parse_type_name()?;
            let args = if self.at_kind_name("LT") {
                self.parse_generic_args()?
            } else {
                self.pool.alloc_type_expr_list(&[])
            };
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_type_expr(TypeExpr::Named { span, name, args }));
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedType,
            "expected type",
            self.current(),
            self.file_id,
            self.source,
        ))
    }

    pub(super) fn parse_type_name(&mut self) -> Result<TypeName, ParseError> {
        let start = self.mark();
        let mut path = Vec::new();
        while matches!(self.current().kind, TokenKind::IdentValue)
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
            TokenKind::IdentValue if self.current_text() == "void" => {
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

    pub(super) fn can_start_type(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::KwPtr | TokenKind::LBracket | TokenKind::KwFunc | TokenKind::LParen
        ) || is_type_token(&self.current().kind)
    }

    pub(super) fn looks_like_generic_call_or_block_suffix(&self) -> bool {
        self.find_matching_gt(self.pos)
            .and_then(|gt| self.tokens.get(gt + 1))
            .is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::LParen | TokenKind::LBrace | TokenKind::KwAs
                )
            })
    }

    pub(super) fn looks_like_bare_generic_args(&self) -> bool {
        self.find_matching_gt(self.pos).is_some()
    }

    pub(super) fn looks_like_generic_args_boundary(&self) -> bool {
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
    pub(super) fn find_matching_gt(&self, start: usize) -> Option<usize> {
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
