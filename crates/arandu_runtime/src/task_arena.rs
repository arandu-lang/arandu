//! Linear per-task region allocator (`TaskArena`) for the Dual-Locus Semantic
//! Memory Model (RFC 0043).
//!
//! When OSSA escape analysis proves that a local variable's address escapes its
//! defining stack frame through an aggregate carrier or indirect store, the
//! compiler promotes the variable's **Storage Locus (`\lambda_store`)** to the
//! thread-local `TaskArena` while preserving its **Reference Locus
//! (`\lambda_ref`)** as a direct native pointer (`*T`).
//!
//! Allocation is $O(1)$ bump-pointer arithmetic over 64 KiB slabs, and loops
//! that promote iteration-scoped values use `ar_task_arena_checkpoint` and
//! `ar_task_arena_rewind` to recycle slab space in $O(1)$ without heap traffic.

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::cell::UnsafeCell;
use std::ptr::NonNull;

/// Default capacity of the first linear arena slab (64 KiB).
pub const DEFAULT_CHUNK_CAPACITY: usize = 64 * 1024;

/// Maximum automatic slab size under geometric growth (4 MiB).
const MAX_GROWTH_CHUNK_CAPACITY: usize = 4 * 1024 * 1024;

/// Default base alignment of each linear arena slab (cache-line aligned: 64 B).
const DEFAULT_CHUNK_ALIGN: usize = 64;

struct ArenaChunk {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl ArenaChunk {
    fn new(min_capacity: usize, min_align: usize) -> Self {
        let Some(align) = min_align
            .max(DEFAULT_CHUNK_ALIGN)
            .checked_next_power_of_two()
        else {
            std::process::abort();
        };
        let capacity = min_capacity
            .max(DEFAULT_CHUNK_CAPACITY)
            .checked_next_multiple_of(align)
            .unwrap_or(min_capacity.max(DEFAULT_CHUNK_CAPACITY));
        let Ok(layout) = Layout::from_size_align(capacity, align) else {
            std::process::abort();
        };
        // SAFETY: `capacity >= DEFAULT_CHUNK_CAPACITY > 0` and `align` is a valid power of two.
        // Promoted Dual-Locus slots are initialized before any read by AMIR Store statements.
        let raw = unsafe { alloc(layout) };
        let Some(ptr) = NonNull::new(raw) else {
            handle_alloc_error(layout);
        };
        Self { ptr, layout }
    }

    #[inline(always)]
    fn base(&self) -> usize {
        self.ptr.as_ptr() as usize
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.layout.size()
    }
}

impl Drop for ArenaChunk {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was allocated with `alloc(self.layout)`.
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

/// Per-thread linear bump allocator supporting $O(1)$ allocation and loop-scoped
/// checkpoint/rewind.
///
/// Caches the active slab's `[cur, end, base)` pointers inline so the common
/// bump-pointer fast path executes in a handful of instructions without `Vec`
/// indexing or overflow-checked division. `#[repr(C)]` fixes `cur` at offset 0
/// and `end` at offset `size_of::<usize>()` for LLVM's inline fast path.
#[repr(C)]
pub struct TaskArena {
    cur: usize,
    end: usize,
    base: usize,
    current_chunk: usize,
    chunks: Vec<ArenaChunk>,
}

impl Default for TaskArena {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskArena {
    /// Creates an empty `TaskArena` that allocates its first 64 KiB slab on demand.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cur: 0,
            end: 0,
            base: 0,
            current_chunk: 0,
            chunks: Vec::new(),
        }
    }

    #[inline(always)]
    fn set_active_chunk(&mut self, chunk_idx: usize, offset: usize) {
        let chunk = &self.chunks[chunk_idx];
        let base = chunk.base();
        self.current_chunk = chunk_idx;
        self.base = base;
        self.cur = base + offset;
        self.end = base + chunk.capacity();
    }

    /// Allocates `size` bytes aligned to `align` bytes in $O(1)$ bump-pointer time.
    #[inline(always)]
    pub fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        let alloc_size = if size == 0 { 1 } else { size };
        let align = if align.is_power_of_two() {
            align
        } else {
            let Some(pow2) = align.max(1).checked_next_power_of_two() else {
                std::process::abort();
            };
            pow2
        };
        let mask = align.wrapping_sub(1);
        let cur = self.cur;
        let aligned = cur.wrapping_add(mask) & !mask;
        let next = aligned.wrapping_add(alloc_size);
        if aligned >= cur && next >= aligned && next <= self.end {
            self.cur = next;
            return aligned as *mut u8;
        }
        self.alloc_slow(alloc_size, align)
    }

    #[cold]
    #[inline(never)]
    fn alloc_slow(&mut self, alloc_size: usize, align: usize) -> *mut u8 {
        let mask = align.wrapping_sub(1);

        // Before allocating a new slab from the OS, check whether a previously
        // allocated slab (kept across a loop rewind) can satisfy the request.
        if !self.chunks.is_empty() {
            while self.current_chunk + 1 < self.chunks.len() {
                let next_idx = self.current_chunk + 1;
                let base = self.chunks[next_idx].base();
                let end = base + self.chunks[next_idx].capacity();
                let aligned = base.wrapping_add(mask) & !mask;
                let next = aligned.wrapping_add(alloc_size);
                if aligned >= base && next >= aligned && next <= end {
                    self.current_chunk = next_idx;
                    self.base = base;
                    self.cur = next;
                    self.end = end;
                    return aligned as *mut u8;
                }
                self.current_chunk = next_idx;
            }
        }

        let prev_cap = self
            .chunks
            .last()
            .map_or(DEFAULT_CHUNK_CAPACITY / 2, ArenaChunk::capacity);
        let grown_cap = prev_cap.saturating_mul(2).min(MAX_GROWTH_CHUNK_CAPACITY);
        let needed_capacity = alloc_size.saturating_add(align).max(grown_cap);
        let chunk = ArenaChunk::new(needed_capacity, align);
        let base = chunk.base();
        let end = base + chunk.capacity();
        let aligned = base.wrapping_add(mask) & !mask;
        let next = aligned.wrapping_add(alloc_size);
        if aligned < base || next < aligned || next > end {
            std::process::abort();
        }
        self.chunks.push(chunk);
        self.current_chunk = self.chunks.len() - 1;
        self.base = base;
        self.cur = next;
        self.end = end;
        aligned as *mut u8
    }

    /// Captures the current bump-pointer position as a 64-bit mark `(chunk_idx << 32) | cursor`.
    #[must_use]
    #[inline(always)]
    pub fn checkpoint(&self) -> u64 {
        if self.chunks.is_empty() {
            return 0;
        }
        let cursor = self.cur.wrapping_sub(self.base);
        ((self.current_chunk as u64) << 32) | (cursor as u64)
    }

    /// Rewinds the arena cursor to a mark previously returned by [`Self::checkpoint`].
    #[inline(always)]
    pub fn rewind(&mut self, mark: u64) {
        if self.chunks.is_empty() {
            return;
        }
        let chunk_idx = (mark >> 32) as usize;
        let cursor = (mark & 0xFFFF_FFFF) as usize;
        if chunk_idx == self.current_chunk {
            if cursor <= self.cur.saturating_sub(self.base) {
                self.cur = self.base + cursor;
            }
            return;
        }
        if chunk_idx < self.current_chunk && cursor <= self.chunks[chunk_idx].capacity() {
            self.set_active_chunk(chunk_idx, cursor);
        }
    }

    /// Resets the arena to the start of its first slab, retaining allocated slabs for reuse.
    #[inline(always)]
    pub fn reset(&mut self) {
        if self.chunks.is_empty() {
            self.current_chunk = 0;
            self.base = 0;
            self.cur = 0;
            self.end = 0;
        } else {
            self.set_active_chunk(0, 0);
        }
    }

    /// Returns the number of allocated slabs currently held by the arena.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Returns the active byte offset within the current slab.
    #[must_use]
    pub fn current_cursor(&self) -> usize {
        self.cur.wrapping_sub(self.base)
    }
}

thread_local! {
    static TASK_ARENA: UnsafeCell<TaskArena> = const { UnsafeCell::new(TaskArena::new()) };
}

/// Returns a pointer to the current thread's `TaskArena` `[cur, end]` pair
/// for inline bump-pointer allocation in LLVM-compiled code.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_fast_state() -> *mut usize {
    // SAFETY: `TASK_ARENA` is thread-local and `TaskArena` is `#[repr(C)]`
    // with `cur: usize` at offset 0 and `end: usize` at offset `size_of::<usize>()`.
    TASK_ARENA.with(|cell| cell.get().cast::<usize>())
}

/// Allocates `size` bytes with `align` alignment in the current thread's `TaskArena`.
///
/// Always returns a non-null pointer aligned to `align.max(1).next_power_of_two()`.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_alloc(size: usize, align: usize) -> *mut u8 {
    // SAFETY: `TASK_ARENA` is thread-local and `TaskArena::alloc` is a leaf
    // operation that never re-enters user code or unwinds across FFI.
    TASK_ARENA.with(|cell| unsafe { (&mut *cell.get()).alloc(size, align) })
}

/// Returns a 64-bit checkpoint mark representing the current `TaskArena` cursor.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_checkpoint() -> u64 {
    // SAFETY: `TASK_ARENA` is thread-local and `checkpoint` is a non-reentrant leaf.
    TASK_ARENA.with(|cell| unsafe { (&*cell.get()).checkpoint() })
}

/// Rewinds the current thread's `TaskArena` cursor to `mark`.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_rewind(mark: u64) {
    // SAFETY: `TASK_ARENA` is thread-local and `rewind` is a non-reentrant leaf.
    TASK_ARENA.with(|cell| unsafe { (&mut *cell.get()).rewind(mark) });
}

/// Resets the current thread's `TaskArena` cursor to the beginning of its first slab.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_reset() {
    // SAFETY: `TASK_ARENA` is thread-local and `reset` is a non-reentrant leaf.
    TASK_ARENA.with(|cell| unsafe { (&mut *cell.get()).reset() });
}
