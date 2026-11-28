//! RAII guard for W^X memory protection on Apple Silicon (macOS aarch64).
//!
//! Under macOS Hardened Runtime, threads cannot write to executable memory
//! without explicitly toggling thread-level write permission via
//! `pthread_jit_write_protect_np`. On other platforms, this compiles down to
//! a zero-cost, zero-sized type (ZST).

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) struct JitWriteScope;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl JitWriteScope {
    #[inline(always)]
    pub(crate) fn enter() -> Self {
        unsafe {
            unsafe extern "C" {
                fn pthread_jit_write_protect_np(enabled: i32);
            }
            pthread_jit_write_protect_np(0); // 0 = write enabled
        }
        Self
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for JitWriteScope {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            unsafe extern "C" {
                fn pthread_jit_write_protect_np(enabled: i32);
            }
            pthread_jit_write_protect_np(1); // 1 = write disabled, execute enabled
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub(crate) struct JitWriteScope;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl JitWriteScope {
    #[inline(always)]
    pub(crate) fn enter() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_lifecycle_runs_safely() {
        let _guard = JitWriteScope::enter();
        // Simulates JIT memory modification inside the write-enabled scope.
    }
}
