//! Versioned, self-verifying envelopes for persistent compiler artifacts.
//!
//! Salsa remains the owner of incremental execution and is never serialized.
//! These envelopes are a cold-path boundary for canonical `.air`, `.amir`, and
//! `.ameta` payloads. Filesystem publication belongs to the CLI.

use std::fmt;

const MAGIC: [u8; 8] = *b"ARANCAS\0";
const HEADER_LEN: usize = 88;

/// First stable schema for persistent compiler artifacts.
pub const ARTIFACT_SCHEMA_VERSION: u16 = 1;

/// A compiler artifact namespace. The numeric tags are part of the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CompilerArtifactKind {
    Air = 1,
    Amir = 2,
    Ameta = 3,
}

impl CompilerArtifactKind {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Air => "air",
            Self::Amir => "amir",
            Self::Ameta => "ameta",
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ArtifactDecodeError> {
        match tag {
            1 => Ok(Self::Air),
            2 => Ok(Self::Amir),
            3 => Ok(Self::Ameta),
            other => Err(ArtifactDecodeError::UnknownKind(other)),
        }
    }
}

/// BLAKE3 identity carried without hexadecimal heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArtifactDigest([u8; 32]);

impl ArtifactDigest {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Verified view borrowing directly from the serialized buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedCompilerArtifact<'a> {
    pub kind: CompilerArtifactKind,
    pub schema_version: u16,
    pub input_digest: ArtifactDigest,
    pub payload_digest: ArtifactDigest,
    pub payload: &'a [u8],
}

/// Serialize a canonical payload into the stable envelope.
#[must_use]
pub fn encode_compiler_artifact(
    kind: CompilerArtifactKind,
    input_digest: ArtifactDigest,
    payload: &[u8],
) -> Vec<u8> {
    let payload_digest = ArtifactDigest::of(payload);
    let mut output = Vec::with_capacity(HEADER_LEN + payload.len());
    output.extend_from_slice(&MAGIC);
    output.push(kind as u8);
    output.push(0); // flags, reserved by schema v1
    output.extend_from_slice(&ARTIFACT_SCHEMA_VERSION.to_le_bytes());
    output.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    output.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    output.extend_from_slice(&input_digest.0);
    output.extend_from_slice(&payload_digest.0);
    output.extend_from_slice(payload);
    output
}

/// Verify and borrow a persistent compiler artifact. Corruption and future
/// schemas fail closed and must trigger a clean rebuild.
pub fn decode_compiler_artifact(
    bytes: &[u8],
    expected_kind: CompilerArtifactKind,
) -> Result<VerifiedCompilerArtifact<'_>, ArtifactDecodeError> {
    if bytes.len() < HEADER_LEN {
        return Err(ArtifactDecodeError::Truncated);
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err(ArtifactDecodeError::InvalidMagic);
    }
    let kind = CompilerArtifactKind::from_tag(bytes[8])?;
    if kind != expected_kind {
        return Err(ArtifactDecodeError::WrongKind {
            expected: expected_kind,
            actual: kind,
        });
    }
    if bytes[9] != 0 {
        return Err(ArtifactDecodeError::UnsupportedFlags(bytes[9]));
    }
    let schema_version = u16::from_le_bytes([bytes[10], bytes[11]]);
    if schema_version != ARTIFACT_SCHEMA_VERSION {
        return Err(ArtifactDecodeError::UnsupportedSchema(schema_version));
    }
    let mut header_len_bytes = [0_u8; 4];
    header_len_bytes.copy_from_slice(&bytes[12..16]);
    let header_len = u32::from_le_bytes(header_len_bytes);
    if header_len as usize != HEADER_LEN {
        return Err(ArtifactDecodeError::InvalidHeaderLength(header_len));
    }
    let mut payload_len_bytes = [0_u8; 8];
    payload_len_bytes.copy_from_slice(&bytes[16..24]);
    let payload_len = u64::from_le_bytes(payload_len_bytes);
    let payload_len = usize::try_from(payload_len).map_err(|_| ArtifactDecodeError::Truncated)?;
    if bytes.len() != HEADER_LEN.saturating_add(payload_len) {
        return Err(ArtifactDecodeError::Truncated);
    }
    let mut input_digest_bytes = [0_u8; 32];
    input_digest_bytes.copy_from_slice(&bytes[24..56]);
    let input_digest = ArtifactDigest(input_digest_bytes);
    let mut payload_digest_bytes = [0_u8; 32];
    payload_digest_bytes.copy_from_slice(&bytes[56..88]);
    let payload_digest = ArtifactDigest(payload_digest_bytes);
    let payload = &bytes[HEADER_LEN..];
    if ArtifactDigest::of(payload) != payload_digest {
        return Err(ArtifactDecodeError::DigestMismatch);
    }
    Ok(VerifiedCompilerArtifact {
        kind,
        schema_version,
        input_digest,
        payload_digest,
        payload,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactDecodeError {
    Truncated,
    InvalidMagic,
    UnknownKind(u8),
    WrongKind {
        expected: CompilerArtifactKind,
        actual: CompilerArtifactKind,
    },
    UnsupportedFlags(u8),
    UnsupportedSchema(u16),
    InvalidHeaderLength(u32),
    DigestMismatch,
}

impl fmt::Display for ArtifactDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("compiler artifact is truncated"),
            Self::InvalidMagic => formatter.write_str("compiler artifact has invalid magic"),
            Self::UnknownKind(kind) => write!(formatter, "unknown compiler artifact kind {kind}"),
            Self::WrongKind { expected, actual } => {
                write!(
                    formatter,
                    "expected {expected:?} artifact, found {actual:?}"
                )
            }
            Self::UnsupportedFlags(flags) => {
                write!(
                    formatter,
                    "unsupported compiler artifact flags {flags:#04x}"
                )
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported compiler artifact schema {version}")
            }
            Self::InvalidHeaderLength(length) => {
                write!(
                    formatter,
                    "invalid compiler artifact header length {length}"
                )
            }
            Self::DigestMismatch => formatter.write_str("compiler artifact BLAKE3 mismatch"),
        }
    }
}

impl std::error::Error for ArtifactDecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_round_trip_without_copying_payload() {
        let input = ArtifactDigest::of(b"source closure");
        for kind in [
            CompilerArtifactKind::Air,
            CompilerArtifactKind::Amir,
            CompilerArtifactKind::Ameta,
        ] {
            let encoded = encode_compiler_artifact(kind, input, b"canonical payload");
            let decoded = decode_compiler_artifact(&encoded, kind).unwrap();
            assert_eq!(decoded.input_digest, input);
            assert_eq!(decoded.payload, b"canonical payload");
            assert_eq!(decoded.payload.as_ptr(), encoded[HEADER_LEN..].as_ptr());
        }
    }

    #[test]
    fn corruption_and_cross_kind_reads_fail_closed() {
        let mut encoded = encode_compiler_artifact(
            CompilerArtifactKind::Amir,
            ArtifactDigest::of(b"input"),
            b"payload",
        );
        assert!(matches!(
            decode_compiler_artifact(&encoded, CompilerArtifactKind::Air),
            Err(ArtifactDecodeError::WrongKind { .. })
        ));
        *encoded.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode_compiler_artifact(&encoded, CompilerArtifactKind::Amir),
            Err(ArtifactDecodeError::DigestMismatch)
        );
    }

    #[test]
    fn encoding_is_byte_deterministic() {
        let input = ArtifactDigest::of(b"input");
        let first = encode_compiler_artifact(CompilerArtifactKind::Ameta, input, b"metadata");
        let second = encode_compiler_artifact(CompilerArtifactKind::Ameta, input, b"metadata");
        assert_eq!(first, second);
    }
}
