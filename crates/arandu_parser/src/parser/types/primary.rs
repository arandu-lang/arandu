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
        let result = if self.eat_name("LPAREN") {
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
        let mut ty = self.parse_type_primary_inner()?;
        if self.eat_name("QUESTION") {
            let span = self.span_from_mark(start);
            ty = self
                .pool
                .alloc_type_expr(TypeExpr::Nullable { span, inner: ty });
        }
        Ok(ty)
    }

    // Array/slice elements skip nullable postfix parsing, but share the recursion budget.
    pub(in crate::parser) fn parse_type_primary(&mut self) -> Result<TypeExprId, ParseError> {
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
        let result = self.parse_type_primary_inner();
        self.recursion_depth -= 1;
        result
    }

    fn parse_type_primary_inner(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        // Canonical ownership spelling: `own T`, `ref T`, `mut ref T`.
        // `own` is the default and lowers directly to its operand.
        if self.eat_name("KW_OWN") {
            return self.parse_type();
        }
        if self.eat_name("KW_REF") {
            return self.parse_reference_type(start, false, false);
        }
        if self.eat_name("KW_MUT") {
            if self.eat_name("KW_REF") {
                return self.parse_reference_type(start, true, false);
            }
            if self.eat_name("LBRACKET") {
                self.expect_name("RBRACKET")?;
                return self.parse_mut_slice_type(start);
            }
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                "expected 'ref' or '[]' after 'mut' in type",
                self.current(),
                self.file_id,
                self.source,
            ));
        }
        if self.eat_name("AMP") {
            let is_mut = self.eat_name("KW_MUT");
            return self.parse_reference_type(start, is_mut, false);
        }
        if self.eat_name("KW_PTR") {
            self.expect_name("LBRACKET")?;
            return self.parse_reference_type(start, false, true);
        }
        if self.eat_name("LBRACKET") {
            return self.parse_array_type(start);
        }
        if self.eat_name("KW_FUNC") {
            return self.parse_func_type(start);
        }
        if self.eat_name("LPAREN") {
            return self.parse_grouped_type(start);
        }
        self.parse_type_primary_dispatch(start)
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
        let elem = self.parse_type_primary()?;
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
        let inner = self.parse_type_primary()?;
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_type_expr(TypeExpr::Slice { span, inner }))
    }

    fn parse_mut_slice_type(&mut self, start: usize) -> Result<TypeExprId, ParseError> {
        let inner = self.parse_type_primary()?;
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
        let result = if self.can_start_type() || self.at_kind_name("LPAREN") {
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
