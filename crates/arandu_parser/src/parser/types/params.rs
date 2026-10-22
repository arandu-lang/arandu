use super::super::{Ownership, Param, ParseError, ParseErrorCode, Parser, TypeExpr, TypeName};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
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

    pub(in crate::parser) fn parse_params(
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
}
