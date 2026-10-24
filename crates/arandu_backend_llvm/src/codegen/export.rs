//! Marshalling across the C ABI boundary of an `@Export` function.
//!
//! The definition and every in-language caller must agree with the shape the
//! classifier in [`arandu_middle::layout::export_abi`] derived from clang.
//! Memory is the interface: an aggregate is stored at its natural layout and
//! each ABI lane is loaded or stored at the byte offset the classifier
//! assigned. Loads and stores share the target data layout, so endianness
//! never needs a special case.

use arandu_middle::layout::{
    AbiLane, AbiPart, ExportAbi, ExportAbiInfo, ParamAbi, classify_export,
};
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{Diagnostic, SymbolId};

use super::{FuncEmitter, ModuleEmitter, TypedValue};

/// LLVM type of one ABI lane.
pub(super) fn part_ty(part: AbiPart) -> String {
    match part {
        AbiPart::Int(bits) => format!("i{bits}"),
        AbiPart::Float(32) => "float".to_owned(),
        AbiPart::Float(_) => "double".to_owned(),
        AbiPart::FloatPair => "<2 x float>".to_owned(),
        AbiPart::IntWords { word, count } => format!("[{count} x i{word}]"),
        AbiPart::FloatWords { float: 32, count } => format!("[{count} x float]"),
        AbiPart::FloatWords { count, .. } => format!("[{count} x double]"),
    }
}

/// LLVM return type for a direct return of these lanes.
pub(super) fn lanes_ty(lanes: &[AbiLane]) -> String {
    match lanes {
        [lane] => part_ty(lane.part),
        _ => {
            let fields = lanes
                .iter()
                .map(|lane| part_ty(lane.part))
                .collect::<Vec<_>>();
            format!("{{ {} }}", fields.join(", "))
        }
    }
}

/// Extension attribute a C compiler attaches to a sub-word integer at the
/// boundary (`zeroext i8`, `signext i16`).
///
/// AAPCS32 and the RISC-V psABI make the callee widen sub-word return values
/// to a full register, and clang-built callers rely on it (`uint8_t r = f();`
/// is not re-masked). Every `clang -target` probe emits the same attributes
/// on parameters and returns, so the definition mirrors them on all targets.
pub(super) fn scalar_extension(ty: &ArType) -> &'static str {
    let ArType::Primitive(primitive) = ty else {
        return "";
    };
    match primitive {
        Primitive::Bool | Primitive::U8 | Primitive::Byte | Primitive::U16 => "zeroext ",
        Primitive::I8 | Primitive::I16 => "signext ",
        Primitive::Int
        | Primitive::Uint
        | Primitive::ISize
        | Primitive::USize
        | Primitive::Float
        | Primitive::I32
        | Primitive::I64
        | Primitive::U32
        | Primitive::U64
        | Primitive::F32
        | Primitive::F64
        | Primitive::Char
        | Primitive::Str
        | Primitive::Any => "",
    }
}

impl ModuleEmitter<'_> {
    /// True when `ty` is the consuming receiver of the exported destructor
    /// `function`. The typeck `@Destructor` record is the authority; no method
    /// name is inspected.
    pub(super) fn is_exported_destructor_receiver(&self, function: SymbolId, ty: &ArType) -> bool {
        let ArType::Named(receiver, _) = ty else {
            return false;
        };
        self.info.destructor_method(*receiver) == Some(function)
    }

    /// C ABI family of the module target. An empty identity means the host.
    pub(super) fn export_abi(&self) -> Option<ExportAbi> {
        let target = &self.info.target_identity();
        let arch = if target.arch.is_empty() {
            std::env::consts::ARCH
        } else {
            target.arch.as_str()
        };
        let os = if target.os.is_empty() {
            std::env::consts::OS
        } else {
            target.os.as_str()
        };
        let triple = if target.triple.is_empty() {
            ""
        } else {
            target.triple.as_str()
        };
        ExportAbi::for_target(arch, os, triple)
    }

    /// Classifies an aggregate that crosses the exported boundary.
    pub(super) fn classify_export_type(
        &self,
        ty: &ArType,
        span: arandu_middle::Span,
    ) -> Result<ExportAbiInfo, Diagnostic> {
        let Some(abi) = self.export_abi() else {
            return Err(Diagnostic::error(
                arandu_middle::DiagCode::T049InvalidFfiExport,
                format!(
                    "aggregates by value are not classified for target '{}'; pass scalars or pointers",
                    self.info.target_identity().triple
                ),
                span,
            ));
        };
        classify_export(
            abi,
            ty,
            self.interner,
            self.info,
            self.layout.pointer_width(),
        )
        .map_err(|error| super::ice(format!("export ABI classification failed: {error}")))
    }
}

impl FuncEmitter<'_, '_> {
    /// Spills an aggregate value to a fresh buffer and returns its pointer.
    pub(super) fn spill_aggregate(
        &mut self,
        value: &TypedValue,
        aggty: &str,
        align: u64,
    ) -> String {
        let buffer = self.new_value();
        self.writer
            .line(format!("{buffer} = alloca {aggty}, align {align}"));
        self.writer.line(format!(
            "store {} {}, ptr {buffer}, align {align}",
            value.ty, value.value
        ));
        buffer
    }

    /// Stores one ABI lane into `buffer` at the lane offset.
    pub(super) fn store_lane(&mut self, buffer: &str, lane: &AbiLane, value: &TypedValue) {
        let pointer = if lane.offset == 0 {
            buffer.to_owned()
        } else {
            let offset = self.new_value();
            self.writer.line(format!(
                "{offset} = getelementptr i8, ptr {buffer}, i64 {}",
                lane.offset
            ));
            offset
        };
        // Boundary accesses are byte-exact: align 1 is always valid and avoids
        // claiming an alignment the lane type does not have.
        self.writer.line(format!(
            "store {} {}, ptr {pointer}, align 1",
            value.ty, value.value
        ));
    }

    /// Loads one ABI lane from `buffer` at the lane offset.
    pub(super) fn load_lane(&mut self, buffer: &str, lane: &AbiLane) -> TypedValue {
        let pointer = if lane.offset == 0 {
            buffer.to_owned()
        } else {
            let offset = self.new_value();
            self.writer.line(format!(
                "{offset} = getelementptr i8, ptr {buffer}, i64 {}",
                lane.offset
            ));
            offset
        };
        let ty = part_ty(lane.part);
        let loaded = self.new_value();
        self.writer
            .line(format!("{loaded} = load {ty}, ptr {pointer}, align 1"));
        TypedValue::new(ty, loaded)
    }

    /// Loads the aggregate representation out of a spill buffer.
    pub(super) fn load_aggregate(&mut self, buffer: &str, aggty: &str, align: u64) -> String {
        let loaded = self.new_value();
        self.writer.line(format!(
            "{loaded} = load {aggty}, ptr {buffer}, align {align}"
        ));
        loaded
    }

    /// Renders the ABI parameters of an exported definition, spilling each
    /// aggregate lane into a buffer that the body then loads as one value.
    pub(super) fn emit_export_param(
        &mut self,
        temp: arandu_middle::amir::TempId,
        info: &ExportAbiInfo,
        aggty: &str,
    ) -> Result<String, Diagnostic> {
        match &info.param {
            ParamAbi::Ignored => Ok(String::new()),
            ParamAbi::Indirect => {
                let name = format!("%p{}", temp.as_usize());
                // `byval` keeps C's by-value semantics: the callee owns a
                // private copy, so writes never reach the caller's object.
                Ok(format!(
                    "ptr byval({aggty}) align {} {name}",
                    info.align.max(1)
                ))
            }
            ParamAbi::OwnedPointer => Ok(format!("ptr %p{}", temp.as_usize())),
            ParamAbi::Direct(lanes) => {
                let mut rendered = Vec::with_capacity(lanes.len());
                for (index, lane) in lanes.iter().enumerate() {
                    rendered.push(format!(
                        "{} %p{}_{index}",
                        part_ty(lane.part),
                        temp.as_usize()
                    ));
                }
                Ok(rendered.join(", "))
            }
        }
    }

    /// Emits the prologue that materializes an aggregate parameter as the
    /// `%tN` SSA value the AMIR body expects.
    pub(super) fn bind_export_param(
        &mut self,
        temp: arandu_middle::amir::TempId,
        info: &ExportAbiInfo,
        aggty: &str,
    ) {
        match &info.param {
            ParamAbi::Ignored => {}
            ParamAbi::Indirect => {
                let name = format!("%p{}", temp.as_usize());
                let loaded = self.load_aggregate(&name, aggty, info.align.max(1));
                self.temp_values[temp.as_usize()] = Some(TypedValue::new(aggty, loaded));
            }
            ParamAbi::OwnedPointer => {
                // The caller keeps the allocation; the body consumes the value
                // stored there, so loading it transfers the resources it holds.
                let name = format!("%p{}", temp.as_usize());
                let loaded = self.load_aggregate(&name, aggty, info.align.max(1));
                self.temp_values[temp.as_usize()] = Some(TypedValue::new(aggty, loaded));
            }
            ParamAbi::Direct(lanes) => {
                let buffer = self.new_value();
                self.writer.line(format!(
                    "{buffer} = alloca {aggty}, align {}",
                    info.align.max(1)
                ));
                for (index, lane) in lanes.iter().enumerate() {
                    let value = TypedValue::new(
                        part_ty(lane.part),
                        format!("%p{}_{index}", temp.as_usize()),
                    );
                    self.store_lane(&buffer, lane, &value);
                }
                let loaded = self.load_aggregate(&buffer, aggty, info.align.max(1));
                self.temp_values[temp.as_usize()] = Some(TypedValue::new(aggty, loaded));
            }
        }
    }

    /// Returns the lanes of an aggregate argument, ready to be passed.
    pub(super) fn export_argument(
        &mut self,
        value: &TypedValue,
        info: &ExportAbiInfo,
        aggty: &str,
    ) -> Vec<String> {
        match &info.param {
            ParamAbi::Ignored => Vec::new(),
            ParamAbi::Indirect => {
                let buffer = self.spill_aggregate(value, aggty, info.align.max(1));
                vec![format!("ptr {buffer}")]
            }
            ParamAbi::OwnedPointer => {
                // Passing an owned value to an exported destructor transfers
                // it: the callee loads and consumes the copy at `buffer`.
                let buffer = self.spill_aggregate(value, aggty, info.align.max(1));
                vec![format!("ptr {buffer}")]
            }
            ParamAbi::Direct(lanes) => {
                let buffer = self.spill_aggregate(value, aggty, info.align.max(1));
                lanes
                    .iter()
                    .map(|lane| self.load_lane(&buffer, lane).render())
                    .collect()
            }
        }
    }
}
