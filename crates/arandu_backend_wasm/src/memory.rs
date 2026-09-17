//! Linear memory section for wasm32 modules.
//!
//! Emits the `(memory ...)` declaration and the globals required by the
//! shared heap allocator (bump + free list, see [`crate::canonical`]) and the
//! per-function shadow stack emitted by [`crate::emit`] and the function
//! translator.
//!
//! # Memory layout (wasm32, ptr_width = 4)
//!
//! ```text
//! ┌────────────────────────────────┐ 0x00_0000
//! │  Null / trap guard page        │
//! ├────────────────────────────────┤ 0x00_2000 (RODATA_BASE)
//! │  Static literal pool (rodata)  │
//! ├────────────────────────────────┤ 0x01_0000 (STACK_LIMIT)
//! │  Shadow stack (grows down)     │  64 KiB within page 1
//! ├────────────────────────────────┤ 0x02_0000 (STACK_BASE / HEAP_BASE_MIN)
//! │  Heap (bump + free list, up)   │  … memory.grow on demand
//! └────────────────────────────────┘
//! ```

use wasm_encoder::MemArg;

/// Initial linear memory: 4 Wasm pages = 256 KiB (rodata + stack + heap region).
pub const INITIAL_PAGES: u64 = 4;

/// Maximum pages: 64 Ki pages = 4 GiB address-space limit.
pub const MAX_PAGES: Option<u64> = Some(65536);

/// Base of the static literal pool (rodata) inside the data region.
/// Avoids the null page at 0 and keeps alignment up to 8 bytes.
pub const RODATA_BASE: i32 = 0x2000;

/// Low watermark of the shadow stack (64 KiB of scratch inside page 1).
/// The stack pointer is checked against this before every frame push;
/// crossing it grows memory and moves the stack region into grew memory.
pub const STACK_LIMIT: i32 = 0x1_0000;

/// Top of the shadow stack (address `0x2_0000`): the 64 KiB stack grows down
/// from here into lower addresses.
pub const STACK_BASE: i32 = 0x2_0000;

/// Bytes available to the shadow stack inside the initial pages.
pub const STACK_AREA: i32 = STACK_BASE - STACK_LIMIT;

/// Minimum heap base: heap starts strictly at or above the shadow stack base
/// and grows upward into higher memory.
pub const HEAP_BASE_MIN: i32 = STACK_BASE;

/// Index of the `__stack_pointer` global (mutable i32).
pub const GLOBAL_STACK_POINTER: u32 = 0;

/// Index of the `__heap_base` global (immutable i32).
pub const GLOBAL_HEAP_BASE: u32 = 1;

/// Index of the `__heap_ptr` global (mutable i32 — current bump pointer).
pub const GLOBAL_HEAP_PTR: u32 = 2;

/// Index of the `__stack_base` global (immutable i32 — constant).
pub const GLOBAL_STACK_BASE: u32 = 3;

/// Index of the `__freelist_head` global (mutable i32 — head of the heap
/// allocator free list; `0` = empty).
pub const GLOBAL_FREE_LIST_HEAD: u32 = 4;

/// Size in bytes of the per-block header stored by the runtime heap allocator
/// (`[magic: u32][block_size: u32][next: u32]`). Every allocation payload
/// starts `CELL_HEADER_SIZE` bytes past the block base, keeping payloads
/// 4-byte aligned (12 % 4 == 0).
pub const CELL_HEADER_SIZE: i32 = 12;

/// Magic cookie stored in the header of every block handed out by the heap
/// allocator. `__arandu_free` validates the magic before linking a block into
/// the free list, so freeing a pointer that does not point at a tracked block
/// (e.g. rodata) degrades to a safe no-op instead of corrupting the list.
pub const CELL_MAGIC: i32 = 0x5A5A_5A5A;

/// Alignment exponent used for every memory access (4-byte aligned memory).
pub const MEM_ALIGN: u32 = 2;

/// Shorthand for `MemArg` with no offset and 4-byte alignment on memory 0.
#[must_use]
pub const fn noffset_memarg() -> MemArg {
    MemArg {
        offset: 0,
        align: MEM_ALIGN,
        memory_index: 0,
    }
}

/// Static read-only data table for string literals and constant data segments.
#[derive(Debug, Clone, Default)]
pub struct RodataTable {
    /// Byte buffer to be emitted into the WebAssembly DataSection.
    pub bytes: Vec<u8>,
    /// Map from AMIR LiteralId to its static linear memory byte offset.
    pub offsets: rustc_hash::FxHashMap<arandu_middle::literal_pool::LiteralId, u32>,
}

impl RodataTable {
    /// Collect all string literals from `pool` into a contiguous rodata buffer,
    /// aligned at [`RODATA_BASE`].
    #[must_use]
    pub fn from_literal_pool(pool: &arandu_middle::literal_pool::AmirLiteralPool) -> Self {
        let mut table = Self::default();
        let mut current_offset = RODATA_BASE as u32;

        for (idx, entry) in pool.entries.iter().enumerate() {
            if let arandu_middle::literal_pool::AmirLiteralEntry::Str(s) = entry {
                let bytes = s.as_bytes();
                // 4-byte align the start of each string
                let padding = (4 - (table.bytes.len() % 4)) % 4;
                for _ in 0..padding {
                    table.bytes.push(0);
                    current_offset += 1;
                }
                let offset = current_offset;
                table.bytes.extend_from_slice(bytes);
                // Null-terminate for safety and C ABI compatibility
                table.bytes.push(0);
                current_offset += bytes.len() as u32 + 1;
                table
                    .offsets
                    .insert(arandu_middle::literal_pool::LiteralId(idx as u32), offset);
            }
        }
        table
    }

    /// Compute the aligned heap base after the end of rodata.
    /// Never falls below [`HEAP_BASE_MIN`].
    #[must_use]
    pub fn heap_base(&self) -> i32 {
        if self.bytes.is_empty() {
            return HEAP_BASE_MIN;
        }
        let rodata_end = RODATA_BASE as u32 + self.bytes.len() as u32;
        let aligned = (rodata_end + 15) & !15;
        (aligned as i32).max(HEAP_BASE_MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time layout invariants.
    const _: () = assert!(STACK_LIMIT > 0);
    const _: () = assert!(STACK_BASE > STACK_LIMIT);
    const _: () = assert!(RODATA_BASE >= 0x1000);
    const _: () = assert!(RODATA_BASE < STACK_LIMIT);
    const _: () = assert!(HEAP_BASE_MIN >= STACK_BASE);

    #[test]
    fn stack_area_is_64ki() {
        assert_eq!(STACK_AREA, 0x1_0000);
    }

    #[test]
    fn memarg_is_4byte_aligned() {
        assert_eq!(noffset_memarg().align, MEM_ALIGN);
        assert_eq!(noffset_memarg().offset, 0);
    }

    #[test]
    fn rodata_table_builds_and_aligns() {
        let mut pool = arandu_middle::literal_pool::AmirLiteralPool::default();
        let lit1 = pool.intern_str("hello");
        let lit2 = pool.intern_str("world");
        let table = RodataTable::from_literal_pool(&pool);

        assert!(!table.bytes.is_empty());
        assert_eq!(table.offsets.len(), 2);
        let off1 = table.offsets.get(&lit1).copied().unwrap();
        let off2 = table.offsets.get(&lit2).copied().unwrap();
        assert!(off1 >= RODATA_BASE as u32);
        assert!(off2 > off1);
        assert_eq!(off1 % 4, 0);
        assert_eq!(off2 % 4, 0);
        assert_eq!(table.heap_base(), HEAP_BASE_MIN);
    }
}
