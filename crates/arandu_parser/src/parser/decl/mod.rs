mod functions;
mod interfaces;
mod module_import;
mod types;

use super::{
    Attribute, ParseError, ParseErrorCode, Parser, TokenKind, TopLevelDecl, Visibility,
    user_facing_token_name,
};
use crate::{ExprId, ExprKind};

impl<'a> Parser<'a> {
    pub(crate) fn parse_top_level_decls(&mut self) -> Result<Vec<TopLevelDecl>, ParseError> {
        let start = self.mark();
        match self.try_parse_top_level_decls() {
            Ok(decls) => Ok(decls),
            Err(err) => {
                self.diagnostics.push(err);
                self.synchronize_top_level();
                Ok(vec![TopLevelDecl::Error(self.span_from_mark(start))])
            }
        }
    }

    pub(in crate::parser) fn try_parse_top_level_decls(
        &mut self,
    ) -> Result<Vec<TopLevelDecl>, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let item_kind = self.peek_top_level_item_kind();
        self.start_node(item_kind);
        let result = (|| {
            let attrs = self.parse_attributes()?;
            let visibility = self.parse_visibility();
            match self.current().kind {
                TokenKind::KwConst => Ok(vec![TopLevelDecl::Const(
                    self.parse_const(attrs, visibility)?,
                )]),
                TokenKind::KwType => Ok(vec![TopLevelDecl::TypeAlias(
                    self.parse_type_alias(attrs, visibility)?,
                )]),
                TokenKind::KwAsync | TokenKind::KwFunc => Ok(vec![TopLevelDecl::Func(
                    self.parse_func(attrs, visibility)?,
                )]),
                TokenKind::KwStruct => Ok(vec![TopLevelDecl::Struct(
                    self.parse_struct_decl(attrs, visibility)?,
                )]),
                TokenKind::KwEnum => Ok(vec![TopLevelDecl::Enum(
                    self.parse_enum_decl(attrs, visibility)?,
                )]),
                TokenKind::KwInterface => Ok(vec![TopLevelDecl::Interface(
                    self.parse_interface_decl(attrs, visibility)?,
                )]),
                TokenKind::KwExtern => {
                    Ok(vec![TopLevelDecl::Extern(self.parse_extern_decl(attrs)?)])
                }
                TokenKind::KwImpl => self.parse_impl_decl(attrs, visibility),
                _ => Err(ParseError::new(
                    ParseErrorCode::ExpectedTopLevelDecl,
                    "expected top-level declaration",
                    self.current(),
                    self.file_id,
                    self.source,
                )),
            }
        })();
        self.finish_node();
        let decls = result?;
        if let Some(first) = decls.first() {
            self.attach_docs(docs, first.span());
        }
        Ok(decls)
    }

    /// Look ahead past `@attr` / `public` / `async` to classify the green item kind.
    fn peek_top_level_item_kind(&self) -> crate::syntax::SyntaxKind {
        use crate::syntax::SyntaxKind;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match self.tokens[i].kind {
                TokenKind::At => {
                    i += 1;
                    // skip attr name and optional ( … )
                    if i < self.tokens.len()
                        && matches!(
                            self.tokens[i].kind,
                            TokenKind::IdentValue | TokenKind::IdentType
                        )
                    {
                        i += 1;
                    }
                    if i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::LParen) {
                        let mut depth = 0i32;
                        while i < self.tokens.len() {
                            match self.tokens[i].kind {
                                TokenKind::LParen => depth += 1,
                                TokenKind::RParen => {
                                    depth -= 1;
                                    i += 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                }
                                TokenKind::Eof => break,
                                _ => {}
                            }
                            i += 1;
                        }
                    }
                }
                TokenKind::KwPublic | TokenKind::KwAsync | TokenKind::Semicolon => i += 1,
                TokenKind::KwConst => return SyntaxKind::CONST_ITEM,
                TokenKind::KwType => return SyntaxKind::TYPE_ALIAS_ITEM,
                TokenKind::KwFunc => return SyntaxKind::FUNC_ITEM,
                TokenKind::KwStruct => return SyntaxKind::STRUCT_ITEM,
                TokenKind::KwEnum => return SyntaxKind::ENUM_ITEM,
                TokenKind::KwInterface => return SyntaxKind::INTERFACE_ITEM,
                TokenKind::KwExtern => return SyntaxKind::EXTERN_ITEM,
                TokenKind::KwImpl => return SyntaxKind::IMPL_ITEM,
                _ => return SyntaxKind::ITEM,
            }
        }
        SyntaxKind::ITEM
    }

    pub(in crate::parser) fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attrs = Vec::new();
        while self.eat_name("AT") {
            let start = self.pos.saturating_sub(1);
            let name_span = self.current().span(self.file_id);
            let name = self.expect_name_like()?;
            let args = if self.eat_name("LPAREN") {
                let args = self.parse_attribute_arguments()?;
                self.expect_name("RPAREN")?;
                args
            } else {
                Vec::new()
            };
            attrs.push(Attribute {
                span: self.span_from_mark(start),
                name_span,
                name,
                args,
            });
            self.skip_semicolons();
        }
        Ok(attrs)
    }

    pub(in crate::parser) fn parse_attribute_arguments(
        &mut self,
    ) -> Result<Vec<ExprId>, ParseError> {
        let mut args = Vec::new();
        if self.at_kind_name("RPAREN") {
            return Ok(args);
        }
        loop {
            let is_bare_ident = matches!(
                self.current().kind,
                TokenKind::IdentValue | TokenKind::IdentType
            ) && !self.tokens.get(self.pos + 1).is_some_and(|next| {
                matches!(
                    next.kind,
                    TokenKind::Dot | TokenKind::LBrace | TokenKind::LParen
                )
            });
            let arg = if is_bare_ident {
                let start = self.pos;
                let name = self.expect_name_like()?;
                let span = self.span_from_mark(start);
                self.pool.alloc_expr(
                    ExprKind::Path {
                        path: smallvec::smallvec![name],
                    },
                    span,
                )
            } else {
                self.parse_expr(0)?
            };
            args.push(arg);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name("RPAREN") {
                break;
            }
        }
        Ok(args)
    }

    pub(in crate::parser) fn parse_generic_list<T, F>(
        &mut self,
        min_items: usize,
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        if self.at_gt() {
            if min_items == 0 {
                return Ok(Vec::new());
            }
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!("expected item before {}", user_facing_token_name("GT")),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        let mut items = Vec::new();
        loop {
            items.push(parse_item(self)?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_gt() {
                break;
            }
        }

        if items.len() < min_items {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!(
                    "expected at least {min_items} item(s) before {}",
                    user_facing_token_name("GT")
                ),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        Ok(items)
    }

    pub(in crate::parser) fn parse_comma_separated_list<T, F>(
        &mut self,
        end_name: &str,
        min_items: usize,
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        if self.at_kind_name(end_name) {
            if min_items == 0 {
                return Ok(Vec::new());
            }
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!("expected item before {}", user_facing_token_name(end_name)),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        let mut items = Vec::new();
        loop {
            items.push(parse_item(self)?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name(end_name) {
                break;
            }
        }

        if items.len() < min_items {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!(
                    "expected at least {min_items} item(s) before {}",
                    user_facing_token_name(end_name)
                ),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        Ok(items)
    }

    pub(in crate::parser) fn parse_braced_member_list<T, F>(
        &mut self,
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut items = Vec::new();
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
            items.push(parse_item(self)?);
            self.expect_semicolon()?;
            self.finish_node();
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(items)
    }

    pub(in crate::parser) fn parse_visibility(&mut self) -> Visibility {
        if self.eat_name("KW_PUBLIC") {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }
}
