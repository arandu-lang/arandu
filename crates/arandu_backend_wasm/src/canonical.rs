//! Canonical ABI support for the WebAssembly Component Model.
//!
//! This module emits the runtime support functions required by the Canonical
//! ABI specification (Bytecode Alliance) plus the shared guest heap allocator:
//!
//! * **`__arandu_alloc` / `__arandu_free`** (internal, not exported) – the
//!   module-wide heap allocator. It positions a bump frontier for fresh blocks
//!   and a size-checked free list so that `free` (both the surface `free`
//!   operator and the `@Destructor` methods invoked by `Destroy`) actually
//!   reclaims memory instead of leaking it. Every allocated block carries a
//!   `[magic: u32][block_size: u32][next: u32]` header (see
//!   [`crate::memory::CELL_HEADER_SIZE`]); the payload lives at `base + 12`.
//! * **`cabi_realloc`** – called by the host to allocate or resize a memory
//!   region in the guest's linear memory (used for lowering strings and lists
//!   passed *into* the module from the host). It delegates to the same
//!   allocator, so host-allocated buffers use the exact same block format.
//! * **`cabi_post_return_<name>`** – a no-op stub called by the host after it
//!   has finished consuming a value that the guest returned through linear
//!   memory. The stub keeps returning immediately: the retptr pair cell is
//!   owned by the caller and the payload may live in rodata, so reclaiming it
//!   here is deliberately deferred.
//!
//! # Canonical ABI contract for `cabi_realloc`
//!
//! ```text
//! cabi_realloc(old_ptr: i32, old_size: i32, align: i32, new_size: i32) → i32
//! ```
//!
//! * If `new_size == 0` → free `old_ptr` (if non-null) and return 0.
//! * If `old_ptr == 0` → allocate `new_size` bytes through `__arandu_alloc`.
//! * If `old_ptr != 0` → allocate `new_size` bytes, preserve existing data by
//!   copying `min(old_size, new_size)` bytes from `old_ptr` via `memory.copy`,
//!   reclaim `old_ptr` through `__arandu_free`, and return the new pointer.
//!
//! The heap-allocator globals (indices defined in [`crate::memory`]):
//! * **Global 2** (`__heap_ptr`, mutable `i32`) – bump frontier.
//! * **Global 4** (`__freelist_head`, mutable `i32`) – free list head.
//!
//! # Allocator invariants
//!
//! * `__arandu_alloc` pops the *first* free block whose recorded size is >=
//!   the request (first fit). Because the free list is LIFO and the scan order
//!   is deterministic, allocation results are fully deterministic.
//! * `__arandu_free` validates the `magic` cookie before linking a block;
//!   freeing `0` or a pointer that is not a tracked block is a no-op.
//! * The free list is trust-the-MIR for double frees (the move checker rejects
//!   `free` of a moved value), matching the C backend's `malloc`/`free`
//!   contract.

use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    TypeSection, ValType,
};

// Indices of the heap-allocator globals inside the emitted module (must match
// the order in which `WasmModuleBuilder::build` appends globals).
use crate::memory::{CELL_HEADER_SIZE, CELL_MAGIC, GLOBAL_FREE_LIST_HEAD, GLOBAL_HEAP_PTR};
const MEMORY_IDX: u32 = 0;

/// Descriptor for one Wasm function entry to be injected into the module.
///
/// Used by [`CabiSupport`] to collect the type + body of each synthesised
/// function so that callers can insert them into the appropriate sections in
/// the correct Wasm binary order.
pub struct CabiFunc {
    /// Name to use in the export section, or `None` for internal (non-exported)
    /// runtime helpers such as `__arandu_alloc`/`__arandu_free`.
    pub export_name: Option<String>,
    /// Wasm type signature: `(params, results)`.
    pub ty: (Vec<ValType>, Vec<ValType>),
    /// Additional local variable declarations `(count, type)` beyond the
    /// function parameters, in wasm local-group order.
    pub extra_locals: Vec<(u32, ValType)>,
    /// Instruction body (without the final `end` – the helper appends it).
    pub body: Vec<Instruction<'static>>,
}

/// Builder that collects all Canonical ABI helper functions for a single
/// module.
///
/// Call [`Self::add_realloc`] once and, for every exported function that
/// returns a fat pointer (string / list), call [`Self::add_post_return`].
/// Then use the accessors to integrate the collected functions into the
/// module's sections.
#[derive(Default)]
pub struct CabiSupport {
    funcs: Vec<CabiFunc>,
}

impl CabiSupport {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the internal heap allocator functions `__arandu_alloc` and
    /// `__arandu_free`.
    ///
    /// They must be the *first* two entries added to this builder so the module
    /// builder knows their function indices (`base + 0`, `base + 1`) when wiring
    /// `cabi_realloc`'s calls and the translators' `alloc_cell`/`emit_free` call
    /// sites.
    ///
    /// # `__arandu_alloc(size: i32) -> i32`
    ///
    /// Best-fit scan of the LIFO free list for the smallest block whose recorded
    /// size is at least `size`; otherwise bump the frontier (growing memory as
    /// needed) and write the block header. Best-fit preserves larger reusable
    /// blocks when smaller fields are allocated before their containing object.
    /// Returns the payload pointer (`base + 12`).
    ///
    /// # `__arandu_free(ptr: i32)`
    ///
    /// Validates the block magic at `ptr - 12` and, when it matches, links the
    /// block at the head of the free list. `0` and untracked pointers are a safe
    /// no-op.
    pub fn add_allocator(&mut self) {
        self.funcs.push(CabiFunc {
            export_name: None,
            ty: (vec![ValType::I32], vec![ValType::I32]),
            extra_locals: vec![(8, ValType::I32)],
            body: Self::alloc_body(),
        });
        self.funcs.push(CabiFunc {
            export_name: None,
            ty: (vec![ValType::I32], vec![]),
            extra_locals: vec![(1, ValType::I32)],
            body: Self::free_body(),
        });
    }

    /// Body of `__arandu_alloc(size: i32) -> i32`.
    ///
    /// Best-fit scan of the LIFO free list for the smallest block whose recorded
    /// size is at least `size`; otherwise bump the frontier (growing memory as
    /// needed) and write the block header. Locals 7 and 8 retain the selected
    /// block and its predecessor while locals 2/6 walk the current list node.
    ///
    /// Locals beyond the `size` parameter: `1` aligned size, `2` current node,
    /// `3` current node size, `4` new heap pointer, `5` memory size in bytes,
    /// `6` current predecessor, `7` selected best-fit node, `8` its predecessor.
    fn alloc_body() -> Vec<Instruction<'static>> {
        vec![
            // size = (size + 3) & !3
            Instruction::LocalGet(0),
            Instruction::I32Const(3),
            Instruction::I32Add,
            Instruction::I32Const(!3),
            Instruction::I32And,
            Instruction::LocalSet(1),
            // node = __freelist_head
            Instruction::GlobalGet(GLOBAL_FREE_LIST_HEAD),
            Instruction::LocalSet(2),
            Instruction::I32Const(0),
            Instruction::LocalSet(6), // prev = 0
            Instruction::I32Const(0),
            Instruction::LocalSet(7), // best = 0
            Instruction::I32Const(0),
            Instruction::LocalSet(8),             // best_prev = 0
            Instruction::Block(BlockType::Empty), // outer block: bump fallback
            Instruction::Block(BlockType::Empty), // scan-exit target
            Instruction::Loop(BlockType::Empty),  // free-list scan
            // if node == 0 → leave the scan loop and evaluate the best fit
            Instruction::LocalGet(2),
            Instruction::I32Eqz,
            Instruction::BrIf(1),
            // node_size = *(node + 4)
            Instruction::LocalGet(2),
            Instruction::I32Const(4),
            Instruction::I32Add,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::LocalSet(3),
            // if node_size >= size and (no best yet or node_size < best_size)
            Instruction::LocalGet(3),
            Instruction::LocalGet(1),
            Instruction::I32GeU,
            Instruction::If(BlockType::Empty),
            Instruction::LocalGet(7),
            Instruction::I32Eqz,
            Instruction::If(BlockType::Empty),
            Instruction::LocalGet(2),
            Instruction::LocalSet(7),
            Instruction::LocalGet(6),
            Instruction::LocalSet(8),
            Instruction::Else,
            Instruction::LocalGet(3),
            Instruction::LocalGet(7),
            Instruction::I32Const(4),
            Instruction::I32Add,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::I32LtU,
            Instruction::If(BlockType::Empty),
            Instruction::LocalGet(2),
            Instruction::LocalSet(7),
            Instruction::LocalGet(6),
            Instruction::LocalSet(8),
            Instruction::End,
            Instruction::End,
            Instruction::End,
            // Advance to the next free-list node.
            Instruction::LocalGet(2),
            Instruction::LocalSet(6),
            Instruction::LocalGet(2),
            Instruction::I32Const(8),
            Instruction::I32Add,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::LocalSet(2),
            Instruction::Br(0),
            Instruction::End, // scan loop
            Instruction::End, // scan-exit block
            // If a suitable block was selected, unlink and return it.
            Instruction::LocalGet(7),
            Instruction::I32Eqz,
            Instruction::If(BlockType::Empty),
            Instruction::Else,
            // if best_prev == 0 { __freelist_head = *(best + 8) } else { *(best_prev + 8) = *(best + 8) }
            Instruction::LocalGet(8),
            Instruction::I32Eqz,
            Instruction::If(BlockType::Empty),
            Instruction::LocalGet(7),
            Instruction::I32Const(8),
            Instruction::I32Add,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::GlobalSet(GLOBAL_FREE_LIST_HEAD),
            Instruction::Else,
            Instruction::LocalGet(8),
            Instruction::I32Const(8),
            Instruction::I32Add,
            Instruction::LocalGet(7),
            Instruction::I32Const(8),
            Instruction::I32Add,
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            Instruction::End,
            // return best + CELL_HEADER_SIZE
            Instruction::LocalGet(7),
            Instruction::I32Const(CELL_HEADER_SIZE),
            Instruction::I32Add,
            Instruction::Return,
            Instruction::End,
            Instruction::End,
            // ── bump path ──────────────────────────────────────────────────
            // base = __heap_ptr
            Instruction::GlobalGet(GLOBAL_HEAP_PTR),
            Instruction::LocalSet(2),
            // new_heap_ptr = base + CELL_HEADER_SIZE + size
            Instruction::LocalGet(2),
            Instruction::I32Const(CELL_HEADER_SIZE),
            Instruction::I32Add,
            Instruction::LocalGet(1),
            Instruction::I32Add,
            Instruction::LocalSet(4),
            // mem_bytes = memory.size << 16
            Instruction::MemorySize(MEMORY_IDX),
            Instruction::I32Const(16),
            Instruction::I32Shl,
            Instruction::LocalSet(5),
            // if new_heap_ptr > mem_bytes → grow
            Instruction::LocalGet(4),
            Instruction::LocalGet(5),
            Instruction::I32GtU,
            Instruction::If(BlockType::Empty),
            // pages = (new_heap_ptr - mem_bytes + 0xFFFF) >> 16
            Instruction::LocalGet(4),
            Instruction::LocalGet(5),
            Instruction::I32Sub,
            Instruction::I32Const(0xFFFF),
            Instruction::I32Add,
            Instruction::I32Const(16),
            Instruction::I32ShrU,
            Instruction::MemoryGrow(MEMORY_IDX),
            Instruction::I32Const(-1),
            Instruction::I32Eq,
            Instruction::If(BlockType::Empty),
            Instruction::Unreachable, // OOM trap
            Instruction::End,
            Instruction::End,
            // __heap_ptr = new_heap_ptr
            Instruction::LocalGet(4),
            Instruction::GlobalSet(GLOBAL_HEAP_PTR),
            // *(base + 0) = CELL_MAGIC
            Instruction::LocalGet(2),
            Instruction::I32Const(CELL_MAGIC),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            // *(base + 4) = size
            Instruction::LocalGet(2),
            Instruction::I32Const(4),
            Instruction::I32Add,
            Instruction::LocalGet(1),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            // return base + CELL_HEADER_SIZE
            Instruction::LocalGet(2),
            Instruction::I32Const(CELL_HEADER_SIZE),
            Instruction::I32Add,
        ]
    }

    /// Body of `__arandu_free(ptr: i32)`.
    ///
    /// Validates the block magic at `ptr - CELL_HEADER_SIZE` and, when it matches,
    /// links the block at the head of the free list. `0` and untracked pointers
    /// are a safe no-op. Local `1` is the block base.
    fn free_body() -> Vec<Instruction<'static>> {
        vec![
            // if ptr == 0 → return
            Instruction::LocalGet(0),
            Instruction::I32Eqz,
            Instruction::If(BlockType::Empty),
            Instruction::Return,
            Instruction::End,
            // base = ptr - CELL_HEADER_SIZE
            Instruction::LocalGet(0),
            Instruction::I32Const(CELL_HEADER_SIZE),
            Instruction::I32Sub,
            Instruction::LocalSet(1),
            // if *(base) != CELL_MAGIC → return (not a tracked block)
            Instruction::LocalGet(1),
            Instruction::I32Load(crate::memory::noffset_memarg()),
            Instruction::I32Const(CELL_MAGIC),
            Instruction::I32Ne,
            Instruction::If(BlockType::Empty),
            Instruction::Return,
            Instruction::End,
            // *(base + 8) = __freelist_head
            Instruction::LocalGet(1),
            Instruction::I32Const(8),
            Instruction::I32Add,
            Instruction::GlobalGet(GLOBAL_FREE_LIST_HEAD),
            Instruction::I32Store(crate::memory::noffset_memarg()),
            // __freelist_head = base
            Instruction::LocalGet(1),
            Instruction::GlobalSet(GLOBAL_FREE_LIST_HEAD),
        ]
    }

    /// Add the mandatory `cabi_realloc` function.
    ///
    /// It delegates to the shared heap allocator: a zero-size request
    /// frees `old_ptr` and returns `0`; if `old_ptr != 0`, existing data is
    /// preserved by copying `min(old_size, new_size)` bytes to the new buffer
    /// and freeing `old_ptr`.
    pub fn add_realloc(&mut self, alloc_func_idx: u32, free_func_idx: u32) {
        self.funcs.push(CabiFunc {
            export_name: Some("cabi_realloc".to_owned()),
            ty: (
                vec![ValType::I32, ValType::I32, ValType::I32, ValType::I32],
                vec![ValType::I32],
            ),
            extra_locals: vec![(2, ValType::I32)],
            body: Self::realloc_body(alloc_func_idx, free_func_idx),
        });
    }

    /// Body of `cabi_realloc(old_ptr, old_size, align, new_size) -> i32`.
    ///
    /// Delegates to the shared heap allocator: a zero-size request frees `old_ptr`
    /// and returns `0`; otherwise a new buffer is allocated, existing data is
    /// preserved by copying `min(old_size, new_size)` bytes, and `old_ptr` is
    /// freed.
    ///
    /// Extra locals: `4` new pointer, `5` copy length.
    fn realloc_body(alloc_func_idx: u32, free_func_idx: u32) -> Vec<Instruction<'static>> {
        vec![
            // 1. If new_size == 0 → free(old_ptr); return 0
            Instruction::LocalGet(3), // new_size
            Instruction::I32Eqz,
            Instruction::If(wasm_encoder::BlockType::Empty),
            Instruction::LocalGet(0), // old_ptr
            Instruction::Call(free_func_idx),
            Instruction::I32Const(0),
            Instruction::Return,
            Instruction::End,
            // 2. new_ptr = __arandu_alloc(new_size)
            Instruction::LocalGet(3), // new_size
            Instruction::Call(alloc_func_idx),
            Instruction::LocalSet(4),
            // 3. If old_ptr != 0 → copy min(old_size, new_size) and free(old_ptr)
            Instruction::LocalGet(0), // old_ptr
            Instruction::I32Const(0),
            Instruction::I32Ne,
            Instruction::If(wasm_encoder::BlockType::Empty),
            // copy_len = if old_size < new_size { old_size } else { new_size }
            Instruction::LocalGet(1), // old_size
            Instruction::LocalGet(3), // new_size
            Instruction::I32LtU,
            Instruction::If(wasm_encoder::BlockType::Result(ValType::I32)),
            Instruction::LocalGet(1),
            Instruction::Else,
            Instruction::LocalGet(3),
            Instruction::End,
            Instruction::LocalSet(5),
            // if copy_len > 0 → memory.copy(new_ptr, old_ptr, copy_len)
            Instruction::LocalGet(5),
            Instruction::I32Const(0),
            Instruction::I32GtU,
            Instruction::If(wasm_encoder::BlockType::Empty),
            Instruction::LocalGet(4), // dst = new_ptr
            Instruction::LocalGet(0), // src = old_ptr
            Instruction::LocalGet(5), // len = copy_len
            Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
            Instruction::End,
            // free(old_ptr)
            Instruction::LocalGet(0),
            Instruction::Call(free_func_idx),
            Instruction::End,
            // 4. Return new_ptr
            Instruction::LocalGet(4),
        ]
    }

    /// Add a `cabi_post_return_<fn_name>` stub for a function that returns
    /// values through linear memory.
    ///
    /// For single-pointer fat returns (`result_types == [ValType::I32]`), the
    /// stub frees the temporary `(ptr, len)` pair cell allocated in linear memory
    /// via `free_func_idx` (`__arandu_free`), reclaiming memory after the host
    /// lifts the result. For other signatures, the stub is a safe no-op.
    pub fn add_post_return(
        &mut self,
        fn_name: &str,
        result_types: Vec<ValType>,
        free_func_idx: u32,
    ) {
        let body = if result_types == [ValType::I32] {
            vec![Instruction::LocalGet(0), Instruction::Call(free_func_idx)]
        } else {
            vec![]
        };
        self.funcs.push(CabiFunc {
            export_name: Some(format!("cabi_post_return_{fn_name}")),
            ty: (result_types, vec![]),
            extra_locals: Vec::new(),
            body,
        });
    }

    /// Number of Canonical ABI functions added so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.funcs.len()
    }

    /// Returns `true` if no Canonical ABI functions have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.funcs.is_empty()
    }

    /// Append type entries to `type_section` and return a `Vec` of their
    /// indices.
    ///
    /// The caller must insert these indices into the `FunctionSection` in the
    /// same order.
    pub fn append_types(
        &self,
        type_section: &mut TypeSection,
        next_type_idx: &mut u32,
    ) -> Vec<u32> {
        let mut indices = Vec::with_capacity(self.funcs.len());
        for func in &self.funcs {
            type_section
                .ty()
                .function(func.ty.0.clone(), func.ty.1.clone());
            indices.push(*next_type_idx);
            *next_type_idx += 1;
        }
        indices
    }

    /// Append function index entries to `func_section`.
    ///
    /// `type_indices` must be the slice returned by [`Self::append_types`].
    pub fn append_function_section(
        &self,
        func_section: &mut FunctionSection,
        type_indices: &[u32],
    ) {
        for &ti in type_indices {
            func_section.function(ti);
        }
    }

    /// Append export entries to `export_section`.
    ///
    /// `first_func_idx` is the Wasm function index of the *first* Canonical
    /// ABI function in the module (they are consecutive). Internal helpers
    /// (`export_name == None`) are skipped.
    pub fn append_exports(&self, export_section: &mut ExportSection, first_func_idx: u32) {
        for (i, func) in self.funcs.iter().enumerate() {
            if let Some(name) = &func.export_name {
                export_section.export(name, ExportKind::Func, first_func_idx + i as u32);
            }
        }
    }

    /// Append code bodies to `code_section`.
    pub fn append_code(&self, code_section: &mut CodeSection) {
        for func in &self.funcs {
            let mut wasm_func = Function::new(func.extra_locals.clone());
            for instr in &func.body {
                wasm_func.instruction(instr);
            }
            wasm_func.instruction(&Instruction::End);
            code_section.function(&wasm_func);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cabi_support_allocator_and_realloc_added() {
        let mut support = CabiSupport::new();
        support.add_allocator();
        support.add_realloc(0, 1);
        assert_eq!(support.len(), 3);
        assert!(!support.is_empty());
        // Internal helpers are first and not exported.
        assert_eq!(support.funcs[0].export_name, None);
        assert_eq!(support.funcs[1].export_name, None);
        assert_eq!(support.funcs[0].ty.0.len(), 1); // alloc(size) → i32
        assert_eq!(support.funcs[0].ty.1.len(), 1);
        assert_eq!(support.funcs[1].ty.0.len(), 1); // free(ptr) → ()
        assert!(support.funcs[1].ty.1.is_empty());
        // cabi_realloc keeps the Canonical ABI signature.
        assert_eq!(
            support.funcs[2].export_name,
            Some("cabi_realloc".to_owned())
        );
        assert_eq!(support.funcs[2].ty.0.len(), 4); // 4 params
        assert_eq!(support.funcs[2].ty.1.len(), 1); // 1 result
    }

    #[test]
    fn cabi_support_post_return_stub() {
        let mut support = CabiSupport::new();
        support.add_allocator();
        support.add_realloc(0, 1);
        support.add_post_return("apply", vec![ValType::I32, ValType::I32], 1);
        assert_eq!(support.len(), 4);
        assert_eq!(
            support.funcs[3].export_name,
            Some("cabi_post_return_apply".to_owned())
        );
        assert_eq!(support.funcs[3].ty.0.len(), 2); // 2 params (the return flat vals)
        assert!(support.funcs[3].ty.1.is_empty()); // no results
        assert!(support.funcs[3].body.is_empty()); // no-op for non-single-pointer
    }

    #[test]
    fn cabi_support_type_indices_sequential() {
        let mut support = CabiSupport::new();
        support.add_allocator();
        support.add_realloc(0, 1);
        support.add_post_return("foo", vec![ValType::I32], 1);
        assert_eq!(support.funcs[3].body.len(), 2); // LocalGet(0) + Call(1)
        let mut ts = TypeSection::new();
        let mut next = 7u32; // simulate 7 prior types in module
        let indices = support.append_types(&mut ts, &mut next);
        assert_eq!(indices, vec![7, 8, 9, 10]);
        assert_eq!(next, 11);
    }
}
