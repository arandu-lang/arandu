use super::super::{
    Attribute, ExternDecl, FuncDecl, FuncName, FuncSignature, ParseError, ParseErrorCode, Parser,
    TokenKind, TypeName, Visibility,
};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_func(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<FuncDecl, ParseError> {
        let start = self.mark();
        let is_async = self.eat_name("KW_ASYNC");
        self.expect_name("KW_FUNC")?;
        let name = self.parse_func_name()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("LPAREN")?;
        let method_receiver = match &name {
            FuncName::Method { receiver, .. } => Some(receiver),
            FuncName::Free { .. } => None,
        };
        let params = self.parse_params(method_receiver)?;
        self.expect_name("RPAREN")?;
        let result = if self.eat_name("COLON") {
            Some(self.parse_result_type()?)
        } else {
            None
        };
        let where_clause = self.parse_where_clause("LBRACE")?;
        let body = self.parse_block()?;
        Ok(FuncDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            is_async,
            name,
            generic_params,
            params,
            result,
            where_clause,
            body,
        })
    }

    pub(in crate::parser) fn parse_extern_decl(
        &mut self,
        attrs: Vec<Attribute>,
    ) -> Result<ExternDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_EXTERN")?;
        let abi = self.parse_abi_literal()?;
        let members = self.parse_braced_member_list(|parser| {
            let attrs = parser.parse_attributes()?;
            parser.parse_func_signature(attrs)
        })?;
        Ok(ExternDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            abi,
            members,
        })
    }

    pub(in crate::parser) fn parse_abi_literal(&mut self) -> Result<SmolStr, ParseError> {
        self.expect_name("STRING_START")?;
        let abi = if self.at_kind_name("STRING_END") {
            SmolStr::default()
        } else {
            match &self.current().kind {
                TokenKind::StringText => {
                    let text = SmolStr::new(self.current_text());
                    self.advance();
                    text
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "expected static ABI string",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
        };
        self.expect_name("STRING_END")?;
        Ok(abi)
    }

    pub(in crate::parser) fn parse_func_signature(
        &mut self,
        attrs: Vec<Attribute>,
    ) -> Result<FuncSignature, ParseError> {
        self.parse_func_signature_with_receiver(attrs, None)
    }

    pub(in crate::parser) fn parse_func_signature_with_receiver(
        &mut self,
        attrs: Vec<Attribute>,
        receiver: Option<&TypeName>,
    ) -> Result<FuncSignature, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        self.expect_name("KW_FUNC")?;
        let name = self.expect_ident_value()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("LPAREN")?;
        let params = self.parse_params(receiver)?;
        self.expect_name("RPAREN")?;
        let result = if self.eat_name("COLON") {
            Some(self.parse_result_type()?)
        } else {
            None
        };
        let where_clause = self.parse_where_clause("SEMICOLON")?;
        let signature = FuncSignature {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            name,
            generic_params,
            params,
            result,
            where_clause,
        };
        self.attach_docs(docs, signature.span);
        Ok(signature)
    }

    pub(in crate::parser) fn parse_func_name(&mut self) -> Result<FuncName, ParseError> {
        let start = self.pos;
        if matches!(
            self.current().kind,
            TokenKind::IdentType | TokenKind::IdentValue
        ) {
            let checkpoint = self.checkpoint();
            if let Ok(receiver) = self.parse_type_name()
                && self.eat_name("DOT")
            {
                let name = self.expect_member_name()?;
                return Ok(FuncName::Method {
                    span: self.span_from_mark(start),
                    receiver,
                    name,
                });
            }
            checkpoint.rollback(self);
        }

        let name = self.expect_ident_value()?;
        Ok(FuncName::Free {
            span: self.span_from_mark(start),
            name,
        })
    }
}
