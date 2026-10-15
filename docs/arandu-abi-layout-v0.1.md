# Arandu ABI Layout Specification (v0.1)

This document defines the physical memory layouts, alignment rules, and canonical ABI representation of types in the Arandu compiler.

---

## Visão Geral e Contexto

Este contrato define layout físico e representações ABI dependentes do alvo
para que middle, runtime e backends concordem byte a byte.

## Detalhes Técnicos da Implementação

### Aggregate equality and register transport

`Option<T>` and `Result<T, E>` equality is lowered in `arandu_mir`, not
implemented separately in the emitters. The CFG first compares discriminants;
different variants compare unequal without reading either payload. Equal
variants select only their active payload, recursively comparing scalars,
strings, nested built-in sum types, nominal enums, tuples, arrays and closed structs.
Nominal enum variants are ordered by their canonical tags, and multi-field
payloads use the same instantiated `EnumPayload` projections as pattern lowering.
Neither a tag mismatch nor an empty variant reads inactive payload bytes.
`!=` negates that result. Struct padding and inactive payload bytes never
participate; floating-point leaves retain ordinary IEEE comparison semantics
(signed zeros compare equal and NaN does not compare equal to itself).
The recursive expansion has a depth and work bound, including when nested
inside a static loop. Reference leaves retain reference identity semantics.

Concrete arrays now use the same target-aware aggregate classifier as tuples
at every Cranelift boundary: declaration, argument packing, entry unpacking,
result packing and caller result materialization. The classifier walks array
elements, including nested arrays, only after checking the total layout size.
SysV integer/SSE classes are not substituted for Windows x64 rules: Windows
uses integer slots only for the supported exact aggregate sizes. Shared
Cranelift slot packing/unpacking handles partial final slots byte by byte
according to target endianness; an array with three bytes therefore cannot
cause an eight-byte load or store past its allocation, without changing its
SysV register class to an incorrect indirect convention.
This describes Arandu's value-array transport, not C's array-to-pointer decay.

### Residual aggregate storage: safety boundary

Cranelift promotes admitted constructors, value copies and caller result
buffers to independent frame slots. The pure admission policy belongs to MIR
and is shared with Wasm: safe borrowed leaves, pointer-returning views,
suspension, unknown foreign calls and aggregate back-edge phi values fail
closed. Raw ownership handles inside affine containers are distinct from the
container's backing bytes. Non-Copy moves and user destruction remain governed
by shared ownership/drop elaboration, not by backend storage reclamation.
Ready-only coroutine creators with scalar payloads may reclaim their private
aggregate scratch; suspended states and aggregate coroutine payloads do not
qualify.

Promoted scratch plus existing native explicit slots has a 1 KiB admission
budget (including alignment allowance); this does not bound register spills
or implement stack coloring. Admitted values beyond that budget use private
heap scratch, allocated lazily per static materialization site, reused in
loops, and reclaimed on normal returns after packing/copying the result.
Entry-backedge shapes are excluded from native heap-handle initialization.
This releases backing bytes only, never a second owner of nested resources.

Arandu-to-Arandu indirect aggregate results use a target-aware hidden
`StructReturn` destination supplied by the caller. Constructors and value
copies into the return temporary may write directly to it; other return paths
copy before the callee exits. This includes `Option`, `Result` and `Poll`
carriers, while host imports keep their separate ABI. By-value parameters
materialize private callee backing, including pointer-transported carriers.
JIT host lookups preserve their pointer-returning ABI through narrow adapters
that allocate a transferable result cell and call the internal destination ABI.
Internal calls do not traverse these adapters. Host callers own the returned
backing and must release it according to the runtime allocation contract.
Unit-success `nil` in `Result<void, E>` lowers to an ordinary tagged constructor,
not a null backing pointer, consistently across backends.
Contextual constructors, including nominal variant sugar, propagate expected
payload types into nested array literals; native payload copies additionally
check instantiated storage bounds.

Frozen initialization sources are immutable, but runtime values retain
independent writable storage. One bounded MIR serializer handles relocation-free
scalar arrays, tuples and closed Copy structs, including eligible nested
literal temporaries. It follows target layout and endianness, zeroes padding,
and uses software float conversion or frozen IEEE bits. Alias-bearing,
multiply-used aggregate temporaries, dynamic inputs and pointer relocations
fall back to ordinary evaluation. Serialization is bounded by 65,536 operand
visits, depth 64 and 1 MiB of admitted bytes per function; objects below 32
bytes retain immediate stores. Native object data and Wasm data segments consume
the same serialized bytes. This is not global-constant CTFE or enum reflection.

Linux validation used an AOT workload with 100,000 struct/array constructions
and independent mutable copies. Interposed native malloc calls fell from
500,000 (18,400,000 requested bytes) to zero; peak RSS in the measured run fell
from 25,768 KiB to 2,204 KiB. This is workload-specific evidence, not a general
speed guarantee. The real `ita` SHA-256 workload retained the reference digest;
its malloc calls fell from 33 to 30, without a demonstrated timing improvement.
Regression tests count runtime allocations independently of compiler/JIT
allocations, cover all three AMIR optimization levels, inspect read-only native
object data, and verify mutable-copy isolation and conservative fallbacks.

A separate native workload constructs independent 2 KiB arrays in 10,000 loop
iterations. Before heap-scratch reuse it made 30,000 malloc calls requesting
61,440,000 bytes, with peak RSS 62,520 KiB. Afterwards it made three malloc calls
requesting 6,144 bytes, with peak RSS 2,104 KiB. Test-local malloc/free accounting
also verifies balanced releases at O0/O1/O2, unentered branches, early returns
and recursive caller-owned results. The Linux interposition figures are
workload-specific and do not establish behavior on Windows or macOS.

The Pypor kernel workload (Linux, 2026-10-04) retained exactly 65,479 files and
37,995,166 total lines. Five alternating warm-cache runs with eight workers on
eight physical cores measured median external wall time of 1.31 s before and
1.28 s after; user CPU was 6.90 s versus 6.84 s. This small timing difference
does not establish a large speedup. Median peak RSS fell from 233,648 KiB to
202,528 KiB (about 13%). Separate malloc interposition measured 4,780,729 versus
3,826,624 calls; requested bytes remained about 6.2 GB over the whole execution.
These counts include traversal, runtime and process teardown, not only scanner
aggregates, and are not a proof of balanced resource ownership in every fallback.

Wasm reserves a checked, aligned 64 KiB shadow-stack region after rodata and
before the allocator heap, moving the entire region upward for large static
data. Reservation traps before changing the stack pointer on overflow; normal
returns restore it. Admitted scratch exceeds neither a 1 KiB frame budget nor
its invocation lifetime: larger objects use lazily allocated allocator cells
with a private scratch marker, ignored by ordinary move/drop frees and released
by the owning invocation. Closed owned core results are transferred into
heap cells before private backing is reclaimed; admitted callers reclaim those
cells after their final use. Affine payloads follow the existing AMIR moves and
drops; reclaiming a backing cell never invokes a second destructor. Regressions
exercise recursive affine returns, early returns and exactly-once destruction
of real allocated resources without linear-memory growth.
Component signatures keep their canonical ABI.
This shadow stack is separate from WebAssembly's operand/call stacks. Traps do
not provide stack or resource unwinding; a trapped guest instance must be
discarded rather than treated as a normal reusable invocation.

Residual exclusions remain explicit: unknown foreign retention, aggregate
back-edge phi lifetimes, borrowed carriers, general suspension/aggregate
coroutine payloads and view-bearing pointer-returning Wasm results still require
escape/liveness proofs and compatible ownership transfer. Legacy backing in
those paths may retain the existing cleanup gap. They are not evidence that
all aggregates are stack allocated or every program is allocation-free.

### 1. Type Layout Calculation Algorithm

Memory layout in Arandu follows the standard C ABI layout rules (`#[repr(C)]`). Each type is represented by a `TypeLayout` structure:

- **Size**: Total size of the type in bytes, including internal and trailing padding.
- **Alignment**: Required boundary alignment in bytes (must be a power of two).
- **Field Offsets**: The byte offset from the start of the structure for each field (applicable to structs/tuples/results).

### Padding and Alignment Formula

The alignment of a composite type (struct or tuple) is the maximum alignment of all its fields:

$$\text{Alignment}_{\text{composite}} = \max(\text{Alignment}_{\text{field}_1}, \text{Alignment}_{\text{field}_2}, \dots)$$

When laying out fields, each field's offset must be aligned to its own alignment constraint. The formula to align an offset is:

$$\text{aligned\_offset} = (\text{offset} + \text{align} - 1) \ \& \ \sim(\text{align} - 1)$$

Finally, the total size of the composite type is aligned to the composite alignment constraint:

$$\text{aligned\_size} = (\text{total\_size} + \text{align}_{\text{composite}} - 1) \ \& \ \sim(\text{align}_{\text{composite}} - 1)$$

---

### 2. Primitive Type Layouts

The size and alignment of primitive types are defined below (under a target pointer width of $W$ bytes, where $W = 4$ or $W = 8$):

| Primitive Type | Size (Bytes) | Alignment (Bytes) | Notes |
| :--- | :--- | :--- | :--- |
| `bool`, `byte`, `i8`, `u8` | 1 | 1 | |
| `char` | 4 | 4 | Unicode scalar value, not a UTF-8 byte |
| `i16`, `u16` | 2 | 2 | |
| `i32`, `u32`, `f32` | 4 | 4 | |
| `i64`, `u64`, `f64` | 8 | 8 | Fixed-width types |
| `int`, `uint` | 4 | 4 | Fixed-width signed/unsigned 32-bit integers |
| `isize`, `usize` | $W$ | $W$ | Pointer-width signed/unsigned integers |
| `float` | 8 | 8† | Always IEEE f64 (`DataLayout`); †i686 may use abi_align 4 |
| `ptr[T]` | $W$ | $W$ | Platform-dependent pointer |
| `any` | $W$ | $W$ | Boxed dynamic pointer |
| `void`, typeck `error` | 0 | 1 | ZSTs (Zero Sized Types) |
| `Err` | $W$ | $W$ | Message handle: non-null pointer to a NUL-terminated UTF-8 buffer from `err.new` |

### Primitive Backend Mappings

For compilation backends (such as the C backend and Cranelift JIT), platform-dependent types map to the corresponding native sized types:
- **`int` / `IntLiteral`**: Represented as signed 32-bit (`int32_t` in C; `I32` in Cranelift).
- **`uint`**: Represented as unsigned 32-bit (`uint32_t` in C; `I32` in Cranelift).
- **`isize` / `usize`**: Represented at the target pointer width in C and Cranelift.
- **`float` / `FloatLiteral`**: Always IEEE **f64** (`double` in C; `F64` in Cranelift) on all targets — **not** reduced to 4 bytes on 32-bit. Alignment may be 4 under `DataLayout::i686_sysv()`.

---

### 3. Canonical Fat Pointer Layouts (`str` and `[]T`)

Strings and slices use a **Fat Pointer ABI** (`pointer + length`). This is only
the physical representation; it does not by itself provide lifetime or
ownership safety. The compiler separately proves the provenance of safe
references. `str` is a managed language value and `[]T` is a non-owning safe
borrowed view whose origin and live range are erased only after validation.

### String Layout (`str`)

The layout of `str` is exactly equivalent to the following C structure:

```rust
struct StrLayout {
    ptr: ptr[u8],  // Pointer to the start of utf-8 buffer
    len: usize,    // Number of bytes in buffer (target pointer width)
}
```

## PONTOS DE MELHORIA (O que não está no roadmap)

O suporte de um layout não promove automaticamente o target na distribuição.
ABI de FFI e targets adicionais precisam de runners/artefatos nativos.

Agregados pequenos ainda são passados indiretamente no Cranelift. Não existe
uma regra portável “struct com até 16 bytes vai em registradores”: no Windows
x64, agregados comuns só são tratados como inteiros quando têm exatamente 1,
2, 4 ou 8 bytes; no SysV AMD64, até dois eightbytes são classificados por seus
campos; AArch64 possui classificação própria, inclusive para agregados
homogêneos de ponto flutuante. A otimização BC.5 exige um classificador por
target compartilhado por declaração, call e return, mais testes contra C em
runners nativos. Até isso existir, a passagem indireta é a opção conservadora.

## Futuro e Próximos Passos

Ampliar matrizes de layout e chamada junto da matriz de release; nunca inferir
ponteiro, float ou alinhamento a partir do host do compilador.

Referências normativas para BC.5:

- [System V AMD64 ABI](https://refspecs.linuxfoundation.org/elf/x86_64-abi-0.98.pdf)
- [Microsoft x64 calling convention](https://learn.microsoft.com/cpp/build/x64-calling-convention)
- [AAPCS64](https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst)

- **64-bit Target**: `size = 16`, `align = 8`, field offsets: `ptr` at offset `0`, `len` at offset `8`.
- **32-bit Target**: `size = 8`, `align = 4`, field offsets: `ptr` at offset `0`, `len` at offset `4`.

`len` is always target **`usize`** (same width as a pointer), never a fixed `u64` on 32-bit.
Cranelift uses host `usize` (typically I64 on 64-bit hosts) and is **not** a 32-bit backend.

### Slice Layout (`[]T`)

Slices (`[]T`) use the same layout structure. Borrowing the dynamically-sized
sequence does not add an indirection: `ref []T` and `mut ref []T` retain the
same two-word representation, while shared/exclusive access is enforced by
type checking and OSSA before code generation.

```rust
struct SliceLayout {
    ptr: ptr[T],   // Pointer to first element of the slice
    len: usize,    // Number of elements (target pointer width)
}
```

- **64-bit Target**: `size = 16`, `align = 8`, field offsets: `ptr` at offset `0`, `len` at offset `8`.
- **32-bit Target**: `size = 8`, `align = 4`, field offsets: `ptr` at offset `0`, `len` at offset `4`.

The two-word layout is not itself the safety proof. A raw `ptr[T]` may be null
or dangling and requires `unsafe` dereference; `[]T` additionally carries
compiler-only origin, mutability and live-range facts checked before code
generation. C and Cranelift receive only the same two target-width slots.

### Generational reference (`GenRef`) — F2.3.runtime

See **`docs/arandu-genref-gold-rfc-v0.1.md`**. Summary:

```text
struct GenRef {
    index: u32,        // offset 0
    generation: u32,   // offset 4
}
// size = 8, align = 4 on all targets
```

Not a `(ptr, len)` fat pointer. Payload lives in `std.alloc.gen_arena` slots.
Mismatch on use → `std.core.intrinsics.abortGenerationalMismatch` (trap, not UB).

---

### 4. Enums and Sum Types (`Result<T, E>` and `Option<T>`)

### `Result<T, E>` Layout

A `Result` is represented as a tagged union:

```rust
struct ResultLayout {
    tag: u64, // 0 = Ok, 1 = Err (or pointer width)
    payload: union { ok: T, err: E }
}
```

- **Alignment**: $\max(8, \text{align}(T), \text{align}(E))$
- **Offsets**: Tag at offset `0`, Payload at offset `pointer_width`.
- **Size**: $\text{align\_to}(\text{pointer\_width} + \max(\text{size}(T), \text{size}(E)), \text{Alignment})$.

### `Option<T>` Layout

Similarly:

```rust
struct OptionLayout {
    tag: u64, // 0 = None, 1 = Some (or pointer width)
    payload: T
}
```
