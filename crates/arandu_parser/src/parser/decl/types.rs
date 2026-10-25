use super::super::{
    Attribute, ConstDecl, EnumDecl, EnumPayload, EnumVariant, FieldDecl, ParseError,
    ParseErrorCode, Parser, StructDecl, TypeAliasDecl, Visibility,
};

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_const(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<ConstDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_CONST")?;
        let name = self.expect_name_like()?;
        let ty = if self.can_start_type() {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_name("EQUAL")?;
        let value = self.parse_expr(0)?;
        self.expect_semicolon()?;
        Ok(ConstDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            ty,
            value,
        })
    }

    pub(in crate::parser) fn parse_type_alias(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<TypeAliasDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_TYPE")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("EQUAL")?;
        let ty = self.parse_type()?;
        self.expect_semicolon()?;
        Ok(TypeAliasDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            ty,
        })
    }

    pub(in crate::parser) fn parse_struct_decl(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<StructDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_STRUCT")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        let where_clause = self.parse_where_clause("LBRACE")?;
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut fields = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
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
            self.start_node(crate::syntax::SyntaxKind::STMT);
            fields.push(self.parse_field_decl(true)?);
            self.finish_node();
            if self.eat_name("COMMA") {
                continue;
            }
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(StructDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            where_clause,
            fields,
        })
    }

    pub(in crate::parser) fn parse_field_decl(
        &mut self,
        _require_semicolon: bool,
    ) -> Result<FieldDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        let attrs = self.parse_attributes()?;
        let visibility = self.parse_visibility();
        let name = self.expect_ident_value()?;
        self.expect_name("COLON")?;
        let ty = self.parse_type()?;
        let default = if self.eat_name("EQUAL") {
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        if self.at_kind_name("SEMICOLON") {
            self.advance();
        }
        let field = FieldDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            ty,
            default,
        };
        self.attach_docs(docs, field.span);
        Ok(field)
    }

    pub(in crate::parser) fn parse_enum_decl(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<EnumDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_ENUM")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        let where_clause = self.parse_where_clause("LBRACE")?;
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut variants = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
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
            self.start_node(crate::syntax::SyntaxKind::STMT);
            variants.push(self.parse_enum_variant()?);
            self.finish_node();
            if self.eat_name("COMMA") {
                continue;
            }
            self.skip_semicolons();
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(EnumDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            where_clause,
            variants,
        })
    }

    pub(in crate::parser) fn parse_enum_variant(&mut self) -> Result<EnumVariant, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        let attrs = self.parse_attributes()?;
        let name = self.expect_ident_type()?;
        let payload = if self.eat_name("LPAREN") {
            let payload_start = self.pos.saturating_sub(1);
            let types =
                self.parse_comma_separated_list("RPAREN", 0, super::super::Parser::parse_type)?;
            self.expect_name("RPAREN")?;
            let range = self.pool.alloc_type_expr_list(&types);
            Some(EnumPayload::Tuple {
                span: self.span_from_mark(payload_start),
                types: range,
            })
        } else if self.eat_name("LBRACE") {
            let payload_start = self.pos.saturating_sub(1);
            let fields = self
                .parse_comma_separated_list("RBRACE", 0, |parser| parser.parse_field_decl(false))?;
            self.expect_name("RBRACE")?;
            Some(EnumPayload::Struct {
                span: self.span_from_mark(payload_start),
                fields,
            })
        } else {
            None
        };
        let variant = EnumVariant {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            name,
            payload,
        };
        self.attach_docs(docs, variant.span);
        Ok(variant)
    }
}
