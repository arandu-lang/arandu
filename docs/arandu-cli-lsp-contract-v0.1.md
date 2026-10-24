# Arandu CLI and LSP failure contract v0.1

**Status:** S1-D public contract; audited 2026-08-20.

## Visão Geral e Contexto

Este contrato alinha falhas e diagnósticos entre a CLI batch e o LSP
revisionado sem misturar suas políticas de transporte e lifetime.

## Detalhes Técnicos da Implementação

### Failure classes

| Class | Examples | CLI | LSP |
|---|---|---|---|
| Usage | unknown command or flag, missing required argument | message plus exit `2` | JSON-RPC invalid request/method error |
| Source program | parse, name, type, ownership, or documented unsupported feature | rendered coded diagnostic plus exit `1` | diagnostics for the live document/revision only |
| Operational | missing file/manifest/stdlib/toolchain, unreadable path, worker creation failure | contextual error plus exit `1` | request error; server remains alive when its state is valid |
| Compiler invariant | invalid HIR/AMIR, backend contract breach | rendered `ICE-*` plus exit `1`; no artifact | internal-error response; failed snapshot is discarded |
| Program result | return value from a successfully JIT-compiled `main` | `run` forwards the program exit code | not applicable |

Exit `101` belongs to Cargo when Cargo itself fails; it is not an Arandu CLI
exit-code contract. Shells and operating systems may restrict the observable
range of a program return value.

Test harness children return parse/type/AMIR compilation failures through the
terminal IPC event, rather than exiting inside the compiler pipeline. A missing
control frame denotes a process/protocol failure, not an ordinary source error.

Invalid lockfile hashes are never integrity evidence. For a local-only graph,
`arandu update --accept` explicitly regenerates a malformed UTF-8-encoded lockfile and
retains the original bytes in `.arandu/locks/invalid-arandu.lock`. An existing,
different recovery copy is not overwritten. `--locked` forbids recovery, and
remote graphs do not use this local recovery path.

### Command matrix

| Command | Purpose | Backend | Stability | Success |
|---|---|---|---|---|
| `lex`, `parse` | inspect frontend stages | none | development tool | `0` |
| `check` | parse, resolve, and type-check | none | stable project workflow | `0` |
| `hir`, `amir`, `graph` | inspect compiler IR/graphs | none | development tool; textual form is unstable | `0` |
| `fmt` | format source files | none | partial | `0` |
| `run` | execute `main` | Cranelift host JIT | experimental | program return code |
| `emit-c` | emit GNU C source | C | experimental; see backend contract | `0` |
| `build` | publish a host-native package executable | Cranelift AOT object + native linker | stable project workflow | `0` |
| `build --release` | publish a speed-oriented host executable | AMIR O2 + Cranelift AOT `speed` | stable project workflow | `0` |
| `build --size`, `build -Oz` | publish an executable optimized for size | AMIR Os + Cranelift AOT `speed_and_size`, or selected Wasm emission | experimental size profile | `0` |
| `new`, `doctor`, `watch`, `hash-file` | project/tooling operations | varies | partial | `0` |

Backend and cross-target details are defined in
[`arandu-backend-contract-v0.1.md`](arandu-backend-contract-v0.1.md).

The size profile uses `target/size` and its own incremental cache identity.
`--size` and `-Oz` are aliases; combining them with `--release` or `--debug`
is a usage failure. `run` rejects build profiles. Inspection commands select
the AMIR level with the same rule as `build`, so they print what that build
compiles: baseline by default, O1 with `--opt`, O2 with `--release` and Os with
`--size`. `emit-llvm` is always O2, like the LLVM product. `--size` is accepted
by `emit-c`, `emit-wasm` and `emit-component`. Linux native size builds strip symbols, so use a separate debug
build when those symbols are required.

### LSP request and snapshot rules

1. Each semantic job owns an `AnalysisSnapshot`; the main thread owns edits.
2. A result is publishable only while its `DocumentId` is live and its
   `AnalysisRevision` equals the current revision.
3. A stale request receives LSP `ContentModified` (`-32801`), never a successful
   `null` pretending that analysis completed.
4. A worker panic is isolated at the thread/job boundary. The captured snapshot
   is discarded, diagnostics are not published, and a request receives JSON-RPC
   `InternalError` (`-32603`). The same analysis is not resumed.
5. Closing a document removes pending VFS edits so a delayed flush cannot reopen
   it. Invalid URI text is rejected without panic.
6. Salsa cancellation caused by a concurrent input revision is not converted
   into a compiler diagnostic; only analysis from the latest valid revision may
   become visible.

This isolation assumes Rust panic unwinding. A build configured with
`panic=abort`, process corruption, allocation failure, or a foreign unwind can
still terminate the server; the editor may restart it as a new process.

### Evidence and primary references

- [LSP 3.18 specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/)
  defines request responses, cancellation, `ContentModified`, and server
  lifecycle behavior.
- [Rust thread panic result](https://doc.rust-lang.org/std/thread/type.Result.html)
  explicitly permits a thread to act as a subsystem failure-isolation boundary.
- [Rust unwind safety](https://doc.rust-lang.org/std/panic/trait.UnwindSafe.html)
  requires avoiding reuse of state whose invariants may have been interrupted.
- [Cargo exit status](https://doc.rust-lang.org/cargo/commands/cargo-run.html#exit-status)
  separates Cargo's `0`/`101` contract from the executed program's behavior.

Executable evidence includes the LSP worker-survival, closed pending-document,
malformed-URI, stale revision, concurrent Salsa cancellation, and stdlib
resolution tests, plus the CLI smoke and project suites.

## PONTOS DE MELHORIA (O que não está no roadmap)

CLI e LSP ainda compartilham alguns tipos por dependência ampla. DTOs públicos
devem continuar estruturados; nenhuma integração pode parsear texto renderizado
de diagnóstico.

## Futuro e Próximos Passos

Extrair contratos compartilhados somente quando houver segundo consumidor e
preservar testes de erro, stale revision e worker survival.

### Compile-time resource configuration

The global CLI flags `--ctfe-fuel`, `--ctfe-frames`, and `--ctfe-values` accept
positive integers; missing, zero, negative, malformed or overflowing values are
usage failures (exit 2). They apply before analysis, including project and
manifestless commands, and participate in incremental build fingerprints.
LSP reads `initializationOptions.ctfe` with optional `fuel`, `frames`, and
`values` fields. Invalid options return InvalidParams during initialization,
without starting workspace discovery. VS Code supplies these through
`arandu.comptime.*` settings on server start. Resource exhaustion in valid
source configuration remains a structured T045 source diagnostic.
