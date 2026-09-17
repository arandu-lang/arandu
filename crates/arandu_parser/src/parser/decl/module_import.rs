use super::super::{
    ImportDecl, ImportItem, ModuleDecl, ParseError, ParseErrorCode, Parser, TokenKind,
    is_contextual_module_segment,
};
use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn expect_optional_semicolon_after_module_path(
        &mut self,
    ) -> Result<(), ParseError> {
        if self.at_kind_name("SEMICOLON") {
            self.expect_semicolon()?;
        } else {
            let last_segment_is_contextual = is_contextual_module_segment(&self.previous().kind);
            let next_starts_top_level = self.at_kind_name("EOF")
                || self.at_kind_name("KW_IMPORT")
                || self.at_soft_keyword("from")
                || matches!(
                    self.current().kind,
                    TokenKind::At
                        | TokenKind::KwPublic
                        | TokenKind::KwConst
                        | TokenKind::KwType
                        | TokenKind::KwAsync
                        | TokenKind::KwFunc
                        | TokenKind::KwStruct
                        | TokenKind::KwEnum
                        | TokenKind::KwInterface
                        | TokenKind::KwExtern
                        | TokenKind::KwImpl
                );
            if !(last_segment_is_contextual && next_starts_top_level) {
                self.expect_semicolon()?;
            }
        }
        Ok(())
    }

    pub(crate) fn parse_module(&mut self) -> Result<ModuleDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        self.expect_name("KW_MODULE")?;
        let path = self.parse_module_path()?;
        self.expect_optional_semicolon_after_module_path()?;
        let module = ModuleDecl {
            span: self.span_from_mark(start),
            path,
        };
        self.attach_docs(docs, module.span);
        Ok(module)
    }

    pub(crate) fn parse_import(&mut self) -> Result<ImportDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();

        if self.at_soft_keyword("from") {
            self.advance();
            if self.at_kind_name("STRING_START") {
                let source = self.parse_string_literal()?;
                self.expect_name("KW_IMPORT")?;
                self.expect_name("LBRACE")?;
                let items = self.parse_comma_separated_list("RBRACE", 1, |parser| {
                    let item_start = parser.mark();
                    let name = parser.expect_import_name()?;
                    let alias = if parser.eat_name("KW_AS") {
                        Some(parser.expect_import_name()?)
                    } else {
                        None
                    };
                    Ok(ImportItem {
                        span: parser.span_from_mark(item_start),
                        name,
                        alias,
                    })
                })?;
                self.skip_semicolons();
                self.expect_name("RBRACE")?;
                self.expect_optional_semicolon_after_module_path()?;
                let import = ImportDecl::ExternalNamed {
                    span: self.span_from_mark(start),
                    source,
                    items,
                };
                self.attach_docs(docs, import.span());
                return Ok(import);
            } else {
                let path = self.parse_module_path()?;
                self.expect_name("KW_IMPORT")?;
                self.expect_name("LBRACE")?;
                let items = self.parse_comma_separated_list("RBRACE", 1, |parser| {
                    let item_start = parser.mark();
                    let name = parser.expect_import_name()?;
                    let alias = if parser.eat_name("KW_AS") {
                        Some(parser.expect_import_name()?)
                    } else {
                        None
                    };
                    Ok(ImportItem {
                        span: parser.span_from_mark(item_start),
                        name,
                        alias,
                    })
                })?;
                self.skip_semicolons();
                self.expect_name("RBRACE")?;
                self.expect_optional_semicolon_after_module_path()?;
                let import = ImportDecl::Named {
                    span: self.span_from_mark(start),
                    path,
                    items,
                };
                self.attach_docs(docs, import.span());
                return Ok(import);
            }
        }

        // `import <path> as <alias>` or `import "<source>" as <alias>`
        self.expect_name("KW_IMPORT")?;

        if self.at_kind_name("STRING_START") {
            let source = self.parse_string_literal()?;
            self.expect_name("KW_AS")?;
            let alias = self.expect_import_name()?;
            self.expect_optional_semicolon_after_module_path()?;
            let import = ImportDecl::ExternalAlias {
                span: self.span_from_mark(start),
                source,
                alias,
            };
            self.attach_docs(docs, import.span());
            return Ok(import);
        }

        let path = self.parse_module_path()?;
        let alias = if self.eat_name("KW_AS") {
            self.expect_import_name()?
        } else {
            // `parse_module_path` always pushes at least one segment.
            path.last().cloned().ok_or_else(|| {
                ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected module path segment",
                    self.current(),
                    self.file_id,
                    self.source,
                )
            })?
        };
        self.expect_optional_semicolon_after_module_path()?;
        let import = ImportDecl::ModuleAlias {
            span: self.span_from_mark(start),
            path,
            alias,
        };
        self.attach_docs(docs, import.span());
        Ok(import)
    }

    pub(in crate::parser) fn parse_string_literal(&mut self) -> Result<SmolStr, ParseError> {
        self.expect_name("STRING_START")?;
        let mut text = String::new();
        while !self.at_kind_name("STRING_END") {
            match &self.current().kind {
                TokenKind::StringText | TokenKind::StringEscape => {
                    text.push_str(self.current_text());
                    self.advance();
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "expected string content",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
        }
        self.expect_name("STRING_END")?;
        Ok(text.into())
    }

    pub(in crate::parser) fn parse_module_path(
        &mut self,
    ) -> Result<SmallVec<[SmolStr; 3]>, ParseError> {
        let mut path = SmallVec::new();
        path.push(self.expect_module_segment()?);
        while self.eat_name("DOT") {
            path.push(self.expect_module_segment()?);
        }
        Ok(path)
    }

    pub(in crate::parser) fn expect_import_name(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue | TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected import identifier",
                self.current(),
                self.file_id,
                self.source,
                &["import identifier"],
            )),
        }
    }
}
