# Arandu Compiler IR Architecture v0.1

> **Roadmap e checklist executivo:** [arandu-compiler-roadmap-v0.1.md](./arandu-compiler-roadmap-v0.1.md)

This document defines the intermediate representation (IR) strategy for the Arandu compiler. To keep the compiler development pragmatic, modular, and maintainable, Arandu strictly separates semantic analysis from control flow and memory analysis using a two-level IR architecture.

---

## Visão Geral e Contexto

Este documento registra por que o compilador separa AST, AHIR/HIR e AMIR e
quais responsabilidades podem cruzar cada fronteira.

## Detalhes Técnicos da Implementação

### 1. Compiler Pipeline

The compilation pipeline for an Arandu source file follows a structured phase-by-phase layout:

```text
Source Code (.aru)
       ↓
     Lexer       (Tokenizes source code)
       ↓
    Parser       (Canonical Rowan CST → AST)
       ↓
   Resolver      (Name resolution, symbol definitions)
       ↓
 Type Checker    (Infers & validates types, populates TypeInfo)
       ↓
   Lowering      (Converts AST + TypeInfo to AHIR)
       ↓
   AHIR v0.1     (High-level Intermediate Representation / Typed AST)
       ↓
   AMIR v0.1     (Mid-level Intermediate Representation / CFG & OSSA)
       ↓
Borrow Checker   (Ownership, lifetimes, and generational checks)
       ↓
  Backend IR     (Lowered instructions for code generation)
       ↓
 Code Gen Targets (C / Cranelift / Wasm; LLVM is proposed)
```

---

### 2. AST vs AHIR vs AMIR

> **Implementation note (C5):** In `arandu_middle::hir`, **AHIR** is represented by pooled HIR nodes; lowering lives in `arandu_semantics::passes::lower_hir` (`HirProgram`, `lower_to_hir`, CLI `arandu hir`). Documentation and CLI may say “AHIR” or “HIR”; they refer to the same IR. This table describes logical shape, not recursive heap ownership.

The intermediate representations serve distinct purposes at different stages of the compiler:

| Feature / Goal | AST (Abstract Syntax Tree) | AHIR (Arandu High-level IR) | AMIR (Arandu Mid-level IR) |
| :--- | :--- | :--- | :--- |
| **Primary Goal** | Syntax validation | High-level semantics & Types | Flow, memory & code gen |
| **Structure** | Tree (matching source) | Tree (typed, resolved) | Control Flow Graph (CFG) |
| **Names** | Unresolved strings | Resolved `SymbolId` / `LocalId` | SSA Temp registers (`_0`, `%0`) |
| **Types** | Unresolved (`TypeExpr`) | Fully resolved (`ArType`) | Lowered representation types |
| **Memory / Safety** | None (implicit/syntax only) | None | Explicit Ownership SSA (OSSA) |

---

### 3. AHIR v0.1 Responsibilities

AHIR v0.1 serves as the **Typed AST**. It represents the program semantically while maintaining tree structure and source-level constructs.

### Scope of AHIR

- **Fully Resolved Names**: Every identifier reference is resolved to its defined `SymbolId`.
- **Fully Inferred/Checked Types**: Every expression and variable binding carries its concrete `ArType`.
- **Generics & Interfaces**: Preserves structural interface constraints, generic parameters, and `where` clauses in their source-level semantic form.
- **Result/Option Constructors**: `Result.Ok`, `Result.Err`, and `Option.Some` are represented semantically before AMIR lowering.
- **Diagnostics Context**: Preserves source spans (`Span`) to emit high-quality diagnostics close to the user's code.

### Excluded from AHIR

- **No CFG**: Loops, conditions, and logical evaluations remain as nested tree statements.
- **No Borrow Checking**: Lifetime analysis and mutation conflicts are not evaluated here.
- **No Lowering**: Features like `defer`, `errdefer`, catch blocks, and safe-navigation (`?`) are preserved as-is.

---

### 4. AMIR Responsibilities

AMIR represents the program as a Control Flow Graph (CFG) with explicit control-flow blocks and dataflow edges. It is designed to perform dataflow analysis and enforce memory safety.

### AMIR v0.1 (Current)

- **Explicit Control Flow Graph (CFG)**: Lowers `if`, `while`, `for`, and control flow into basic blocks and jumps.
- **Local Registers**: Every variable and temporary is a numbered local (`_0`, `_1`, ...) with explicit type.
- **Place Projections**: Supports nested field and index mutations via `AmirPlace` with projections.
- **Basic Expressions**: Literals, binary/unary ops, field access, index access, array literals, struct literals, calls.
- **Lowered in AMIR:** `match`, `if is`, `defer`/`errdefer`, `?`, `?.`, `?[]`, `??`, `for in`, `alloc`, and `free`.
- **Result/Option Representation:** HIR uses `ResultCtor`; AMIR materializes the concrete variant through `EnumConstruct` and target-derived layout. Source-level `Result<T,E>`/`Option<T>` do not expose the internal representation as tuple fallback syntax.
- **Definite Initialization:** `arandu_mir/src/definite_init.rs` runs a CFG dataflow analysis and reports O008 for possibly uninitialized local reads.
- **OSSA ownership:** AMIR models `copy`, `move`, `borrow`, `borrow_mut`, `destroy`, `StorageLive`, and `StorageDead`. BV.1 derives a stable structural `ReturnBorrowSummary` from typed AMIR flow, unions only origins that reach each result path, solves recursive calls to fixpoint and carries the converged contract on calls. The narrow `borrow_interfaces` Salsa query cuts off body-only changes while changed contracts invalidate importers. AUD.4 keeps IDE analysis aligned and implicit `Destroy` is elaborated before M2 so O006 observes semantic AMIR. BV.2/BV.3 carriers and safe slice/String APIs are integrated in their admitted scope. Full stdlib promotion still depends on allocator, drops, aggregate backing and target proofs; see the memory/stdlib contracts.
- **Also lowered today:** `catch`, unsafe blocks, async blocks and `await` through the existing coroutine model. General lambda/closure execution stays U001. The admitted `std.thread.spawn(move || ...)` shape, with no parameters and owned Send captures, lowers to a capture struct plus `run()` and is not a general closure ABI.

### Implemented analyses and remaining boundaries

Move/borrow checking, catch/unsafe lowering and GenRef insertion are existing
paths, not a future 0.2 IR. `SymbolId` remains `{file_id, local_id}`; document
handles and analysis revisions are distinct identities. Borrowed carriers and
coroutine-relative references do not prove arbitrary FFI or suspension safety.
The [master roadmap](arandu-compiler-roadmap-v0.1.md) owns the remaining gates;
internal IR schema revisions are not language release versions.

---

### 5. Deferred Features

To ensure steady progress and prevent overengineering, several advanced compiler design patterns are deferred to later versions:

1. **Warm CLI service and complete instance granularity**:
   - Salsa queries and IDE snapshots already exist. The CLI service and complete instance-level product gates belong to 0.2/0.3 respectively.
2. **Broader compile-time metaprogramming**:
   - Bounded scalar/aggregate CTFE is implemented. Reflection, generated declarations and executable attributes require separate contracts; see the public comptime contract.
3. **GHC-style Core Rewrite Rules**:
   - *Strategy*: We avoid high-level algebraic optimization rules in favor of direct lowering and backend-level optimizations.
4. **Stable Public AHIR/AMIR API**:
   - *Strategy*: Internal representations remain versioned implementation details even though end-to-end programs already compile. Public compatibility is defined separately from Rust crate/IR internals.

---

### 6. Pretty-Printing Contract

The `arandu hir` subcommand outputs a pretty-printed representation of the AHIR. This format is designed for readability, debugging, and verification via golden tests.

### Contract Rules

1. **Source/AST Order**: The pretty printer outputs declarations and statements in the exact order they appeared in the source file.
2. **Deterministic Output**: For structures without a natural AST order (like member sets or generic associations), elements must be sorted alphabetically by name before printing.
3. **Indentation**: Indentation uses two spaces (`  `) per level to represent hierarchical nesting.
4. **Expression Types**: Every expression must suffix its resolved type (e.g. `LocalRef(a): int` or `Binary(+): int`).

### Pretty-Printing Examples

#### Module

```text
Program
  Module examples.stable.syntax.hello
```

#### Function with Binary Expression

```text
Func add(a: int, b: int) -> int
  Return
    Binary(+): int
      LocalRef(a): int
      LocalRef(b): int
```

#### Structs

```text
Struct User
  id: int
  name: str
  email: str?
```

#### Enums

```text
Enum LoadState
  Idle
  Loading
  Success(User)
  Failed(Err)
```

#### Constants

```text
Const maxRetries: int =
  Int(3): int
```

## PONTOS DE MELHORIA (O que não está no roadmap)

Os itens deferred acima são extensões, não permissão para fundir IRs. Mudanças
devem evitar clones profundos e projeções monolíticas no caminho incremental.

## Futuro e Próximos Passos

Promover novos IR features apenas pelo roadmap mestre, acompanhados de
invariantes, visitors completos e testes de paridade entre backends.
