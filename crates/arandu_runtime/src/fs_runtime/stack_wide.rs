//! Stack-allocated wide (UTF-16) buffer with heap fallback for Win32 paths.
//!
//! Avoids heap allocation for 99%+ of filesystem path components (<= 256 chars),
//! eliminating malloc/free churn in directory traversal and capability checks.

/// Stack-allocated wide buffer for Windows NT Unicode strings.
///
/// Default capacity `N = 260` covers standard `MAX_PATH` without heap allocation.
#[derive(Debug)]
pub(crate) struct StackWideBuffer<const N: usize = 260> {
    inline: [u16; N],
    heap: Vec<u16>,
    len: usize,
}

impl<const N: usize> StackWideBuffer<N> {
    /// Encodes a UTF-8 string into UTF-16 without NUL termination.
    ///
    /// Returns `None` if the text contains more than 32,767 code units (the NT maximum).
    pub fn from_str(text: &str) -> Option<Self> {
        let mut buf = Self {
            inline: [0u16; N],
            heap: Vec::new(),
            len: 0,
        };
        for unit in text.encode_utf16() {
            buf.push(unit)?;
        }
        Some(buf)
    }

    /// Encodes a UTF-8 string into UTF-16 and appends a trailing NUL (0).
    ///
    /// Useful for Win32 APIs like `CreateFileW` that require null-terminated strings.
    pub fn from_str_nul(text: &str) -> Option<Self> {
        let mut buf = Self::from_str(text)?;
        buf.push(0)?;
        Some(buf)
    }

    #[inline]
    fn push(&mut self, unit: u16) -> Option<()> {
        if self.len >= 32_767 {
            return None;
        }
        if self.len < N {
            self.inline[self.len] = unit;
        } else {
            if self.heap.is_empty() {
                self.heap.reserve(N * 2);
                self.heap.extend_from_slice(&self.inline[..N]);
            }
            self.heap.push(unit);
        }
        self.len += 1;
        Some(())
    }

    #[inline]
    #[allow(dead_code)]
    pub fn as_slice(&self) -> &[u16] {
        if self.heap.is_empty() {
            &self.inline[..self.len]
        } else {
            &self.heap
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn as_ptr(&self) -> *const u16 {
        if self.heap.is_empty() {
            self.inline.as_ptr()
        } else {
            self.heap.as_ptr()
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn as_mut_ptr(&mut self) -> *mut u16 {
        if self.heap.is_empty() {
            self.inline.as_mut_ptr()
        } else {
            self.heap.as_mut_ptr()
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_heap_allocated(&self) -> bool {
        !self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_strings_stay_on_stack() {
        let short = "simple_component.aru";
        let buf = StackWideBuffer::<260>::from_str(short).expect("valid utf8");
        assert!(!buf.is_heap_allocated());
        assert_eq!(buf.len(), short.encode_utf16().count());

        let expected: Vec<u16> = short.encode_utf16().collect();
        assert_eq!(buf.as_slice(), &expected[..]);
    }

    #[test]
    fn small_strings_with_nul_stay_on_stack() {
        let short = "path/to/file";
        let buf = StackWideBuffer::<260>::from_str_nul(short).expect("valid utf8");
        assert!(!buf.is_heap_allocated());
        assert_eq!(buf.len(), short.encode_utf16().count() + 1);

        let mut expected: Vec<u16> = short.encode_utf16().collect();
        expected.push(0);
        assert_eq!(buf.as_slice(), &expected[..]);
    }

    #[test]
    fn strings_exceeding_capacity_fallback_to_heap() {
        // N = 8 to trigger heap fallback quickly
        let text = "abcdefghijklmnop"; // 16 chars
        let buf = StackWideBuffer::<8>::from_str(text).expect("valid utf8");
        assert!(buf.is_heap_allocated());
        assert_eq!(buf.len(), 16);

        let expected: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(buf.as_slice(), &expected[..]);
    }

    #[test]
    fn strings_exceeding_capacity_with_nul_fallback_to_heap() {
        let text = "abcdefghijklmnop";
        let buf = StackWideBuffer::<8>::from_str_nul(text).expect("valid utf8");
        assert!(buf.is_heap_allocated());
        assert_eq!(buf.len(), 17);

        let mut expected: Vec<u16> = text.encode_utf16().collect();
        expected.push(0);
        assert_eq!(buf.as_slice(), &expected[..]);
    }

    #[test]
    fn empty_string_is_zero_len_and_on_stack() {
        let buf = StackWideBuffer::<260>::from_str("").expect("valid utf8");
        assert!(!buf.is_heap_allocated());
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert_eq!(buf.as_slice(), &[]);
    }
}
