//! Immutable literal backing and checked logical views, never host addresses.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A frozen literal/view can outlive evaluation: the residual materializer
/// interns its visible bytes into the destination's static literal storage.
#[derive(Debug, Clone)]
pub struct ConstString {
    backing: Arc<str>,
    start: usize,
    len: usize,
}

/// Owned backing of a frozen byte sequence.
///
/// `b"..."` literals keep their decoded bytes verbatim, so the backing is not
/// necessarily UTF-8 (e.g. `b"\xFF"`). String views still carry a `str`
/// allocation, which keeps `ConstString::as_str` infallible.
#[derive(Debug, Clone)]
enum BytesBacking {
    Utf8(Arc<str>),
    Raw(Arc<[u8]>),
}

impl BytesBacking {
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Utf8(backing) => backing.as_bytes(),
            Self::Raw(backing) => backing,
        }
    }
}

/// Byte views may start inside a Unicode codepoint; conversion back to `str`
/// must validate UTF-8. Their backing is immutable and owned, not a VM pointer.
#[derive(Debug, Clone)]
pub struct ConstBytes {
    backing: BytesBacking,
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringError {
    Bounds,
    InvalidUtf8,
}

impl PartialEq for ConstString {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for ConstString {}
impl Hash for ConstString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}
impl PartialEq for ConstBytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}
impl Eq for ConstBytes {}
impl Hash for ConstBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl ConstString {
    #[must_use]
    pub fn new(backing: impl Into<Arc<str>>) -> Self {
        let backing = backing.into();
        Self {
            len: backing.len(),
            backing,
            start: 0,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // All constructors below retain checked UTF-8 boundaries.
        &self.backing[self.start..self.start + self.len]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Full retained allocation, including bytes outside a visible subview.
    #[must_use]
    pub fn backing_len(&self) -> usize {
        self.backing.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn view(&self, start: usize, len: usize) -> Result<Self, StringError> {
        let end = start
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or(StringError::Bounds)?;
        if !self.as_str().is_char_boundary(start) || !self.as_str().is_char_boundary(end) {
            return Err(StringError::InvalidUtf8);
        }
        let start = self.start.checked_add(start).ok_or(StringError::Bounds)?;
        Ok(Self {
            backing: Arc::clone(&self.backing),
            start,
            len,
        })
    }

    #[must_use]
    pub fn bytes(&self) -> ConstBytes {
        ConstBytes {
            backing: BytesBacking::Utf8(Arc::clone(&self.backing)),
            start: self.start,
            len: self.len,
        }
    }

    /// Identity is content, not the original backing/view offset. Equal frozen
    /// values produced from different literals must cut off downstream equally.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(10 + self.len);
        bytes.extend_from_slice(&[1, 4]);
        bytes.extend_from_slice(&(self.len as u64).to_le_bytes());
        bytes.extend_from_slice(self.as_str().as_bytes());
        bytes
    }
}

impl ConstBytes {
    /// Frozen bytes that are not necessarily UTF-8 (`b"\xFF"`).
    #[must_use]
    pub fn from_raw(bytes: impl Into<Arc<[u8]>>) -> Self {
        let backing = bytes.into();
        Self {
            len: backing.len(),
            backing: BytesBacking::Raw(backing),
            start: 0,
        }
    }

    /// Full retained allocation, including bytes outside a visible subview.
    #[must_use]
    pub fn backing_bytes(&self) -> &[u8] {
        self.backing.as_bytes()
    }

    /// `Some` when the backing is a UTF-8 allocation, which lets callers reuse
    /// the interned string literal instead of copying raw bytes.
    #[must_use]
    pub fn backing_str(&self) -> Option<&str> {
        match &self.backing {
            BytesBacking::Utf8(backing) => Some(backing),
            BytesBacking::Raw(_) => None,
        }
    }

    #[must_use]
    pub const fn start(&self) -> usize {
        self.start
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.backing.as_bytes()[self.start..self.start + self.len]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn view(&self, start: usize, len: usize) -> Result<Self, StringError> {
        start
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or(StringError::Bounds)?;
        let start = self.start.checked_add(start).ok_or(StringError::Bounds)?;
        Ok(Self {
            backing: self.backing.clone(),
            start,
            len,
        })
    }

    pub fn to_string_view(&self) -> Result<ConstString, StringError> {
        let bytes = self.as_bytes();
        let text = std::str::from_utf8(bytes).map_err(|_| StringError::InvalidUtf8)?;
        Ok(match &self.backing {
            BytesBacking::Utf8(backing) => ConstString {
                backing: Arc::clone(backing),
                start: self.start,
                len: self.len,
            },
            // Raw backings have no `str` allocation to borrow; validation above
            // makes the copy sound.
            BytesBacking::Raw(_) => ConstString::new(text),
        })
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(10 + self.len);
        bytes.extend_from_slice(&[1, 5]);
        bytes.extend_from_slice(&(self.len as u64).to_le_bytes());
        bytes.extend_from_slice(self.as_bytes());
        bytes
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn unicode_nul_and_views_have_checked_owned_backing() {
        let text = ConstString::new("Olá\0🦀");
        assert_eq!(text.len(), 9);
        assert_eq!(text.view(2, 2).unwrap().as_str(), "á");
        assert_eq!(text.view(3, 1), Err(StringError::InvalidUtf8));
        assert_eq!(text.view(9, 0).unwrap().as_str(), "");
        assert_eq!(text.view(usize::MAX, 1), Err(StringError::Bounds));
        let bytes = text.bytes();
        assert_eq!(bytes.view(3, 1).unwrap().as_bytes(), &[0xa1]);
        assert_eq!(
            bytes.view(3, 1).unwrap().to_string_view(),
            Err(StringError::InvalidUtf8)
        );
        assert_eq!(
            text.view(2, 2).unwrap().canonical_bytes(),
            ConstString::new("á").canonical_bytes()
        );
        assert_eq!(text.view(2, 2).unwrap(), ConstString::new("á"));
        drop(text);
        assert_eq!(
            bytes.view(5, 4).unwrap().to_string_view().unwrap().as_str(),
            "🦀"
        );
    }

    #[test]
    fn raw_backing_keeps_non_utf8_bytes_with_checked_views() {
        let bytes = ConstBytes::from_raw(vec![0xFF, 0x00, 0x41, 0x42]);
        assert_eq!(bytes.backing_bytes(), &[0xFF, 0x00, 0x41, 0x42]);
        assert_eq!(bytes.as_bytes(), &[0xFF, 0x00, 0x41, 0x42]);
        assert_eq!(bytes.len(), 4);
        assert!(bytes.backing_str().is_none());
        assert_eq!(bytes.to_string_view(), Err(StringError::InvalidUtf8));
        assert_eq!(bytes.view(5, 0), Err(StringError::Bounds));
        assert_eq!(bytes.view(usize::MAX, 1), Err(StringError::Bounds));

        let text_view = bytes.view(2, 2).unwrap();
        assert_eq!(text_view.as_bytes(), b"AB");
        assert_eq!(text_view.to_string_view().unwrap().as_str(), "AB");
        // Identity is the visible content, not the backing representation.
        assert_eq!(
            text_view,
            ConstString::new("AB").bytes(),
            "a raw blob and a UTF-8 literal view must compare equal"
        );
    }
}
