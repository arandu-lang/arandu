# Arandu Lexer Contract v0.1

Status: implemented lexer; token schemas below are conceptual projections
Grammar source: `arandu-grammar-v0.6.ebnf`
Depends on: `docs/arandu-ast-v0.1.md`

## Visão Geral e Contexto

Este contrato define a transformação determinística de texto UTF-8 na
sequência de tokens consumida pelo parser e pelos testes dourados.

## Detalhes Técnicos da Implementação

### Goal

Define the token stream that the parser will consume. The lexer is implemented in `arandu_lexer`; this document describes its lexical contract and explanatory token projections. Physical token packing, compact byte spans and exact output are defined by the Rust types and golden tests, not by these JSON examples.

The lexer must:

- classify keywords, primitive types, value identifiers, and type identifiers;
- preserve spans for every token;
- preserve doc comments for later attachment to AST nodes;
- support string interpolation;
- normalize statement endings by inserting logical semicolons;
- report malformed tokens with precise source spans.

### Token Record

Every token has:

- `kind`: token kind from this contract.
- `lexeme`: exact source text, except inserted semicolons use an empty lexeme.
- `span`: source span.
- `inserted`: bool, true only for lexer-inserted semicolons.

Example:

```json
{
  "kind": "IDENT_VALUE",
  "lexeme": "idade",
  "span": { "start_line": 1, "start_col": 5, "end_line": 1, "end_col": 10 },
  "inserted": false
}
```

### Lexing Order

Use longest-match first.

Priority:

1. EOF.
2. Doc comments: `///...` and `/**...*/`.
3. Normal comments: `//...` and `/*...*/`.
4. String literals, raw strings, multiline strings, and char literals.
5. Multi-character operators and punctuation.
6. Numeric literals.
7. Identifiers and keywords.
8. Single-character operators and punctuation.
9. Whitespace and newlines.
10. Invalid characters.

Reason:

- `///` must not become `//` plus `/`.
- `?.`, `?[`, `??`, `=>`, `+=`, `<<=`, and similar operators must remain single tokens.
- `r"""..."""` must be recognized before identifier `r`.

### Whitespace and Comments

Whitespace:

- Spaces and tabs are skipped.
- Newlines are skipped after semicolon insertion is considered.
- CRLF counts as one newline.

Normal comments:

- `// text\n` is skipped.
- `/* text */` is skipped.
- Block comments nest (e.g. `/* outer /* inner */ */`).

Doc comments:

- `/// text\n`
- `/** text */`

Doc comments are emitted as ordinary `DOC_COMMENT` tokens in the token stream, but they are not significant for semicolon insertion. The parser may later attach them to the next declaration or field.

Doc comment token fields:

- `lexeme`: exact comment text.
- `style`: `line` or `block`.
- `span`.

Example:

```arandu
/// Computes the distance.
func distance(x f64, y f64) f64 {
    return x + y
}
```

Doc comment stream:

```text
DOC_COMMENT("/// Computes the distance.")
FUNC IDENT_VALUE(distance) ...
```

### Identifiers

Value identifiers:

```text
value_start = "a".."z" | "_"
identifier_continue = "a".."z" | "A".."Z" | "0".."9" | "_"
```

Token:

```text
IDENT_VALUE(name)
```

Examples:

- `user`
- `parseJson`
- `_tmp`

Type identifiers:

```text
type_start = "A".."Z"
identifier_continue = "a".."z" | "A".."Z" | "0".."9" | "_"
```

Token:

```text
IDENT_TYPE(name)
```

Examples:

- `User`
- `Vec2`
- `Result`
- `T`

Invariant:

- Identifier category is syntactic and decided by the lexer, not by name resolution.
- Primitive types are keywords, not identifiers.

### Keywords

Reserved control and declaration keywords:

```text
if else for in while match return break continue
func async await struct enum interface const type module import from as public
extern unsafe where catch is set
own mut ptr alloc free defer errdefer
```

Primitive type keywords:

```text
int uint float
i8 i16 i32 i64
u8 u16 u32 u64
f32 f64
bool byte char str any Err
```

Literal keywords:

```text
true false nil
```

Keyword token naming:

```text
KW_IF
KW_ELSE
KW_FOR
KW_IN
KW_WHILE
KW_MATCH
KW_RETURN
KW_BREAK
KW_CONTINUE
KW_FUNC
KW_ASYNC
KW_AWAIT
KW_STRUCT
KW_ENUM
KW_INTERFACE
KW_CONST
KW_TYPE
KW_MODULE
KW_IMPORT
KW_AS
KW_PUBLIC
KW_EXTERN
KW_UNSAFE
KW_WHERE
KW_CATCH
KW_IS
KW_SET
KW_OWN
KW_MUT
KW_PTR
KW_ALLOC
KW_FREE
KW_DEFER
KW_ERRDEFER
TYPE_INT
TYPE_UINT
TYPE_FLOAT
TYPE_I8
TYPE_I16
TYPE_I32
TYPE_I64
TYPE_U8
TYPE_U16
TYPE_U32
TYPE_U64
TYPE_F32
TYPE_F64
TYPE_BOOL
TYPE_BYTE
TYPE_CHAR
TYPE_STR
TYPE_ANY
TYPE_ERR
BOOL_TRUE
BOOL_FALSE
NIL
```

Invariant:

- `Err` is a primitive type token despite starting uppercase.
- Keywords cannot be used as identifiers in v0.1.
- **Soft keyword `from`**: `from` lexes as an ordinary value identifier
  (`IDENT`), not as `KW_FROM`. The parser recognizes a `from` identifier in
  import position (`{ a, b } from <path>`) as the import keyword, so
  `func from(...)`, `String.from`, variable and field names are legal. The
  `KW_FROM` token kind is retired; it is never produced by the lexer (the
  enum variant is kept as a sentinel for CST/import reconstruction).

### Numeric Literals

Integer tokens:

```text
INT_DEC
INT_HEX
INT_BIN
INT_OCT
```

Float token:

```text
FLOAT
```

Rules:

- Decimal integer: `0` or nonzero digit followed by digits or `_`.
- Hex integer: `0x` followed by hex digits or `_`.
- Binary integer: `0b` followed by binary digits or `_`.
- Octal integer: `0o` followed by octal digits or `_`.
- Float: decimal literal with fraction or exponent.
- Preserve the raw lexeme.
- Numeric value normalization happens after lexing.

Valid examples:

```text
0
25
1_000
0xff
0b1010_0011
0o755
3.14
1e9
1.0e-3
```

Invalid examples and diagnostics:

```text
1_
```

```text
error: numeric literal cannot end with `_`
```

```text
0x
```

```text
error: expected hexadecimal digit after `0x`
```

```text
0b102
```

```text
error: invalid binary digit `2`
```

```text
01
```

```text
error: decimal literals cannot have leading zeroes
```

### String and Char Literals

Tokens:

```text
STRING_START
STRING_TEXT
STRING_ESCAPE
INTERP_START
INTERP_END
STRING_END
RAW_STRING
MULTILINE_STRING_START
MULTILINE_STRING_END
CHAR
```

The lexer may either emit a single structured `STRING` token or emit string-part tokens. For v0.1 parser clarity, use string-part tokens.

Normal string:

```arandu
"Ola, ${name}"
```

Token sketch:

```text
STRING_START
STRING_TEXT("Ola, ")
INTERP_START
IDENT_VALUE(name)
INTERP_END
STRING_END
```

Multiline string:

```arandu
"""
linha 1
${value}
"""
```

Raw string:

```arandu
r"C:\tmp\file.txt"
```

Raw strings:

- do not process escapes;
- do not process interpolation;
- end at the next matching quote sequence.

Escapes:

```text
\n
\t
\r
\0
\\
\"
\'
\$
\u{HEX+}
```

Char literal:

```arandu
'a'
'\n'
'\u{1f44d}'
```

Char diagnostics:

```text
error: empty char literal
error: char literal contains more than one codepoint
error: unterminated char literal
```

String diagnostics:

```text
error: unterminated string literal
error: invalid escape sequence
error: expected `}` to close string interpolation
error: unterminated multiline string literal
```

### Operators and Punctuation

Multi-character tokens:

```text
?. ?[ ?? => += -= *= /= %= &= |= ^= <<= >>= << >> == != <= >= ..= ..
...
```

Single-character tokens:

```text
( ) [ ] { } , . : ; @
= - * / % & | ^ < > = ! ~ ?
```

Token names:

```text
LPAREN RPAREN
LBRACKET RBRACKET
LBRACE RBRACE
COMMA DOT COLON SEMICOLON AT
PLUS MINUS STAR SLASH PERCENT
AMP PIPE CARET
LT GT EQUAL BANG TILDE QUESTION
SAFE_DOT SAFE_INDEX_START NULL_COALESCE LOGICAL_OR LOGICAL_AND FAT_ARROW
PLUS_EQUAL MINUS_EQUAL STAR_EQUAL SLASH_EQUAL PERCENT_EQUAL
AMP_EQUAL PIPE_EQUAL CARET_EQUAL SHIFT_LEFT_EQUAL SHIFT_RIGHT_EQUAL
SHIFT_LEFT SHIFT_RIGHT
EQUAL_EQUAL BANG_EQUAL LT_EQUAL GT_EQUAL
RANGE_INCLUSIVE RANGE_EXCLUSIVE
ELLIPSIS
```

Longest-match examples:

```text
set items[0] += 1
```

Tokens:

```text
KW_SET IDENT_VALUE(items) LBRACKET INT_DEC(0) RBRACKET PLUS_EQUAL INT_DEC(1)
```

```text
user?.name ?? "unknown"
```

Tokens:

```text
IDENT_VALUE(user) SAFE_DOT IDENT_VALUE(name) NULL_COALESCE STRING_START STRING_TEXT("unknown") STRING_END
```

### Logical Semicolon Insertion

The grammar uses `stmt_end = ";"`, but source files do not require semicolons.

The lexer inserts `SEMICOLON(inserted=true)` at a newline or EOF when the previous significant token can end a statement or expression.

Tokens that can end a statement:

```text
IDENT_VALUE
IDENT_TYPE
TYPE_INT TYPE_UINT TYPE_FLOAT
TYPE_I8 TYPE_I16 TYPE_I32 TYPE_I64
TYPE_U8 TYPE_U16 TYPE_U32 TYPE_U64
TYPE_F32 TYPE_F64
TYPE_BOOL TYPE_BYTE TYPE_CHAR TYPE_STR TYPE_ANY TYPE_ERR
INT_DEC INT_HEX INT_BIN INT_OCT FLOAT
BOOL_TRUE BOOL_FALSE NIL
CHAR STRING_END RAW_STRING MULTILINE_STRING_END
RPAREN RBRACKET RBRACE
QUESTION
KW_RETURN KW_BREAK KW_CONTINUE
```

Do not insert a semicolon before:

```text
RPAREN RBRACKET COMMA
PLUS MINUS STAR SLASH PERCENT
AMP PIPE CARET
SHIFT_LEFT SHIFT_RIGHT
DOT SAFE_DOT SAFE_INDEX_START
QUESTION
NULL_COALESCE
LOGICAL_OR LOGICAL_AND
EQUAL EQUAL_EQUAL BANG_EQUAL LT GT LT_EQUAL GT_EQUAL
RANGE_EXCLUSIVE RANGE_INCLUSIVE
FAT_ARROW
KW_ELSE
KW_CATCH
KW_AS
```

`RBRACE` is intentionally absent from this list. A line before `}` can still need a logical semicolon:

```arandu
func add(a int, b int) int {
    return a + b
}
```

The lexer inserts `SEMICOLON(inserted=true)` after `b` and before `}`.

The outer closing brace of `${expression}` is an exception: it terminates an
expression interpolation, so a preceding newline does not insert a semicolon.
Lookahead skips line comments and nested block comments before that delimiter.
Braces belonging to blocks nested inside the interpolation retain ordinary
statement-ending rules.

`KW_ELSE` is intentionally present in the prevention list. This keeps both same-line and next-line `else` forms valid:

```arandu
if ok {
    println("sim")
}
else {
    println("nao")
}
```

Do not insert after tokens that clearly continue a construct:

```text
LPAREN LBRACKET LBRACE COMMA DOT SAFE_DOT SAFE_INDEX_START
PLUS MINUS STAR SLASH PERCENT AMP PIPE CARET
SHIFT_LEFT SHIFT_RIGHT
EQUAL
KW_IF KW_ELSE KW_FOR KW_WHILE KW_MATCH KW_FUNC KW_STRUCT KW_ENUM
KW_INTERFACE KW_EXTERN KW_IMPORT KW_WHERE KW_CATCH
KW_DEFER KW_ERRDEFER KW_UNSAFE KW_ASYNC KW_AWAIT
```

Examples:

```arandu
name = "Bruno"
io.println(name)
```

Tokens include:

```text
IDENT_VALUE(name) EQUAL STRING_START STRING_TEXT("Bruno") STRING_END SEMICOLON(inserted)
IDENT_VALUE(io) DOT IDENT_VALUE(println) LPAREN IDENT_VALUE(name) RPAREN SEMICOLON(inserted)
```

No insertion after binary operator:

```arandu
value = 1 +
    2
```

Tokens include no semicolon after `+`.

No insertion before trailing block call:

```arandu
column {
    text("Oi")
}
```

The newline after `column` is absent here; if users write `column\n{ ... }`, v0.1 treats that as statement end before `{`.

### Lexer Contract Notes

The lexer contract suite pins a few behaviors that are easy to drift:

- `DOC_COMMENT` tokens preserve both line and block doc comments verbatim.
- Normal comments are skipped.
- `SEMICOLON(inserted=true)` is emitted before `EOF` and before `}` when the preceding token can end a statement.
- `KW_ELSE` suppresses insertion so newline-separated `if`/`else` remains valid.

### EOF Handling

The lexer emits:

```text
EOF
```

If the final significant token can end a statement and no explicit semicolon was emitted, insert `SEMICOLON(inserted=true)` before `EOF`.

Example:

```arandu
module examples.hello
```

Tokens:

```text
KW_MODULE IDENT_VALUE(examples) DOT IDENT_VALUE(hello) SEMICOLON(inserted) EOF
```

### Error Tokens and Diagnostics

The lexer should stop at the first invalid token in v0.1. Later tooling can add recovery.

Diagnostic shape:

```json
{
  "phase": "lexer",
  "code": "E_LEX_INVALID_ESCAPE",
  "message": "invalid escape sequence `\\q`",
  "span": { "start_line": 3, "start_col": 12, "end_line": 3, "end_col": 14 }
}
```

Required diagnostic codes:

```text
E_LEX_INVALID_CHAR
E_LEX_UNTERMINATED_STRING
E_LEX_UNTERMINATED_MULTILINE_STRING
E_LEX_UNTERMINATED_RAW_STRING
E_LEX_UNTERMINATED_CHAR
E_LEX_EMPTY_CHAR
E_LEX_CHAR_TOO_LONG
E_LEX_INVALID_ESCAPE
E_LEX_INVALID_UNICODE_ESCAPE
E_LEX_UNTERMINATED_BLOCK_COMMENT
E_LEX_INVALID_NUMERIC_LITERAL
E_LEX_INVALID_BINARY_DIGIT
E_LEX_INVALID_OCTAL_DIGIT
E_LEX_INVALID_HEX_DIGIT
E_LEX_LEADING_ZERO
E_LEX_UNCLOSED_INTERPOLATION
```

### Lexer Test Fixtures

When implementation starts, create tests from these fixtures.

### Fixture: declaration and mutation

Input:

```arandu
let mut idade = 25
set idade = idade + 1
```

Expected tokens:

```text
KW_LET KW_MUT IDENT_VALUE(idade) EQUAL INT_DEC(25) SEMICOLON(inserted)
KW_SET IDENT_VALUE(idade) EQUAL IDENT_VALUE(idade) PLUS INT_DEC(1) SEMICOLON(inserted)
EOF
```

### Fixture: TypeName vs valueName

Input:

```arandu
Vec2 { x: 1, y: 2 }
column { text("Oi") }
```

Expected token distinction:

```text
IDENT_TYPE(Vec2)
IDENT_VALUE(column)
```

### Fixture: comments

Input:

```arandu
/// User name.
name = "Ana" // inline comment
```

Expected significant tokens:

```text
DOC_COMMENT("/// User name.")
IDENT_VALUE(name) EQUAL STRING_START STRING_TEXT("Ana") STRING_END SEMICOLON(inserted)
EOF
```

### Fixture: interpolation

Input:

```arandu
io.println("Ola, ${name}")
```

Expected tokens include:

```text
STRING_START STRING_TEXT("Ola, ") INTERP_START IDENT_VALUE(name) INTERP_END STRING_END
```

Interpolation may contain nested braces from `if`, `match`, blocks, and struct literals. The lexer closes interpolation only at a `}` with interpolation brace depth zero.

Example:

```arandu
"status=${if ok { "sim" } else { "nao" }}"
```

### Fixture: EOF semicolon

Input:

```arandu
return value
```

Expected tokens:

```text
KW_RETURN IDENT_VALUE(value) SEMICOLON(inserted) EOF
```

### Known v0.1 Decisions

- Nested block comments are supported.
- Unicode identifiers are fully supported.
- No keyword escaping syntax such as `` `type` ``.
- No numeric suffixes such as `42u32`; type selection is handled by context or annotations.
- No single-token interpolated string. Interpolation produces nested expression tokens between `INTERP_START` and `INTERP_END`.
- Newline before `{` after a bare value path is treated as statement boundary. Same-line trailing block calls remain valid.

## PONTOS DE MELHORIA (O que não está no roadmap)

As decisões v0.1 acima permanecem restrições deliberadas. Novos literais,
escapes ou interpolação não devem entrar apenas no lexer: precisam atualizar
gramática, CST, parser, formatter e fixtures em conjunto.

## Futuro e Próximos Passos

Manter fixtures de Unicode, erros e inserção de semicolon alinhadas ao parser;
qualquer extensão léxica futura é ordenada exclusivamente pelo roadmap mestre.
