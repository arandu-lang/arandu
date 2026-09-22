//! Host symbols for `std.alloc.vec` (two layers — do not conflate).
//!
//! ## Canonical path (L6.1 pure-buffer)
//! Language code in `stdlib/alloc/vec.aru` uses only:
//! - `ar_vec_malloc` / `ar_vec_realloc` / `ar_vec_buf_free` (raw bytes)
//! - mem intrinsics (`sizeOf` / `ptrOffset` / `ptrRead` / `ptrWrite`) for typed access
//!
//! ## Legacy handle API (GenArena-style table)
//! `ar_vec_new` / `ar_vec_push` / `ar_vec_get` / … remain registered for JIT
//! unit tests and any residual host-table experiments. **They are not the
//! stdlib surface.** Prefer pure-buffer when changing product behaviour.
//!
//! Elements on the handle API are **i64 bit patterns**. Typed Drop / non-int
//! payloads remain post-Minimal.
//!
//! # Safety
//! All `pub unsafe extern "C"` entry points are ABI host functions invoked only
//! from JIT-compiled Arandu code (or unit tests). Handles are opaque indices
//! into the process-local table; invalid ids are treated as no-ops or abort
//! on write paths that would corrupt storage.

use std::sync::Mutex;

struct Slot {
    data: Vec<i64>,
}

static VECS: Mutex<Vec<Option<Slot>>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<Slot>>> {
    VECS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create an empty vector; returns handle `>= 0`.
///
/// # Safety
/// Safe to call from any thread; acquires process-local synchronized table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_new() -> i64 {
    let mut g = lock();
    let slot = Slot { data: Vec::new() };
    if let Some(idx) = g.iter().position(|s| s.is_none()) {
        g[idx] = Some(slot);
        return idx as i64;
    }
    let id = g.len();
    g.push(Some(slot));
    id as i64
}

/// Push `value` onto vector `id`. Invalid id aborts.
///
/// # Safety
/// `id` must be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_push(id: i64, value: i64) {
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        std::process::abort();
    };
    slot.data.push(value);
}

/// Length of vector `id`, or `usize::MAX` if invalid.
///
/// # Safety
/// `id` should be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_len(id: i64) -> usize {
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) => slot.data.len(),
        None => usize::MAX, // sentinel para inválido
    }
}

/// `1` if `index` is in range, else `0`. Invalid id → `0`.
///
/// # Safety
/// `id` should be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_has(id: i64, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) if (index as usize) < slot.data.len() => 1,
        _ => 0,
    }
}

/// Get element at `index`. Invalid / OOB → `0` (check [`ar_vec_has`] first).
///
/// # Safety
/// `id` should be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_get(id: i64, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) => slot.data.get(index as usize).copied().unwrap_or(0),
        None => 0,
    }
}

/// Overwrite index; returns `1` on success, `0` on OOB/invalid.
///
/// # Safety
/// `id` should be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_put(id: i64, index: i64, value: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        return 0;
    };
    match slot.data.get_mut(index as usize) {
        Some(cell) => {
            *cell = value;
            1
        }
        None => 0,
    }
}

/// Pop last element and return it. Caller must ensure non-empty (`ar_vec_len > 0`).
/// Empty / invalid → `0` (ambiguous with a stored zero — check length first).
///
/// # Safety
/// `id` must be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_pop(id: i64) -> i64 {
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        return 0;
    };
    slot.data.pop().unwrap_or(0)
}

/// Set length to 0; capacity retained.
///
/// # Safety
/// `id` should be a valid vector handle allocated by [`ar_vec_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_clear(id: i64) {
    let mut g = lock();
    if let Some(Some(slot)) = g.get_mut(id as usize) {
        slot.data.clear();
    }
}

/// Destroy handle and free storage.
///
/// # Safety
/// `id` must be a valid vector handle allocated by [`ar_vec_new`] and must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_destroy(id: i64) {
    let mut g = lock();
    if (id as usize) < g.len() {
        g[id as usize] = None;
    }
}

// ── Raw buffer helpers for pure-Arandu Vec growth (L6.1) ─────────────────

/// Default 8-byte alignment required by JIT raw buffers and vectors.
const VEC_BUFFER_ALIGN: usize = 8;

/// Allocate `size` bytes (8-aligned). Null on OOM / invalid size.
///
/// # Safety
/// JIT host only; free with [`ar_vec_buf_free`] using the same size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_malloc(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }
    let layout = match std::alloc::Layout::from_size_align(size, VEC_BUFFER_ALIGN) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    unsafe { std::alloc::alloc(layout) }
}

/// Copy one compiler-proven `Copy` value into initialized chunk storage.
///
/// # Safety
/// Both pointers must be valid for `size` bytes, properly aligned for their
/// concrete type, and non-overlapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_copy_value(dest: *mut u8, source: *const u8, size: usize) {
    if size != 0 && !dest.is_null() && !source.is_null() {
        // SAFETY: guaranteed by the ABI contract above.
        unsafe { std::ptr::copy_nonoverlapping(source, dest, size) };
    }
}

/// Allocate a raw buffer with the target-provided size and alignment.
/// Returns null for invalid layouts or allocation failure.
///
/// # Safety
/// The returned pointer must be released exactly once with
/// [`ar_rt_free_aligned`] using the same `size` and `align`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_alloc_aligned(size: usize, align: usize) -> *mut u8 {
    let layout = match std::alloc::Layout::from_size_align(size, align) {
        Ok(layout) if size != 0 => layout,
        _ => return std::ptr::null_mut(),
    };
    // SAFETY: layout was validated above.
    unsafe { std::alloc::alloc(layout) }
}

/// Release a buffer returned by [`ar_rt_alloc_aligned`].
///
/// # Safety
/// `pointer`, `size`, and `align` must describe the same successful allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_free_aligned(pointer: *mut u8, size: usize, align: usize) {
    if pointer.is_null() || size == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(size, align) {
        // SAFETY: guaranteed by the ABI contract above.
        unsafe { std::alloc::dealloc(pointer, layout) };
    }
}

/// Free buffer from [`ar_vec_malloc`].
///
/// # Safety
/// `p`/`size` must match a prior `ar_vec_malloc` pair (or `p` null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_buf_free(p: *mut u8, size: usize) {
    if p.is_null() || size == 0 {
        return;
    }
    let layout = match std::alloc::Layout::from_size_align(size, VEC_BUFFER_ALIGN) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe { std::alloc::dealloc(p, layout) }
}

/// Grow/shrink raw buffer; copies `min(old,new)` bytes.
///
/// # Safety
/// Same as malloc/free pair.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_realloc(p: *mut u8, old_size: usize, new_size: usize) -> *mut u8 {
    if new_size == 0 {
        unsafe { ar_vec_buf_free(p, old_size) };
        return std::ptr::null_mut();
    }
    let new_ptr = unsafe { ar_vec_malloc(new_size) };
    if new_ptr.is_null() {
        return std::ptr::null_mut();
    }
    if !p.is_null() && old_size > 0 {
        let n = std::cmp::min(old_size, new_size);
        unsafe {
            std::ptr::copy_nonoverlapping(p, new_ptr, n);
            ar_vec_buf_free(p, old_size);
        }
    } else if p.is_null() && old_size > 0 {
        // Caller capacity out of sync with data (mut writeback partial) — treat
        // as fresh alloc without free/copy of a null base.
    }
    new_ptr
}

#[repr(C)]
pub struct ArOwnedString {
    pub data: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

/// Append a UTF-8 byte sequence to the owned String buffer.
/// Returns 0 on overflow/OOM and leaves the original value unchanged.
///
/// # Safety
/// `string` must point to the std.alloc.String layout and `value` must be
/// readable for `value_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_string_push_str(
    string: *mut ArOwnedString,
    value: *const u8,
    value_len: usize,
) -> i8 {
    let Some(string) = (unsafe { string.as_mut() }) else {
        return 0;
    };
    if value_len > 0 && value.is_null() {
        return 0;
    }
    let Some(required) = string.len.checked_add(value_len) else {
        return 0;
    };
    if required > i32::MAX as usize {
        return 0;
    }
    let source_offset = if value_len > 0 && !string.data.is_null() {
        let buffer_start = string.data as usize;
        let source_start = value as usize;
        buffer_start
            .checked_add(string.capacity)
            .filter(|&buffer_end| source_start >= buffer_start && source_start < buffer_end)
            .map(|_| source_start - buffer_start)
    } else {
        None
    };
    if source_offset
        .is_some_and(|offset| offset > string.len || value_len > string.len.saturating_sub(offset))
    {
        return 0;
    }
    if required > string.capacity {
        let mut capacity = string.capacity.max(8);
        while capacity < required {
            let Some(doubled) = capacity.checked_mul(2) else {
                capacity = required;
                break;
            };
            capacity = doubled.min(i32::MAX as usize);
            if capacity == i32::MAX as usize && capacity < required {
                return 0;
            }
        }
        let replacement = unsafe { ar_vec_realloc(string.data, string.capacity, capacity) };
        if replacement.is_null() {
            return 0;
        }
        string.data = replacement;
        string.capacity = capacity;
    }
    if value_len > 0 {
        let source = source_offset.map_or(value, |offset| unsafe { string.data.add(offset) });
        unsafe {
            // `value` may be a borrowed view into this same String. `copy`
            // handles overlap, while `source_offset` rebases that view if
            // the preceding reallocation moved the owned buffer.
            std::ptr::copy(source, string.data.add(string.len), value_len);
        }
    }
    string.len = required;
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_get_len_free() {
        unsafe {
            let id = ar_vec_new();
            ar_vec_push(id, 10);
            ar_vec_push(id, 20);
            assert_eq!(ar_vec_len(id), 2);
            assert_eq!(ar_vec_has(id, 0), 1);
            assert_eq!(ar_vec_get(id, 0), 10);
            assert_eq!(ar_vec_get(id, 1), 20);
            assert_eq!(ar_vec_put(id, 1, 5), 1);
            assert_eq!(ar_vec_get(id, 1), 5);
            assert_eq!(ar_vec_pop(id), 5);
            assert_eq!(ar_vec_len(id), 1);
            ar_vec_destroy(id);
            assert_eq!(ar_vec_len(id), usize::MAX);
        }
    }

    #[test]
    fn string_push_str_is_utf8_preserving_and_failure_atomic() {
        unsafe {
            let mut string = ArOwnedString {
                data: std::ptr::null_mut(),
                len: 0,
                capacity: 0,
            };
            let first = "olá".as_bytes();
            assert_eq!(
                ar_string_push_str(&mut string, first.as_ptr(), first.len()),
                1
            );
            assert_eq!(string.len, 4);
            assert_eq!(std::slice::from_raw_parts(string.data, string.len), first);

            let before = (string.data, string.len, string.capacity);
            assert_eq!(
                ar_string_push_str(&mut string, first.as_ptr(), usize::MAX),
                0
            );
            assert_eq!((string.data, string.len, string.capacity), before);

            ar_vec_buf_free(string.data, string.capacity);
        }
    }

    #[test]
    fn string_push_str_supports_appending_its_own_buffer_across_growth() {
        unsafe {
            let mut string = ArOwnedString {
                data: ar_vec_malloc(3),
                len: 3,
                capacity: 3,
            };
            assert!(!string.data.is_null());
            std::ptr::copy_nonoverlapping(b"abc".as_ptr(), string.data, 3);

            let source = string.data;
            assert_eq!(ar_string_push_str(&mut string, source, 3), 1);
            assert_eq!(
                std::slice::from_raw_parts(string.data, string.len),
                b"abcabc"
            );
            ar_vec_buf_free(string.data, string.capacity);
        }
    }

    #[test]
    fn aligned_buffer_honors_layout_and_supports_copy_values() {
        unsafe {
            let source = [0xA5_u8; 96];
            let buffer = ar_rt_alloc_aligned(source.len(), 64);
            assert!(!buffer.is_null());
            assert_eq!((buffer as usize) % 64, 0);

            ar_rt_copy_value(buffer, source.as_ptr(), source.len());
            assert_eq!(
                std::slice::from_raw_parts(buffer, source.len()),
                source.as_slice()
            );
            ar_rt_free_aligned(buffer, source.len(), 64);
        }
    }

    #[test]
    fn aligned_buffer_rejects_zero_and_invalid_alignment() {
        unsafe {
            assert!(ar_rt_alloc_aligned(0, 8).is_null());
            assert!(ar_rt_alloc_aligned(8, 0).is_null());
            assert!(ar_rt_alloc_aligned(8, 3).is_null());
        }
    }
}
