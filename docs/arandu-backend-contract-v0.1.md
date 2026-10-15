# Arandu backend contract v0.1

**Status:** S1-C contract; audited 2026-08-20.
**Scope:** the public boundary from validated AMIR to C source, host JIT code,
or a host-native AOT object.

This document is intentionally conservative. A green frontend test means that a
program reaches AMIR; it does not automatically promote every runtime facility,
target ABI, or external toolchain to supported backend status.

## Visão Geral e Contexto

O contrato separa o AMIR validado das implementações C, JIT e AOT e define o
que constitui suporte real em vez de apenas compilação local.

## Detalhes Técnicos da Implementação

### Target model

LLVM associates a target triple with a data layout, while GCC and Clang also
require ABI, CPU, object format, system headers, libraries, and linker choices.
Arandu v0.1 models only the layout subset below. Consequently, `ptr4` and `ptr8`
are layout profiles, not target triples and not cross-toolchain configurations.

| Profile | Pointer / `int` | `float` | `i64` / `f64` ABI alignment | Contract |
|---|---:|---:|---:|---|
| `host` | host width | IEEE f64 | host profile | Required for C ↔ Cranelift parity. |
| `ptr4` | 4 bytes | IEEE f64 | 8 bytes | Generic ILP32-style layout model; emit-only and experimental. |
| `ptr8` | 8 bytes | IEEE f64 | 8 bytes | Generic LP64-style layout model; emit-only unless it equals the host. |
| `i686` | 4 bytes | IEEE f64 | 4 bytes | i686 SysV data-layout model; emit-only and tested structurally. |

All profiles reject objects at or above the positive `isize` bound of the
modeled address space. Layout arithmetic is checked and a failure returns a
typed diagnostic before a successful artifact is exposed.

### Temporary storage and bounded inlining

C and Cranelift share a conservative block-local temporary storage plan in
`arandu_codegen::temp_slots`. Only numeric, boolean and character values with
one definition and all uses strictly after that definition in the same block
can reuse storage. Exact AMIR types remain separate. Function/receiver
parameters, the return register, block parameters, cross-block values and
redefined values retain dedicated storage; addressable and aggregate
representations are excluded by the backends. Interval endpoints overlap:
an instruction's inputs cannot alias its output through this plan.

The plan reuses C declarations and Cranelift frontend variables, not AMIR
identities or native register-allocation spill slots. Mutable source locals
remain independent. Operand collection uses the shared AMIR visitors,
including projections and all terminator arguments. This plan does not
require a global interference graph or change SSA/control-flow semantics.

Inlining retains its cumulative pristine-size relative budget, but rejects
sites whose cost exceeds the remaining budget. A separate ceiling of 32768
combined statements, blocks, temporaries and locals bounds the expanded
caller, including parameter assignments and continuation blocks introduced
by splicing. Functions already larger than this ceiling remain valid and
are not expanded by this pass. The ceiling is a compiler resource heuristic,
not a language limit or a claim that this number is optimal for every workload.

The manual `profile_seed_eight_codegen` test records C declaration/source size
and JIT/C timings for the representative seed-eight O1 corpus. Timings are
not test thresholds; execution must still agree in result and output.

The ceiling retains headroom for the measured SHA-256 compression kernel;
a smaller 4096-unit prototype reintroduced helper calls into its hot loop and
was rejected. Storage reuse measurements must keep the inliner and other
backend changes identical; fewer declared variables alone do not establish
a JIT throughput improvement.

### Expression flow and checked O2 transforms

The C emitter reconstructs adjacent, single-use scalar expressions using the
canonical rvalue emitter. Each substituted node retains its assignment cast,
including integer narrowing and f32 rounding. Calls, memory reads, ownership
moves, trapping operations and cross-block values stay materialized. Chains
are bounded to eight substitutions to avoid unbounded expression depth.

Wasm emission removes an immediately adjacent `local.set`/`local.get` pair
when that local has no other accesses; shared locals retain their write via
`local.tee`. Neither transform moves instructions across effects or control
boundaries. Structured bodies explicitly reject implicit fallthrough, including
functions whose only return occurs inside a tail-recursion loop.

The checked O2 entrypoint scalarizes nonescaping Copy record/tuple locals with
scalar fields. Partial stores and joins remain ordinary scalar place operations;
aggregate loads capture the fields at the original load point, preserving Copy
snapshots across mutations. Borrows, whole-value escapes, destructors, nested
projections and unsupported field types retain their original representation.
This is not unrestricted SROA for ownership-bearing or escaping aggregates.

O2 also eliminates proven self-tail calls in scalar destructor-free functions.
The ABI entry jumps to a separate loop header; arguments are transferred as
block arguments, preserving simultaneous assignment. Non-tail calls, borrowed
locals, suspension and cleanup obligations keep their calls. Os/O2 share only
identical cleanup suffixes of parameter-free return blocks, in original order.

LICM discovers natural loops from dominators, rejects irreducible regions,
processes inner loops first and requires unique Copy definitions and operands
available at the preheader. Header parameters are not available before their
incoming edge. Divisions, modulo, shifts, loads, addresses and ownership moves
are not speculated. Loop analysis caching remains a future infrastructure task.

SCCP uses the canonical type domain to normalize fixed-width integer constants
after every assignment. Default int/uint remain 32-bit; widths come from the
shared CTFE integer contract. Folding fails closed for pointer-sized integers,
floating conversions or numeric values without type context. Typed compiler
entrypoints validate each transformation; untyped pass-manager clients can
provide their domain through `run_program_with_types`.

A Linux SHA-256 smoke corpus emitted at O1 with GCC 16.2.1 and Node 24.20.0
gave the following local before/after observations against d1f76f20:

| Metric | Before | After |
| --- | ---: | ---: |
| C source bytes | 231639 | 225682 |
| C temporary assignments | 7059 | 4349 |
| Wasm module bytes | 48331 | 46453 |
| C compilation O0, median of three | 518 ms | 439 ms |
| C compilation O2, median of three | 823 ms | 803 ms |

Both C optimization levels and both Wasm modules returned the expected digest
check result. These are corpus-specific observations; the O2 timing difference
does not establish a general throughput improvement. No 30–50% Wasm reduction
or 15–30% runtime speedup is promised.

### Backend and platform matrix

| Capability | C backend | Cranelift backend |
|---|---|---|
| Product | One GNU C translation unit | JIT module or relocatable native object |
| Execution model | External compiler and linker | Host JIT; host AOT uses a native linker and packaged static runtime |
| Target selection | Explicit `DataLayout`; no triple | Explicit Cranelift triple; public build selects a baseline host ISA |
| Cross-target status | Source emission only; caller supplies matching compiler, flags, sysroot, libc/runtime, and linker | Unsupported |
| C dialect/toolchain | GCC or Clang GNU extensions; MSVC is unsupported | Not applicable |
| Validated CI hosts | Host parity on the S0 Linux gate; install smoke on Linux and macOS | Object structure/determinism locally; installed native build smoke on release hosts |
| Invalid AMIR | Shared validator runs before emission | Shared validator runs before JIT mutation |
| Backend failure | `Err(Diagnostic)`; no successful partial source | `Err(Diagnostic)`; no successful partial module |

Windows builds are locally supported by the Rust workspace, but Windows backend
Gold requires a dedicated mandatory CI runner and runtime campaign. Until that
exists, this contract does not claim Windows backend Gold.

### Type and representation matrix

| Family | Representation | C | Cranelift host JIT |
|---|---|---|---|
| `bool`, bytes, chars, fixed integers | fixed-width scalar | Supported | Supported |
| `int`, `uint` | fixed 32-bit scalar | Supported by selected layout | `I32` |
| `isize`, `usize` | target pointer-width scalar | Supported by selected layout | Supported at host width |
| `float`, `f32`, `f64` | IEEE scalar; language `float` is f64 | Supported | Supported |
| pointers, references, borrows | pointer plus AMIR ownership rules | Supported after borrow lowering | Supported after borrow lowering |
| arrays, slices, `str` | inline array or `{ptr,len}` fat pointer | Supported; target layout applies | Supported on host layout |
| tuples and structs | ordered fields plus ABI padding | Supported | Supported |
| enums, `Option`, `Result` | pointer-width tag plus payload | Supported | Supported |
| functions and `extern "C"` | declared ABI boundary | Partial: caller must provide compatible symbols | Partial: only registered host/runtime symbols are linkable |
| allocation and `free` | libc/runtime calls | Hosted runtime only | Host runtime only |
| string interpolation / primitive `ToStr` | runtime helpers | Supported for primitives and `str` | Supported for primitives and `str` |
| user-defined formatting | no stable ABI yet | Unsupported | Unsupported |
| async/await | current ready/poll runtime model | Partial/experimental | Partial/experimental host runtime |
| OS, sockets, supervisor, generational runtime | runtime-specific symbols | Partial; not a freestanding contract | Experimental host implementation |

“Supported” here means covered at the backend boundary for well-typed AMIR. It
does not override language-phase status or promise a stable external ABI.

`void` has no parameter or result slot. Erasing a unit argument from a call's
ABI must preserve evaluation of the statements that produced it. A nullable
pointer still occupies one slot when its value is `nil`; the declared parameter
type determines that distinction for local and external calls. `Err` is also a
pointer-sized message handle, including in `Result` payloads; the Wasm32 core ABI
represents it with one `i32` slot.

The C emitter represents nullable thin pointers and references with their
underlying pointer type. Nullable scalar boxing and nullable fat slice borrows
are not covered by that correction. Generic `ConstArray` dimensions must become
concrete `Array` lengths before executable code generation; backends do not
guess a layout for an unresolved value parameter.

### Rejection and artifact rules

1. Both backends run `validate_amir_program` before backend work.
2. Malformed SSA edges, poison types, and invalid statement ranges must produce
   the same `ICE-GEN-002` diagnostic in both backends.
3. A valid AMIR construct that was not lowered to the backend contract produces
   `ICE-GEN-001`; it must never be replaced by a successful placeholder.
4. A source-language feature deliberately unavailable to users produces its
   documented user diagnostic before code generation.
5. C source is returned only after the entire translation succeeds. Cranelift
   returns a JIT module only after finalization or an object only after complete
   serialization; the CLI commits build provenance only after successful link.

### C code quality and structuring model (RFC 0018)

The implemented C backend currently emits the flat, canonical compiler form:
explicit basic-block labels and `goto` edges used by AMIR parity tests against
Cranelift, with adjacent expression reconstruction inside blocks. The proposed `--c-style=pretty` and explicit `--c-style=raw` CLI modes
belong to RFC 0018 and are not implemented yet. Any future pretty emitter remains
subject to the same `validate_amir_program` invariants and parity gates.

### C emission size and bounded optimization

The C emitter selects runtime families from operands and rvalues in retained
functions, including their dependencies. Formatting helpers are selected by
primitive type; string equality does not require the path or allocator runtime.
This is a hosted-runtime optimization, not a freestanding ABI guarantee.

Executable emission retains `main`, public/internal linkage, referenced
functions and callbacks, explicit `@Test`/`@Benchmark` tooling entrypoints, and
conservatively roots implicit destructors. Module
and private functions unreachable from these roots are omitted. Without `main`,
library inspection retains all functions. The string literal pool is still
conservative and is not a complete garbage collector for static data.

Non-addressable, uniquely defined scalar temporaries share C slots only when
their exact AMIR types agree and the shared liveness interference graph proves
their lifetimes disjoint. Return storage, parameters and block parameters stay
dedicated. User locals retain their own storage. Above 16,384 temporaries the
emitter uses the linear block-local fallback instead of allocating a dense
graph larger than approximately 32 MiB. Wasm uses the same canonical interference
analysis, with its existing allocation policy.

Address simplification tracks place projections structurally. Rotation
recognition requires complementary, nonzero constant shifts of the same
unsigned 32- or 64-bit value, unique shift uses and no intervening effects.
Signed, dynamic, shared or otherwise unproven forms retain ordinary emission.
The portable helper evaluates its input once. Fixed-width float `black_box`
preserves the input type; the Cranelift f32 barrier transports raw bits through
the integer runtime helper.

`tests/perf/sha256-array.aru` provides a SHA-256 corpus with a fixed 64-byte
buffer, a comptime K table and a 16-word circular schedule in a 64-round loop.
The K value is materialized once before the loop; this preserves value semantics
and does not promise zero-copy access to global aggregates. Eight padding and
message vectors are checked across C, Cranelift and Wasm at O0/O1/O2.

Linux measurements with GCC 16.2.1 compare the original unrolled SHA source
against the same source with the new emitter, then against the array/loop
algorithm with the same `abc` entrypoint:

| Measurement | Previous emitter | New emitter, same source | Array/loop source |
| --- | ---: | ---: | ---: |
| Generated C bytes | 225,682 | 111,818 | 28,493 |
| Generated C lines | 8,343 | 4,494 | 938 |
| Compression function machine-code bytes, GCC O2 | 10,745 | 10,519 | 1,150 |
| GCC O2 compilation median, three runs | 828 ms | 646 ms | 176 ms |

These are workload measurements, not general performance guarantees. A separate
identical streaming entrypoint verified the digest of 1 MiB of zero bytes:
seven-run process medians were 33.3 ms for the unrolled version and 16.9 ms for
the array/loop version, both using the new emitter and GCC O2. These times include
process startup. Indexed loads still have addressing and bounds-check costs;
C line counts do not prove instruction-cache occupancy, and source-size
reductions do not imply proportional execution-time reductions.

### Size profile and emission reachability (RFC 0027)

C and Cranelift AOT share the pure MIR function closure. Executable roots retain
main, public/internal linkage, tooling entrypoints, transitive function
references in statements and terminators, and implicit destructors of reachable
types. With no main all definitions remain available; the JIT also keeps all
definitions for private host lookup.

`--size` / `-Oz` selects AMIR Os without the speed inliner, Cranelift
`speed_and_size`, and a distinct `target/size` build/cache profile. Linux native
linking collects unused sections and strips symbols. Explicit Wasm size APIs
prune the same function closure, associated imports/WIT metadata and unused
literals. They omit allocator helpers only when translated core code does not
need them. Components and default inspection APIs retain canonical reallocation
and post-return contracts. Optional Binaryen `-Oz` applies to core project
builds, not components.

Literal string interpolation folds before ownership lowering when every part
has a proven string, integer, bool or char value. UTF-8, embedded NUL and
target-width integer validation are preserved. Dynamic expressions and floats
retain their runtime behavior. Existing target-layout static serialization
retains copies for mutable aggregates; size optimization does not weaken value
semantics, bounds checks or string lifetimes.

Native object emission declares runtime imports at actual call/address use.
GenRef shutdown in `main` follows the reachable program, retaining conservative
cleanup for unknown foreign code and indirect calls. JIT lookup remains eager.
The main CGU hash includes this cleanup policy. Runtime testing DTOs live in the
leaf `arandu_testing_contract`; compiler IR and Salsa must not enter the runtime
dependency closure.

Wasm may omit storage for a serialized Copy aggregate with a unique definition
and one proven initializer consumer. The consumer still owns independent mutable
storage; references, returns, jump arguments and unproven uses keep ordinary
materialization. Bounded stack forwarding crosses only balanced independent
scalar expressions, with calls, memory, control, traps and overwrites as barriers.
Heap payloads are aligned to 16 bytes, including reused blocks, and canonical
reallocation validates supported alignment and arithmetic before writing.

The [RFC](./rfcs/0027-zero-bloat-binary-diet-and-on-demand-runtime.md) defines
the scope and validation. No absolute executable size or freestanding support
is inferred from the profile.

### What is required for real cross compilation

Adding a new Gold target requires one named target specification containing at
least architecture, OS/environment, ABI/calling convention, endianness, object
format, data layout, runtime availability, and linker/toolchain configuration.
It also requires compile, link, and execution tests on that target or an
explicitly documented emulator. Adding another pointer-width alias is not
sufficient.

Primary references used for this decision:

- [LLVM Language Reference: target triple and data layout](https://llvm.org/docs/LangRef.html)
- [Clang cross-compilation guide](https://clang.llvm.org/docs/CrossCompilation.html)
- [GCC target-specific options](https://gcc.gnu.org/onlinedocs/gcc/Target-Specific-Options.html)
- [GCC ABI compatibility scope](https://gcc.gnu.org/onlinedocs/gcc/Compatibility.html)
- [Cranelift IR invariants](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/ir.md)

### Promotion criteria

The C backend becomes Gold for a named target only when its generated source is
compiled and linked with the declared toolchain and runtime, then executed in a
mandatory CI job. Cranelift JIT and AOT are promoted independently per host:
AOT additionally requires object validation, link, execution and
failure-retention tests using the distributed runtime. Cross-target execution,
LLVM release codegen, freestanding C, and a stable public ABI remain separate
future milestones.

## PONTOS DE MELHORIA (O que não está no roadmap)

Cross-compilation, LLVM release, C freestanding e ABI pública continuam fora do
escopo publicado. Arquivos grandes de emitter/translator são revistos por
responsabilidade, não divididos mecanicamente.

## Futuro e Próximos Passos

Cada target novo precisa de layout, emissão, link, execução e pacote exercitado
em ambiente nativo antes de entrar na matriz de suporte.
