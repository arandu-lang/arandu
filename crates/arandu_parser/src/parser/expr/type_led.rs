use super::super::{FieldInit, ParseError, ParseErrorCode, Parser, TypeExpr, TypeName};
use crate::ast::ast_pool::{ExprId, ExprKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    fn parse_struct_literal_fields(&mut self) -> Result<crate::ast::IndexRange, ParseError> {
        let mut fields = Vec::new();
        if !self.at_kind_name("RBRACE") {
            loop {
                if self.eat_name("RANGE_EXCLUSIVE") {
                    // Struct update syntax: `{ ..base }` or `{ field: val, ..base }`
                    let base_expr = self.parse_expr(0)?;
                    let base_span = self.pool.expr_span(base_expr);
                    let init = FieldInit {
                        span: base_span,
                        name: SmolStr::new(".."),
                        value: base_expr,
                    };
                    let init_id = self.pool.alloc_field_init(init);
                    fields.push(init_id);
                    if self.eat_name("COMMA") {
                        // optional trailing comma after ..base
                    }
                    break;
                }
                let field_start = self.mark();
                let name = self.expect_ident_value()?;
                let value = if self.eat_name("COLON") {
                    self.parse_expr(0)?
                } else {
                    // Field init shorthand: `{ x }` desugars to `{ x: x }`
                    let field_span = self.span_from_mark(field_start);
                    self.pool.alloc_expr(
                        ExprKind::Path {
                            path: smallvec::smallvec![name.clone()],
                        },
                        field_span,
                    )
                };
                let init = FieldInit {
                    span: self.span_from_mark(field_start),
                    name,
                    value,
                };
                let init_id = self.pool.alloc_field_init(init);
                fields.push(init_id);
                if !self.eat_name("COMMA") {
                    break;
                }
                if self.at_kind_name("RBRACE") {
                    break;
                }
            }
        }
        self.expect_name("RBRACE")?;
        Ok(self.pool.alloc_field_init_list(&fields))
    }

    fn looks_like_struct_literal_brace(&self) -> bool {
        if !self.allow_block_calls || !self.at_kind_name("LBRACE") {
            return false;
        }
        let Some(next) = self.tokens.get(self.pos + 1) else {
            return false;
        };
        match next.kind {
            super::super::TokenKind::RBrace | super::super::TokenKind::RangeExclusive => true,
            super::super::TokenKind::IdentValue | super::super::TokenKind::IdentType => {
                self.tokens.get(self.pos + 2).is_some_and(|after| {
                    matches!(
                        after.kind,
                        super::super::TokenKind::Colon
                            | super::super::TokenKind::Comma
                            | super::super::TokenKind::RBrace
                    )
                })
            }
            _ => false,
        }
    }

    pub(super) fn parse_type_led_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        let ty = self.parse_type()?;
        if self.eat_name("LBRACE") {
            let range = self.parse_struct_literal_fields()?;
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_expr(ExprKind::StructLiteral { ty, fields: range }, span));
        }
        let named_info = match self.pool.type_expr(ty) {
            TypeExpr::Named { name, args, .. } => Some((name.clone(), *args)),
            TypeExpr::Primitive { span, name } => Some((
                TypeName {
                    span: *span,
                    path: smallvec::smallvec![name.clone()],
                },
                crate::ast::IndexRange::empty(),
            )),
            _ => None,
        };
        if let Some((mut type_name, args)) = named_info.clone()
            && self.eat_name("DOT")
        {
            let member = self.expect_name_like()?;
            let member_span = self.span_from_mark(start);
            if self.looks_like_struct_literal_brace() && self.eat_name("LBRACE") {
                type_name.path.push(member);
                type_name.span = member_span;
                let variant_ty = self.pool.alloc_type_expr(TypeExpr::Named {
                    span: member_span,
                    name: type_name,
                    args,
                });
                let range = self.parse_struct_literal_fields()?;
                let span = self.span_from_mark(start);
                return Ok(self.pool.alloc_expr(
                    ExprKind::StructLiteral {
                        ty: variant_ty,
                        fields: range,
                    },
                    span,
                ));
            }
            let tp = self
                .pool
                .alloc_expr(ExprKind::TypePath { type_name, member }, member_span);
            if args.is_empty() {
                return Ok(tp);
            }
            return Ok(self
                .pool
                .alloc_expr(ExprKind::Generic { callee: tp, args }, member_span));
        }
        // A qualified uppercase name may denote an exported constant. Leave
        // its classification to resolution, just as for an unqualified path.
        if let Some((name, args)) = named_info
            && args.is_empty()
            && name.path.len() > 1
        {
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_expr(ExprKind::Path { path: name.path }, span));
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedExpression,
            "expected type-qualified expression or struct literal",
            self.current(),
            self.file_id,
            self.source,
        ))
    }
}
