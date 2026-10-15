# JIT memory policy (v0.1 / debug Cranelift)

**Status:** caller-owned compiler string buffers coexist with legacy
process-lifetime host results. This is not a garbage-collected runtime.

## Visão Geral e Contexto

A política depende da origem do buffer. `str` é um descritor de ponteiro e
comprimento, sem bit de ownership; sua representação sozinha não autoriza
liberação. Literais, strings materializadas pelo compilador e retornos de hosts
não têm o mesmo contrato.

## Detalhes Técnicos da Implementação

### String storage and release

| Source | Allocator | Lifetime / release |
|--------|-----------|--------------------|
| String literals | module data | until `CompiledModule` drop; never `free` |
| Allocating `ToStr` and `StringInterp` | `malloc` | caller-owned; AMIR emits `Free(str)` for admitted owned temporaries |
| `err.new` | `malloc` | separate error-handle contract; `ToStr(Err)` borrows the same message |
| `fat_str_from_string` | Rust `Box<str>` via `Box::into_raw` | legacy process lifetime; no matching release ABI |
| `ar_str_concat` host | Rust boxed byte slice | legacy process lifetime; distinct from AMIR `StringInterp` |
| `ar_path_join_owned` | vector buffer allocator | owned `String`; `ar_vec_buf_free(ptr, capacity)` |

Drop elaboration deliberately requires unambiguous provenance for string locals
(one root store receiving a fresh owned string). Reassignment, escaping values
and borrowed host results must not be freed merely because they have type `str`.
This table does not claim complete reclamation of every string path.

### Allocation-free formatting and integer concat

The Rust integer helpers format into 20 local bytes before allocating the
caller-owned result. Character conversion uses four UTF-8 bytes, preserving
U+FFFD for invalid scalars. Float conversion writes the existing Rust Display
policy into a bounded 384-byte local buffer; NaN/infinities and integer-looking
values retain their previous spelling, including `-0.0` becoming `0`.
`format_f64_v01` still returns a `String` for callers requiring that public API.
The C backend's existing `%.15g` policy is not made byte-identical to Rust
Display by this optimization.

Cranelift and C share a pure codegen admission proof for integer interpolation
parts. A `ToStr` temporary must have one definition, exactly one use by a later
concat in the same block, and exactly one subsequent `Free`. Any additional
use, store, call argument or terminator argument rejects admission. Backends
capture the numeric value at the original conversion statement, measure its
length without allocation, then write its digits directly into the final concat
buffer. They omit only that proven-private temporary's allocation and free.
The materialized result and its existing ownership/drop remain unchanged.
This handles repeated execution in loops without frame-backed string lifetimes
or a new AMIR variant. Floats and chars inside concat keep ordinary ToStr
buffers; their Rust formatting no longer needs a temporary `String`.

The C fast path also avoids the ordinary varargs helper's heap array of parts.
String bytes and embedded NULs are copied by length; only the final buffer gets
a trailing NUL. Checked length accumulation prevents undersized allocation.

### Measured allocation evidence (2026-10-05, Linux x64)

A native release build of Katu was compared with the integer-stack-helper
baseline, using the same corpus, libc malloc/free interposition, and
byte-for-byte stdout comparison. Package copies were used for both builds.

| Corpus | Matches | Baseline malloc / free | Fused malloc / free | Live balance (both) |
|--------|---------|------------------------|---------------------|---------------------|
| hit | 50 | 148 / 136 | 98 / 86 | 12 |
| m10 | 10 | 66 / 54 | 56 / 44 | 12 |
| m100 | 100 | 249 / 237 | 149 / 137 | 12 |
| nom | 0 | 43 / 32 | 43 / 32 | 11 |

All four stdout streams were identical. Each numeric match line loses one
allocation/free pair: two allocations become one final concat allocation.
The no-match baseline is unchanged; the live balance does not grow with matches.
Katu's eight tests and Ita's seven tests passed with the new compiler. Native
Ita output also matched `sha256sum` byte-for-byte on five inputs, including
binary content.

A standalone emitted-C loop printing `line:${i}` for 1,000 integers was built
with `cc -O2 -fwrapv`. Counts fell from 3,001 malloc / 3,000 free to 1,001 /
1,000, with identical stdout and a constant live balance of one (stdout
buffer). The C path removes both the ToStr buffer and the varargs parts array.

A separate C driver linked each Rust runtime archive and called the float or
character helper 110,000 times, freeing every result and hashing all bytes.
For both conversion workloads malloc/free counts fell from 220,001 / 220,000
to 110,001 / 110,000; the one remaining allocation is the driver's stdout
buffer. Hashes were identical. The float workload cycled zero/signed zero,
fractions, 1e15, tiny values, extrema, infinities and NaN; the char workload
cycled NUL, ASCII, two/three/four-byte UTF-8 and an invalid scalar.

Without allocator interposition, 15 alternating before/after samples gave
median 427.53 → 334.89 ns/call for float and 202.63 → 107.05 ns/call for char,
including byte hashing and release. These are local development-runtime
microbenchmarks, not a claim about end-to-end release throughput.

### Host-result ownership audit

`fat_str_from_string` is used by legacy path join/file-name hosts,
`ar_str_split_last`, and the testing sandbox-path host. Public path wrappers
return borrowed `str`/`Path` descriptors; `PathBuf` has no buffer destructor.
These calls do not register their result as an owned ToStr/interpolation temp.
The sandbox's directory cleanup does not reclaim its boxed path spelling.
`ar_str_concat` constructs a boxed byte slice independently of this helper.

Do not pair these pointers with `ar_rt_raw_free`: a Rust allocator is not
contractually interchangeable with libc `malloc`, and empty boxed strings may
use a dangling non-null pointer rather than a freeable allocation. Reclamation
requires an explicit owned ABI with allocator and capacity metadata, or a
matching Rust release operation plus a proven language owner. The existing
`path.joinOwned` provides the explicit owned alternative for joins. Returning a
borrowed slice for file names or split results would additionally require
borrow-provenance/lifetime analysis; do not silently change those hosts.

### Other backing and ABI

Aggregate backing is governed by its separate codegen storage policy; frame
promotion and invocation-owned scratch must not be conflated with string
ownership. `T?` remains a null-or-pointer handle, so scalar payload `0` differs
from `nil`. Explicit frame budgeting excludes register-allocation spills.
Cranelift x64 0.136.1 sizes integer spills by register class in eight-byte units
and spills using the canonical I64 type. Frame/cache effects require measurement;
no local change to upstream spill-slot sizing is made here.

### Strings independent of invocation scratch

The shared MIR storage policy admits a `str` result only when all its producers
forward static literal-pool data or a caller parameter. An unprojected local
load also qualifies when every store has such provenance and its descriptor
address is never exposed. The proof uses a fixed depth bound of 64 and rejects
cycles, block parameters, missing definitions, projected stores, destruction,
opaque calls and locally derived views. A successful proof allows invocation
scratch to be reclaimed without reclaiming the returned string's backing.

Native regressions cover literal early/normal returns and a forwarded caller
string at O0/O1/O2, including aggregates larger than the frame budget. They
require balanced scratch allocations/frees independent of loop iterations.
The corresponding Wasmtime regression checks repeated calls and stable linear
memory size. This is a narrow backing proof; unknown retention, borrowed views,
cyclic aggregate phis and suspended aggregate payloads retain their guards.

### C backend

Generated C mirrors the formatting/concat ownership contract and emits its own
allocator calls. Native AOT links the same exported Rust digit-writing helpers
that the JIT registers. Neither backend introduces an arena, refcounting, or
multiple writes to replace a single `io.println` call.

## PONTOS DE MELHORIA (O que não está no roadmap)

Legacy borrowed host results can accumulate process-lifetime allocations. Their
owned replacements must be designed and measured separately from interpolation
churn. The raw `str` representation also limits path-sensitive local destruction.

## Futuro e Próximos Passos

Further concat fusion for floats or other values must preserve formatting,
evaluation order and independent uses. A lifetime redesign or module arena
requires a representative workload and an explicit ownership contract.
