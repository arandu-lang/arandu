#![allow(clippy::unwrap_used, clippy::expect_used)]
use arandu_lexer::lex_to_string;
use arandu_test_support::{assert_golden_text, read_golden_text};

fn assert_golden(name: &str) {
    let source = read_golden_text("lexer", name, "aru");
    let actual = lex_to_string(&source).expect("lexer should succeed");
    assert_golden_text("lexer", name, "tokens", &actual);
}

#[test]
fn lexes_hello_fixture() {
    assert_golden("hello");
}

#[test]
fn lexes_strings_fixture() {
    assert_golden("strings");
}

#[test]
fn lexes_semicolon_fixture() {
    assert_golden("semicolon");
}

#[test]
fn lexes_numbers_fixture() {
    assert_golden("numbers");
}

#[test]
fn lexes_comments_fixture() {
    assert_golden("comments");
}

#[test]
fn lexes_if_else_fixture() {
    assert_golden("if_else");
}

#[test]
fn lexes_nested_interpolation_fixture() {
    assert_golden("interpolation_nested");
}

#[test]
fn lexes_hex_and_binary_fixture() {
    assert_golden("hex_and_binary");
}

#[test]
fn lexes_slices_and_casts_fixture() {
    assert_golden("slices_and_casts");
}

#[test]
fn lexes_attributes_canonical_fixture() {
    assert_golden("attributes_canonical");
}

#[test]
fn lexes_bitwise_hash_fixture() {
    assert_golden("bitwise_hash");
}

#[test]
fn lexes_compression_ops_fixture() {
    assert_golden("compression_ops");
}

#[test]
fn lexes_hexdump_tokens_fixture() {
    assert_golden("hexdump_tokens");
}

#[test]
fn lexes_ring_buffer_tokens_fixture() {
    assert_golden("ring_buffer_tokens");
}

#[test]
fn lexes_thread_pool_tokens_fixture() {
    assert_golden("thread_pool_tokens");
}
