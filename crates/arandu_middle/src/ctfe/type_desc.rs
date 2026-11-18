//! Compile-time type descriptor independent of any interner domain (RFC 0013 §10.1).

use crate::types::TypeShape;
use smol_str::SmolStr;

/// Stable compile-time description of a type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeDesc {
    /// Fully-qualified canonical definition path, e.g. `std.core.types.TypeInfo` or `app.User`.
    pub canonical_path: SmolStr,
    /// Structural shape of the type.
    pub shape: TypeShape,
    /// 64-bit fingerprint of public surface for cache invalidation.
    pub public_fingerprint: u64,
    /// Target layout size in bytes.
    pub size: u32,
    /// Target layout alignment in bytes.
    pub align: u32,
    /// Whether all members were visible at the query site.
    pub is_complete: bool,
    /// Visible members (fields, variants, methods).
    pub members: Vec<TypeMemberDesc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeMemberDesc {
    pub name: SmolStr,
    pub kind: TypeMemberKind,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeMemberKind {
    Field,
    Variant,
    Method,
}

impl TypeDesc {
    #[must_use]
    pub fn new(
        canonical_path: impl Into<SmolStr>,
        shape: TypeShape,
        public_fingerprint: u64,
        size: u32,
        align: u32,
        is_complete: bool,
        members: Vec<TypeMemberDesc>,
    ) -> Self {
        Self {
            canonical_path: canonical_path.into(),
            shape,
            public_fingerprint,
            size,
            align,
            is_complete,
            members,
        }
    }

    /// Canonical bytes encoding (tag 7) for stable hashing in CTFE.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 + self.canonical_path.len());
        bytes.extend_from_slice(&[1, 7]); // version 1, tag 7
        bytes.extend_from_slice(&self.public_fingerprint.to_le_bytes());
        bytes.extend_from_slice(&self.size.to_le_bytes());
        bytes.extend_from_slice(&self.align.to_le_bytes());
        bytes.push(u8::from(self.is_complete));
        let path_bytes = self.canonical_path.as_bytes();
        bytes.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path_bytes);
        bytes.extend_from_slice(&(self.members.len() as u32).to_le_bytes());
        for m in &self.members {
            let m_name = m.name.as_bytes();
            bytes.extend_from_slice(&(m_name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(m_name);
            let kind_byte = match m.kind {
                TypeMemberKind::Field => 0u8,
                TypeMemberKind::Variant => 1u8,
                TypeMemberKind::Method => 2u8,
            };
            bytes.push(kind_byte);
            bytes.extend_from_slice(&m.offset.unwrap_or(u32::MAX).to_le_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Primitive;

    #[test]
    fn type_desc_canonical_bytes_deterministic() {
        let desc1 = TypeDesc::new(
            "std.core.types.TypeInfo",
            TypeShape::Primitive(Primitive::Int),
            0x1234_5678_9abc_def0,
            24,
            8,
            true,
            vec![
                TypeMemberDesc {
                    name: "canonicalName".into(),
                    kind: TypeMemberKind::Field,
                    offset: Some(0),
                },
                TypeMemberDesc {
                    name: "size".into(),
                    kind: TypeMemberKind::Field,
                    offset: Some(16),
                },
            ],
        );
        let desc2 = desc1.clone();
        assert_eq!(desc1, desc2);
        assert_eq!(desc1.canonical_bytes(), desc2.canonical_bytes());
        assert_eq!(desc1.canonical_bytes()[..2], [1, 7]);
    }
}
