#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs;
use std::path::PathBuf;

use arandu_lexer::lex_to_string;

#[test]
fn shorthand_interpolation_decodes_unicode_identifier_starts() {
    use arandu_lexer::{Lexer, TokenKind};
    for name in ["name", "variável", "é", "世界", "_valor"] {
        let source = format!("\"${name}\"");
        let tokens = Lexer::new(&source)
            .lex()
            .expect("valid interpolation")
            .tokens;
        let identifiers: Vec<_> = tokens
            .iter()
            .filter(|token| matches!(token.kind, TokenKind::IdentValue | TokenKind::IdentType))
            .map(|token| token.lexeme(&source))
            .collect();
        assert_eq!(identifiers, [name]);
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == TokenKind::InterpStart)
                .count(),
            1
        );
        for token in &tokens {
            assert!(source.is_char_boundary(token.start as usize));
            assert!(source.is_char_boundary(token.end() as usize));
        }
    }
    for literal in ["$🦀", "$1", "$", "$\u{301}"] {
        let source = format!("\"{literal}\"");
        let tokens = Lexer::new(&source).lex().expect("literal string").tokens;
        assert!(
            !tokens
                .iter()
                .any(|token| token.kind == TokenKind::InterpStart)
        );
    }
}

#[test]
fn radix_literals_require_a_digit_before_any_separator() {
    use arandu_lexer::{LexErrorCode, Lexer};

    for literal in ["0x_FF", "0b_10", "0o_7"] {
        let error = Lexer::new(literal)
            .lex()
            .expect_err("a radix literal must begin with a digit after its prefix");
        assert_eq!(error.code, LexErrorCode::InvalidNumericLiteral, "{literal}");
    }
}

#[test]
fn generic_type_closers_end_before_interface_members_and_eof() {
    use arandu_lexer::{Lexer, TokenKind};
    assert!(!TokenKind::Gt.can_end_statement());

    let source =
        "interface Reader {\nfunc read(): Result<int, err>\nfunc write(): Result<int, err>\n}";
    let tokens = Lexer::new(source).lex().expect("interface tokens").tokens;
    assert!(tokens.windows(2).any(|pair| {
        pair[0].kind == TokenKind::Gt && pair[1].kind == TokenKind::Semicolon && pair[1].inserted
    }));
    assert!(tokens.windows(2).any(|pair| {
        pair[0].kind == TokenKind::Semicolon
            && pair[0].inserted
            && pair[1].kind == TokenKind::RBrace
    }));

    let eof = Lexer::new("type Output = Result<int, err>")
        .lex()
        .expect("type alias tokens")
        .tokens;
    assert_eq!(eof[eof.len() - 2].kind, TokenKind::Semicolon);
    assert!(eof[eof.len() - 2].inserted);

    let nested = Lexer::new("type Output = Box<Box<int>>")
        .lex()
        .expect("nested type alias tokens")
        .tokens;
    assert_eq!(nested[nested.len() - 2].kind, TokenKind::Semicolon);
    assert!(nested[nested.len() - 2].inserted);

    let comparison = Lexer::new("func greater(): bool { return 1 >\n0 }")
        .lex()
        .expect("continued comparison tokens")
        .tokens;
    let greater = comparison
        .iter()
        .position(|token| token.kind == TokenKind::Gt)
        .expect("greater-than token");
    assert_ne!(
        comparison[greater + 1].kind,
        TokenKind::Semicolon,
        "{comparison:?}"
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate should be under workspace/crates")
        .to_path_buf()
}

fn assert_contract(name: &str) {
    let root = workspace_root();
    let source_path = root
        .join("tests")
        .join("lexer_contract")
        .join(format!("{name}.aru"));
    let expected_path = root
        .join("tests")
        .join("lexer_contract")
        .join(format!("{name}.tokens"));

    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
    let expected = fs::read_to_string(&expected_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", expected_path.display()));

    let actual = lex_to_string(&source).expect("lexer should succeed");
    assert_eq!(actual.trim_end(), expected.trim_end());
}

#[test]
fn doc_comments() {
    assert_contract("doc_comments");
}

#[test]
fn semicolon_before_rbrace() {
    assert_contract("semicolon_before_rbrace");
}

#[test]
fn semicolon_before_else() {
    assert_contract("semicolon_before_else");
}

#[test]
fn bidi_trojan_source_rejected_in_comments_and_strings() {
    use arandu_lexer::{LexErrorCode, Lexer};

    let bidi_chars = [
        '\u{202A}', // LRE
        '\u{202B}', // RLE
        '\u{202C}', // PDF
        '\u{202D}', // LRO
        '\u{202E}', // RLO
        '\u{2066}', // LRI
        '\u{2067}', // RLI
        '\u{2068}', // FSI
        '\u{2069}', // PDI
        '\u{200E}', // LRM
        '\u{200F}', // RLM
        '\u{061C}', // ALM
    ];

    for ch in bidi_chars {
        // Line comment
        let line_src = format!("// evil comment {ch} rest\nlet x = 1");
        let lexed = Lexer::new(&line_src).lex();
        assert_eq!(
            lexed.err().map(|e| e.code),
            Some(LexErrorCode::BidiTrojanSource),
            "should reject bidi char {ch:?} in line comment"
        );

        // Block comment
        let block_src = format!("/* evil comment {ch} rest */\nlet x = 1");
        let lexed = Lexer::new(&block_src).lex();
        assert_eq!(
            lexed.err().map(|e| e.code),
            Some(LexErrorCode::BidiTrojanSource),
            "should reject bidi char {ch:?} in block comment"
        );

        // String literal
        let str_src = format!("let s = \"evil string {ch} rest\";");
        let lexed = Lexer::new(&str_src).lex();
        assert_eq!(
            lexed.err().map(|e| e.code),
            Some(LexErrorCode::BidiTrojanSource),
            "should reject bidi char {ch:?} in string literal"
        );

        // Raw string
        let raw_src = format!("let s = r\"evil raw {ch} rest\";");
        let lexed = Lexer::new(&raw_src).lex();
        assert_eq!(
            lexed.err().map(|e| e.code),
            Some(LexErrorCode::BidiTrojanSource),
            "should reject bidi char {ch:?} in raw string"
        );

        // Outside tokens
        let raw_code = format!("func foo() {{ {ch} }}");
        let lexed = Lexer::new(&raw_code).lex();
        assert_eq!(
            lexed.err().map(|e| e.code),
            Some(LexErrorCode::BidiTrojanSource),
            "should reject bidi char {ch:?} outside tokens"
        );
    }
}
