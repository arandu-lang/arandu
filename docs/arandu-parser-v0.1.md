# Arandu Parser Contract v0.1

Status: specification plus current Rust implementation snapshot
Grammar source: `arandu-grammar-v0.6.ebnf`
Consumes: `docs/arandu-lexer-v0.1.md`
Produces: `docs/arandu-ast-v0.1.md`

## Visão Geral e Contexto

Este contrato descreve o parser CST-first e o lowering canônico do CST para
AST, incluindo recuperação de erro e fixtures públicas.

## Detalhes Técnicos da Implementação

### Goal

Define how the parser turns lexer tokens into the Arandu AST. This contract fixes parser behavior and documents the current `crates/arandu_parser/` implementation slice.

The parser must:

- consume lexer-inserted and explicit semicolons uniformly;
- build the AST described in `docs/arandu-ast-v0.1.md`;
- use recursive descent for declarations and statements;
- use Pratt parsing for expressions;
- preserve spans on every AST node;
- report syntax errors with useful recovery points.

Current implemented slice:

- `module` and module imports;
- named imports: `from ui import { Button, Window as AppWindow }`;
- attributes, `const`, `type`, `struct`, `enum`, `interface`, `extern`, and functions;
- `public`, `async`, and type-qualified method names such as `func User.greet(...)`;
- generic params, generic type args, where clauses, and generic calls;
- blocks, multi-binding variable declarations, `set`, `return`, expression statements, `if`, `match`, `for`, and `while`;
- `defer`, `errdefer`, `free`, and `unsafe` statements;
- primitive, named, generic, slice, array, nullable, pointer, grouped, and function types;
- literals, paths, type-qualified paths, calls, calls with trailing blocks, bare block calls, struct literals, arrays, field access, safe field access, index expressions, safe index expressions, unary and binary expressions;
- `alloc`, `async` blocks, `unsafe` blocks, lambdas, `catch`, null coalescing, postfix try, casts, and expression ranges;
- match patterns for wildcard, literals, bindings, enum variants, tuple payloads, struct patterns, tuples, and ranges;
- structured interpolated strings, raw strings, and multiline strings.
- `DOC_COMMENT` tokens consumed without disrupting parsing and attached to the next documentable node in `Program.docs`.

Still outside this parser contract: type checking, name resolution, mutability validation, ownership/memory validation, FFI legality, exhaustiveness, unsafe legality, and other checker-only rules.

### Compiler Pipeline Status

| Feature | Lexer | Parser | Name Resolution | Type Check |
| --- | --- | --- | --- | --- |
| `module` / `import` | yes | yes | single-file namespace imports and named aliases; no file loading | prelude/import checks for current subset |
| `func` / `struct` / `enum` / `interface` / `extern` | yes | yes | yes, including type-qualified associated funcs | v0.1 core yes |
| generics / `where` | yes | yes | names and constraints | generic calls, constraints, and interface satisfaction |
| `for` / `while` | yes | yes | loop bindings | partial |
| `defer` / `errdefer` | yes | yes | names in cleanup blocks | partial |
| `unsafe` / `free` / `alloc` | yes | yes | names | `free` requires ptr; unsafe legality deferred |
| `catch` / `as` / `?` / `??` / safe access | yes | yes | names | checked for v0.1 cases; full catch lowering deferred |
| lambdas / arrays / block calls | yes | yes | names and lambda params | arrays yes; lambda semantics deferred |

### Input and Output

Input:

```text
TokenStream
```

Output on success:

```text
Program
```

Output on failure:

```text
ParseDiagnostics
PartialProgram?
```

v0.1 parser may stop at first fatal error. It should still structure diagnostics so later versions can recover and continue.

### Parser Architecture

Use two cooperating parsers:

- Declaration/statement parser: recursive descent.
- Expression parser: Pratt parser driven by binding power.

Rationale:

- Top-level and statement syntax is keyword-led and regular.
- Expressions have many postfix, unary, binary, and control forms; Pratt parsing keeps precedence extensible.

### Entry Point

### parse_program

Grammar:

```text
program = [ module_decl ] { top_level_decl } EOF
```

Behavior:

- If the first token is `KW_MODULE`, parse one `ModuleDecl`.
- Parse top-level declarations until `EOF`.
- Reject statements at top level.
- Require `EOF` after the final declaration.

Diagnostic:

```text
E_PARSE_EXPECTED_TOP_LEVEL_DECL
```

when a top-level token cannot start any declaration.

### Semicolons

The parser treats explicit and inserted `SEMICOLON` tokens identically.

Rules:

- `stmt_end` consumes one `SEMICOLON`.
- Multiple semicolons between declarations or statements are permitted and ignored.
- Empty statements are not represented in the AST.

Example:

```arandu
name = "Bruno";

io.println(name)
```

AST:

```text
VarDecl(name)
ExprStmt(CallExpr(io.println))
```

### Top-Level Declarations

Top-level declaration starters:

```text
AT
KW_PUBLIC
KW_CONST
KW_TYPE
KW_ASYNC
KW_FUNC
KW_STRUCT
KW_ENUM
KW_INTERFACE
KW_EXTERN
KW_IMPORT
```

### Attributes and Visibility

Public annotation names follow the normative
[`@PascalCase` contract](./arandu-attribute-naming-v0.1.md). The parser
preserves spelling; semantic analysis owns recognition and migration aliases.

Before declarations that allow attributes:

```text
attribute_list = { attribute }
visibility = "public"
```

Parser rule:

- Parse all leading attributes.
- Parse optional `KW_PUBLIC`.
- Dispatch on the next declaration keyword.

Related diagnostics:

- `E_PARSE_EXPECTED_ATTRIBUTE_NAME`
- `E_PARSE_EXPECTED_DECL_AFTER_VISIBILITY`

### module_decl

Consumes:

```text
KW_MODULE module_path stmt_end
```

AST:

```text
ModuleDecl(path)
```

Parser decisions:

- `module_path` is a dotted path of value-like segments.
- Selected contextual keywords are accepted as module-path segments so the stable corpus can use names such as `examples.stable.syntax.match`.
- The contextual-segment exception applies only while parsing `module` and `import` paths.

### import_decl

Forms:

```text
import io
from io import { println as print, Reader }
```

AST:

```text
ImportDecl.ModuleImport
ImportDecl.NamedImport
```

Parser decisions:

- Named imports use a comma-separated item list.
- Named imports require at least one item.
- Trailing commas are allowed in named imports when they appear before the closing `}`.

Related diagnostics:

- `E_PARSE_EXPECTED_IMPORT_PATH`
- `E_PARSE_EXPECTED_IMPORT_ITEM`
- `E_PARSE_EXPECTED_FROM`

### Comma-separated contracts

The parser uses the same comma-list shape across named imports, generic params, generic args, function params, `where` items, tuple result types, and tuple/struct enum payloads.

Rules:

- List cardinality is context-dependent and must remain explicit in the parser.
- Trailing commas are allowed only where the EBNF includes `[ "," ]`.
- Empty named imports are rejected.
- Empty function parameter lists are allowed.
- Empty tuple result types are rejected; tuple results require at least two types.

### Parser Lookahead Contracts

Some statement and expression decisions are implemented with lookahead. These rules must remain named and tested:

- Variable declaration lookahead: a statement beginning with `IDENT_VALUE` is a `VarDecl` when the parser finds `=` at depth zero before `;`, `EOF`, or `{`.
- Generic call lookahead: a `<...>` sequence in expression position is treated as generic arguments only when the matching `>` is immediately followed by `(`.

### const_decl

Form:

```text
const answer int = 42
```

Decision:

- The optional type expression begins when the token after the name can start a type and is not `EQUAL`.
- Type-expression ambiguity is limited by identifier categories.

### type_alias_decl

Form:

```text
type UserId = int
```

Supports generic params and where clauses.

### func_decl

Forms:

```text
func main() {}
public func main() {}
async func fetch(path str) Result<str, Err> {}
public async func fetch(path str) Result<str, Err> {}
func User.greet(user User) str {}
```

Parser decisions:

- `KW_PUBLIC KW_FUNC` is a public function declaration.
- `KW_ASYNC KW_FUNC` is an async function declaration.
- `KW_PUBLIC KW_ASYNC KW_FUNC` is a public async function declaration.
- `IDENT_VALUE` after `func` is a free function.
- `IDENT_TYPE DOT IDENT_VALUE` after `func` is a method-like function name.
- Function result type is present when the token after `)` can start a type or is `LPAREN` for multi-value returns.
- Error-bearing functions must use `Result<T, E>`. Tuple returns like `(T, E)` where `E` is `Err` are rejected at parse time (**P006**).

Related diagnostics:

- `E_PARSE_EXPECTED_FUNC_NAME`
- `E_PARSE_EXPECTED_PARAM_NAME`
- `E_PARSE_EXPECTED_PARAM_TYPE`
- `E_PARSE_VARIADIC_PARAM_NOT_LAST`

### struct_decl

Form:

```arandu
struct User {
    name str
    age int
}
```

Fields require `stmt_end`.

### enum_decl

Form:

```arandu
enum LoadState {
    Idle,
    Loaded(str),
    Failed { message str }
}
```

Parser decisions:

- Commas separate variants.
- Empty enum bodies are allowed.
- Tuple and struct payload variants map to distinct AST payload kinds.

### interface_decl

Form:

```arandu
interface Writer {
    func write(bytes []byte) Result<void, Err>
}
```

Members are function signatures followed by `stmt_end`.

### extern_decl

Form:

```arandu
extern "C" {
    func puts(text ptr[u8]) int
}
```

Parser decisions:

- ABI must be a static simple string literal token sequence: `STRING_START STRING_TEXT STRING_END`.
- ABI literals must not use interpolation, escapes, raw strings, or multiline strings in v0.1.
- Extern members are signatures only.

Diagnostic:

```text
E_PARSE_EXTERN_MEMBER_MUST_BE_SIGNATURE
```

### Types

Type expression starters:

```text
TYPE_INT TYPE_UINT TYPE_FLOAT
TYPE_I8 TYPE_I16 TYPE_I32 TYPE_I64
TYPE_U8 TYPE_U16 TYPE_U32 TYPE_U64
TYPE_F32 TYPE_F64
TYPE_BOOL TYPE_BYTE TYPE_CHAR TYPE_STR TYPE_ANY TYPE_ERR
IDENT_TYPE
KW_MUT followed by IDENT_VALUE
LBRACKET
KW_FUNC
LPAREN
KW_PTR
```

Note:

- `ptr` is reserved in v0.1 because pointer syntax is core systems-language syntax.

Type forms:

- Primitive: `int`
- Named: `User`
- Generic named: `Box<int>`
- Slice: `[]int`
- Mutable slice: `mut []int`
- Array: `[4]int`
- Pointer: `ptr[u8]`
- Function: `func(int, int) int`
- Parenthesized: `(int)`
- Nullable: `T?` (optional value types; function errors use `Result<T, E>`)

Parser decision:

- Nullable wraps only the immediately preceding type expression.

### Blocks and Statements

Statement starters:

```text
KW_RETURN
KW_BREAK
KW_CONTINUE
KW_FREE
KW_IF
KW_FOR
KW_WHILE
KW_MATCH
KW_DEFER
KW_ERRDEFER
KW_UNSAFE
KW_SET
KW_MUT
IDENT_VALUE
literal starters
LPAREN
LBRACKET
KW_ALLOC
KW_ASYNC
```

### block

Form:

```text
LBRACE { statement } RBRACE
```

Behavior:

- Blocks do not require trailing semicolon.
- Empty blocks are allowed.

### var_decl

Forms:

```arandu
name = "Bruno"
mut age int = 25
file, err = open("data.txt")
```

Disambiguation:

- A statement starting with `KW_MUT` is a `VarDecl`.
- A statement starting with `IDENT_VALUE` is a `VarDecl` only if the parser sees a binding-list shape followed by `EQUAL`.
- Otherwise parse as `ExprStmt`.

Binding-list shape:

```text
[mut] IDENT_VALUE [type_expr] { "," [mut] IDENT_VALUE [type_expr] } "="
```

### set_stmt

Forms:

```arandu
set age = age + 1
set items[0] += 1
set matrix[i][j] = 2
set user.name >>= 1
```

AST:

```text
SetStmt.Assign
SetStmt.CompoundAssign
```

Supported assignment operators in the current parser slice:

```text
= -= *= /= %= &= |= ^= <<= >>=
```

### return_stmt

Forms:

```arandu
return
return value
return value, err
```

Decision:

- If the next token is `SEMICOLON`, return value list is empty.

### if_stmt and if_expr

Statement form:

```arandu
if condition {
} else if condition {
} else {
}
```

Expression form requires final `else`.

Diagnostic:

```text
E_PARSE_IF_EXPR_REQUIRES_ELSE
```

### match_stmt and match_expr

Forms:

```arandu
match value {
    Pattern => expr
    Pattern if guard => {
    }
}
```

Parser decisions:

- Expression-arm bodies require `stmt_end`.
- Block-arm bodies do not require `stmt_end`.
- Exhaustiveness is not checked by parser.

### defer and errdefer

Forms:

```arandu
defer file.close()
defer {
    file.close()
}
```

Decision:

- If the token after `defer` or `errdefer` is `{`, parse block body.
- Otherwise parse expression body and require `stmt_end`.

### Patterns

Pattern starters:

```text
literal starters
IDENT_VALUE
IDENT_TYPE
UNDERSCORE via IDENT_VALUE("_")
LPAREN
```

Pattern forms:

- Literal pattern
- Binding pattern: `value` or mutable `mut value`
- Wildcard pattern
- Type variant pattern: `Some(value)`
- Enum pattern: `Result.Ok(value)`
- Struct pattern: `User { name, age: value }`
- Tuple pattern: `(a, b)`
- Range pattern: `1..=10`

Parser decision:

- `_` lexes as `IDENT_VALUE("_")`, but parser converts it to `WildcardPattern` in pattern context.
- `mut` is accepted only before a value binding and marks that arm-local binding mutable.
- Try wildcard, range, enum, struct, and tuple patterns before simple binding/type patterns.
- `IDENT_TYPE DOT IDENT_TYPE` starts an enum pattern.
- `IDENT_TYPE LBRACE` starts a struct pattern.
- A literal followed by `..` or `..=` starts a range pattern.

### Pratt Expression Parser

Expression entry:

```text
parse_expr(min_bp = 0)
```

Prefix parselets:

```text
literal
IDENT_VALUE path
IDENT_TYPE struct literal or type-qualified value path
LPAREN grouped expression or lambda
LBRACKET array literal
KW_ALLOC
KW_ASYNC block
KW_UNSAFE block
KW_IF if expression
KW_MATCH match expression
BANG
MINUS
TILDE
KW_AWAIT
```

Type-qualified value path:

```arandu
User.greet(user)
math.Vec2.dot(a, b)
```

Parser decision:

- `IDENT_TYPE LBRACE` starts a struct literal.
- `IDENT_TYPE DOT IDENT_VALUE` starts a type-qualified value path.
- `IDENT_VALUE DOT IDENT_VALUE` remains a normal value path or field expression.

Interpolated strings:

- `STRING_TEXT` parts become `StringPart::Text`.
- `${ ... }` parts are parsed with the normal expression parser and become `StringPart::Expr`.
- Plain strings without interpolation are still dumped as `String("...")` for stable debug output.

Postfix parselets:

```text
generic call: <T>(args)
call: (args)
trailing block call: (args)? block
field: .name
safe field: ?.name
index: [expr]
safe index: ?[expr]
try: ?
```

Infix parselets, low to high:

```text
catch
??
||
&&
== !=
< > <= >=
.. ..=
|
^
&
<< >>
+ -
* / %
as
```

Binding power table:

```text
catch              10, right
??                 20, left
||                 30, left
&&                 40, left
== !=              50, left
< > <= >=          60, left
.. ..=             70, non-associative
|                  80, left
^                  90, left
&                  100, left
<< >>              110, left
+ -                120, left
* / %              130, left
as                 140, left
prefix             150, right
postfix            160, left
```

Non-associative range rule:

```arandu
1..2..3
```

Diagnostic:

```text
E_PARSE_CHAINED_RANGE
```

Generic call ambiguity:

- `foo<int>(value)` is a generic call.
- `a < b > c` is comparison syntax, not generic arguments.
- Generic arguments in expressions are accepted only when immediately followed by `(`.

### Error Recovery

v0.1 may stop at first fatal error, but diagnostics must include recovery hints.

Synchronization tokens:

```text
SEMICOLON
RBRACE
EOF
KW_FUNC
KW_STRUCT
KW_ENUM
KW_INTERFACE
KW_EXTERN
KW_IMPORT
KW_CONST
KW_TYPE
```

Common diagnostics:

```text
E_PARSE_UNEXPECTED_TOKEN
E_PARSE_EXPECTED_TOKEN
E_PARSE_EXPECTED_EXPR
E_PARSE_EXPECTED_TYPE
E_PARSE_EXPECTED_PATTERN
E_PARSE_EXPECTED_BLOCK
E_PARSE_EXPECTED_STMT_END
E_PARSE_EXPECTED_TOP_LEVEL_DECL
E_PARSE_EXPECTED_DECL_AFTER_VISIBILITY
E_PARSE_EXTERN_MEMBER_MUST_BE_SIGNATURE
E_PARSE_IF_EXPR_REQUIRES_ELSE
E_PARSE_CHAINED_RANGE
E_PARSE_VARIADIC_PARAM_NOT_LAST
```

Diagnostic shape:

```json
{
  "phase": "parser",
  "code": "E_PARSE_EXPECTED_EXPR",
  "message": "expected expression after `=`",
  "span": { "start_line": 4, "start_col": 12, "end_line": 4, "end_col": 12 },
  "expected": ["expression"],
  "found": "SEMICOLON"
}
```

### Parser Test Fixtures

### Fixture: hello program

Input:

```arandu
module examples.hello

import io

func main() {
    name = "Bruno"
    io.println("Ola, ${name}")
}
```

Expected AST shape:

```text
Program
  ModuleDecl(examples.hello)
  ImportDecl(io)
  FuncDecl(main)
    VarDecl(name)
    ExprStmt(CallExpr(FieldExpr(io, println)))
```

### Fixture: var decl versus set

Input:

```arandu
let mut idade = 25
set idade = idade + 1
```

Expected AST shape:

```text
Block
  VarDecl(binding idade mutable=true)
  SetStmt.Assign(place idade)
```

### Fixture: expression precedence

Input:

```arandu
value = 1 + 2 * 3
```

Expected AST shape:

```text
VarDecl(value)
  BinaryExpr(+)
    IntLiteral(1)
    BinaryExpr(*)
      IntLiteral(2)
      IntLiteral(3)
```

### Fixture: match arm

Input:

```arandu
match token {
    Token.Number(value) if value > 0 => "positive"
    _ => "other"
}
```

Expected AST shape:

```text
MatchStmt
  value: ValuePath(token)
  arms:
    EnumPattern(Token.Number)
      guard: BinaryExpr(>)
      body: StringLiteral("positive")
    WildcardPattern
      body: StringLiteral("other")
```

### Fixture: generic call ambiguity

Input:

```arandu
ok = identity<int>(42)
compare = a < b > c
```

Expected AST shape:

```text
VarDecl(ok)
  GenericCallExpr(identity, [int], [42])
VarDecl(compare)
  BinaryExpr(>)
    BinaryExpr(<)
      ValuePath(a)
      ValuePath(b)
    ValuePath(c)
```

### Known v0.1 Decisions

- The parser does not perform name resolution.
- The parser does not check mutability, ownership, type validity, exhaustiveness, or unsafe legality.
- The parser may accept semantically invalid ASTs so later phases can produce better diagnostics.
- The parser accepts `any` wherever a type expression is syntactically valid; the type checker rejects it outside variadic parameters, extern/FFI declarations, and compiler builtins.
- General enum value construction is not introduced by this contract because EBNF v0.6 does not define it as an expression form; current checked constructors use type-qualified calls such as `Result.Ok(...)`, `Result.Err(...)`, and `Option.Some(...)`.
- `Err(...)` is not parsed as an error constructor. Use ordinary calls such as `err.new(...)` where an error value is needed.
- Parser tests should use `examples/stable/` as positive fixtures and `examples/invalid/syntax/` as negative fixtures.

## PONTOS DE MELHORIA (O que não está no roadmap)

Os pontos de pressão gramatical registrados acima não autorizam sintaxe nova.
O principal risco é criar um segundo caminho AST-first que duplique parsing e
enfraqueça a recuperação incremental.

## Futuro e Próximos Passos

Evoluir a gramática somente com CST, formatter, erros e testes de recovery
simultâneos; preservar `syntax_tree(file)` como entrada canônica.
