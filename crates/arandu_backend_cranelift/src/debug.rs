//! DWARF v5 emission for AOT objects.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cranelift_object::ObjectProduct;
use cranelift_object::object::write::{
    Relocation as ObjectRelocation, SectionId as ObjectSectionId, StandardSegment,
};
use cranelift_object::object::{RelocationEncoding, RelocationFlags, RelocationKind, SectionKind};
use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, Expression, LineProgram, LineString, Location,
    LocationList, RelocateWriter, Relocation, RelocationTarget, Sections,
};
use gimli::{Encoding, Format, RunTimeEndian, SectionId};

use crate::jit::codegen_ice;
use crate::jit::compiler::DebugCompilation;
use arandu_semantics::types::{ArType, Primitive, TypeId};
use arandu_semantics::{Diagnostic, SymbolTable, TypeInfo};

/// Source text and path used only while emitting native debug metadata.
#[derive(Clone, Debug)]
pub struct DebugSource {
    pub file_id: u32,
    pub path: Arc<PathBuf>,
    pub text: Arc<str>,
}

#[derive(Clone, Debug)]
struct DwarfWriter {
    writer: EndianVec<RunTimeEndian>,
    relocations: Vec<Relocation>,
}

impl RelocateWriter for DwarfWriter {
    type Writer = EndianVec<RunTimeEndian>;

    fn writer(&self) -> &Self::Writer {
        &self.writer
    }

    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn relocate(&mut self, relocation: Relocation) {
        self.relocations.push(relocation);
    }
}

pub(crate) fn emit_dwarf(
    product: &mut ObjectProduct,
    debug: &DebugCompilation,
    sources: &[DebugSource],
    symbols: &SymbolTable,
    type_info: &TypeInfo,
    target_endianness: Result<target_lexicon::Endianness, ()>,
) -> Result<(), Diagnostic> {
    if debug.functions.is_empty() || sources.is_empty() {
        return Ok(());
    }

    let endian = match target_endianness {
        Ok(target_lexicon::Endianness::Little) => RunTimeEndian::Little,
        Ok(target_lexicon::Endianness::Big) => RunTimeEndian::Big,
        Err(()) => return Err(codegen_ice("target endianness is unknown for DWARF v5")),
    };
    let address_size = product
        .object
        .architecture()
        .address_size()
        .ok_or_else(|| codegen_ice("target address size is unknown for DWARF v5"))?
        .bytes();
    let encoding = Encoding {
        format: Format::Dwarf32,
        version: 5,
        address_size,
    };
    let mut dwarf = DwarfUnit::new(encoding);
    let mut ordered_sources = sources.iter().collect::<Vec<_>>();
    ordered_sources.sort_by_key(|source| source.file_id);
    let primary = ordered_sources[0];

    let working_directory = primary.path.parent().unwrap_or_else(|| Path::new("."));
    let primary_filename = primary
        .path
        .file_name()
        .unwrap_or_else(|| OsStr::new("source.aru"));
    let working_line_string = line_string(working_directory, encoding, &mut dwarf);
    let primary_line_string = line_string(primary_filename, encoding, &mut dwarf);
    let mut line_program = LineProgram::new(
        encoding,
        gimli::LineEncoding::default(),
        working_line_string,
        None,
        primary_line_string,
        None,
    );

    let mut dwarf_files = BTreeMap::new();
    for source in &ordered_sources {
        let directory = source.path.parent().unwrap_or_else(|| Path::new("."));
        let filename = source
            .path
            .file_name()
            .unwrap_or_else(|| OsStr::new("source.aru"));
        let directory_string = line_string(directory, encoding, &mut dwarf);
        let filename_string = line_string(filename, encoding, &mut dwarf);
        let directory_id = line_program.add_directory(directory_string);
        let file_id = line_program.add_file(filename_string, directory_id, None);
        dwarf_files.insert(source.file_id, file_id);
    }

    let source_by_id = ordered_sources
        .iter()
        .map(|source| (source.file_id, *source))
        .collect::<BTreeMap<_, _>>();

    for (function_index, function) in debug.functions.iter().enumerate() {
        let symbol = symbols.get(function.symbol);
        let Some(source) = source_by_id.get(&symbol.span.file_id).copied() else {
            continue;
        };
        let Some(&decl_file) = dwarf_files.get(&symbol.span.file_id) else {
            continue;
        };
        let (decl_line, decl_column) = source_position(&source.text, symbol.span.start);
        let address = Address::Symbol {
            symbol: function_index,
            addend: 0,
        };

        line_program.begin_sequence(Some(address));
        let mut last_row = None;
        if function.ranges.first().is_none_or(|range| range.start != 0) {
            emit_line_row(&mut line_program, decl_file, decl_line, decl_column, 0);
            last_row = Some((symbol.span.file_id, decl_line, decl_column, 0));
        }
        for range in &function.ranges {
            if range.start >= range.end {
                continue;
            }
            let Some(range_source) = source_by_id.get(&range.span.file_id).copied() else {
                continue;
            };
            let Some(&file) = dwarf_files.get(&range.span.file_id) else {
                continue;
            };
            let (line, column) = source_position(&range_source.text, range.span.start);
            let row = (range.span.file_id, line, column, range.start);
            if last_row == Some(row) {
                continue;
            }
            emit_line_row(
                &mut line_program,
                file,
                line,
                column,
                u64::from(range.start),
            );
            last_row = Some(row);
        }
        line_program.end_sequence(u64::from(function.code_size));

        let subprogram = dwarf.unit.add(dwarf.unit.root(), gimli::DW_TAG_subprogram);
        let name = dwarf.strings.add(symbol.name.as_bytes());
        let linkage_name = dwarf.strings.add(symbols.host_func_name(symbol).as_bytes());
        let entry = dwarf.unit.get_mut(subprogram);
        entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
        entry.set(
            gimli::DW_AT_linkage_name,
            AttributeValue::StringRef(linkage_name),
        );
        entry.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(decl_file)),
        );
        entry.set(gimli::DW_AT_decl_line, AttributeValue::Udata(decl_line));
        entry.set(gimli::DW_AT_decl_column, AttributeValue::Udata(decl_column));
        entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(address));
        entry.set(
            gimli::DW_AT_high_pc,
            AttributeValue::Udata(u64::from(function.code_size)),
        );
        entry.set(
            gimli::DW_AT_external,
            AttributeValue::Flag(symbol.is_public),
        );
        let mut frame_base = Expression::new();
        frame_base.op(gimli::DW_OP_call_frame_cfa);
        entry.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(frame_base));

        for local in &function.locals {
            let Some(type_entry) = emit_scalar_type(&mut dwarf, local.ty, type_info, address_size)
            else {
                continue;
            };
            let local_symbol = symbols.get(local.symbol);
            let mut locations = Vec::with_capacity(local.ranges.len());
            for range in &local.ranges {
                if range.start >= range.end {
                    continue;
                }
                let mut expression = Expression::new();
                match range.location {
                    crate::jit::compiler::DebugValueLocation::Register(register) => {
                        expression.op_reg(gimli::Register(register));
                    }
                    crate::jit::compiler::DebugValueLocation::CfaOffset(offset) => {
                        expression.op_fbreg(offset);
                    }
                }
                locations.push(Location::StartLength {
                    begin: Address::Symbol {
                        symbol: function_index,
                        addend: i64::from(range.start),
                    },
                    length: u64::from(range.end - range.start),
                    data: expression,
                });
            }
            if locations.is_empty() {
                continue;
            }
            let location_list = dwarf.unit.locations.add(LocationList(locations));
            let tag = if local.is_parameter {
                gimli::DW_TAG_formal_parameter
            } else {
                gimli::DW_TAG_variable
            };
            let variable = dwarf.unit.add(subprogram, tag);
            let name = dwarf.strings.add(local_symbol.name.as_bytes());
            let (line, column) = source_position(&source.text, local.span.start);
            let entry = dwarf.unit.get_mut(variable);
            entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
            entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(type_entry));
            entry.set(
                gimli::DW_AT_location,
                AttributeValue::LocationListRef(location_list),
            );
            entry.set(
                gimli::DW_AT_decl_file,
                AttributeValue::FileIndex(Some(decl_file)),
            );
            entry.set(gimli::DW_AT_decl_line, AttributeValue::Udata(line));
            entry.set(gimli::DW_AT_decl_column, AttributeValue::Udata(column));
        }
    }

    dwarf.unit.line_program = line_program;
    let producer = dwarf.strings.add(b"Arandu compiler".as_slice());
    let unit_name = dwarf.strings.add(path_bytes(primary.path.as_path()));
    let comp_dir = dwarf.strings.add(path_bytes(working_directory));
    let root = dwarf.unit.get_mut(dwarf.unit.root());
    root.set(gimli::DW_AT_producer, AttributeValue::StringRef(producer));
    root.set(
        gimli::DW_AT_language,
        AttributeValue::Language(gimli::DW_LANG_Rust),
    );
    root.set(gimli::DW_AT_name, AttributeValue::StringRef(unit_name));
    root.set(gimli::DW_AT_comp_dir, AttributeValue::StringRef(comp_dir));

    let writer = DwarfWriter {
        writer: EndianVec::new(endian),
        relocations: Vec::new(),
    };
    let mut sections = Sections::new(writer);
    dwarf
        .write(&mut sections)
        .map_err(|error| codegen_ice(format!("failed to encode DWARF v5: {error}")))?;
    append_sections(product, sections, debug)
}

fn emit_scalar_type(
    dwarf: &mut DwarfUnit,
    type_id: TypeId,
    type_info: &TypeInfo,
    address_size: u8,
) -> Option<gimli::write::UnitEntryId> {
    let (name, byte_size, encoding) = match type_info.resolve_type_id(type_id) {
        ArType::Primitive(primitive) => match primitive {
            Primitive::Int => ("int", address_size, gimli::DW_ATE_signed),
            Primitive::Uint => ("uint", address_size, gimli::DW_ATE_unsigned),
            Primitive::I8 => ("i8", 1, gimli::DW_ATE_signed),
            Primitive::I16 => ("i16", 2, gimli::DW_ATE_signed),
            Primitive::I32 => ("i32", 4, gimli::DW_ATE_signed),
            Primitive::I64 => ("i64", 8, gimli::DW_ATE_signed),
            Primitive::U8 | Primitive::Byte => ("u8", 1, gimli::DW_ATE_unsigned),
            Primitive::U16 => ("u16", 2, gimli::DW_ATE_unsigned),
            Primitive::U32 | Primitive::Char => ("u32", 4, gimli::DW_ATE_unsigned),
            Primitive::U64 => ("u64", 8, gimli::DW_ATE_unsigned),
            Primitive::F32 => ("f32", 4, gimli::DW_ATE_float),
            Primitive::F64 => ("f64", 8, gimli::DW_ATE_float),
            Primitive::Bool => ("bool", 1, gimli::DW_ATE_boolean),
            Primitive::Float | Primitive::Str | Primitive::Any => return None,
        },
        ArType::IntLiteral => ("int", address_size, gimli::DW_ATE_signed),
        _ => return None,
    };
    let type_entry = dwarf.unit.add(dwarf.unit.root(), gimli::DW_TAG_base_type);
    let name = dwarf.strings.add(name.as_bytes());
    let entry = dwarf.unit.get_mut(type_entry);
    entry.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
    entry.set(
        gimli::DW_AT_byte_size,
        AttributeValue::Udata(u64::from(byte_size)),
    );
    entry.set(gimli::DW_AT_encoding, AttributeValue::Encoding(encoding));
    Some(type_entry)
}

fn line_string(path: impl AsRef<OsStr>, encoding: Encoding, dwarf: &mut DwarfUnit) -> LineString {
    LineString::new(path_bytes(path), encoding, &mut dwarf.line_strings)
}

fn path_bytes(path: impl AsRef<OsStr>) -> Vec<u8> {
    path.as_ref()
        .to_string_lossy()
        .replace('\0', "�")
        .into_bytes()
}

fn source_position(source: &str, byte_offset: u32) -> (u64, u64) {
    let limit = usize::try_from(byte_offset)
        .unwrap_or(usize::MAX)
        .min(source.len());
    let prefix = &source.as_bytes()[..limit];
    let line_start = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let line = 1 + prefix.iter().filter(|byte| **byte == b'\n').count() as u64;
    let column = 1 + (limit - line_start) as u64;
    (line, column)
}

fn emit_line_row(
    program: &mut LineProgram,
    file: gimli::write::FileId,
    line: u64,
    column: u64,
    address_offset: u64,
) {
    let row = program.row();
    row.file = file;
    row.line = line;
    row.column = column;
    row.address_offset = address_offset;
    row.is_statement = true;
    program.generate_row();
}

fn append_sections(
    product: &mut ObjectProduct,
    mut sections: Sections<DwarfWriter>,
    debug: &DebugCompilation,
) -> Result<(), Diagnostic> {
    let mut emitted = Vec::<(SectionId, ObjectSectionId, Vec<Relocation>)>::new();
    sections
        .for_each_mut(|id, section| {
            let body = section.writer_mut().take();
            if body.is_empty() {
                return Ok::<_, gimli::write::Error>(());
            }
            let relocations = std::mem::take(&mut section.relocations);
            let segment = product.object.segment_name(StandardSegment::Debug).to_vec();
            let kind = if matches!(id, SectionId::DebugStr | SectionId::DebugLineStr) {
                SectionKind::DebugString
            } else {
                SectionKind::Debug
            };
            let object_section =
                product
                    .object
                    .add_section(segment, id.name().as_bytes().to_vec(), kind);
            product.object.append_section_data(object_section, &body, 1);
            emitted.push((id, object_section, relocations));
            Ok(())
        })
        .map_err(|error| codegen_ice(format!("failed to materialize DWARF sections: {error}")))?;

    for (_, object_section, relocations) in &emitted {
        for relocation in relocations {
            let symbol = match relocation.target {
                RelocationTarget::Symbol(index) => debug
                    .functions
                    .get(index)
                    .map(|function| product.function_symbol(function.func_id))
                    .ok_or_else(|| codegen_ice("DWARF references an unknown function symbol"))?,
                RelocationTarget::Section(target) => {
                    let target_section = emitted
                        .iter()
                        .find_map(|(id, object, _)| (*id == target).then_some(*object))
                        .ok_or_else(|| codegen_ice("DWARF references an absent section"))?;
                    product.object.section_symbol(target_section)
                }
            };
            product
                .object
                .add_relocation(
                    *object_section,
                    ObjectRelocation {
                        offset: relocation.offset as u64,
                        symbol,
                        addend: relocation.addend,
                        flags: RelocationFlags::Generic {
                            kind: RelocationKind::Absolute,
                            encoding: RelocationEncoding::Generic,
                            size: relocation.size.saturating_mul(8),
                        },
                    },
                )
                .map_err(|error| {
                    codegen_ice(format!("failed to add DWARF object relocation: {error}"))
                })?;
        }
    }
    Ok(())
}
