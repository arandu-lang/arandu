# Arandu — Async Runtime Design (SL_R) v0.1

**Status:** SL_R.0–.3 + SL_R.1 host surfaces implemented, with the payload,
backend and platform limits below. Consumes A3 compiler semantics. Full runtime
parity and the complete Future interface remain open.

| Layer | Owns | Does not own |
|-------|------|----------------|
| **Compiler (A3)** | `async`/`await`, `Coroutine[T]`, `Poll[T]`, suspend CFG | threads, epoll, spawn queues |
| **`std.core.future`** | `Poll` enum | OS reactor |
| **`std.runtime.executor`** | SL_R.0 executor (`SyncExecutor`, `TaskHandle`, `spawn`/`join`/`blockOn`/`cancel`) | OS reactor, wakers, sockets, supervisor |
| **`std.runtime.waker`** | `Waker` / `Context` (async token) | OS reactor, sockets |
| **`std.runtime.reactor`** | OS Reactor (epoll/io_uring) | sockets, supervisor |
| **`std.runtime.supervisor`** | Worker-process isolation/restart | executor, sockets |
| **`std.net`** | Raw + high-level TCP sockets | OS reactor, wakers |

---

## Visão Geral e Contexto

O contrato separa semântica `async` do compilador, tipos fundamentais e runtime
de host, evitando que scheduler ou I/O vazem para a linguagem/IR.

## Detalhes Técnicos da Implementação

O antigo monólito `stdlib/std/runtime.aru` foi quebrado em submódulos
`stdlib/std/runtime/*` (padrão de namespace por diretório, como
`stdlib/alloc/*`). A superfície de mono auxiliar (`*_int` / `*_i64`) foi
removida da API pública: hoje `spawn<T>` / `join<T>` / `blockOn<T>` inferem
`T` de `Coroutine[T]` entre módulos.

### Shipped surfaces

### SL_R.0 — SyncExecutor + Coroutine (`std.runtime.executor`)

```text
spawn<T> / join<T> / block_on<T>   // generic; infers T from Coroutine[T]
cancel<T>
```

ABI: `Coroutine[T]` is a state-blob pointer; `job as ptr[u8]` is the host bridge.

`spawn<T>` returns `TaskHandle<T>` and `join<T>` consumes that type information
to infer/check its result. Requesting `bool` from a handle returned by an `int`
coroutine is a type error. `cancel<T>` accepts the same typed handle. The type
argument adds no runtime field: the handle retains its integer queue ID layout.
Code that explicitly annotated the old bare `TaskHandle` must now supply its
result argument; ordinary inferred spawn/join/cancel calls need no annotation.

This repairs result-type erasure, not the host payload ABI: the cooperative MVP
still transports i64-sized result bits. It is not the generic aggregate-result
transport required for structured parallel jobs. Handles remain copyable and
the existing cancellation/reuse restrictions below continue to apply. The canonical
`TaskHandle<T>` is rejected by the initial `Send`/`Sync` storage proof: an opaque
integer queue ID does not establish a cross-thread ownership contract.

The C backend represents coroutine values as opaque state pointers, consistent
with `LayoutEngine`, and supplies `ar_co_block_on_i64`. When the tasks feature
is live it also emits `ar_rt_spawn_i64`, `ar_rt_join_i64`, and `ar_rt_cancel_i64`.
The joining thread still drives the blob; this is not an OS thread-pool executor.
The task table lock covers registration and retirement and is released while the
blob runs. Coroutine pointer ABI parity still does not by itself establish a
preemptive `std.runtime.executor`.

The host task table distinguishes pending, running and completed tasks. A join
claims the pending blob under the table lock and becomes its sole polling/free
owner. Cancellation of a running task requests retirement after that join;
cancellation of a pending task frees it immediately. A completed live handle
retains its result for a sequential rejoin until cancellation releases its slot.
Cancellation invalidates the handle. Concurrent cancellation must follow the
join's ownership claim; this API does not promise arbitrary concurrent use of
retired/reused handles. Tests coordinate that claim with channels rather than
timing assumptions, and verify slot reuse before/during/after join.

### SL_R.2 / SL_R.3 — Reactor (`std.runtime.reactor`)

- `EpollReactor` + sleep/arm/poll
- `reactor_backend()`: **0** portable, **1** epoll, **2** io_uring (runtime detect)
- Sleep prefers io_uring timeout when backend is 2, else epoll+timerfd

#### Native platform contract

| Operation | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Timer sleep/arm/poll | epoll + timerfd; optional io_uring sleep | portable deadline | portable deadline |
| Direct TCP wait / wait-wake | poll | WSAPoll | poll |
| Register TCP socket with reactor | epoll, one-shot; register again to rearm | unsupported (`-1`) | unsupported (`-1`) |

Socket readiness on the Linux reactor is independent of an armed timer.
Polling dispatches registered socket events to their wakers even when there
is no timer. The poll return value retains the timer contract (1 when the
timer fires, 0 otherwise); socket notification is observed through the waker.
The portable reactor supports timers, not socket registration. Direct TCP
wait remains available on Windows/macOS; it is a distinct operation.

The v0.1.10 Release run
[38779239029](https://github.com/arandu-lang/arandu/actions/runs/38779239029),
verified on 2026-10-15, passed native SDK/VSIX and public archive install/smoke
on Linux x86-64, macOS ARM64 and Windows x86-64, including the CTFE smoke corpus.
These tests exercise selected installed-product flows on all three platforms. They do not imply complete runtime parity or replace the entire
Rust workspace suite. Socket tests allocate ephemeral loopback ports and
fail if networking is unavailable instead of reporting success without
exercising their assertions.

### Waker / Context (`std.runtime.waker`)

- `Waker`, `new_waker`, `waker_wake`, `waker_wait`, `destroy_waker`
- `Context` holds a `Waker` (explicit, no global)

### TCP sockets (`std.net`)

- raw: `tcp_listen` / `tcp_accept` / `tcp_connect` / `tcp_read` / `tcp_write` / close
- safe: `TcpListener.bind` / `TcpStream.connect` / `read` / `write` / `close`

### SL_R.1 — Supervisor (`std.runtime.supervisor`)

- `Supervisor` + `supervisor_spawn(path, max_restarts)` / `poll` / `wait` / `kill`
- Worker processes bound blast radius under process abort policy

---

### Gold decisions (unchanged)

1. **No global executor** — all handles are explicit values.
2. **Runtime backend select** — io_uring when kernel allows, else epoll.
3. **Isolation via processes** — not catch_unwind; supervisor restarts workers.

---

### Honesty

| Item | Status |
|------|--------|
| Multi-file inferred `rt.spawn` / `rt.join` | Done (namespace generic infer + HIR specialized types) |
| Same-module inferred `join_g` mono | Done |
| TCP nonblocking + `tcp_wait` / `tcp_wait_wake` | Done |
| io_uring read/write when backend=2 | Done (`tcp_read_async` / `tcp_write_async`) |
| Full Future trait on Coroutine | Open (Waker/Context handles exist) |

## PONTOS DE MELHORIA (O que não está no roadmap)

O Future trait completo permanece aberto e as superfícies de host não implicam
paridade em todos os targets. Esse limite deve continuar explícito.

## Futuro e Próximos Passos

A fila de ampliação pertence ao roadmap mestre. Promoção Gold exige evidência
específica de scheduler, cancelamento, I/O e shutdown por sistema operacional;
o smoke da distribuição não substitui essa prova.
