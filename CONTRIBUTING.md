# Contributing to Arandu

Thank you for your interest in contributing to Arandu! We welcome contributions of all forms, including bug reports, feature requests, documentation improvements, and code changes.

Arandu is an experimental Brazilian systems programming language focused on memory safety, clean syntax, explicit errors, and native tooling.

---

## Getting Started

### Prerequisites

To build and test Arandu, install `rustup`. The repository pins the exact
verified toolchain in `rust-toolchain.toml`:

- **Verified Rust:** Rust 1.97.1 with `rustfmt` and Clippy. Rustup selects it automatically.
- **MSRV:** not promised yet; do not infer it from the verified development toolchain.

### Setup and Compilation

1. Clone the repository:
   ```bash
   git clone https://github.com/arandu-lang/arandu.git
   cd arandu
   cargo check --workspace --locked
   ```

2. Build the workspace:
   ```bash
   cargo build --workspace --locked
   ```

3. Run the test suite:
   ```bash
   cargo test --workspace --locked
   ```

4. *(Optional)* Enable repository Git hooks and `git blame` ignore list:
   ```bash
   git config core.hooksPath .githooks
   git config blame.ignoreRevsFile .git-blame-ignore-revs
   ```

---

## Coding Standards & Guidelines

- **Formatting**: Always run `cargo fmt --all -- --check` before submitting a pull request.
- **Clippy**: Run the same warning gate as CI:
   ```bash
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   ```
- **No `unwrap` / `expect` in library production code** (`crates/*/src`, excluding `#[cfg(test)]`):
  - Prefer `Result<T, Diagnostic>` (or `thiserror` mapped to `Diagnostic` at the CLI/LSP edge).
  - Compiler invariant breaks → `Diagnostic::ice(DiagCode::…)` (reportable ICE), **not** `panic!`/`expect`.
  - Writing into a `String` buffer: `let _ = write!(…)` or propagate `fmt::Result` — never `.unwrap()` on format.
  - `Cursor::expect` in the hand-parser is a **`Result`**, not `Option::expect` — that pattern is fine.
  - Allowed only in tests and, as a last resort, binary entrypoints with a clear user-facing message.
- **No deep-clone of heavy IR on hot paths**:
  - Salsa memos already wrap payloads in `HashEq` → `Arc<T>`. Prefer `Arc::clone` / `HashEq::share`.
  - Do **not** write `(*hash_eq).clone()` or `program.as_ref().clone()` for `Program` / `AmirProgram`.
  - Mutate shared data with `Arc::make_mut` or `Arc::unwrap_or_clone` only when ownership is required (e.g. `--opt`).
- **Casing Rules**: Arandu uses strict casing rules to distinguish values and types:
  - **Values & Functions**: `camelCase` (e.g. `userName`, `totalPrice`, `parseJson`).
  - **Types & Structs**: `PascalCase` (e.g. `User`, `HttpClient`).
  - **Enum Variants**: `PascalCase` (e.g. `Ok`, `Err`, `Loading`).

Inventory helper (optional):

```bash
./scripts/count_unwrap_clone.sh
```

---

## Adding or Modifying Diagnostics

Arandu enforces a strict **1-to-1 bijection** between declared compiler diagnostic codes and their detailed markdown documentation. If you add a new diagnostic code, you **must** also add its corresponding documentation file, or the workspace build will fail.

### Step-by-Step Guide to Add a Diagnostic

1. **Declare the Code in `arandu_diagnostics`**:
   Add your new diagnostic variant to the `DiagCode` enum in [`crates/arandu_diagnostics/src/lib.rs`](crates/arandu_diagnostics/src/lib.rs):
   ```rust
   pub enum DiagCode {
       // ...
       T026CannotAssignImmutable, // New code
   }
   ```
   And map it to its string representation (e.g. `"T026"`) in the `as_str` method.

2. **Catalog in `SPEC.md`**:
   Document the code, its default message template, and its compiler version inside [`docs/diagnostics/SPEC.md`](docs/diagnostics/SPEC.md).

3. **Create the Documentation File**:
   Create a detailed documentation file under `docs/errors/<code>.md` (e.g. `docs/errors/T026.md`) written in **English**. 
   Ensure it includes:
   - An explanation of the diagnostic.
   - An **Erroneous Code Example** using ` ```arandu ` blocks.
   - A **Semantic Explanation** of the error.
   - A **How to Fix** section with a corrected code example.

4. **Verify the Bijection**:
   Run the canonical xtask check:
   ```bash
   cargo run --locked -p xtask -- check-diag-docs
   ```
   This verifies that every diagnostic code matches a documentation file, and vice-versa.

---

## Testing Contributions

Arandu relies on various test types:
- **Unit & Integration Tests**: Standard cargo test files found inside crate `tests/` directories.
- **Golden Tests**: Tests that compare the output of parser/AST/AMIR lowering stages against stable snapshots (`.hir`, `.amir`, `.diag` files).
  To update golden tests after making valid parser/compiler modifications, run:
  ```bash
  UPDATE_GOLDEN=1 cargo test --workspace --locked
  ```

Thank you again for contributing to Arandu!
