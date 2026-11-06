/// A compact, 12-byte representation of a source code span.
/// Stores `file_id`, `start`, and `end` byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// Creates a new `Span` with the given file identifier, start byte offset, and end byte offset.
    #[must_use]
    pub const fn new(file_id: u32, start: u32, end: u32) -> Self {
        Self {
            file_id,
            start,
            end,
        }
    }
}

/// Identity of a top-level source item that does not depend on byte
/// positions: the ordinal of the item among the top-level items of its file.
///
/// Editing an item's body moves the items after it but keeps their `AstId`;
/// absolute positions are recovered at the boundaries through the file's item
/// map (RFC 0042).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AstId {
    pub file_id: u32,
    pub index: u32,
}

impl AstId {
    #[must_use]
    pub const fn new(file_id: u32, index: u32) -> Self {
        Self { file_id, index }
    }
}
