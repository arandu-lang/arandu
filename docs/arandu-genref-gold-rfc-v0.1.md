# RFC: GenRef Gold v0.1

**Status:** contrato implementado da baseline `Frozen-R0`; governança canônica na [RFC 0001](rfcs/0001-generational-fallback-genref.md). Não autoriza expansão concorrente/FFI desta baseline.

**Supersedes:** the F2.3 i64 MVP contract preserved in Git history

## Visão Geral e Contexto

GenRef é o fallback geracional controlado para referências que precisam
escapar, mas não podem ser provadas como empréstimos locais. Ele preserva a
estratégia stack-first da linguagem: código comprovadamente local não paga por
arena, geração ou contagem de referência escondida. Este RFC registra o
contrato implementado; o planejamento temporário da campanha permanece apenas
no histórico Git.

## Detalhes Técnicos da Implementação

### Contract

Arandu remains stack-first. A reference proven local uses a direct borrow and
emits no generational operation. A representable escaping root uses a typed
GenRef and always emits O004. Returning a borrow from a frame remains O010;
GenRef never hides it. `@NoFallback` and `--no-generational-fallback` turn the
controlled fallback into a hard error before partial lowering.

The logical identity is `{ arena, arena_generation, slot, slot_generation }`.
All zero is invalid. Counters never wrap: exhausted slots and arenas retire.
Handles are opaque, process-local and non-serializable. They are not pointers,
capabilities for FFI, persistent IDs or proof of spatial/alias/thread safety.

### Payload and ownership

Compiler-managed storage receives target-derived size/alignment and optional
drop glue. `Copy` payloads need no glue; non-trivial payloads require one
explicit `@Destructor` specialized for the concrete type. `get` borrows and
cannot escape a projection-only use. `remove` moves ownership. Replacement is
transactional: any reported failure leaves the caller owning the source and
the old slot unchanged. Arena destruction drops only occupied values, once.

Arandu rejects divergent move states with O007, so valid programs do not need
runtime drop flags. Normal returns elaborate `Destroy` for initialized,
available locals. The destructor's `own self` is not recursively destroyed.

### Failure model

The safe arena distinguishes invalid handle, wrong arena, destroyed arena,
stale generation, capacity overflow, allocation failure, invalid layout and
invalid payload pointer. Compiler-inserted dereference traps deterministically;
explicit safe APIs return typed failure. No failure is encoded as a payload
sentinel.

### Observability

O004 contains the source label, AMIR escape path, reason and stack-first
alternatives. The LSP carries structured labels/notes/hints and can insert
`@NoFallback` without parsing diagnostic text. `--genref-report` prints static
promotion/check counts to stderr after querying. `ArenaRegistry::metrics`
provides allocation-free runtime state snapshots.

### Safety boundary

`ArenaRegistry<T>` is safe Rust and deliberately `!Send + !Sync`. No concurrent
surface is promised. Type-erased payload storage contains the narrow unsafe
boundary; its pointer, layout and ownership preconditions are validated before
reads, moves or drops. Miri covers this boundary with strict provenance and
symbolic alignment checks.

The emitted C runtime is a separate unsafe implementation boundary and is
tested with ASan+UBSan. Miri cannot validate C or arbitrary FFI. GenRef has no
stable direct C representation: FFI must use opaque handles and validation
functions or copy values across the boundary.

This contract describes the compiler-managed fallback and `ArenaRegistry`,
not the separate `std.alloc.gen_arena.GenArena<T>` collection. In the local
stdlib, `GenArena.get` copies only `T: Copy`; `getRef` borrows a non-Copy
payload and ties its lifetime to the arena, while `remove` moves it out.
The collection's public slot/generation representation is not the opaque
four-part compiler handle. Retirement, forging protection and thread-safety
claims from `ArenaRegistry` must not be transferred to that collection.

### Reproducible campaigns

```text
cargo test --locked -p arandu_runtime million_cycle_endurance_retires_without_aba
cargo test --locked -p arandu_fuzz_support
cargo +nightly fuzz run --fuzz-dir arandu_fuzz fuzz_genref -- \
  -max_total_time=1800 -max_len=65536
MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check" \
  cargo +nightly miri test --locked -p arandu_runtime genref_payload::tests
CC=gcc ARANDU_C_SANITIZERS=1 \
  cargo test --locked -p arandu_backend_c --test parity_tests \
  c_backend_genref_runtime_executes_beyond_legacy_capacity -- --exact
```

The ordinary workspace suite proves byte-deterministic C emission,
C/Cranelift result and trap parity, target-derived host/i686/pointer-width
layouts, incremental cutoff, diagnostic determinism and zero GenRef operations
for a proven-local borrow. Weekly workflows extend it with bounded libFuzzer,
Miri, ASan and UBSan campaigns.

### Published limits

- thread-confined only; no implicit Send/Sync;
- no direct FFI, persistence or cross-process validity;
- no user API for raw bits, slot access or forced generations;
- projected escaping borrows without a first-class owner/path representation
  are rejected rather than snapshotted;
- target layout tests do not by themselves claim a published native package;
  release support is governed by the release matrix.

## PONTOS DE MELHORIA (O que não está no roadmap)

- O contrato publicado é restrito a uma thread. Uma superfície concorrente
  exigiria uma decisão separada sobre sincronização, ordering e custo; ela não
  deve surgir como `Send`/`Sync` implícito.
- Projeções que escapam continuam rejeitadas quando não existe representação
  de owner/path de primeira classe. Copiar silenciosamente o valor mudaria a
  semântica e não é uma correção aceitável.
- A implementação Rust e o runtime C são fronteiras unsafe independentes. A
  paridade funcional não elimina a necessidade das campanhas Miri e sanitizer.
- As métricas atuais descrevem estado do runtime, mas ainda não formam um
  perfilador de lifetime/escape voltado ao usuário.

## Futuro e Próximos Passos

- Manter fuzzing, Miri e ASan/UBSan nas campanhas periódicas e promover novos
  alvos somente pela matriz de release.
- Integrar relatórios de escape ao tooling de performance quando o profiler da
  linguagem existir, sem inserir telemetria observável em queries Salsa.
- Reavaliar GenRef concorrente ou FFI somente por RFC próprio com modelo de
  memória, ABI, testes de adversário e custo explicitamente definidos.
