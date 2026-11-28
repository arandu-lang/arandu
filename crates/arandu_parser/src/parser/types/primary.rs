use super::super::{
    ParseError, ParseErrorCode, Parser, ResultType, TokenKind, TypeExpr, primitive_type_name,
};
use crate::TypeExprId;
use smol_str::SmolStr;

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

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_result_type(&mut self) -> Result<ResultType, ParseError> {
        let start = self.mark();
        // `(A, B) -> R` is a function type, not two results.
        let result = if !self.paren_group_is_function_type() && self.eat_name("LPAREN") {
            let types =
                self.parse_comma_separated_list("RPAREN", 2, super::super::Parser::parse_type)?;
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

    /// Whether the `(` at the cursor closes into `) ->`.
    fn paren_group_is_function_type(&self) -> bool {
        if !self.at_kind_name("LPAREN") {
            return false;
        }
        let mut depth = 0usize;
        for (offset, token) in self.tokens[self.pos..].iter().enumerate() {
            match token.kind.name() {
                "LPAREN" => depth += 1,
                "RPAREN" => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .tokens
                            .get(self.pos + offset + 1)
                            .is_some_and(|next| next.kind.name() == "ARROW");
                    }
                }
                "EOF" => return false,
                _ => {}
            }
        }
        false
    }

    pub(in crate::parser) fn parse_type(&mut self) -> Result<TypeExprId, ParseError> {
        if self.recursion_depth >= 105 {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedType,
                "type recursion limit exceeded",
                self.current(),
                self.file_id,
                self.source,
            ));
        }
        self.recursion_depth += 1;
        let res = self.parse_type_inner();
        self.recursion_depth -= 1;
        res
    }

    fn parse_type_inner(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        if self.eat_name("KW_OWN") {
            return self.parse_own_type(start);
        }
        if self.eat_name("KW_REF") {
            return self.parse_reference_type(start, false, false);
        }
        if self.eat_name("KW_MUT") {
            return self.parse_mut_type(start);
        }
        if self.eat_name("AMP") {
            let is_mut = self.eat_name("KW_MUT");
            return self.parse_reference_type(start, is_mut, false);
        }
        if self.eat_name("LBRACKET") {
            return self.parse_array_type(start);
        }
        if self.eat_name("KW_FUNC") {
            return self.parse_func_type(start);
        }

        let mut ty = self.parse_postfix_question_base(start)?;
        if !matches!(self.pool.type_expr(ty), TypeExpr::Func { .. }) && self.eat_name("QUESTION") {
            let span = self.span_from_mark(start);
            ty = self
                .pool
                .alloc_type_expr(TypeExpr::Nullable { span, inner: ty });
        }
        Ok(ty)
    }

    #[inline(never)]
    fn parse_own_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        let once = if self.at_soft_keyword("once") {
            self.advance();
            true
        } else {
            false
        };
        let inner = self.parse_type()?;
        if !matches!(
            self.pool.type_expr(inner),
            TypeExpr::Func {
                is_bare_func: false,
                ..
            }
        ) {
            if once {
                let token = *self.current();
                return Err(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "`once` modifier is only valid on owning closure types `own once (A) -> R`",
                    &token,
                    self.file_id,
                    self.source,
                ));
            }
            return Ok(inner);
        }
        let mut bounds = Vec::new();
        while self.eat_name("PLUS") {
            bounds.push(self.parse_bound_type()?);
        }
        let bounds = self.pool.alloc_type_expr_list(&bounds);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_type_expr(TypeExpr::OwnFunc {
            span,
            func: inner,
            bounds,
            once,
        }))
    }

    #[inline(never)]
    fn parse_mut_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        if self.eat_name("KW_REF") {
            return self.parse_reference_type(start, true, false);
        }
        if self.eat_name("LBRACKET") {
            self.expect_name("RBRACKET")?;
            return self.parse_mut_slice_type(start);
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedToken,
            "expected 'ref' or '[]' after 'mut' in type",
            self.current(),
            self.file_id,
            self.source,
        ))
    }

    #[inline(never)]
    fn parse_postfix_question_base(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        if self.eat_name("KW_PTR") {
            self.expect_name("LBRACKET")?;
            return self.parse_reference_type(start, false, true);
        }
        if self.eat_name("BANG") {
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Never { span }));
        }
        if self.eat_name("LPAREN") {
            return self.parse_grouped_type(start);
        }
        self.parse_type_primary_dispatch(start)
    }

    #[inline(never)]
    fn parse_bound_type(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        if matches!(
            self.current().kind,
            TokenKind::IdentValue | TokenKind::IdentType
        ) {
            return self.parse_type_primary_dispatch(start);
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedType,
            "expected capability bound name after '+'",
            self.current(),
            self.file_id,
            self.source,
        ))
    }

    fn parse_reference_type(
        &mut self,
        start: usize,
        is_mut: bool,
        pointer: bool,
    ) -> Result<TypeExprId, ParseError> {
        let inner = self.parse_type()?;
        if pointer {
            self.expect_name("RBRACKET")?;
        }
        let span = self.span_from_mark(start);
        let ty = if pointer {
            TypeExpr::Pointer { span, inner }
        } else if is_mut {
            TypeExpr::RefMut { span, inner }
        } else {
            TypeExpr::Ref { span, inner }
        };
        Ok(self.pool.alloc_type_expr(ty))
    }

    fn parse_array_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        if self.eat_name("RBRACKET") {
            return self.parse_slice_type(start);
        }
        let (size, size_span, size_expression) = self.parse_array_size()?;
        self.expect_name("RBRACKET")?;
        let elem = self.parse_type()?;
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_type_expr(TypeExpr::Array {
            span,
            size,
            size_span,
            size_expression,
            elem,
        }))
    }

    fn parse_slice_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        let inner = self.parse_type()?;
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_type_expr(TypeExpr::Slice { span, inner }))
    }

    fn parse_mut_slice_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        let inner = self.parse_type()?;
        let span = self.span_from_mark(start);
        Ok(self
            .pool
            .alloc_type_expr(TypeExpr::MutSlice { span, inner }))
    }

    fn parse_array_size(
        &mut self,
    ) -> Result<(SmolStr, arandu_base::Span, Option<TypeExprId>), ParseError> {
        let size_span = self.current().span(self.file_id);
        if self.at_kind_name("KW_COMPTIME") {
            return Ok((
                SmolStr::new(""),
                size_span,
                Some(self.parse_generic_argument()?),
            ));
        }
        match &self.current().kind {
            TokenKind::IntDec | TokenKind::IdentValue | TokenKind::IdentType => {
                let value = SmolStr::new(self.current_text());
                self.advance();
                Ok((value, size_span, None))
            }
            _ => Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                "expected array size",
                self.current(),
                self.file_id,
                self.source,
            )),
        }
    }

    fn parse_func_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
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
        let result =
            if self.eat_name("ARROW") || self.can_start_type() || self.at_kind_name("LPAREN") {
                Some(self.parse_result_type()?)
            } else {
                None
            };
        let span = self.span_from_mark(start);
        let params_range = self.pool.alloc_type_expr_list(&params);
        Ok(self.pool.alloc_type_expr(TypeExpr::Func {
            span,
            params: params_range,
            result,
            is_bare_func: true,
        }))
    }

    fn parse_grouped_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
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
                is_bare_func: false,
            }));
        }
        if params.len() >= 2 {
            let span = self.span_from_mark(start);
            let items = self.pool.alloc_type_expr_list(&params);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Tuple { span, items }));
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
        Ok(self
            .pool
            .alloc_type_expr(TypeExpr::Group { span, inner: ty }))
    }

    fn parse_type_primary_dispatch(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
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
}
