use arandu_middle::SymbolId;
use smallvec::SmallVec;
use smol_str::SmolStr;

/// Primitive types recognized by the WebAssembly Component Model WIT specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WitPrimitive {
    Bool,
    U8,
    S8,
    U16,
    S16,
    U32,
    S32,
    U64,
    S64,
    F32,
    F64,
    Char,
    String,
}

impl WitPrimitive {
    #[must_use]
    pub const fn wit_name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U8 => "u8",
            Self::S8 => "s8",
            Self::U16 => "u16",
            Self::S16 => "s16",
            Self::U32 => "u32",
            Self::S32 => "s32",
            Self::U64 => "u64",
            Self::S64 => "s64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Char => "char",
            Self::String => "string",
        }
    }
}

/// Compact 4-byte handle into a [`WitTypePool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WitTypeId(pub u32);

/// Strongly-typed structural representation of a WIT type (DoD: no heap boxing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WitTypeKind {
    Primitive(WitPrimitive),
    Named(SymbolId),
    List(WitTypeId),
    Tuple(SmallVec<[WitTypeId; 4]>),
    Option(WitTypeId),
    Result {
        ok: Option<WitTypeId>,
        err: Option<WitTypeId>,
    },
}

/// A contiguous, flat pool of WIT type descriptors.
#[derive(Default)]
pub struct WitTypePool {
    kinds: Vec<WitTypeKind>,
}

impl WitTypePool {
    #[must_use]
    pub fn new() -> Self {
        Self { kinds: Vec::new() }
    }

    pub fn add(&mut self, kind: WitTypeKind) -> WitTypeId {
        for (i, existing) in self.kinds.iter().enumerate() {
            if *existing == kind {
                return WitTypeId(i as u32);
            }
        }
        let id = WitTypeId(self.kinds.len() as u32);
        self.kinds.push(kind);
        id
    }

    /// Look up a type kind by its handle.
    ///
    /// Returns `None` for an out-of-range handle instead of panicking, so a
    /// malformed [`WitTypeId`] can never abort translation.
    #[must_use]
    pub fn get(&self, id: WitTypeId) -> Option<&WitTypeKind> {
        self.kinds.get(id.0 as usize)
    }
}

/// Kinds of nominal type definitions declared in a WIT interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WitTypeDefKind {
    Record {
        fields: SmallVec<[(SmolStr, WitTypeId); 8]>,
    },
    Enum {
        cases: SmallVec<[SmolStr; 8]>,
    },
    Variant {
        cases: SmallVec<[(SmolStr, Option<WitTypeId>); 8]>,
    },
}

/// A nominal type definition in a WIT interface (`record`, `enum`, or `variant`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WitTypeDef {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub kind: WitTypeDefKind,
}
