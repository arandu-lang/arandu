# Arandu AST v0.1

Status: implemented AST with conceptual schema examples; Rust definitions are authoritative for physical representation
Grammar source: `arandu-grammar-v0.6.ebnf`
Owner: `arandu_parser::ast`; the parser lowers canonical Rowan CST to this AST. The compiler is implemented in Rust; schemas below describe logical records, not a stable serialization API.

## Visão Geral e Contexto

A AST representa a estrutura semântica produzida a partir do CST, antes de
resolução de nomes, tipos e lowering para os IRs.

## Detalhes Técnicos da Implementação

### Principles

- The AST represents program meaning, not punctuation. Logical semicolons, commas, comments, and grouping parentheses are not nodes unless they affect meaning.
- Every node has a `span`, even when it is synthesized later. Spans are required for diagnostics, editor tooling, and future source maps.
- Names keep their syntactic category: `ValueName` starts lowercase or `_`; `TypeName` starts uppercase; primitive types are keywords.
- Declaration and mutation remain distinct in the AST: `x = 1` is `VarDecl`; `set x = 2` is `SetStmt`.
- The AST does not perform type checking. It may contain semantically invalid programs so later passes can report precise errors.

### Common Records

### Span

Fields:

- `file_id`: source file identity (`u32`).
- `start`: inclusive UTF-8 byte offset (`u32`).
- `end`: exclusive UTF-8 byte offset (`u32`).

The physical `arandu_base::Span` is 12 bytes. Paths/line/column and UTF-16
positions are derived at rendering/protocol boundaries, not stored in every span.
Older expanded JSON examples below are explanatory projections, not a promised wire format.

Invariant:

- `start <= end`.

Example:

```json
{
  "file_id": 1,
  "start": 0,
  "end": 29
}
```

### Identifier

Variants:

- `ValueName`: lowercase or `_` start, used for variables, functions, modules, fields, and value paths.
- `TypeName`: uppercase start, used for structs, enums, interfaces, variants, and generic parameters.
- `PrimitiveTypeName`: lowercase keyword type such as `int`, `str`, `bool`, `Err`.

Fields:

- `text`
- `span`

Invariant:

- The lexer decides the identifier category before parsing.

### Attribute

Fields:

- `name`: `ValueName`
- `args`: list of `Argument`
- `span`

Example:

```arandu
@Link("m")
extern "C" {
    func cos(value f64) f64
}
```

### Program Structure

### Program

Fields:

- `module`: optional `ModuleDecl`
- `imports`: list of `ImportDecl`
- `decls`: list of `TopLevelDecl`
- `docs`: list of `DocCommentAttachment`
- `span`

Invariant:

- At most one module declaration, and it must appear before top-level declarations.
- Consecutive doc comments attach to the next documentable node. Doc comments inside statement blocks are consumed and ignored in v0.1.

JSON sketch:

```json
{
  "kind": "Program",
  "module": { "kind": "ModuleDecl", "path": ["examples", "stable", "syntax", "hello"] },
  "imports": [
    { "kind": "ImportDecl", "path": ["io"] }
  ],
  "decls": [
    { "kind": "FuncDecl", "name": "main", "params": [], "body": [] }
  ],
  "docs": []
}
```

### DocCommentAttachment

Fields:

- `span`: span of the doc comment token.
- `text`: original comment text.
- `target_span`: span of the documented node.

Documentable targets:

- module declarations;
- imports;
- top-level declarations;
- struct fields;
- enum variants;
- interface and extern function signatures.

### ModuleDecl

Fields:

- `path`: list of `ValueName`
- `span`

Example:

```arandu
module examples.stable.syntax.hello
```

### ImportDecl

Variants:

- `ModuleImport`: `import io`
- `NamedImport`: `from io import { println as print }`

Fields:

- `path`: module path for module imports.
- `from`: source module path for named imports.
- `items`: list of `ImportItem` for named imports.
- `span`

Invariant:

- `items` is empty for `ModuleImport` and non-empty for `NamedImport`.

`ImportItem` fields:

- `name`: imported value or type name.
- `alias`: optional local name from `as`.
- `span`

### Declarations

`TopLevelDecl` variants:

- `ImportDecl`
- `ConstDecl`
- `TypeAliasDecl`
- `FuncDecl`
- `StructDecl`
- `EnumDecl`
- `InterfaceDecl`
- `ExternDecl`

Common declaration fields:

- `attrs`: list of `Attribute`
- `visibility`: `private` or `public`
- `span`

### ConstDecl

Fields:

- `name`: `ValueName` or `TypeName`
- `type`: optional `TypeExpr`
- `value`: `Expr`
- common declaration fields

Invariant:

- Constants are immutable by definition.

### TypeAliasDecl

Fields:

- `name`: `TypeName`
- `generic_params`: list of `GenericParam`
- `aliased_type`: `TypeExpr`
- `where`: list of `WhereItem`
- common declaration fields

### FuncDecl

Fields:

- `name`: `FuncName`
- `is_async`: bool
- `generic_params`: list of `GenericParam`
- `params`: list of `Param`
- `result`: optional `ResultType`
- `where`: list of `WhereItem`
- `body`: `Block`
- common declaration fields

Invariant:

- A method-like function uses `FuncName.Method(receiver_type, method_name)`.
- Variadic `...` is only valid on the final parameter.

Example:

```arandu
func add(a int, b int) int {
    return a + b
}
```

AST sketch:

```json
{
  "kind": "FuncDecl",
  "name": { "kind": "FreeFunc", "name": "add" },
  "params": [
    { "name": "a", "type": { "kind": "PrimitiveType", "name": "int" } },
    { "name": "b", "type": { "kind": "PrimitiveType", "name": "int" } }
  ],
  "result": { "kind": "SingleResult", "type": { "kind": "PrimitiveType", "name": "int" } }
}
```

### Param

Fields:

- `attrs`: list of `Attribute`
- `ownership`: `borrow`, `own`, or `mut_borrow`
- `name`: `ValueName`
- `type`: `TypeExpr`
- `is_variadic`: bool
- `span`

Invariant:

- Missing ownership means safe non-owning borrow.

### StructDecl

Fields:

- `name`: `TypeName`
- `generic_params`: list of `GenericParam`
- `where`: list of `WhereItem`
- `fields`: list of `FieldDecl`
- common declaration fields

### FieldDecl

Fields:

- `name`: `ValueName`
- `type`: `TypeExpr`
- `default`: optional `Expr`
- common declaration fields

A field default is evaluated during type checking (constant foldable) and is
applied when a struct literal omits the field:

```arandu
struct Config {
    port: int = 8080
    label: str
}

let cfg = Config { label: "api" } // port == 8080
```

### EnumDecl

Fields:

- `name`: `TypeName`
- `generic_params`: list of `GenericParam`
- `where`: list of `WhereItem`
- `variants`: list of `EnumVariant`
- common declaration fields

### EnumVariant

Variants:

- `UnitVariant`: `Idle`
- `TupleVariant`: `Loaded(str)`
- `StructVariant`: `Failed { message str }`

Fields:

- `attrs`
- `name`: `TypeName`
- `payload`
- `span`

### InterfaceDecl

Fields:

- `name`: `TypeName`
- `generic_params`: list of `GenericParam`
- `where`: list of `WhereItem`
- `members`: list of `FuncSignature`
- common declaration fields

### ExternDecl

Fields:

- `abi`: string literal value, for example `"C"`
- `members`: list of `FuncSignature`
- `attrs`
- `span`

Invariant:

- Extern members have signatures only, never bodies.

### Generics and Constraints

### GenericParam

Fields:

- `name`: `TypeName` (identifier segment)
- `constraints`: list of `TypeName`
- `default`: optional `TypeExprId` (T2.1 — e.g. `A = GlobalAllocator`)
- `span`

Invariant:

- Defaults apply only when fewer type arguments are written than parameters;
  trailing parameters without a default still require an explicit arg.

### WhereItem

Fields:

- `target`: `TypeName`
- `constraints`: list of `TypeName`
- `span`

### Types

`TypeExpr` variants:

- `PrimitiveType`
- `NamedType`
- `SliceType`
- `ArrayType`
- `PtrType`
- `FuncType`
- `NullableType`

Common field:

- `span`

### PrimitiveType

Fields:

- `name`: one of `int`, `uint`, `float`, `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `f32`, `f64`, `bool`, `byte`, `char`, `str`, `any`, `Err`.

### NamedType

Fields:

- `path`: optional module path
- `name`: `TypeName`
- `generic_args`: list of `TypeExpr`

### SliceType

Fields:

- `element`: `TypeExpr`

Example:

```arandu
[]int
```

### ArrayType

Fields:

- `length`: integer literal token value
- `element`: `TypeExpr`

### PtrType

Fields:

- `pointee`: `TypeExpr`

Example:

```arandu
ptr[u8]
```

### FuncType

Fields:

- `params`: list of `TypeExpr`
- `result`: optional `ResultType`

### NullableType

Fields:

- `inner`: `TypeExpr`

Invariant:

- `T?` is represented as a wrapper node, not a flag on every type.

### Blocks and Statements

### Block

Fields:

- `statements`: list of `Statement`
- `span`

`Statement` variants:

- `VarDecl`
- `SetStmt`
- `ReturnStmt`
- `BreakStmt`
- `ContinueStmt`
- `FreeStmt`
- `ExprStmt`
- `IfStmt`
- `ForStmt`
- `WhileStmt`
- `MatchStmt`
- `DeferStmt`
- `ErrDeferStmt`
- `UnsafeStmt`

### VarDecl

Fields:

- `bindings`: list of `BindingItem`
- `value`: `Expr`
- `span`

Invariant:

- `bindings.length >= 1`.
- This node declares new bindings and never mutates existing places.

Example:

```arandu
mut age int = 25
```

### BindingItem

Fields:

- `name`: `ValueName`
- `is_mutable`: bool
- `type`: optional `TypeExpr`
- `span`

### Let Else and Guard

`let <pattern> = <expr> else { <else_block> }` and
`guard <condition> else { <else_block> }` have no AST node of their own. The
parser folds the **remaining statements of the enclosing block** into an
equivalent `Stmt::If`:

```arandu
// input
let .Some(value) = parse(input) else { return .None }
use_value(value)

// lowers to
if parse(input) is .Some(value) { use_value(value) } else { return .None }
```

Rules:

- `let` with a bare binding list (`let (a, b) = pair`) stays a plain
  `Stmt::VarDecl` with two `BindingItem`s; only the refutable form binds a
  pattern and needs `else`.
- The refutable form requires `else`. Without it the statement falls back to
  the ordinary `let` binding form, which only accepts binding names, so a
  refutable pattern there is a parse error.
- Because the rest of the block moves into `then_block`, a refutable `let`
  must be the last statement of its block.
- `guard` takes a full `Condition` (so `guard x is .Some(v)` and
  `guard a > 0 && b > 0` both work) and also requires `else`.
- Bindings introduced by the pattern are visible in the moved statements, which
  is exactly the scope the written program implies.

### SetStmt

Variants:

- `Assign`: list of `PlaceExpr`, value `Expr`
- `CompoundAssign`: place `PlaceExpr`, operator, value `Expr`

Invariant:

- Targets are places, not arbitrary expressions.

Compound assignment operators:

- `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`

### PlaceExpr

Fields:

- `root`: `ValueName`
- `suffixes`: list of `PlaceSuffix`
- `span`

`PlaceSuffix` variants:

- `Field(name)`
- `Index(expr)`

### ReturnStmt

Fields:

- `values`: list of `Expr`
- `span`

Invariant:

- Empty list means bare `return`.

### Control Statements

`BreakStmt`, `ContinueStmt`, and `FreeStmt` keep only their span, except `FreeStmt`, which also has `value: Expr`.

### IfStmt

Fields:

- `condition`: `Condition`
- `then_block`: `Block`
- `else_if`: list of `IfBranch`
- `else_block`: optional `Block`
- `span`

### Condition

Variants:

- `ExprCondition(expr)`
- `IsCondition(expr, pattern)`

### ForStmt

Variants:

- `ForIn(bindings, iterable, body)`
- `ForCStyle(init, condition, step, body)`

### MatchStmt

Fields:

- `value`: `Expr`
- `arms`: list of `MatchArm`
- `span`

### DeferStmt and ErrDeferStmt

Fields:

- `body`: `DeferBody`
- `span`

`DeferBody` variants:

- `Expr(expr)`
- `Block(block)`

### Patterns

`Pattern` variants:

- `LiteralPattern`
- `BindingPattern` with a `mutable` flag
- `TypeVariantPattern`
- `WildcardPattern`
- `EnumPattern`
- `StructPattern`
- `TuplePattern`
- `RangePattern`
- `OrPattern` (`p1 | p2`)

Invariant:

- `_` is always `WildcardPattern`, never a binding.
- `mut name` creates a mutable binding while preserving the matched value's type.
- Pattern exhaustiveness is checked later, not during parsing.

Leading-dot variant patterns:

```arandu
match slot {
    .Occupied(value) => value
    .Empty => 0
}
```

`.Variant` and `.Variant(payload)` parse as the leading-dot form of
`TypeVariantPattern`: the type is inferred from the scrutinee, so only the
variant name is written. This form is accepted in `match` arms, `if … is`
conditions, `let`/`let … else` and `guard`; it is never a value expression in
pattern position and never a bare path.

### MatchArm

Fields:

- `pattern`: `Pattern`
- `guard`: optional `Expr`
- `body`: `MatchArmBody`
- `span`

`MatchArmBody` variants:

- `Expr`
- `Block`

Example:

```arandu
Token.Number(value) if value > 0 => "positive number"
```

### Expressions

`Expr` variants:

- `LiteralExpr`
- `ValuePathExpr`
- `TypeQualifiedValuePathExpr`
- `VariantSugarExpr`
- `StructLiteralExpr`
- `ArrayLiteralExpr`
- `LambdaExpr`
- `AllocExpr`
- `AsyncBlockExpr`
- `UnsafeBlockExpr`
- `IfExpr`
- `MatchExpr`
- `BareBlockCallExpr`
- `CallExpr`
- `GenericCallExpr`
- `FieldExpr`
- `SafeFieldExpr`
- `IndexExpr`
- `SafeIndexExpr`
- `TryExpr`
- `AwaitExpr`
- `UnaryExpr`
- `BinaryExpr`
- `CastExpr`
- `CatchExpr`

Invariant:

- Precedence is not represented by nested precedence nodes. It is represented by the shape of expression trees.

### TypeQualifiedValuePathExpr

Fields:

- `type`: `NamedType`
- `member`: `ValueName`
- `span`

### VariantSugarExpr

Fields:

- `name`: `ValueName`
- `args`: list of `Expr` (empty for unit variants)
- `span`

Example:

```arandu
let color = .Red
return .Some(10)
```

Invariant:

- The enum or result type comes from the expected type of the expression
  (return type, binding type, argument type). A variant sugar that has no
  expected enum type is a type-checking error, not a parse error.
- `TypeQualifiedValuePathExpr` is unrelated: it is a static member lookup on a
  named type, not a variant constructor.

### Pipe Expressions

`|>` has no dedicated AST node. The parser lowers it eagerly:

```arandu
value |> add(5)
value |> add(_, 5)
value |> handler
```

- Right-hand call with a `_` placeholder: the piped value replaces the
  placeholder argument in place.
- Right-hand call without `_`: the piped value becomes the first argument.
- Right-hand non-call: it becomes a one-argument call.

The result is always an ordinary `CallExpr`, so later phases never see a pipe.

Example:

```arandu
User.greet(user)
```

Invariant:

- This node represents statically qualified value lookup, not field access on an instance.

Example:

```arandu
age + 1 * 2
```

AST sketch:

```json
{
  "kind": "BinaryExpr",
  "op": "+",
  "left": { "kind": "ValuePathExpr", "path": ["age"] },
  "right": {
    "kind": "BinaryExpr",
    "op": "*",
    "left": { "kind": "IntLiteral", "value": "1" },
    "right": { "kind": "IntLiteral", "value": "2" }
  }
}
```

### Calls

`CallExpr` fields:

- `callee`: `Expr`
- `args`: list of `Argument`
- `trailing_block`: optional `Block`
- `span`

`GenericCallExpr` adds:

- `generic_args`: list of `TypeExpr`

Invariant:

- Generic arguments in expressions are only valid as part of a call.

### Argument

Arguments are a positional list of expressions. Labels and the `_` pipe
placeholder are parser-only syntax:

- `f(expr)` — positional.
- `f(label: expr)` — the label is consumed and the expression is stored
  positionally, so the AST does not distinguish `f(label: x)` from `f(x)`.
- `f(label:)` — shorthand for `f(label)`; the label name becomes a
  `ValuePathExpr` argument.
- `_` — a `ValuePathExpr` whose only path segment is `_`; the pipe lowering
  replaces it. A `_` that survives to type checking is an unresolved value.

### StructLiteralExpr

Fields:

- `type`: `NamedType`
- `fields`: list of `FieldInit`
- `span`

### FieldInit

Fields:

- `name`: `ValueName`
- `value`: `Expr`
- `span`

### LambdaExpr

Fields:

- `params`: list of `LambdaParam`
- `body`: `Expr` or `Block`
- `span`

### IfExpr and MatchExpr

Expression forms mirror `IfStmt` and `MatchStmt`, but they must produce values. Type checking enforces branch compatibility.

### CatchExpr

Fields:

- `body`: `Expr`
- `handler`: optional `Expr` or `CatchBlock`
- `span`

### TryExpr

Fields:

- `body`: `Expr`
- `span`

Invariant:

- `expr?` is parsed before the type checker decides whether the expression is `Result<T, E>`, `Option<T>`, or an allowed `Nullable` (`T?`).

### Literals

`Literal` variants:

- `IntLiteral`
- `FloatLiteral`
- `BoolLiteral`
- `CharLiteral`
- `ByteLiteral`
- `ByteStringLiteral`
- `StringLiteral`
- `NilLiteral`

### Byte Literals

`b'A'` is a `ByteLiteral` with a `u8` value already decoded from the token (the
lexer rejects non-ASCII bytes and invalid escapes).

`b"..."` is a `ByteStringLiteral` that keeps the decoded payload as raw bytes
(`Arc<[u8]>`), not as a `str`:

```arandu
let raw = b"\xFF\x00AB"
```

Invariants:

- The element type is `u8`; the expression type is `[]u8` (a slice), so
  `raw.len`, `raw[i]` and sub-slicing behave like any other byte slice.
- Bytes are preserved exactly. A payload that is not valid UTF-8 never becomes
  a `StringLiteral` and never round-trips through `str`.
- Byte strings do not interpolate: `b"${x}"` is six literal bytes.
- The payload is stored in the AMIR literal pool as raw bytes
  (`AmirLiteralEntry::Bytes`) and every backend emits it as a static array.

### Numeric Literals

Fields:

- `raw`: original token text
- `base`: decimal, hex, binary, or octal for integers
- `span`

Invariant:

- Underscores are preserved in `raw` and normalized later.

### StringLiteral

Variants:

- `NormalString`
- `MultilineString`
- `RawString`

Fields:

- `parts`: list of `StringPart`
- `span`

`StringPart` variants:

- `Text`
- `Escape`
- `Interpolation(expr)`

Example:

```arandu
"Ola, ${name}"
```

AST sketch:

```json
{
  "kind": "StringLiteral",
  "parts": [
    { "kind": "Text", "value": "Ola, " },
    { "kind": "Interpolation", "expr": { "kind": "ValuePathExpr", "path": ["name"] } }
  ]
}
```

### v0.1 Pass Boundaries

- Lexer: produces tokens, doc comments, inserted logical semicolons, and identifier categories.
- Parser: produces this AST, including invalid semantic programs.
- Name resolver: resolves module/value/type paths.
- Type checker: validates type expressions, calls, generics, nullability, errors, and match exhaustiveness.
- Memory checker: validates ownership, moves, borrows, `free`, `defer`, `errdefer`, and `unsafe`.

### Type Checker Semantic Restrictions

- The parser accepts primitive type `any` anywhere a type expression is allowed.
- The type checker rejects `any` outside variadic parameters, extern/FFI declarations, and compiler builtins.
- This keeps the grammar simple while preserving Arandu's goal of explicit, safe types in ordinary code.

### Known Grammar Pressure Points

- General enum declarations and enum patterns exist in EBNF v0.6, but general enum value construction is not represented as a standalone primary expression. Current checked constructors use type-qualified calls such as `Result.Ok(...)`, `Result.Err(...)`, and `Option.Some(...)`.
- `Err` is a primitive type keyword, not currently a value constructor. Stable examples use value-path helpers such as `err.new(...)` where an error value is needed.
- UI and web examples live under `examples/draft/` because they intentionally explore syntax beyond the stable compiler contract.

## PONTOS DE MELHORIA (O que não está no roadmap)

As grammar pressure points acima são limites conhecidos. A AST não deve
absorver tipos resolvidos, IDs de símbolo ou detalhes de backend.

## Futuro e Próximos Passos

Adicionar nós apenas quando a gramática/CST correspondente estiver aceita e
coberta; manter spans reais e lowering total para HIR.
