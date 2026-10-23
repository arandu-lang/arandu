# Arandu — Async Runtime Design (SL_R) v0.1

**Status:** SL_R.0–.3 + SL_R.1 host surfaces implemented, with the payload,
backend and platform limits below. Consumes A3 compiler semantics. Full runtime
parity and the complete Future interface remain open.

| Layer                        | Owns                                                                              | Does not own                            |
| ---------------------------- | --------------------------------------------------------------------------------- | --------------------------------------- |
| **Compiler (A3)**            | `async`/`await`, `Coroutine[T]`, `Poll[T]`, suspend CFG                           | threads, epoll, spawn queues            |
| **`std.core.future`**        | `Poll` enum                                                                       | OS reactor                              |
| **`std.runtime.executor`**   | SL_R.0 executor (`SyncExecutor`, `TaskHandle`, `spawn`/`join`/`blockOn`/`cancel`) | OS reactor, wakers, sockets, supervisor |
| **`std.runtime.waker`**      | `Waker` / `Context` (async token)                                                 | OS reactor, sockets                     |
| **`std.runtime.reactor`**    | OS Reactor (epoll/io_uring)                                                       | sockets, supervisor                     |
| **`std.runtime.supervisor`** | Worker-process isolation/restart                                                  | executor, sockets                       |
| **`std.net`**                | Raw + high-level TCP sockets                                                      | OS reactor, wakers                      |

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
- `reactor_backend()`: **0** portable, **1** epoll, **2** io_uring (runtime
  detect), **3** kqueue, **4** WSAPoll
- Sleep prefers io_uring timeout when backend is 2, else the backend timer
- Code: `crates/arandu_runtime/src/reactor_runtime/{mod,kqueue,wsapoll}.rs`

#### Native platform contract

| Operation                        | Linux                                    | macOS / FreeBSD / OpenBSD / NetBSD / DragonFly                                         | Windows                                   | Other hosts                                 |
| -------------------------------- | ---------------------------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------- | ------------------------------------------- |
| Timer sleep/arm/poll             | epoll + timerfd; optional io_uring sleep | `EVFILT_TIMER` `EV_ONESHOT` (ms)                                                       | deadline carried in the `WSAPoll` timeout | deadline + sleep                            |
| Direct TCP wait / wait-wake      | poll                                     | poll                                                                                   | WSAPoll                                   | —                                           |
| Register TCP socket with reactor | epoll, `EPOLLONESHOT`                    | kqueue `EVFILT_READ`/`EVFILT_WRITE`, `EV_ONESHOT` + sibling filter deleted on dispatch | WSAPoll, one-shot enforced in the runtime | fails closed: `REGISTER_UNSUPPORTED` (`-2`) |

The contract is the same on every backend:

- Registration is one-shot per socket: the first matching event (including
  error/hangup) wakes the waker once; register again to rearm. Re-registering
  replaces the interest set and the waker.
- Readiness does not depend on an armed timer. `poll` returns 1 only when the
  armed timer fired, 0 otherwise (including "only sockets woke") and -1 on an
  invalid handle or OS failure; socket notification is observed via the waker.
- Reactor ids are generation-tagged like TCP and waker handles: the first id
  of a slot equals its index, and destroy bumps the generation, so a stale id
  is rejected instead of aliasing a newer reactor. Destroy releases the OS
  objects exactly once; double destroy is a no-op.
- Closing a registered socket is safe. epoll and kqueue drop the kernel
  registration on close; WSAPoll registrations are keyed by the
  generation-tagged socket id and revalidated on every poll.
- `register_socket` returns `-1` for a stale reactor/socket id or OS failure.
  `-2` means only "this host has no readiness backend".

`events = 0` registers no interest. epoll still reports `EPOLLERR`/`EPOLLHUP`
for such a registration; kqueue and WSAPoll report nothing. No caller in the
stdlib registers with an empty mask.

#### ST.13 — backend choice and prior art

**kqueue.** Matches epoll most closely. Differences handled in
`kqueue.rs`: one-shot is per filter rather than per fd, so dispatch deletes
the sibling filter and ignores a second event for the same fd in one batch.
Re-arming the timer deletes the old knote first, which also drops an
undelivered expiry, like closing the old timerfd on Linux. `EVFILT_TIMER` gets
at least 1 ms because some BSDs reject a zero period; a 0 ms timer is still
reported on the next poll because `poll` checks the stored deadline at entry.
The kqueue fd gets `FD_CLOEXEC` through `fcntl`, since `kqueue1` is not
available on every target. macOS may coalesce timers (leeway), so wake-ups
can be late but never early.

**Windows: WSAPoll.** We weighed three designs:

1. _AFD poll over IOCP_, used by wepoll, mio/tokio, libuv `uv_poll` and
   smol `polling`. It scales best and gives epoll-like semantics. It also
   depends on the undocumented `\Device\Afd` driver and
   `IOCTL_AFD_POLL` (0x12024), on base-handle lookup (`SIO_BASE_HANDLE`,
   which breaks under LSPs), and on careful cancel/re-submit handling when a
   registration changes. Its documented successor,
   `ProcessSocketNotifications`, needs Windows build 20348+, so Windows 10
   clients would still need a second backend.
2. _IOCP completion model_, used by Go netpoll and libuv TCP handles. The
   runtime owns overlapped operations and buffers. That conflicts with this
   ABI, where the caller is woken and then calls nonblocking
   `ar_rt_tcp_read`/`write` itself, and it would change I/O semantics on
   Windows only.
3. _WSAPoll_: documented, level-triggered, with no extra `windows-sys`
   features. Zig's `std.Io.Threaded` likewise stays on plain Winsock calls.
   One-shot is enforced in user space by disarming on dispatch.

We chose WSAPoll. Its known defect, failed nonblocking `connect` not reported
before Windows 10 2004 (Microsoft bug 309411, "won't fix", the reason curl
dropped WSAPoll in 2012), cannot be reached through this ABI:
`ar_rt_tcp_connect` is a blocking connect that reports failure itself. A
future nonblocking connect must either check `SO_ERROR` after a timeout or
move Windows to AFD.

To avoid handle-reuse races, registrations are keyed by the generation-tagged
TCP id, not by the `SOCKET` value. Each poll revalidates the id, drops
registrations of closed sockets, and holds a lease (`SocketLease`, an `Arc` on
the socket slot) across `WSAPoll`, so `closesocket` is deferred until the wait
returns and the value cannot be recycled mid-wait. The lease does not take the
socket mutex, so a blocking read on another thread never stalls the reactor.
WSAPoll waits in scheduler ticks (about 15.6 ms by default; the runtime does
not call `timeBeginPeriod`). The remaining wait is rounded up, and an early
return without events goes back to waiting instead of reporting a timeout.

**Limits.** WSAPoll costs O(registrations) per poll. A reactor cannot be woken
from another thread (there is no `notify` ABI), on every backend. A `poll(-1)`
with sockets but no timer blocks until readiness, and `destroy` from another
thread waits for it. Only TCP ids can be registered, not UDP ids. Promotion
to Gold requires native macOS/Windows CI runs of the `readiness` tests in
`reactor_runtime/mod.rs`. The BSDs other than macOS are compile-gated by
`cfg` but are not built in CI.

Sources:
[WSAPoll](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsapoll),
[WSAPoll is broken (curl)](https://daniel.haxx.se/blog/2012/10/10/wsapoll-is-broken/),
[Winsock socket state notifications](https://learn.microsoft.com/en-us/windows/win32/winsock/winsock-socket-state-notifications),
[wepoll](https://github.com/piscisaureus/wepoll),
[smol `polling` IOCP notes](https://github.com/smol-rs/polling/blob/master/src/iocp/mod.rs),
[Adventures with \Device\Afd](https://lenholgate.com/blog/2023/04/adventures-with-afd.html),
[Zig std.Io Windows networking (PR 31089)](https://github.com/ziglang/zig/pull/31089).

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

| Item                                           | Status                                                 |
| ---------------------------------------------- | ------------------------------------------------------ |
| Multi-file inferred `rt.spawn` / `rt.join`     | Done (namespace generic infer + HIR specialized types) |
| Same-module inferred `join_g` mono             | Done                                                   |
| TCP nonblocking + `tcp_wait` / `tcp_wait_wake` | Done                                                   |
| io_uring read/write when backend=2             | Done (`tcp_read_async` / `tcp_write_async`)            |
| Reactor socket registration: kqueue (macOS/BSD), WSAPoll (Windows) | Implemented; cross-compiled here, native runs pending on CI runners |
| Full Future trait on Coroutine                 | Open (Waker/Context handles exist)                     |

## PONTOS DE MELHORIA (O que não está no roadmap)

O Future trait completo permanece aberto e as superfícies de host não implicam
paridade em todos os targets. Esse limite deve continuar explícito.

## Futuro e Próximos Passos

A fila de ampliação pertence ao roadmap mestre. Promoção Gold exige evidência
específica de scheduler, cancelamento, I/O e shutdown por sistema operacional;
o smoke da distribuição não substitui essa prova.
