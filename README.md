# Arandu

<p align="center">
  <strong>A safe, fast, and ergonomic systems programming language.</strong>
</p>

<p align="center">
  <a href="https://github.com/arandu-lang/arandu/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/arandu-lang/arandu/ci.yml?branch=main&label=CI%20Gate&style=flat-square" alt="CI Gate"></a>
  <a href="https://github.com/arandu-lang/arandu/releases/latest"><img src="https://img.shields.io/github/v/release/arandu-lang/arandu?label=release&style=flat-square" alt="Latest Release"></a>
  <a href="https://arandu-lang.dev"><img src="https://img.shields.io/badge/docs-arandu--lang.dev-207080?style=flat-square" alt="Documentation"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=flat-square" alt="License"></a>
</p>

<p align="center">
  <a href="https://arandu-lang.dev">Website</a> •
  <a href="#overview">Overview</a> •
  <a href="#key-features">Key Features</a> •
  <a href="#a-quick-taste-of-arandu">A Quick Taste</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#command-line-interface">CLI</a> •
  <a href="#compiler-architecture">Architecture</a> •
  <a href="#roadmap--rfcs">Roadmap & RFCs</a> •
  <a href="#contributing">Contributing</a>
</p>

---

## Overview

**Arandu** is a modern systems programming language engineered for developers who demand deterministic control over memory, execution, and machine code without sacrificing ergonomic syntax or understandable compiler diagnostics.

Arandu uses a **hybrid memory model**: static ownership (`own`) and borrowing (`ref`), complemented by a thread-confined generational fallback runtime (**GenRef**). Supported safe surfaces combine compile-time checks and explicit runtime validation without tracing garbage collection. Unsafe code, FFI and residual aggregate representations have separate contracts and limitations.

### Why Arandu?

- **Ownership and Explicit Fallback Without Tracing GC**: Values have owners and borrowed references. Representable escaping roots can use typed GenRef handles with runtime validation and deterministic cleanup; unsupported projections or borrows are rejected. This is not a universal zero-cost or lifetime-recovery guarantee.
- **Incremental by Design**: Built from the ground up on Rowan concrete syntax trees (CST) and Salsa tracked queries. Item-level queries preserve early-cutoff when dependencies are unchanged. CLI builds still create a fresh database; warm-service latency is a future milestone with a defined benchmark gate.
- **Empathetic Developer Experience**: Compiler diagnostics (via Miette and LSP) explain *what* failed, *why* it failed with precise source labels, and *how* to resolve it, linked to documented explanation pages for every error code.
- **Three Code Generation Backends**: Cranelift JIT and native compilation (`arandu run`, `arandu build`), portable GNU C emission (`arandu emit-c`), and WebAssembly modules and components (`arandu emit-wasm`, `arandu emit-component`).

---

## Key Features

| Pillar | Description |
| :--- | :--- |
| **Memory Safety Without GC** | Single-owner linear types (OSSA), borrowed references (`ref T`), and thread-confined generational references (**GenRef**). |
| **Incremental Salsa Queries** | CST-first incremental re-parsing and fine-grained Salsa caching preserve query early-cutoff across edits. |
| **Human-Centric Diagnostics** | Structured error codes (`LX*`, `P*`, `N*`, `T*`, `O*`, `W*`), secondary context labels, and suggested quick fixes. |
| **Native, C and WebAssembly** | Cranelift JIT/native objects, portable GNU C99/C11, and WebAssembly with a Component Model adapter; supported surfaces are defined in the [backend contract](docs/arandu-backend-contract-v0.1.md). |
| **Deterministic Compile-Time Evaluation** | Bounded `comptime` expressions and blocks, frozen Copy aggregates, static array repetition, and target-aware `mem.sizeOf<T>()` / `mem.alignOf<T>()`; see the [comptime contract](docs/arandu-comptime-core-v0.1.md). |
| **Batteries-Included Tooling** | Integrated package management (`arandu new`), dependency verification, code formatter (`arandu fmt`), and system doctor. |
| **First-Class Editor Support** | Full-featured Language Server Protocol (LSP) with type-aware semantic tokens, hover, completions, and code actions. |

---

## A Quick Taste of Arandu

Here is an example demonstrating structs, explicit ownership transfer, borrowing, and standard I/O:

```arandu
module app.main

import io

struct Message {
    text: str
    priority: int
}

// Borrowing: `msg` is lent temporarily; the caller retains ownership
func inspect(msg: ref Message): bool {
    return msg.priority > 0
}

// Ownership transfer: `msg` is consumed and freed by the callee
func send(msg: own Message) {
    io.println("Sending: ${msg.text} (priority: ${msg.priority})")
}

func main(): int {
    let msg = Message {
        text: "Hello from Arandu!",
        priority: 1,
    }

    if inspect(ref msg) {
        send(msg) // ownership transferred here
    }

    return 0
}
```

### Compiler Diagnostics in Action

When memory safety invariants or ownership rules are violated, the compiler provides clear and actionable feedback:

```text
error[O001]: use of moved value `msg`
  --> src/main.aru:25:17
   |
24 |         send(msg)
   |              --- value moved here
25 |         inspect(ref msg)
   |                 ^^^^^^^ value borrowed here after move
   |
   = note: `Message` is an owned type; passing it to `send(own Message)` moved ownership
   = help: borrow the value instead using `ref msg` or clone it before moving
```

---

## Quick Start

### Installation

Install the official Arandu SDK with a single command:

#### Linux & macOS
```bash
curl -sSf https://arandu-lang.dev/install | sh
```

#### Windows (PowerShell)
```powershell
irm https://arandu-lang.dev/install.ps1 | iex
```

Pre-compiled release binaries and cryptographic signatures (`SHA256SUMS`, `.blake3`, GitHub attestations) are available on [GitHub Releases](https://github.com/arandu-lang/arandu/releases).

### Create Your First Project

```bash
# 1. Initialize a new binary project
arandu new hello_arandu
cd hello_arandu

# 2. Run immediately via Cranelift JIT
arandu run

# 3. Type-check and validate without code generation
arandu check

# 4. Compile an optimized native executable
arandu build --release

# Or optimize for size and strip native symbols
arandu build --size
```

---

## Command-Line Interface

The `arandu` CLI provides a unified toolchain for compiling, testing, and managing packages:

```text
The Arandu Programming Language Compiler

Usage:
  arandu <COMMAND> [OPTIONS] [PACKAGE-PATH | FILE]
  arandu [OPTIONS] <FILE.aru>

Build & Execution:
  run        Compile and execute a package or file via Cranelift JIT
  build      Compile package to native executable or library
  check      Type-check and validate without code generation
  test       Execute unit and integration test suites
  bench      Run benchmarks and compare baseline metrics

Project & Packaging:
  new        Create a new Arandu project directory
  init       Initialize an Arandu package in current directory
  watch      Watch filesystem and re-check package incrementally
  clean      Remove project build artifacts and scratch cache
  doc        Generate package documentation (html, md, json)

Dependencies & Verification:
  tree       Display canonical resolved dependency graph
  audit      Audit locked provenance and security policies
  vendor     Create verified offline source snapshot
  verify     Verify offline cache integrity against lockfile
  update     Review and update dependency locks (--accept)

Plumbing & Inspection:
  lex        Dump concrete syntax tokens
  parse      Dump concrete syntax tree (Rowan CST / AST)
  hir        Dump High-Level Intermediate Representation
  amir       Dump Arandu Mid-Level IR (SSA/OSSA)
  graph      Emit module dependency or control flow graph
  emit-c     Emit portable C source code
  emit-wasm  Emit a WebAssembly module
  emit-component  Emit a WebAssembly component
  fmt        Format source files according to style rules
  doctor     Inspect toolchain, environment, and stdlib paths
  cache      Inspect, prune, and verify compiler cache
  hash-file  Compute BLAKE3 checksum for packaging

Target & Toolchain Options:
  --release              Enable speed optimizations (Cranelift + AMIR O2)
  --size, -Oz            Optimize for size (AMIR Os; strip native symbols)
  --stdlib-path <DIR>    Override path to standard library
  --cache-dir <DIR>      Override compiler cache directory
  --layout <MODEL>       Data layout model: host, ptr4, ptr8, i686 (default: host)
  --vcs <VCS>            VCS for new projects: auto, git, none (default: auto)
  -v, --verbose          Enable detailed progress and timing logs
  -V, --version          Print compiler version and exit
  -h, --help             Print this help message

Generational Memory Safety:
  --no-generational-fallback  Promote fallback warnings (O004) to compile-time errors
  --genref-report             Emit per-function GenRef check and allocation statistics

Developer & Debug Flags (-Z):
  -Ztime-passes          Measure and display pass execution timings
  -Zprofile-queries      Profile Salsa incremental semantic query costs
  -Zdump-mir             Dump intermediate AMIR between optimization passes
  -Zdebug-parser         Trace Rowan CST parsing steps
  -Zdebug-typeck         Trace bidirectional type inference & constraints
  -Zdebug-ossa           Trace ownership SSA generation and joins
  -Zdebug-all            Enable all compiler debug traces
```

`--release` and `--size` select different optimization profiles. AMIR O2 remains experimental; selecting release does not promote every optimization to a stable guarantee. Size builds use
an isolated `target/size` cache; a smaller result depends on the workload. See
[RFC 0027](docs/rfcs/0027-zero-bloat-binary-diet-and-on-demand-runtime.md).

Compile-time values can initialize fixed arrays without a runtime calculation:

```arandu
import std.core.mem as mem

func main(): int {
    let count = comptime (2 + 2)
    let flags = [false; comptime (2 + 2)]
    let width = mem.sizeOf<u32>()
    let alignment = mem.alignOf<u32>()
    return 0
}
```

Array repetition evaluates its initializer once and requires Copy elements when
the length exceeds one. Layout intrinsics use the selected compilation target.
The earlier `@sizeOf(Type)` / `@alignOf(Type)` expressions remain supported for
compatibility; new code should use `std.core.mem`. CTFE rejects runtime captures
and effects, and enforces resource limits rather than silently falling back to
runtime evaluation.

---

## Compiler Architecture

Arandu is structured as an incremental, query-driven compilation pipeline:

```mermaid
flowchart TD
    Source["Source Code (.aru)"] --> Lexer["arandu_lexer"]
    Lexer --> Parser["arandu_parser<br/>(Rowan CST)"]
    Parser --> AST["AST Lowering"]
    AST --> Salsa["arandu_query<br/>(Salsa Database)"]
    Salsa --> Resolve["arandu_resolve<br/>(Name Resolution)"]
    Salsa --> Typeck["arandu_typeck<br/>(Type Inference & Checking)"]
    Resolve --> AHIR["AHIR<br/>(High-Level IR)"]
    Typeck --> AHIR
    AHIR --> MIR["arandu_mir<br/>(OSSA & Definite Init)"]
    MIR --> AMIR["AMIR<br/>(Mid-Level IR / CFG)"]
    AMIR --> Cranelift["arandu_backend_cranelift<br/>(Cranelift JIT & Native)"]
    AMIR --> CBackend["arandu_backend_c<br/>(Portable GNU C)"]
    AMIR --> WasmBackend["arandu_backend_wasm<br/>(WebAssembly & Components)"]
```

### Workspace Structure

| Area | Crates | Responsibilities |
| :--- | :--- | :--- |
| **Frontend** | [`arandu_base`](crates/arandu_base)<br>[`arandu_lexer`](crates/arandu_lexer)<br>[`arandu_parser`](crates/arandu_parser)<br>[`arandu_diagnostics`](crates/arandu_diagnostics) | Lexer, Rowan CST-first parsing, token streams, diagnostic definitions (`DiagCode`), and Miette report rendering. |
| **Middle-end** | [`arandu_middle`](crates/arandu_middle)<br>[`arandu_resolve`](crates/arandu_resolve)<br>[`arandu_typeck`](crates/arandu_typeck)<br>[`arandu_semantics`](crates/arandu_semantics)<br>[`arandu_mir`](crates/arandu_mir) | Name resolution, bidirectional type inference, generic monomorphization, OSSA, borrow checking, and AMIR optimizations. |
| **Incremental** | [`arandu_query`](crates/arandu_query) | Sole owner of Salsa database state, tracking query inputs, item-level hashes, and early-cutoff caching. |
| **Backends** | [`arandu_backend_cranelift`](crates/arandu_backend_cranelift)<br>[`arandu_backend_c`](crates/arandu_backend_c)<br>[`arandu_backend_wasm`](crates/arandu_backend_wasm)<br>[`arandu_codegen`](crates/arandu_codegen)<br>[`arandu_runtime`](crates/arandu_runtime) | Cranelift JIT/native emission, portable GNU C, WebAssembly modules/components, and runtime support. |
| **Tooling & IDE** | [`arandu_ide`](crates/arandu_ide)<br>[`arandu_web`](crates/arandu_web)<br>[`arandu_cli`](crates/arandu_cli)<br>[`arandu_lsp`](crates/arandu_lsp)<br>[`arandu_fmt`](crates/arandu_fmt)<br>[`arandu_doc`](crates/arandu_doc) | Compiler CLI, Language Server (`arandu-lsp`), pure CST formatter, and documentation extractor. |
| **Test & Infra** | [`arandu_test_support`](crates/arandu_test_support)<br>[`arandu_fuzz_support`](crates/arandu_fuzz_support)<br>[`xtask`](xtask) | Golden test runners, grammar fuzzing harness, release contract verification, and architecture invariants. |

---

## Editor & IDE Support

Official support for Visual Studio Code is provided in the [`editors/vscode`](editors/vscode) directory:

- **Syntax Highlighting**: TextMate grammar and Rowan CST-backed classification.
- **Language Server**: Backed by `arandu-lsp` with:
  - Type-aware semantic token highlighting
  - Hover documentation with rendered function signatures
  - Go to Definition and Find References across modules
  - Document & workspace symbol search
  - Diagnostic squiggles with quick-fix code actions
  - Formatting on save via `arandu_fmt`

---

## Building from Source

### Prerequisites

- **Rust toolchain**: Exactly Rust 1.97.1 (managed automatically via `rustup` and `rust-toolchain.toml`).
- **C Compiler**: GCC or Clang (required for linking native binaries and C backend verification).

### Build Commands

```bash
# Clone the repository
git clone https://github.com/arandu-lang/arandu.git
cd arandu

# Build the release compiler binary
cargo build --release --locked -p arandu_cli

# Source builds use the arandu_cli binary; SDK packages also provide arandu
./target/release/arandu_cli --version
```

### Invariant Verification Suite

Every change to the compiler must satisfy the canonical validation suite:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo run --locked -p xtask -- check-diag-docs
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo run --locked -p xtask -- check-architecture
cargo run --locked -p xtask -- check-line-endings
```

---

## Roadmap & RFCs

The published baseline is **0.1.10**. The [master roadmap](docs/arandu-compiler-roadmap-v0.1.md)
is the single source for implemented scope, gaps and release gates:

| Series | Planned product milestone |
| --- | --- |
| 0.1.11 | Authorized stdlib closure and correctness fixes; in development, gated by the master roadmap |
| 0.2.x | Warm compiler service, recoverable dev image, measured p95 ≤100 ms on a reference workload/host |
| 0.3.x | Instance-level incrementality; closures and further staging under separate contracts |
| 0.4.x | Measured release optimization; optional LLVM only if its proposal is accepted and validated |
| 0.5.x–0.6.x | Portable ABI/runtime/interop, tooling and sustainable cache storage |
| 0.7.x–0.9.x | Contract convergence, endurance, migration and release candidates |
| 1.0 | Explicitly stable public contract and supported platform matrix |
| 1.x and later | Compatible evolution; breaking changes require a separately approved contract and migration |

These are dependency-based plans, not delivery dates. GPU, eBPF, MLIR,
self-hosting and the full scientific ecosystem are independent proposals.


- **Master Compiler Roadmap**: [docs/arandu-compiler-roadmap-v0.1.md](docs/arandu-compiler-roadmap-v0.1.md) — The single authoritative execution queue.
- **RFC Catalog**: [docs/rfcs/](docs/rfcs/) — Formal architectural specifications (RFC 0000 through RFC 0037).
- **Diagnostic Catalog**: [docs/errors/](docs/errors/) — Detailed guides and solutions for every user-facing compiler diagnostic.
- **Documentation Hub**: [docs/README.md](docs/README.md) — Map of all technical specifications and architecture contracts.

---

## Contributing

We welcome contributions to Arandu! Please read our [CONTRIBUTING.md](CONTRIBUTING.md) guide and [AGENTS.md](AGENTS.md) for architectural invariants before submitting pull requests.

All discussions, bug reports, and feature proposals take place on [GitHub Issues](https://github.com/arandu-lang/arandu/issues) and [GitHub Discussions](https://github.com/arandu-lang/arandu/discussions).

---

## License

Arandu is dual-licensed under:

- **MIT License** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))

You may choose either license at your option.
