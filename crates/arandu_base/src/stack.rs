//! Stack growth for the compiler's recursive descents.
//!
//! Expressions, patterns and blocks nest as deeply as a program writes them,
//! and every phase walks them recursively. A generated or transpiled program
//! can chain thousands of operands; a Windows main thread has 1 MiB of stack
//! and a debug build spends kilobytes per level. Like rustc's
//! `ensure_sufficient_stack`, a recursive entry point checks the stack left
//! and continues on a fresh segment when it runs low, so the depth a program
//! may reach does not depend on the host's thread stack.

/// Below this much stack left, the next call runs on a new segment.
const RED_ZONE: usize = 128 * 1024;

/// Size of each new segment.
const SEGMENT: usize = 2 * 1024 * 1024;

/// Runs `f`, first moving to a fresh stack segment if the current one has
/// less than `RED_ZONE` bytes left. On targets where the stack cannot be
/// inspected, `f` runs where it is.
#[inline]
pub fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}

#[cfg(test)]
mod tests {
    use super::ensure_sufficient_stack;

    fn depth(n: u32) -> u32 {
        // A large frame per level, as a debug build of a dispatcher has.
        let pad = std::hint::black_box([0u8; 8 * 1024]);
        if n == 0 {
            return u32::from(pad[0]);
        }
        ensure_sufficient_stack(|| depth(n - 1)) + 1
    }

    #[test]
    fn deep_recursion_outgrows_a_small_thread_stack() {
        // 4096 levels of 8 KiB need 32 MiB; the thread has 256 KiB.
        let levels = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| depth(4096))
            .expect("spawn")
            .join()
            .expect("no stack overflow");
        assert_eq!(levels, 4096);
    }
}
