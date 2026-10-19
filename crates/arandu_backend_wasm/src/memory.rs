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

/// Minimum low watermark of the shadow stack. Large rodata moves the entire
/// stack upward; exhausting its bounded region traps before modifying memory.
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
/// (`[magic: u32][block_size: u32][next: u32][generation: u32]`). Both block
/// sizes and the header are aligned to [`CELL_ALIGNMENT`], so every payload
/// satisfies the alignment of layouts accepted by the aggregate frame planner.
pub const CELL_HEADER_SIZE: i32 = 16;
pub const CELL_ALIGNMENT: i32 = 16;
/// Reserved header word holding the odd live GenRef generation tag. Ordinary
/// allocations and all released cells hold zero here.
pub const CELL_GENERATION_OFFSET: i32 = CELL_HEADER_SIZE - 4;

/// Magic cookie stored in the header of every block handed out by the heap
/// allocator. `__arandu_free` validates the magic before linking a block into
/// the free list, so freeing a pointer that does not point at a tracked block
/// (e.g. rodata) degrades to a safe no-op instead of corrupting the list.
pub const CELL_MAGIC: i32 = 0x5A5A_5A5A;
/// Private invocation backing: ordinary move/drop frees must not reclaim it.
pub const CELL_SCRATCH_MAGIC: i32 = 0x5A5A_5354;

/// Alignment exponent used for every memory access (4-byte aligned memory).
pub const MEM_ALIGN: u32 = 2;

/// Reserved low-memory slot (`< RODATA_BASE`) holding the monotonic `GenRef`
/// generation counter.
pub const GEN_COUNTER_ADDR: u64 = 0x1FE8;

/// SIMD BlackBox value (16 bytes) followed by its opaque zero offset (4 bytes).
/// Kept separate from both the generation counter and scalar scratch storage.
pub const VECTOR_BLACK_BOX_SCRATCH_ADDR: u64 = 0x1FD0;

/// Reserved 16-byte low-memory scratch area (`< RODATA_BASE`) used to launder
/// `BlackBox` values through exported linear memory (`[0x1FF0..0x1FF8)` holds
/// the value and `0x1FF8` holds an opaque zero base offset).
pub const BLACK_BOX_SCRATCH_ADDR: u64 = 0x1FF0;

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
        Self::collect_literals(pool, |_| true)
    }

    /// Keep original LiteralIds while packing only strings used by live code.
    #[must_use]
    pub fn from_used_literals(
        pool: &arandu_middle::literal_pool::AmirLiteralPool,
        used: &rustc_hash::FxHashSet<arandu_middle::literal_pool::LiteralId>,
    ) -> Self {
        Self::collect_literals(pool, |id| used.contains(&id))
    }

    fn collect_literals(
        pool: &arandu_middle::literal_pool::AmirLiteralPool,
        include: impl Fn(arandu_middle::literal_pool::LiteralId) -> bool,
    ) -> Self {
        let mut table = Self::default();
        let mut current_offset = RODATA_BASE as u32;

        for (idx, entry) in pool.entries.iter().enumerate() {
            if !include(arandu_middle::literal_pool::LiteralId(idx as u32)) {
                continue;
            }
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

    /// Place the bounded shadow stack after rodata, with a page of initial
    /// heap space. Checked signed addresses match emitted i32 constants.
    pub fn memory_regions(&self) -> Option<(i32, u64)> {
        let end = usize::try_from(RODATA_BASE)
            .ok()?
            .checked_add(self.bytes.len())?;
        let limit = end.checked_add(15)? & !15;
        let limit = limit.max(usize::try_from(STACK_LIMIT).ok()?);
        let base = limit.checked_add(usize::try_from(STACK_AREA).ok()?)?;
        let pages = base.checked_add(65536)?.checked_add(65535)? / 65536;
        Some((
            i32::try_from(base).ok()?,
            u64::try_from(pages).ok()?.max(INITIAL_PAGES),
        ))
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
        assert_eq!(table.memory_regions(), Some((HEAP_BASE_MIN, INITIAL_PAGES)));
    }

    #[test]
    fn large_rodata_cannot_overlap_frames() {
        let table = RodataTable {
            bytes: vec![0; 180_000],
            ..RodataTable::default()
        };
        let (base, pages) = table.memory_regions().unwrap();
        assert!(base - STACK_AREA >= RODATA_BASE + 180_000);
        assert!(pages * 65536 >= u64::try_from(base).unwrap() + 65536);
    }
}
