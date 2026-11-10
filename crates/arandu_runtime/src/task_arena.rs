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

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::RefCell;
use std::ptr::NonNull;

/// Default capacity of each linear arena slab (64 KiB).
pub const DEFAULT_CHUNK_CAPACITY: usize = 64 * 1024;

/// Default base alignment of each linear arena slab (cache-line aligned: 64 B).
const DEFAULT_CHUNK_ALIGN: usize = 64;

struct ArenaChunk {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl ArenaChunk {
    fn new(min_capacity: usize, min_align: usize) -> Self {
        let align = min_align.max(DEFAULT_CHUNK_ALIGN).next_power_of_two();
        let capacity = min_capacity
            .max(DEFAULT_CHUNK_CAPACITY)
            .checked_next_multiple_of(align)
            .unwrap_or(min_capacity.max(DEFAULT_CHUNK_CAPACITY));
        let Ok(layout) = Layout::from_size_align(capacity, align) else {
            std::process::abort();
        };
        // SAFETY: `capacity >= DEFAULT_CHUNK_CAPACITY > 0` and `align` is a valid power of two.
        let raw = unsafe { alloc_zeroed(layout) };
        let Some(ptr) = NonNull::new(raw) else {
            handle_alloc_error(layout);
        };
        Self { ptr, layout }
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.layout.size()
    }

    #[inline]
    fn try_alloc(
        &self,
        cursor: usize,
        alloc_size: usize,
        align: usize,
    ) -> Option<(usize, *mut u8)> {
        let base = self.ptr.as_ptr() as usize;
        let current = base.checked_add(cursor)?;
        let mask = align.wrapping_sub(1);
        let aligned = current.checked_add(mask)? & !mask;
        let offset = aligned.checked_sub(base)?;
        let next_cursor = offset.checked_add(alloc_size)?;
        if next_cursor <= self.capacity() {
            // SAFETY: `offset + alloc_size <= self.capacity()`, so `offset` is within the allocation.
            let ptr = unsafe { self.ptr.as_ptr().add(offset) };
            Some((next_cursor, ptr))
        } else {
            None
        }
    }
}

impl Drop for ArenaChunk {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was allocated with `alloc_zeroed(self.layout)`.
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

/// Per-thread linear bump allocator supporting $O(1)$ allocation and loop-scoped
/// checkpoint/rewind.
pub struct TaskArena {
    chunks: Vec<ArenaChunk>,
    current_chunk: usize,
    cursor: usize,
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
            chunks: Vec::new(),
            current_chunk: 0,
            cursor: 0,
        }
    }

    /// Allocates `size` bytes aligned to `align` bytes in $O(1)$ bump-pointer time.
    pub fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        let alloc_size = size.max(1);
        let align = align.max(1).next_power_of_two();

        if !self.chunks.is_empty() {
            if let Some((next_cursor, ptr)) =
                self.chunks[self.current_chunk].try_alloc(self.cursor, alloc_size, align)
            {
                self.cursor = next_cursor;
                return ptr;
            }

            // Before allocating a new slab from the OS, check whether a previously
            // allocated slab (kept across a loop rewind) can satisfy the request.
            while self.current_chunk + 1 < self.chunks.len() {
                self.current_chunk += 1;
                self.cursor = 0;
                if let Some((next_cursor, ptr)) =
                    self.chunks[self.current_chunk].try_alloc(0, alloc_size, align)
                {
                    self.cursor = next_cursor;
                    return ptr;
                }
            }
        }

        let needed_capacity = alloc_size.saturating_add(align);
        let chunk = ArenaChunk::new(needed_capacity, align);
        let Some((next_cursor, ptr)) = chunk.try_alloc(0, alloc_size, align) else {
            std::process::abort();
        };
        self.chunks.push(chunk);
        self.current_chunk = self.chunks.len() - 1;
        self.cursor = next_cursor;
        ptr
    }

    /// Captures the current bump-pointer position as a 64-bit mark `(chunk_idx << 32) | cursor`.
    #[must_use]
    pub fn checkpoint(&self) -> u64 {
        if self.chunks.is_empty() {
            return 0;
        }
        ((self.current_chunk as u64) << 32) | (self.cursor as u64)
    }

    /// Rewinds the arena cursor to a mark previously returned by [`Self::checkpoint`].
    pub fn rewind(&mut self, mark: u64) {
        if self.chunks.is_empty() {
            return;
        }
        let chunk_idx = (mark >> 32) as usize;
        let cursor = (mark & 0xFFFF_FFFF) as usize;
        if chunk_idx < self.chunks.len()
            && cursor <= self.chunks[chunk_idx].capacity()
            && (chunk_idx < self.current_chunk
                || (chunk_idx == self.current_chunk && cursor <= self.cursor))
        {
            self.current_chunk = chunk_idx;
            self.cursor = cursor;
        }
    }

    /// Resets the arena to the start of its first slab, retaining allocated slabs for reuse.
    pub fn reset(&mut self) {
        self.current_chunk = 0;
        self.cursor = 0;
    }

    /// Returns the number of allocated slabs currently held by the arena.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Returns the active byte offset within the current slab.
    #[must_use]
    pub fn current_cursor(&self) -> usize {
        self.cursor
    }
}

thread_local! {
    static TASK_ARENA: RefCell<TaskArena> = const { RefCell::new(TaskArena::new()) };
}

/// Allocates `size` bytes with `align` alignment in the current thread's `TaskArena`.
///
/// Always returns a non-null pointer aligned to `align.max(1).next_power_of_two()`.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_alloc(size: usize, align: usize) -> *mut u8 {
    crate::ffi::guard(|| TASK_ARENA.with_borrow_mut(|arena| arena.alloc(size, align)))
}

/// Returns a 64-bit checkpoint mark representing the current `TaskArena` cursor.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_checkpoint() -> u64 {
    crate::ffi::guard(|| TASK_ARENA.with_borrow(TaskArena::checkpoint))
}

/// Rewinds the current thread's `TaskArena` cursor to `mark`.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_rewind(mark: u64) {
    crate::ffi::guard(|| TASK_ARENA.with_borrow_mut(|arena| arena.rewind(mark)));
}

/// Resets the current thread's `TaskArena` cursor to the beginning of its first slab.
#[unsafe(no_mangle)]
pub extern "C" fn ar_task_arena_reset() {
    crate::ffi::guard(|| TASK_ARENA.with_borrow_mut(TaskArena::reset));
}
