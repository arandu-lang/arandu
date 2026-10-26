use rustc_hash::FxHashMap;

use crate::Span;

pub type DocCommentMap = FxHashMap<NodeKey, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey {
    pub start: u32,
    pub end: u32,
}

impl From<Span> for NodeKey {
    fn from(span: Span) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}
