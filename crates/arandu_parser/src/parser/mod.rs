mod decl;
mod error;
mod expr;
mod pattern;
mod stmt;
mod types;

pub use error::{ParseError, ParseErrorCode};

use arandu_lexer::{Span, Token, TokenKind};
use smol_str::SmolStr;

use crate::{
    Attribute, BinaryOp, BindingItem, Block, CatchHandler, Condition, ConstDecl, DeferBody,
    DocCommentAttachment, EnumDecl, EnumPayload, EnumVariant, Expr, ExternDecl, FieldDecl,
    FieldInit, ForBinding, ForClause, FuncDecl, FuncName, FuncSignature, GenericParam, ImportDecl,
    ImportItem, InterfaceDecl, LambdaBody, LambdaParam, MatchArm, MatchArmBody, ModuleDecl,
    Ownership, Param, Pattern, Place, PlaceSuffix, Program, ResultType, SetOp, SimpleStmt, Stmt,
    StringPart, StructDecl, TopLevelDecl, TypeAliasDecl, TypeExpr, TypeName, UnaryOp, Visibility,
    WhereItem,
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
        let module = if self.at_kind_name("KW_MODULE") {
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
            if self.at_kind_name("KW_IMPORT") || self.at_soft_keyword("from") {
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

    /// Statement terminator: explicit `;`, newline ASI (already a `SEMICOLON` token),
    /// or omitted before `}` / EOF (one-line bodies like `return 1 }`).
    pub(super) fn expect_semicolon(&mut self) -> Result<(), ParseError> {
        if self.at_kind_name("SEMICOLON") {
            self.advance();
            return Ok(());
        }
        // Optional terminator before block/file end (does not affect if-expr blocks:
        // those use expression parsing, not `expect_semicolon`).
        if self.at_kind_name("RBRACE") || self.at_kind_name("EOF") {
            return Ok(());
        }
        self.expect_name("SEMICOLON")
    }

    pub(super) fn expect_ident_value(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected value identifier",
                self.current(),
                self.file_id,
                self.source,
                &["value identifier"],
            )),
        }
    }

    pub(super) fn expect_ident_type(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentType | TokenKind::TypeErr => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected type identifier",
                self.current(),
                self.file_id,
                self.source,
                &["type identifier"],
            )),
        }
    }

    pub(super) fn expect_member_name(&mut self) -> Result<SmolStr, ParseError> {
        if self.current().kind.is_contextual_member_name() {
            let name = SmolStr::new(self.current_text());
            self.advance();
            Ok(name)
        } else {
            Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected member name",
                self.current(),
                self.file_id,
                self.source,
                &["member name"],
            ))
        }
    }

    pub(super) fn expect_name_like(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::TypeErr | TokenKind::IdentValue | TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            kind if is_contextual_module_segment(kind) => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected identifier",
                self.current(),
                self.file_id,
                self.source,
                &["identifier"],
            )),
        }
    }

    pub(super) fn expect_module_segment(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            kind if is_contextual_module_segment(kind) => {
                let text = SmolStr::new(self.current_text());
                self.advance();
                Ok(text)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected module path segment",
                self.current(),
                self.file_id,
                self.source,
                &["module path segment"],
            )),
        }
    }

    pub(super) fn expect_name(&mut self, name: &str) -> Result<(), ParseError> {
        if self.at_kind_name(name) {
            self.advance();
            return Ok(());
        }
        Err(ParseError::expected(
            ParseErrorCode::ExpectedToken,
            format!("expected {}", user_facing_token_name(name)),
            self.current(),
            self.file_id,
            self.source,
            token_expectation_names(name),
        ))
    }

    pub(super) fn eat_name(&mut self, name: &str) -> bool {
        if self.at_kind_name(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn at_gt(&self) -> bool {
        matches!(self.current().kind, TokenKind::Gt | TokenKind::ShiftRight)
    }

    pub(super) fn eat_gt(&mut self) -> bool {
        if let Some(split) = self.split_gt.take() {
            self.suppression_window += 1;
            self.emit_token_event(&split);
            if self.pos < self.tokens.len() - 1 {
                self.pos += 1;
            }
            return true;
        }
        if self.tokens[self.pos].kind == TokenKind::ShiftRight {
            self.suppression_window += 1;
            let tok = self.tokens[self.pos];
            let first = Token {
                kind: TokenKind::Gt,
                start: tok.start,
                len: 1,
                inserted: false,
            };
            let second = Token {
                kind: TokenKind::Gt,
                start: tok.start + 1,
                len: 1,
                inserted: false,
            };
            self.emit_token_event(&first);
            self.split_gt = Some(second);
            return true;
        }
        if self.tokens[self.pos].kind == TokenKind::Gt {
            self.advance();
            return true;
        }
        false
    }

    pub(super) fn expect_gt(&mut self) -> Result<(), ParseError> {
        if self.eat_gt() {
            return Ok(());
        }
        Err(ParseError::expected(
            ParseErrorCode::ExpectedToken,
            format!("expected {}", user_facing_token_name("GT")),
            self.current(),
            self.file_id,
            self.source,
            token_expectation_names("GT"),
        ))
    }

    pub(super) fn expect_kind(&mut self, kind: TokenKind) -> Result<(), ParseError> {
        if self.current().kind == kind {
            self.advance();
            Ok(())
        } else {
            let name = kind.name();
            Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                format!("expected {}", user_facing_token_name(name)),
                self.current(),
                self.file_id,
                self.source,
                token_expectation_names(name),
            ))
        }
    }

    pub(super) fn eat_kind(&mut self, kind: TokenKind) -> bool {
        if self.current().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn at_kind_name(&self, name: &str) -> bool {
        self.current().kind.name() == name
    }

    /// Soft keywords lex as plain identifiers but keep a keyword meaning in
    /// specific positions (e.g. `from` inside import syntax).
    pub(super) fn at_soft_keyword(&self, word: &str) -> bool {
        self.current().kind == TokenKind::IdentValue && self.token_text(self.current()) == word
    }

    pub(super) fn current(&self) -> &Token {
        if let Some(ref tok) = self.split_gt {
            tok
        } else {
            &self.tokens[self.pos]
        }
    }

    pub(super) fn token_text(&self, token: &Token) -> &str {
        token.lexeme(self.source)
    }

    pub(super) fn current_text(&self) -> &str {
        self.token_text(self.current())
    }

    pub(super) fn previous(&self) -> &Token {
        &self.tokens[self.pos - 1]
    }

    pub(super) fn advance(&mut self) -> &Token {
        self.advance_raw()
    }

    pub(super) fn advance_raw(&mut self) -> &Token {
        self.suppression_window += 1;
        if let Some(split) = self.split_gt.take() {
            self.emit_token_event(&split);
            if self.pos < self.tokens.len() - 1 {
                self.pos += 1;
            }
            return if self.pos > 0 {
                &self.tokens[self.pos - 1]
            } else {
                &self.tokens[0]
            };
        }
        let idx = self.pos;
        let token = self.tokens[idx];
        self.emit_token_event(&token);
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        &self.tokens[idx]
    }

    pub(crate) fn report_error(&mut self, err: ParseError) {
        if self.suppression_window >= 3 {
            self.diagnostics.push(err);
        }
        self.suppression_window = 0;
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn synchronize_top_level(&mut self) {
        if matches!(
            self.current().kind,
            TokenKind::KwFunc
                | TokenKind::KwStruct
                | TokenKind::KwEnum
                | TokenKind::KwInterface
                | TokenKind::KwImpl
                | TokenKind::KwExtern
                | TokenKind::KwType
                | TokenKind::KwConst
                | TokenKind::KwImport
                | TokenKind::KwFrom
                | TokenKind::KwModule
        ) {
            return;
        }
        self.advance_raw();
        while !self.at_kind_name("EOF") {
            if matches!(
                self.current().kind,
                TokenKind::KwFunc
                    | TokenKind::KwStruct
                    | TokenKind::KwEnum
                    | TokenKind::KwInterface
                    | TokenKind::KwImpl
                    | TokenKind::KwExtern
                    | TokenKind::KwType
                    | TokenKind::KwConst
                    | TokenKind::KwImport
                    | TokenKind::KwFrom
                    | TokenKind::KwModule
            ) {
                break;
            }
            self.advance_raw();
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) fn synchronize_stmt(&mut self) {
        if matches!(
            self.current().kind,
            TokenKind::RBrace
                | TokenKind::KwLet
                | TokenKind::KwReturn
                | TokenKind::KwIf
                | TokenKind::KwFor
                | TokenKind::KwWhile
                | TokenKind::KwMatch
                | TokenKind::KwBreak
                | TokenKind::KwContinue
                | TokenKind::KwDefer
                | TokenKind::KwErrdefer
        ) {
            return;
        }
        self.advance_raw();
        while !self.at_kind_name("EOF") {
            if self.previous().kind == TokenKind::Semicolon {
                break;
            }
            if matches!(
                self.current().kind,
                TokenKind::RBrace
                    | TokenKind::KwLet
                    | TokenKind::KwReturn
                    | TokenKind::KwIf
                    | TokenKind::KwFor
                    | TokenKind::KwWhile
                    | TokenKind::KwMatch
                    | TokenKind::KwBreak
                    | TokenKind::KwContinue
                    | TokenKind::KwDefer
                    | TokenKind::KwErrdefer
            ) {
                break;
            }
            self.advance_raw();
        }
    }

    pub(super) fn parse_expr_without_block_calls(
        &mut self,
        min_bp: u8,
    ) -> Result<Expr, ParseError> {
        let previous = self.allow_block_calls;
        self.allow_block_calls = false;
        let result = self.parse_expr(min_bp);
        self.allow_block_calls = previous;
        result
    }
}

pub(super) fn is_type_token(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].is_type_token
}

pub(super) fn primitive_type_name(kind: &TokenKind) -> Option<&'static str> {
    TOKEN_INFO_TABLE[kind.index()].primitive_type_name
}

#[derive(Clone, Copy)]
struct TokenInfo {
    is_type_token: bool,
    is_contextual_module_segment: bool,
    primitive_type_name: Option<&'static str>,
}

static TOKEN_INFO_TABLE: [TokenInfo; TokenKind::COUNT] = {
    let mut table = [TokenInfo {
        is_type_token: false,
        is_contextual_module_segment: false,
        primitive_type_name: None,
    }; TokenKind::COUNT];
    let mut i = 0;
    while i < TokenKind::COUNT {
        let kind = TokenKind::index_to_token_kind(i);
        let prim = match kind {
            TokenKind::TypeInt => Some("int"),
            TokenKind::TypeUint => Some("uint"),
            TokenKind::TypeFloat => Some("float"),
            TokenKind::TypeI8 => Some("i8"),
            TokenKind::TypeI16 => Some("i16"),
            TokenKind::TypeI32 => Some("i32"),
            TokenKind::TypeI64 => Some("i64"),
            TokenKind::TypeU8 => Some("u8"),
            TokenKind::TypeU16 => Some("u16"),
            TokenKind::TypeU32 => Some("u32"),
            TokenKind::TypeU64 => Some("u64"),
            TokenKind::TypeF32 => Some("f32"),
            TokenKind::TypeF64 => Some("f64"),
            TokenKind::TypeBool => Some("bool"),
            TokenKind::TypeByte => Some("byte"),
            TokenKind::TypeChar => Some("char"),
            TokenKind::TypeStr => Some("str"),
            TokenKind::TypeAny => Some("any"),
            TokenKind::TypeErr => Some("Err"),
            _ => None,
        };
        let is_type = prim.is_some()
            || matches!(
                kind,
                TokenKind::IdentType
                    | TokenKind::IdentValue
                    | TokenKind::KwOwn
                    | TokenKind::KwRef
                    | TokenKind::KwMut
            );
        let is_contextual = matches!(
            kind,
            TokenKind::IdentType
                | TokenKind::KwIf
                | TokenKind::KwElse
                | TokenKind::KwFor
                | TokenKind::KwIn
                | TokenKind::KwWhile
                | TokenKind::KwMatch
                | TokenKind::KwReturn
                | TokenKind::KwBreak
                | TokenKind::KwContinue
                | TokenKind::KwFunc
                | TokenKind::KwAsync
                | TokenKind::KwAwait
                | TokenKind::KwStruct
                | TokenKind::KwEnum
                | TokenKind::KwInterface
                | TokenKind::KwConst
                | TokenKind::KwType
                | TokenKind::KwModule
                | TokenKind::KwImport
                | TokenKind::KwFrom
                | TokenKind::KwAs
                | TokenKind::KwPublic
                | TokenKind::KwExtern
                | TokenKind::KwUnsafe
                | TokenKind::KwWhere
                | TokenKind::KwCatch
                | TokenKind::KwIs
                | TokenKind::KwSet
                | TokenKind::KwOwn
                | TokenKind::KwMut
                | TokenKind::KwShared
                | TokenKind::KwRef
                | TokenKind::KwSelf
                | TokenKind::KwPtr
                | TokenKind::KwDefer
                | TokenKind::KwErrdefer
                | TokenKind::KwLet
                | TokenKind::TypeInt
                | TokenKind::TypeUint
                | TokenKind::TypeFloat
                | TokenKind::TypeI8
                | TokenKind::TypeI16
                | TokenKind::TypeI32
                | TokenKind::TypeI64
                | TokenKind::TypeU8
                | TokenKind::TypeU16
                | TokenKind::TypeU32
                | TokenKind::TypeU64
                | TokenKind::TypeF32
                | TokenKind::TypeF64
                | TokenKind::TypeBool
                | TokenKind::TypeByte
                | TokenKind::TypeChar
                | TokenKind::TypeStr
                | TokenKind::TypeAny
                | TokenKind::TypeErr
        );
        table[i] = TokenInfo {
            is_type_token: is_type,
            is_contextual_module_segment: is_contextual,
            primitive_type_name: prim,
        };
        i += 1;
    }
    table
};

pub(super) fn user_facing_token_name(name: &str) -> &'static str {
    token_expectation_names(name)
        .first()
        .copied()
        .unwrap_or("token")
}

pub(super) fn token_expectation_names(name: &str) -> &'static [&'static str] {
    match name {
        // Punctuation and symbol spellings (never the internal kind name).
        "AMP" => &["&"],
        "ARROW" => &["->"],
        "AT" => &["@"],
        "COLON" => &[":"],
        "COMMA" => &[","],
        "DOT" => &["."],
        "ELLIPSIS" => &["..."],
        "EQUAL" => &["="],
        "FAT_ARROW" => &["=>"],
        "GT" => &[">"],
        "INTERP_END" | "RBRACE" => &["}"],
        "LBRACE" => &["{"],
        "LBRACKET" => &["["],
        "LPAREN" => &["("],
        "LT" => &["<"],
        "PIPE" => &["|"],
        "PLUS" => &["+"],
        "QUESTION" => &["?"],
        "RANGE_EXCLUSIVE" => &[".."],
        "RANGE_INCLUSIVE" => &["..="],
        "RBRACKET" => &["]"],
        "RPAREN" => &[")"],
        "EOF" => &["end of file"],
        "SEMICOLON" => &["statement terminator"],
        "STRING_END" => &["end of string"],
        "STRING_START" => &["string literal"],
        // Keywords and type keywords spell their source text.
        "KW_AS" => &["as"],
        "KW_ASYNC" => &["async"],
        "KW_BREAK" => &["break"],
        "KW_CONST" => &["const"],
        "KW_CONTINUE" => &["continue"],
        "KW_DEFER" => &["defer"],
        "KW_ELSE" => &["else"],
        "KW_ENUM" => &["enum"],
        "KW_ERRDEFER" => &["errdefer"],
        "KW_EXTERN" => &["extern"],
        "KW_FOR" => &["for"],
        "KW_FROM" => &["from"],
        "KW_FUNC" => &["func"],
        "KW_IF" => &["if"],
        "KW_IMPL" => &["impl"],
        "KW_IMPORT" => &["import"],
        "KW_IN" => &["in"],
        "KW_INTERFACE" => &["interface"],
        "KW_IS" => &["is"],
        "KW_LET" => &["let"],
        "KW_MATCH" => &["match"],
        "KW_MODULE" => &["module"],
        "KW_MUT" => &["mut"],
        "KW_OWN" => &["own"],
        "KW_PTR" => &["ptr"],
        "KW_PUBLIC" => &["public"],
        "KW_REF" => &["ref"],
        "KW_RETURN" => &["return"],
        "KW_SELF" => &["self"],
        "KW_SET" => &["set"],
        "KW_SHARED" => &["shared"],
        "KW_STRUCT" => &["struct"],
        "KW_TYPE" => &["type"],
        "KW_UNSAFE" => &["unsafe"],
        "KW_WHERE" => &["where"],
        "KW_WHILE" => &["while"],
        _ => &["token"],
    }
}

pub(super) fn merge_text_parts(parts: Vec<StringPart>) -> Vec<StringPart> {
    let mut merged = Vec::new();
    for part in parts {
        match (merged.last_mut(), part) {
            (
                Some(StringPart::Text {
                    span: existing_span,
                    text: existing,
                }),
                StringPart::Text { span, text: next },
            ) => {
                *existing = SmolStr::new(format!("{}{}", existing, next));
                *existing_span = span_between(*existing_span, span);
            }
            (_, part) => merged.push(part),
        }
    }
    merged
}

pub(super) fn is_contextual_module_segment(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].is_contextual_module_segment
}

pub(crate) fn is_primitive_type_token(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].primitive_type_name.is_some()
}

pub(super) fn span_between(start: Span, end: Span) -> Span {
    Span {
        file_id: start.file_id,
        start: start.start,
        end: end.end,
    }
}

pub(crate) struct Checkpoint {
    pos: usize,
    events_len: usize,
    diagnostics_len: usize,
    pending_docs_len: usize,
}

impl Checkpoint {
    pub(crate) fn new(parser: &Parser<'_>) -> Self {
        let pos = parser.pos;
        let events_len = parser.events.as_ref().map_or(0, |evs| evs.len());
        let diagnostics_len = parser.diagnostics.len();
        let pending_docs_len = parser.pending_docs.len();
        Self {
            pos,
            events_len,
            diagnostics_len,
            pending_docs_len,
        }
    }

    pub(crate) fn rollback(self, parser: &mut Parser<'_>) {
        parser.pos = self.pos;
        if let Some(evs) = &mut parser.events {
            evs.truncate(self.events_len);
        }
        parser.diagnostics.truncate(self.diagnostics_len);
        parser.pending_docs.truncate(self.pending_docs_len);
    }
}
