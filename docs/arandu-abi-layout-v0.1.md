# Arandu ABI Layout Specification (v0.1)

This document defines the physical memory layouts, alignment rules, and canonical ABI representation of types in the Arandu compiler.

---

## Visão Geral e Contexto

Este contrato define layout físico e representações ABI dependentes do alvo
para que middle, runtime e backends concordem byte a byte.

## Detalhes Técnicos da Implementação

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
| `bool`, `byte`, `char`, `i8`, `u8` | 1 | 1 | |
| `i16`, `u16` | 2 | 2 | |
| `i32`, `u32`, `f32` | 4 | 4 | |
| `i64`, `u64`, `f64` | 8 | 8 | Fixed-width types |
| `int`, `uint` | $W$ | $W$ | Platform-dependent integer types |
| `float` | 8 | 8† | Always IEEE f64 (`DataLayout`); †i686 may use abi_align 4 |
| `ptr[T]` | $W$ | $W$ | Platform-dependent pointer |
| `any` | $W$ | $W$ | Boxed dynamic pointer |
| `void`, typeck `error` | 0 | 1 | ZSTs (Zero Sized Types) |
| `Err` | $W$ | $W$ | Message handle: non-null pointer to a NUL-terminated UTF-8 buffer from `err.new` |

### Platform-Dependent Primitive Mappings

For compilation backends (such as the C backend and Cranelift JIT), platform-dependent types map to the corresponding native sized types:
- **`int` / `IntLiteral`**: Represented as a signed integer of width $W$ bytes (`int64_t` / `int32_t` in C; target `ptr_type` `I64` / `I32` in Cranelift).
- **`uint`**: Represented as an unsigned integer of width $W$ bytes (`uint64_t` / `uint32_t` in C; target `ptr_type` `I64` / `I32` in Cranelift).
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
