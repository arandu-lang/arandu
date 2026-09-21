//! Target-aware ABI classification for aggregates.
//!
//! Normative references:
//! - [System V AMD64 ABI](https://refspecs.linuxfoundation.org/elf/x86_64-abi-0.98.pdf) Section 3.2.3
//! - [Microsoft x64 Calling Convention](https://learn.microsoft.com/cpp/build/x64-calling-convention)
//! - [AAPCS64 Procedure Call Standard](https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst)

use smallvec::{SmallVec, smallvec};

use crate::layout::{LayoutEngine, StructLayoutProvider, TypeLayout, instantiated_field_type};
use crate::types::{ArType, Primitive, TypeInterner};

#[cfg(test)]
mod tests;

/// Supported target ABIs for aggregate classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetAbi {
    /// System V AMD64 ABI (Linux, macOS, BSD on x86_64).
    SystemVAmd64,
    /// Microsoft x64 calling convention (Windows on x86_64).
    WindowsX64,
    /// AAPCS64 Procedure Call Standard (Linux, macOS, Windows on AArch64).
    Aapcs64,
    /// Generic / fallback ABI for 32-bit and unspecified targets.
    Generic,
}

/// Primitive scalar type representing an ABI register slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbiScalar {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl AbiScalar {
    /// Size of this scalar in bytes.
    #[must_use]
    pub const fn size(self) -> u64 {
        match self {
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::I64 | Self::F64 => 8,
        }
    }

    /// Whether this scalar is a floating-point type.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
}

/// A single register slot within a direct aggregate argument/return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AbiSlot {
    /// The scalar type of this slot.
    pub scalar: AbiScalar,
    /// The byte offset within the aggregate memory layout.
    pub offset: u64,
}

/// Aggregates passed directly in machine registers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DirectAbi {
    pub slots: SmallVec<[AbiSlot; 4]>,
}

/// How an argument or return value is passed under the target calling convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArgAbi {
    /// Zero-sized type: occupies 0 registers and 0 stack bytes.
    ZeroSized,
    /// Passed directly in one or more machine registers.
    Direct(DirectAbi),
    /// Passed indirectly by reference (pointer to memory).
    Indirect,
}

/// Target-aware ABI classifier.
#[derive(Debug, Clone)]
pub struct TargetAbiClassifier {
    pub abi: TargetAbi,
    pub pointer_width: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeafKind {
    Integer,
    Float32,
    Float64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LeafField {
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) kind: LeafKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EightbyteClass {
    NoClass,
    Integer,
    Sse,
    Memory,
}

impl TargetAbiClassifier {
    /// Creates a new classifier for the given target ABI and pointer width.
    #[must_use]
    pub fn new(abi: TargetAbi, pointer_width: u64) -> Self {
        Self { abi, pointer_width }
    }

    /// Classifies an [`ArType`] according to the target ABI.
    #[must_use]
    pub fn classify_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> ArgAbi {
        match ty {
            ArType::Void | ArType::Error => ArgAbi::ZeroSized,
            ArType::Named(sym, _) => {
                if provider.get_struct_fields(*sym).is_none() {
                    return ArgAbi::Indirect;
                }
                let engine = LayoutEngine::new(self.pointer_width);
                let Ok(layout) = engine.layout_of_type(ty, interner, provider) else {
                    return ArgAbi::Indirect;
                };
                if layout.size == 0 {
                    return ArgAbi::ZeroSized;
                }
                let mut leaves = Vec::new();
                Self::collect_leaf_fields(
                    ty,
                    0,
                    &layout,
                    interner,
                    provider,
                    self.pointer_width,
                    &mut leaves,
                );
                self.classify_layout(&layout, &leaves)
            }
            ArType::Tuple(_) => {
                let engine = LayoutEngine::new(self.pointer_width);
                let Ok(layout) = engine.layout_of_type(ty, interner, provider) else {
                    return ArgAbi::Indirect;
                };
                if layout.size == 0 {
                    return ArgAbi::ZeroSized;
                }
                let mut leaves = Vec::new();
                Self::collect_leaf_fields(
                    ty,
                    0,
                    &layout,
                    interner,
                    provider,
                    self.pointer_width,
                    &mut leaves,
                );
                self.classify_layout(&layout, &leaves)
            }
            _ => {
                // Non-aggregates are handled as scalars by backends.
                ArgAbi::Indirect
            }
        }
    }

    /// Classifies an aggregate with pre-calculated layout and leaf fields.
    pub(crate) fn classify_layout(&self, layout: &TypeLayout, leaves: &[LeafField]) -> ArgAbi {
        if layout.size == 0 {
            return ArgAbi::ZeroSized;
        }

        match self.abi {
            TargetAbi::SystemVAmd64 => self.classify_sysv_amd64(layout, leaves),
            TargetAbi::WindowsX64 => Self::classify_windows_x64(layout),
            TargetAbi::Aapcs64 => self.classify_aapcs64(layout, leaves),
            TargetAbi::Generic => self.classify_generic(layout),
        }
    }

    /// System V AMD64 ABI Section 3.2.3 aggregate classification.
    fn classify_sysv_amd64(&self, layout: &TypeLayout, leaves: &[LeafField]) -> ArgAbi {
        // Structs exceeding 16 bytes (2 eightbytes) have class MEMORY.
        if layout.size > 16 {
            return ArgAbi::Indirect;
        }

        let num_eightbytes = usize::try_from(layout.size.div_ceil(8)).unwrap_or(2);
        let mut classes = [EightbyteClass::NoClass; 2];

        for (i, class_slot) in classes.iter_mut().take(num_eightbytes).enumerate() {
            let eb_start = (i as u64) * 8;
            let eb_end = (eb_start + 8).min(layout.size);

            let mut eb_class = EightbyteClass::NoClass;
            for leaf in leaves {
                let leaf_end = leaf.offset + leaf.size;
                if leaf.offset < eb_end && leaf_end > eb_start {
                    let field_class = match leaf.kind {
                        LeafKind::Integer => EightbyteClass::Integer,
                        LeafKind::Float32 | LeafKind::Float64 => EightbyteClass::Sse,
                    };

                    eb_class = match (eb_class, field_class) {
                        (EightbyteClass::Memory, _) | (_, EightbyteClass::Memory) => {
                            EightbyteClass::Memory
                        }
                        (EightbyteClass::Integer, _) | (_, EightbyteClass::Integer) => {
                            EightbyteClass::Integer
                        }
                        (EightbyteClass::Sse, _) | (_, EightbyteClass::Sse) => EightbyteClass::Sse,
                        (EightbyteClass::NoClass, c) => c,
                    };
                }
            }
            *class_slot = eb_class;
        }

        // Post-merger: if any eightbyte is MEMORY, the whole aggregate is MEMORY.
        for &c in &classes[..num_eightbytes] {
            if c == EightbyteClass::Memory {
                return ArgAbi::Indirect;
            }
        }

        let mut slots = SmallVec::new();
        for (i, &c) in classes.iter().take(num_eightbytes).enumerate() {
            let offset = (i as u64) * 8;
            let remaining = layout.size.saturating_sub(offset).min(8);

            let scalar = match c {
                EightbyteClass::Sse => {
                    if remaining == 4 {
                        AbiScalar::F32
                    } else {
                        AbiScalar::F64
                    }
                }
                EightbyteClass::Integer | EightbyteClass::NoClass => {
                    if i == 0 && num_eightbytes == 1 {
                        match layout.size {
                            1 => AbiScalar::I8,
                            2 => AbiScalar::I16,
                            4 => AbiScalar::I32,
                            _ => AbiScalar::I64,
                        }
                    } else {
                        match remaining {
                            1 => AbiScalar::I8,
                            2 => AbiScalar::I16,
                            4 => AbiScalar::I32,
                            _ => AbiScalar::I64,
                        }
                    }
                }
                EightbyteClass::Memory => unreachable!(),
            };

            slots.push(AbiSlot { scalar, offset });
        }

        ArgAbi::Direct(DirectAbi { slots })
    }

    /// Microsoft x64 Calling Convention:
    /// Aggregates with sizes exactly 1, 2, 4, or 8 bytes are passed as integers.
    /// All other sizes are passed by pointer / hidden return pointer.
    fn classify_windows_x64(layout: &TypeLayout) -> ArgAbi {
        let scalar = match layout.size {
            1 => AbiScalar::I8,
            2 => AbiScalar::I16,
            4 => AbiScalar::I32,
            8 => AbiScalar::I64,
            _ => return ArgAbi::Indirect,
        };

        ArgAbi::Direct(DirectAbi {
            slots: smallvec![AbiSlot { scalar, offset: 0 }],
        })
    }

    /// AAPCS64 (AArch64) aggregate classification:
    /// - HFA (Homogeneous Floating-point Aggregate): 1-4 members of f32 or f64.
    /// - Composite types <= 16 bytes: passed in general registers (1 or 2 slots).
    /// - Composite types > 16 bytes: passed indirectly.
    fn classify_aapcs64(&self, layout: &TypeLayout, leaves: &[LeafField]) -> ArgAbi {
        if layout.size > 16 {
            return ArgAbi::Indirect;
        }

        // Check for Homogeneous Floating-point Aggregate (HFA).
        let leaf_count = leaves.len();
        if (1..=4).contains(&leaf_count) {
            let all_f32 = leaves.iter().all(|l| l.kind == LeafKind::Float32);
            let expected_f32_size = (leaf_count as u64) * 4;
            if all_f32 && layout.size == expected_f32_size {
                let slots = leaves
                    .iter()
                    .map(|l| AbiSlot {
                        scalar: AbiScalar::F32,
                        offset: l.offset,
                    })
                    .collect();
                return ArgAbi::Direct(DirectAbi { slots });
            }

            let all_f64 = leaves.iter().all(|l| l.kind == LeafKind::Float64);
            let expected_f64_size = (leaf_count as u64) * 8;
            if all_f64 && layout.size == expected_f64_size {
                let slots = leaves
                    .iter()
                    .map(|l| AbiSlot {
                        scalar: AbiScalar::F64,
                        offset: l.offset,
                    })
                    .collect();
                return ArgAbi::Direct(DirectAbi { slots });
            }
        }

        // Non-HFA aggregates <= 16 bytes.
        let mut slots = SmallVec::new();
        if layout.size <= 8 {
            let scalar = match layout.size {
                1 => AbiScalar::I8,
                2 => AbiScalar::I16,
                4 => AbiScalar::I32,
                _ => AbiScalar::I64,
            };
            slots.push(AbiSlot { scalar, offset: 0 });
        } else {
            slots.push(AbiSlot {
                scalar: AbiScalar::I64,
                offset: 0,
            });
            let remaining = layout.size - 8;
            let second_scalar = match remaining {
                1 => AbiScalar::I8,
                2 => AbiScalar::I16,
                4 => AbiScalar::I32,
                _ => AbiScalar::I64,
            };
            slots.push(AbiSlot {
                scalar: second_scalar,
                offset: 8,
            });
        }

        ArgAbi::Direct(DirectAbi { slots })
    }

    /// Generic / fallback ABI for 32-bit and unspecified targets.
    fn classify_generic(&self, layout: &TypeLayout) -> ArgAbi {
        if layout.size <= self.pointer_width {
            let scalar = if self.pointer_width == 4 {
                match layout.size {
                    1 => AbiScalar::I8,
                    2 => AbiScalar::I16,
                    _ => AbiScalar::I32,
                }
            } else {
                match layout.size {
                    1 => AbiScalar::I8,
                    2 => AbiScalar::I16,
                    4 => AbiScalar::I32,
                    _ => AbiScalar::I64,
                }
            };
            ArgAbi::Direct(DirectAbi {
                slots: smallvec![AbiSlot { scalar, offset: 0 }],
            })
        } else {
            ArgAbi::Indirect
        }
    }

    fn collect_leaf_fields(
        ty: &ArType,
        current_offset: u64,
        layout: &TypeLayout,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
        pointer_width: u64,
        leaves: &mut Vec<LeafField>,
    ) {
        match ty {
            ArType::Primitive(p) => match p {
                Primitive::F32 => leaves.push(LeafField {
                    offset: current_offset,
                    size: 4,
                    kind: LeafKind::Float32,
                }),
                Primitive::F64 | Primitive::Float => leaves.push(LeafField {
                    offset: current_offset,
                    size: 8,
                    kind: LeafKind::Float64,
                }),
                Primitive::I8 | Primitive::U8 | Primitive::Byte | Primitive::Bool => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: 1,
                        kind: LeafKind::Integer,
                    });
                }
                Primitive::I16 | Primitive::U16 => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: 2,
                        kind: LeafKind::Integer,
                    });
                }
                Primitive::I32 | Primitive::U32 | Primitive::Char => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: 4,
                        kind: LeafKind::Integer,
                    });
                }
                Primitive::I64 | Primitive::U64 => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: 8,
                        kind: LeafKind::Integer,
                    });
                }
                Primitive::Int | Primitive::Uint | Primitive::Any => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: pointer_width,
                        kind: LeafKind::Integer,
                    });
                }
                Primitive::Str => {
                    leaves.push(LeafField {
                        offset: current_offset,
                        size: pointer_width,
                        kind: LeafKind::Integer,
                    });
                    leaves.push(LeafField {
                        offset: current_offset + pointer_width,
                        size: pointer_width,
                        kind: LeafKind::Integer,
                    });
                }
            },
            ArType::Ref(inner) | ArType::RefMut(inner)
                if interner.with_type(*inner, |inner| matches!(inner, ArType::Slice(_))) =>
            {
                leaves.push(LeafField {
                    offset: current_offset,
                    size: pointer_width,
                    kind: LeafKind::Integer,
                });
                leaves.push(LeafField {
                    offset: current_offset + pointer_width,
                    size: pointer_width,
                    kind: LeafKind::Integer,
                });
            }
            ArType::Ptr(_)
            | ArType::Ref(_)
            | ArType::RefMut(_)
            | ArType::Func(_, _)
            | ArType::Err => {
                leaves.push(LeafField {
                    offset: current_offset,
                    size: pointer_width,
                    kind: LeafKind::Integer,
                });
            }
            ArType::GenRef => {
                leaves.push(LeafField {
                    offset: current_offset,
                    size: 8,
                    kind: LeafKind::Integer,
                });
            }
            ArType::Tuple(tids) => {
                let ty_ids = interner.type_args(*tids);
                for (i, &elem_id) in ty_ids.iter().enumerate() {
                    let offset = layout.field_offsets.get(i).copied().unwrap_or(0);
                    let elem_ty = interner.resolve(elem_id);
                    let engine = LayoutEngine::new(pointer_width);
                    if let Ok(elem_layout) = engine.layout_of_type(&elem_ty, interner, provider) {
                        Self::collect_leaf_fields(
                            &elem_ty,
                            current_offset + offset,
                            &elem_layout,
                            interner,
                            provider,
                            pointer_width,
                            leaves,
                        );
                    }
                }
            }
            ArType::Named(sym, _) => {
                if let Some(field_defs) = provider.get_struct_fields(*sym) {
                    for f in field_defs.iter() {
                        let offset = layout.field_offsets.get(f.index).copied().unwrap_or(0);
                        let field_ty =
                            instantiated_field_type(ty, f.name.as_str(), interner, provider)
                                .map(|id| interner.resolve(id))
                                .unwrap_or_else(|| interner.resolve(f.ty));

                        let engine = LayoutEngine::new(pointer_width);
                        if let Ok(field_layout) =
                            engine.layout_of_type(&field_ty, interner, provider)
                        {
                            Self::collect_leaf_fields(
                                &field_ty,
                                current_offset + offset,
                                &field_layout,
                                interner,
                                provider,
                                pointer_width,
                                leaves,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
