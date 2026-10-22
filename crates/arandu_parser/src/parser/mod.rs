mod cursor;
mod decl;
mod error;
mod expr;
mod pattern;
mod stmt;
mod token_info;
mod types;

pub(crate) use token_info::{Checkpoint, is_primitive_type_token};
pub(super) use token_info::{
    is_contextual_module_segment, is_type_token, merge_text_parts, primitive_type_name,
    span_between, token_expectation_names, user_facing_token_name,
};

pub use error::{ParseError, ParseErrorCode};

use arandu_lexer::{Span, Token, TokenKind};
use smol_str::SmolStr;

use crate::{
    Attribute, BinaryOp, BindingItem, Block, CatchHandler, Condition, ConstDecl, DeferBody,
    DocCommentAttachment, EnumDecl, EnumPayload, EnumVariant, Expr, ExternDecl, FieldDecl,
    FieldInit, ForBinding, ForClause, FuncDecl, FuncName, FuncSignature, GenericParam, ImportDecl,
    ImportItem, InterfaceDecl, LambdaBody, LambdaParam, MatchArm, MatchArmBody, ModuleDecl,
    Ownership, Param, Pattern, Place, PlaceSuffix, Program, ResultType, SetOp, SimpleStmt, Stmt,
    StringPart, StructDecl, SubmoduleDecl, TopLevelDecl, TypeAliasDecl, TypeExpr, TypeName,
    UnaryOp, Visibility, WhereItem,
};

#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub program: Program,
    pub diagnostics: Vec<ParseError>,
}

/// Parses source, stopping at the first parse error.
///
/// # Errors
///
/// Returns the first [`ParseError`] if the source is invalid.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    parse_with_file_id(source, 0)
}

/// Parse source: **CST-first**, then lower with the CST's cached token stream (no re-lex).
#[tracing::instrument(level = "trace", target = "arandu_parser", skip(source))]
pub fn parse_with_file_id(source: &str, file_id: u32) -> Result<Program, ParseError> {
    let tree = crate::syntax::parse_syntax(source);
    crate::syntax::lower_syntax_to_program(&tree, file_id)
}

pub fn parse_recovering(source: &str) -> ParseOutput {
    parse_recovering_with_file_id(source, 0)
}

/// Recovering parse: CST-first, then RD lower on the CST token stream.
pub fn parse_recovering_with_file_id(source: &str, file_id: u32) -> ParseOutput {
    let tree = crate::syntax::parse_syntax(source);
    crate::syntax::lower_syntax_to_program_recovering(&tree, file_id)
}

/// Recursive-descent parse from source: **lex once**, then [`parse_token_stream`].
/// Prefer CST lower ([`crate::lower_syntax_to_program`]) which reuses the CST tokens.
#[doc(hidden)]
pub fn parse_tokens_to_program(source: &str, file_id: u32) -> ParseOutput {
    let lexed = arandu_lexer::lex_recovering(source);
    let lex_diags: Vec<ParseError> = lexed
        .diagnostics
        .into_iter()
        .map(|err| ParseError::from_lex(err, file_id))
        .collect();
    parse_token_stream(
        lexed.source,
        std::sync::Arc::new(lexed.tokens),
        file_id,
        lex_diags,
    )
}

/// RD lower from an **already-lexed** token stream (no lex). Used by CST-first lower.
///
/// Takes [`std::sync::Arc`] so CST lower can share the token buffer without cloning.
pub fn parse_token_stream(
    source: &str,
    tokens: std::sync::Arc<Vec<arandu_lexer::Token>>,
    file_id: u32,
    mut diagnostics: Vec<ParseError>,
) -> ParseOutput {
    let mut parser = Parser::new(source, tokens).with_file_id(file_id);

    let program = match parser.parse_program() {
        Ok(prog) => prog,
        Err(err) => {
            parser.diagnostics.push(err);
            Program {
                span: Span {
                    file_id: parser.file_id,
                    start: 0,
                    end: 0,
                },
                module: None,
                imports: Vec::new(),
                interface_impls: Vec::new(),
                decls: Vec::new(),
                docs: Vec::new(),
                pool: std::mem::take(&mut parser.pool),
            }
        }
    };

    diagnostics.extend(parser.diagnostics);

    ParseOutput {
        program,
        diagnostics,
    }
}

/// Parses source and returns an AST dump string.
///
/// # Errors
///
/// Returns the first [`ParseError`] if the source is invalid.
pub fn parse_to_string(source: &str) -> Result<String, ParseError> {
    Ok(parse(source)?.dump(source))
}

pub struct Parser<'a> {
    source: &'a str,
    /// Shared token buffer (CST lower reuses the same `Arc` — no full-stream clone).
    tokens: std::sync::Arc<Vec<Token>>,
    pos: usize,
    allow_block_calls: bool,
    pub(crate) docs: Vec<DocCommentAttachment>,
    pub(crate) interface_impls: Vec<crate::InterfaceImplDecl>,
    pending_docs: Vec<PendingDoc>,
    pub pool: crate::ast::ast_pool::AstPool,
    pub(crate) diagnostics: Vec<ParseError>,
    pub file_id: u32,
    pub suppression_window: u32,
    /// Optional event sink for green-tree construction (F1 event-driven CST).
    pub(crate) events: Option<Vec<crate::syntax::events::ParseEvent>>,
    pub(crate) split_gt: Option<Token>,
    pub(crate) recursion_depth: u32,
}

#[derive(Debug, Clone)]
pub(super) struct PendingDoc {
    span: Span,
    text: SmolStr,
}

impl<'a> Parser<'a> {
    #[must_use]
    pub fn new(source: &'a str, tokens: impl Into<std::sync::Arc<Vec<Token>>>) -> Self {
        Self {
            source,
            tokens: tokens.into(),
            pos: 0,
            allow_block_calls: true,
            docs: Vec::new(),
            interface_impls: Vec::new(),
            pending_docs: Vec::new(),
            diagnostics: Vec::new(),
            pool: crate::ast::ast_pool::AstPool::new(),
            file_id: 0,
            suppression_window: 0,
            events: None,
            split_gt: None,
            recursion_depth: 0,
        }
    }

    #[must_use]
    pub fn with_file_id(mut self, file_id: u32) -> Self {
        self.file_id = file_id;
        self
    }

    /// Enable recording of [`crate::syntax::events::ParseEvent`]s for green building.
    #[must_use]
    pub fn with_events(mut self) -> Self {
        self.events = Some(Vec::with_capacity(256));
        self
    }

    /// Take recorded events (empty if recording was off).
    #[must_use]
    pub fn take_events(&mut self) -> Vec<crate::syntax::events::ParseEvent> {
        self.events.take().unwrap_or_default()
    }

    #[inline]
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint::new(self)
    }

    #[inline]
    pub(crate) fn start_node(&mut self, kind: crate::syntax::SyntaxKind) {
        if let Some(ev) = &mut self.events {
            ev.push(crate::syntax::events::ParseEvent::Start(kind));
        }
    }

    #[inline]
    pub(crate) fn finish_node(&mut self) {
        if let Some(ev) = &mut self.events {
            ev.push(crate::syntax::events::ParseEvent::Finish);
        }
    }

    #[inline]
    fn emit_token_event(&mut self, token: &Token) {
        if let Some(ev) = &mut self.events {
            let kind = crate::syntax::map_token_kind(token.kind);
            ev.push(crate::syntax::events::ParseEvent::Token {
                kind,
                start: token.start,
                end: token.start.saturating_add(token.len),
            });
        }
    }

    /// Seek to the first non-EOF token whose `start >= offset` (green-guided lower).
    pub(crate) fn seek_to_byte(&mut self, offset: u32) {
        self.pos = self
            .tokens
            .iter()
            .position(|t| !matches!(t.kind, TokenKind::Eof) && t.start >= offset)
            .unwrap_or_else(|| self.tokens.len().saturating_sub(1));
    }

    /// Seek to an item start, rewinding over immediately preceding doc comments /
    /// semicolons so attachments match linear `parse_program`.
    pub(crate) fn seek_to_item_start(&mut self, item_start: u32) {
        self.seek_to_byte(item_start);
        while self.pos > 0 {
            let prev = &self.tokens[self.pos - 1];
            if matches!(prev.kind, TokenKind::DocComment | TokenKind::Semicolon) {
                self.pos -= 1;
            } else {
                break;
            }
        }
    }

    pub(crate) fn is_inline_submodule_at(&self, pos: usize) -> bool {
        let mut i = pos + 1;
        if i >= self.tokens.len() || !matches!(self.tokens[i].kind, TokenKind::IdentValue) {
            return false;
        }
        i += 1;
        while i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::Semicolon) {
            i += 1;
        }
        i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::LBrace)
    }

    /// Parses a full program, collecting recoverable errors in `self.diagnostics`.
    ///
    /// When event recording is enabled ([`Self::with_events`]), emits
    /// `SOURCE_FILE` / item / `BLOCK` / `STMT` structure for green trees.
    ///
    /// # Errors
    ///
    /// Returns a fatal [`ParseError`] when parsing cannot continue (for example at EOF).
    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        use crate::syntax::SyntaxKind;
        self.start_node(SyntaxKind::SOURCE_FILE);
        let start = self.mark();
        self.skip_semicolons();
        self.collect_doc_comments();
        let module = if self.at_kind_name("KW_MODULE") && !self.is_inline_submodule_at(self.pos) {
            self.start_node(SyntaxKind::MODULE_ITEM);
            let m = self.parse_module();
            self.finish_node();
            Some(m?)
        } else {
            None
        };

        let mut imports = Vec::new();
        let mut decls = Vec::new();
        while !self.at_kind_name("EOF") {
            self.skip_semicolons();
            self.collect_doc_comments();
            if self.at_kind_name("EOF") {
                break;
            }
            let is_reexport = matches!(
                self.current().kind,
                TokenKind::KwPublic | TokenKind::KwInternal | TokenKind::KwPrivate
            ) && self.tokens.get(self.pos + 1).is_some_and(|token| {
                token.kind == TokenKind::IdentValue && token.lexeme(self.source) == "use"
            });
            if self.at_kind_name("KW_IMPORT") || self.at_soft_keyword("from") || is_reexport {
                self.start_node(SyntaxKind::IMPORT_ITEM);
                match self.parse_import() {
                    Ok(import) => {
                        self.finish_node();
                        imports.push(import);
                    }
                    Err(err) => {
                        self.finish_node();
                        self.report_error(err);
                        self.synchronize_top_level();
                    }
                }
                continue;
            }
            match self.parse_top_level_decls() {
                Ok(parsed_decls) => {
                    for decl in parsed_decls {
                        let decl_id = self.pool.alloc_decl(decl);
                        decls.push(decl_id);
                    }
                }
                Err(err) => {
                    self.report_error(err);
                    self.synchronize_top_level();
                }
            }
        }

        self.finish_node(); // SOURCE_FILE
        Ok(Program {
            span: self.span_from_mark(start),
            module,
            imports,
            interface_impls: std::mem::take(&mut self.interface_impls),
            decls,
            docs: std::mem::take(&mut self.docs),
            pool: std::mem::take(&mut self.pool),
        })
    }
    pub(crate) fn mark(&self) -> usize {
        self.pos
    }

    pub(crate) fn span_from_mark(&self, start: usize) -> Span {
        let start_span = self.tokens.get(start).map_or_else(
            || self.current().span(self.file_id),
            |token| token.span(self.file_id),
        );
        let end_span = if self.split_gt.is_some() {
            let tok = &self.tokens[self.pos];
            Span::new(self.file_id, tok.start, tok.start + 1)
        } else if self.pos == start {
            start_span
        } else {
            self.tokens
                .get(self.pos.saturating_sub(1))
                .map_or(start_span, |token| token.span(self.file_id))
        };
        span_between(start_span, end_span)
    }

    pub(crate) fn skip_semicolons(&mut self) {
        while self.at_kind_name("SEMICOLON") {
            self.advance();
        }
    }

    pub(crate) fn collect_doc_comments(&mut self) {
        while matches!(self.current().kind, TokenKind::DocComment) {
            self.pending_docs.push(PendingDoc {
                span: self.current().span(self.file_id),
                text: SmolStr::new(self.current_text()),
            });
            self.advance();
            self.skip_semicolons();
        }
    }

    pub(super) fn discard_doc_comments(&mut self) {
        self.collect_doc_comments();
        self.pending_docs.clear();
    }

    pub(crate) fn take_pending_docs(&mut self) -> Vec<PendingDoc> {
        std::mem::take(&mut self.pending_docs)
    }

    pub(crate) fn attach_docs(&mut self, docs: Vec<PendingDoc>, target_span: Span) {
        self.docs
            .extend(docs.into_iter().map(|doc| DocCommentAttachment {
                span: doc.span,
                text: doc.text,
                target_span,
            }));
    }
}

#[cfg(test)]
mod tests {
    use super::merge_text_parts;
    use crate::{ExprId, StringPart};
    use arandu_lexer::Span;
    use smol_str::SmolStr;

    #[test]
    fn adjacent_text_fragments_merge_once() {
        let parts = vec![
            StringPart::Text {
                span: Span::new(0, 0, 1),
                text: SmolStr::new("a"),
            },
            StringPart::Text {
                span: Span::new(0, 1, 2),
                text: SmolStr::new("b"),
            },
            StringPart::Text {
                span: Span::new(0, 2, 3),
                text: SmolStr::new("c"),
            },
            StringPart::Expr {
                span: Span::new(0, 3, 4),
                expr: ExprId::new(7),
            },
            StringPart::Text {
                span: Span::new(0, 4, 5),
                text: SmolStr::new("d"),
            },
        ];
        let merged = merge_text_parts(parts);
        assert_eq!(merged.len(), 3);
        match &merged[0] {
            StringPart::Text { span, text } => {
                assert_eq!(text.as_str(), "abc");
                assert_eq!((span.start, span.end), (0, 3));
            }
            StringPart::Expr { .. } => panic!("expected merged text"),
        }
        match &merged[1] {
            StringPart::Expr { expr, .. } => assert_eq!(*expr, ExprId::new(7)),
            StringPart::Text { .. } => panic!("expected expression"),
        }
        match &merged[2] {
            StringPart::Text { text, .. } => assert_eq!(text.as_str(), "d"),
            StringPart::Expr { .. } => panic!("expected trailing text"),
        }
    }
}
