use super::{
    BindingItem, Block, Condition, DeferBody, Expr, ForBinding, ForClause, ParseError,
    ParseErrorCode, Parser, Place, PlaceSuffix, SetOp, SimpleStmt, Stmt, TokenKind,
};
use crate::ast::ExprKind;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_block(&mut self) -> Result<Block, ParseError> {
        use crate::syntax::SyntaxKind;
        self.start_node(SyntaxKind::BLOCK);
        let start = self.mark();
        self.expect_name("LBRACE")?;
        let mut statements = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            self.discard_doc_comments();
            if self.at_kind_name("RBRACE") {
                break;
            }
            if self.at_kind_name("EOF") {
                self.diagnostics.push(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '}'",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
                break;
            }
            statements.push(self.parse_stmt()?);
        }
        self.expect_name("RBRACE")?;
        self.finish_node();
        let statements = self.pool.alloc_stmt_list(&statements);
        Ok(Block {
            span: self.span_from_mark(start),
            statements,
        })
    }

    pub(super) fn parse_stmt(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        use crate::syntax::SyntaxKind;
        self.start_node(SyntaxKind::STMT);
        let start = self.mark();
        let result = match self.try_parse_stmt() {
            Ok(stmt) => Ok(stmt),
            Err(err) => {
                self.report_error(err);
                self.synchronize_stmt();
                Ok(self
                    .pool
                    .alloc_stmt(Stmt::Error(self.span_from_mark(start))))
            }
        };
        self.finish_node();
        result
    }

    pub(super) fn try_parse_stmt(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        if self.at_kind_name("KW_RETURN") {
            return self.parse_return();
        }
        if self.at_kind_name("KW_BREAK") {
            let start = self.mark();
            self.advance();
            self.expect_semicolon()?;
            return Ok(self.pool.alloc_stmt(Stmt::Break {
                span: self.span_from_mark(start),
            }));
        }
        if self.at_kind_name("KW_CONTINUE") {
            let start = self.mark();
            self.advance();
            self.expect_semicolon()?;
            return Ok(self.pool.alloc_stmt(Stmt::Continue {
                span: self.span_from_mark(start),
            }));
        }
        if self.at_kind_name("KW_IF") {
            return self.parse_if();
        }
        if self.at_kind_name("KW_FOR") {
            return self.parse_for();
        }
        if self.at_kind_name("KW_WHILE") {
            return self.parse_while();
        }
        if self.at_kind_name("KW_DEFER") {
            return self.parse_defer(false);
        }
        if self.at_kind_name("KW_ERRDEFER") {
            return self.parse_defer(true);
        }
        if self.at_kind_name("KW_UNSAFE") {
            let start = self.mark();
            self.advance();
            let block = self.parse_block()?;
            return Ok(self.pool.alloc_stmt(Stmt::Unsafe {
                span: self.span_from_mark(start),
                block,
            }));
        }
        if self.at_kind_name("KW_LET") {
            return self.parse_var_decl();
        }
        if let Some(res) = self.try_parse_assignment() {
            return res;
        }
        let start = self.mark();
        let expr = self.parse_expr(0)?;
        self.expect_semicolon()?;
        let is_match = matches!(self.pool.expr(expr), ExprKind::Match { .. });
        Ok(self.pool.alloc_stmt(if is_match {
            Stmt::Match {
                span: self.span_from_mark(start),
                expr,
            }
        } else {
            Stmt::Expr {
                span: self.span_from_mark(start),
                expr,
                has_semi: true,
            }
        }))
    }

    pub(super) fn try_parse_assignment(
        &mut self,
    ) -> Option<Result<crate::ast_pool::StmtId, ParseError>> {
        let start = self.mark();
        let checkpoint = self.checkpoint();
        // Optional `set` keyword (EBNF mutation form). Both
        // `set x = 1` and `x = 1` lower to the same `Stmt::Set`.
        let explicit_set = self.eat_name("KW_SET");
        let mut places = Vec::new();
        match self.parse_place() {
            Ok(place) => {
                places.push(place);
                while self.eat_name("COMMA") {
                    match self.parse_place() {
                        Ok(p) => places.push(p),
                        Err(e) => {
                            if explicit_set {
                                return Some(Err(e));
                            }
                            checkpoint.rollback(self);
                            return None;
                        }
                    }
                }
                match self.parse_set_op() {
                    Ok(op) => {
                        let value = match self.parse_expr(0) {
                            Ok(val) => val,
                            Err(e) => {
                                return Some(Err(e));
                            }
                        };
                        if let Err(e) = self.expect_semicolon() {
                            return Some(Err(e));
                        }
                        Some(Ok(self.pool.alloc_stmt(Stmt::Set {
                            span: self.span_from_mark(start),
                            places,
                            op,
                            value,
                        })))
                    }
                    Err(e) => {
                        if explicit_set {
                            return Some(Err(e));
                        }
                        checkpoint.rollback(self);
                        None
                    }
                }
            }
            Err(e) => {
                if explicit_set {
                    return Some(Err(e));
                }
                checkpoint.rollback(self);
                None
            }
        }
    }

    pub(super) fn parse_var_decl(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_LET")?;
        let (bindings, value) = self.parse_var_decl_parts()?;
        self.expect_semicolon()?;
        Ok(self.pool.alloc_stmt(Stmt::VarDecl {
            span: self.span_from_mark(start),
            bindings,
            value,
        }))
    }

    pub(super) fn parse_var_decl_parts(&mut self) -> Result<(Vec<BindingItem>, Expr), ParseError> {
        let mut bindings = vec![self.parse_binding_item()?];
        while self.eat_name("COMMA") {
            bindings.push(self.parse_binding_item()?);
        }
        self.expect_name("EQUAL")?;
        let value = self.parse_expr(0)?;
        Ok((bindings, value))
    }

    pub(super) fn parse_binding_item(&mut self) -> Result<BindingItem, ParseError> {
        let start = self.mark();
        let mutable = self.eat_name("KW_MUT");
        let name = self.expect_ident_value()?;
        let ty = if self.eat_name("COLON") {
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok(BindingItem {
            span: self.span_from_mark(start),
            mutable,
            name,
            ty,
        })
    }

    pub(super) fn parse_place(&mut self) -> Result<Place, ParseError> {
        let start = self.mark();
        let root = match &self.current().kind {
            TokenKind::KwSelf => {
                self.advance();
                SmolStr::new("self")
            }
            TokenKind::IdentValue => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            _ => {
                return Err(ParseError::new(
                    ParseErrorCode::ExpectedPlace,
                    "expected assignment target",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
            }
        };
        let mut suffixes = Vec::new();
        loop {
            if self.eat_name("DOT") {
                let suffix_start = self.pos.saturating_sub(1);
                let name = self.expect_ident_value()?;
                suffixes.push(PlaceSuffix::Field {
                    span: self.span_from_mark(suffix_start),
                    name,
                });
            } else if self.eat_name("LBRACKET") {
                let suffix_start = self.pos.saturating_sub(1);
                let index = self.parse_expr(0)?;
                self.expect_name("RBRACKET")?;
                suffixes.push(PlaceSuffix::Index {
                    span: self.span_from_mark(suffix_start),
                    expr: index,
                });
            } else {
                break;
            }
        }
        Ok(Place {
            span: self.span_from_mark(start),
            root,
            suffixes,
        })
    }

    pub(super) fn parse_return(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_RETURN")?;
        if self.at_kind_name("SEMICOLON") {
            self.expect_semicolon()?;
            return Ok(self.pool.alloc_stmt(Stmt::Return {
                span: self.span_from_mark(start),
                values: Vec::new(),
            }));
        }
        let mut values = vec![self.parse_expr(0)?];
        while self.eat_name("COMMA") {
            values.push(self.parse_expr(0)?);
        }
        self.expect_semicolon()?;
        Ok(self.pool.alloc_stmt(Stmt::Return {
            span: self.span_from_mark(start),
            values,
        }))
    }

    pub(super) fn parse_if(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_IF")?;
        let condition = self.parse_condition()?;
        let then_block = self.parse_block()?;
        let else_block = if self.eat_name("KW_ELSE") {
            if self.at_kind_name("KW_IF") {
                let nested = self.parse_if()?;
                Some(Block {
                    span: self.span_from_mark(start),
                    statements: self.pool.alloc_stmt_list(&[nested]),
                })
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        Ok(self.pool.alloc_stmt(Stmt::If {
            span: self.span_from_mark(start),
            condition,
            then_block,
            else_block,
        }))
    }

    pub(super) fn parse_condition(&mut self) -> Result<Condition, ParseError> {
        let start = self.mark();
        if self.eat_name("KW_LET") {
            let pattern = self.parse_pattern()?;
            self.expect_name("EQUAL")?;
            let expr = self.parse_expr_without_block_calls(0)?;
            Ok(Condition::Is {
                span: self.span_from_mark(start),
                expr,
                pattern,
            })
        } else if self.condition_has_top_level_is() {
            let mut conditions = Vec::new();
            loop {
                let clause_start = self.mark();
                // Logical AND has binding power 40. Parsing above it keeps the
                // conjunction as condition structure rather than burying it in
                // a boolean expression, which preserves pattern scopes.
                let expr = self.parse_expr_without_block_calls(41)?;
                let condition = if self.eat_name("KW_IS") {
                    let pattern = self.parse_pattern()?;
                    Condition::Is {
                        span: self.span_from_mark(clause_start),
                        expr,
                        pattern,
                    }
                } else {
                    Condition::Expr {
                        span: self.span_from_mark(clause_start),
                        expr,
                    }
                };
                conditions.push(condition);
                if !self.eat_name("LOGICAL_AND") {
                    break;
                }
            }
            if conditions.len() == 1 {
                Ok(conditions.remove(0))
            } else {
                Ok(Condition::And {
                    span: self.span_from_mark(start),
                    conditions: conditions.into_boxed_slice(),
                })
            }
        } else {
            let expr = self.parse_expr_without_block_calls(0)?;
            if self.eat_name("KW_IS") {
                let pattern = self.parse_pattern()?;
                Ok(Condition::Is {
                    span: self.span_from_mark(start),
                    expr,
                    pattern,
                })
            } else {
                Ok(Condition::Expr {
                    span: self.span_from_mark(start),
                    expr,
                })
            }
        }
    }

    fn condition_has_top_level_is(&self) -> bool {
        let mut paren_depth = 0u32;
        let mut bracket_depth = 0u32;
        for token in &self.tokens[self.pos..] {
            match token.kind {
                TokenKind::LParen => paren_depth = paren_depth.saturating_add(1),
                TokenKind::RParen => paren_depth = paren_depth.saturating_sub(1),
                TokenKind::LBracket => bracket_depth = bracket_depth.saturating_add(1),
                TokenKind::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                TokenKind::LBrace if paren_depth == 0 && bracket_depth == 0 => break,
                TokenKind::KwIs if paren_depth == 0 && bracket_depth == 0 => return true,
                TokenKind::Eof => break,
                _ => {}
            }
        }
        false
    }

    pub(super) fn parse_while(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_WHILE")?;
        let condition = self.parse_condition()?;
        let body = self.parse_block()?;
        Ok(self.pool.alloc_stmt(Stmt::While {
            span: self.span_from_mark(start),
            condition,
            body,
        }))
    }

    pub(super) fn parse_for(&mut self) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_FOR")?;
        let clause = if self.looks_like_for_in_clause() {
            let clause_start = self.mark();
            let mut bindings = vec![self.parse_for_binding()?];
            while self.eat_name("COMMA") {
                bindings.push(self.parse_for_binding()?);
            }
            self.expect_name("KW_IN")?;
            let iterable = self.parse_expr_without_block_calls(0)?;
            ForClause::In {
                span: self.span_from_mark(clause_start),
                bindings,
                iterable,
            }
        } else {
            let clause_start = self.mark();
            let init = if self.at_kind_name("SEMICOLON") {
                None
            } else {
                Some(self.parse_simple_stmt()?)
            };
            self.expect_semicolon()?;
            let condition = if self.at_kind_name("SEMICOLON") {
                None
            } else {
                Some(self.parse_expr(0)?)
            };
            self.expect_semicolon()?;
            let step = if self.at_kind_name("LBRACE") {
                None
            } else {
                Some(self.parse_simple_stmt_without_block_calls()?)
            };
            ForClause::CStyle {
                span: self.span_from_mark(clause_start),
                init,
                condition,
                step,
            }
        };
        let body = self.parse_block()?;
        Ok(self.pool.alloc_stmt(Stmt::For {
            span: self.span_from_mark(start),
            clause,
            body,
        }))
    }

    pub(super) fn parse_for_binding(&mut self) -> Result<ForBinding, ParseError> {
        let start = self.mark();
        let mutable = self.eat_name("KW_MUT");
        let name = self.expect_ident_value()?;
        Ok(ForBinding {
            span: self.span_from_mark(start),
            mutable,
            name,
        })
    }

    pub(super) fn try_parse_simple_assignment(&mut self) -> Option<Result<SimpleStmt, ParseError>> {
        let start = self.mark();
        let checkpoint = self.checkpoint();
        let _explicit_set = self.eat_name("KW_SET");
        let mut places = Vec::new();
        match self.parse_place() {
            Ok(place) => {
                places.push(place);
                while self.eat_name("COMMA") {
                    match self.parse_place() {
                        Ok(p) => places.push(p),
                        Err(_) => {
                            checkpoint.rollback(self);
                            return None;
                        }
                    }
                }
                match self.parse_set_op() {
                    Ok(op) => {
                        let value = match self.parse_expr(0) {
                            Ok(val) => val,
                            Err(e) => {
                                return Some(Err(e));
                            }
                        };
                        Some(Ok(SimpleStmt::Set {
                            span: self.span_from_mark(start),
                            places,
                            op,
                            value,
                        }))
                    }
                    Err(_) => {
                        checkpoint.rollback(self);
                        None
                    }
                }
            }
            Err(_) => {
                checkpoint.rollback(self);
                None
            }
        }
    }

    pub(super) fn parse_simple_stmt(&mut self) -> Result<SimpleStmt, ParseError> {
        let start = self.mark();
        if self.at_kind_name("KW_LET") {
            self.advance();
            let (bindings, value) = self.parse_var_decl_parts()?;
            return Ok(SimpleStmt::VarDecl {
                span: self.span_from_mark(start),
                bindings,
                value,
            });
        }
        if let Some(res) = self.try_parse_simple_assignment() {
            return res;
        }
        let expr = self.parse_expr(0)?;
        Ok(SimpleStmt::Expr {
            span: self.span_from_mark(start),
            expr,
        })
    }

    pub(super) fn parse_simple_stmt_without_block_calls(
        &mut self,
    ) -> Result<SimpleStmt, ParseError> {
        let previous = self.allow_block_calls;
        self.allow_block_calls = false;
        let result = self.parse_simple_stmt();
        self.allow_block_calls = previous;
        result
    }

    pub(super) fn parse_defer(
        &mut self,
        is_errdefer: bool,
    ) -> Result<crate::ast_pool::StmtId, ParseError> {
        let start = self.mark();
        if is_errdefer {
            self.expect_name("KW_ERRDEFER")?;
        } else {
            self.expect_name("KW_DEFER")?;
        }
        let body = if self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            DeferBody::Block {
                span: block.span,
                block,
            }
        } else {
            let body_start = self.mark();
            let expr = self.parse_expr(0)?;
            self.expect_semicolon()?;
            DeferBody::Expr {
                span: self.span_from_mark(body_start),
                expr,
            }
        };
        Ok(self.pool.alloc_stmt(if is_errdefer {
            Stmt::ErrDefer {
                span: self.span_from_mark(start),
                body,
            }
        } else {
            Stmt::Defer {
                span: self.span_from_mark(start),
                body,
            }
        }))
    }

    pub(super) fn parse_set_op(&mut self) -> Result<SetOp, ParseError> {
        let op = match self.current().kind {
            TokenKind::Equal => SetOp::Assign,
            TokenKind::PlusEqual => SetOp::AddAssign,
            TokenKind::MinusEqual => SetOp::SubAssign,
            TokenKind::StarEqual => SetOp::MulAssign,
            TokenKind::SlashEqual => SetOp::DivAssign,
            TokenKind::PercentEqual => SetOp::ModAssign,
            TokenKind::AmpEqual => SetOp::BitAndAssign,
            TokenKind::PipeEqual => SetOp::BitOrAssign,
            TokenKind::CaretEqual => SetOp::BitXorAssign,
            TokenKind::ShiftLeftEqual => SetOp::ShiftLeftAssign,
            TokenKind::ShiftRightEqual => SetOp::ShiftRightAssign,
            _ => {
                return Err(ParseError::expected(
                    ParseErrorCode::ExpectedToken,
                    "expected assignment operator",
                    self.current(),
                    self.file_id,
                    self.source,
                    &[
                        "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=",
                    ],
                ));
            }
        };
        self.advance();
        Ok(op)
    }

    pub(super) fn looks_like_for_in_clause(&self) -> bool {
        let mut index = self.pos;
        loop {
            if self
                .tokens
                .get(index)
                .is_some_and(|token| matches!(token.kind, TokenKind::KwMut))
            {
                index += 1;
            }
            if !self
                .tokens
                .get(index)
                .is_some_and(|token| matches!(token.kind, TokenKind::IdentValue))
            {
                return false;
            }
            index += 1;
            match self.tokens.get(index).map(|token| &token.kind) {
                Some(TokenKind::KwIn) => return true,
                Some(TokenKind::Comma) => index += 1,
                _ => return false,
            }
        }
    }
}
