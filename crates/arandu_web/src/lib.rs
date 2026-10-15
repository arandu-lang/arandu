//! In-browser WebAssembly compiler bridge for Arandu.
//!
//! Provides both a pure Rust compilation API and a zero-dependency C-ABI export
//! allowing web browsers to compile and run Arandu code entirely client-side.

pub mod diagnostics;
pub mod stdlib_core;

use arandu_middle::SymbolKind;
use arandu_middle::layout::DataLayout;
use arandu_query::db::DatabaseImpl;
use diagnostics::{
    WebDiagnostic, WebHint, WebReplacement, WebSeverity, convert_diagnostic, convert_ide_diagnostic,
};
use serde::{Deserialize, Serialize};

/// High-level compilation result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCompileResult {
    /// True if compilation succeeded without errors.
    pub success: bool,
    /// Generated WebAssembly bytecode if successful.
    pub wasm_bytes: Option<Vec<u8>>,
    /// List of diagnostics (errors, warnings, hints).
    pub diagnostics: Vec<WebDiagnostic>,
}

/// Hover presentation for the in-browser playground editor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebHover {
    /// Markdown hover payload (signature + documentation + comptime preview).
    pub contents: String,
    /// Canonical symbol signature when hovering a symbol.
    pub signature: Option<String>,
    /// Cleaned doc comment when available.
    pub documentation: Option<String>,
    /// Start byte offset of the hovered range.
    pub start: u32,
    /// End byte offset of the hovered range.
    pub end: u32,
    /// 1-indexed start line.
    pub line: u32,
    /// 1-indexed start column.
    pub column: u32,
    /// 1-indexed end line.
    pub end_line: u32,
    /// 1-indexed end column.
    pub end_column: u32,
}

/// Single parameter label within signature help.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebParameterInformation {
    pub label: String,
}

/// Signature information for a callable item in the web editor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebSignatureInformation {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<WebParameterInformation>,
    pub active_parameter: Option<u32>,
}

/// Signature help payload for the in-browser playground editor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebSignatureHelp {
    pub signatures: Vec<WebSignatureInformation>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

fn collect_web_diagnostics(
    db: &DatabaseImpl,
    file: arandu_query::SourceFile,
    diags: &[arandu_middle::Diagnostic],
    line_index: &arandu_base::LineIndex,
) -> Vec<WebDiagnostic> {
    let mut web_diags: Vec<WebDiagnostic> = diags
        .iter()
        .map(|d| convert_diagnostic(d, line_index))
        .collect();
    let ide_diags = arandu_query::file_ide_diagnostics(db, file);
    for ide_diag in ide_diags.iter() {
        let converted = convert_ide_diagnostic(ide_diag, line_index);
        if let Some(existing) = web_diags.iter_mut().find(|existing| {
            existing.code == converted.code
                && existing.file_id == converted.file_id
                && existing.line == converted.line
                && existing.column == converted.column
                && existing.end_line == converted.end_line
                && existing.end_column == converted.end_column
                && existing.message == converted.message
        }) {
            for hint in converted.hints {
                if !existing.hints.contains(&hint) {
                    existing.hints.push(hint);
                }
            }
            for rep in converted.replacements {
                if !existing.replacements.contains(&rep) {
                    existing.replacements.push(rep);
                }
            }
            for note in converted.notes {
                if !existing.notes.contains(&note) {
                    existing.notes.push(note);
                }
            }
            for label in converted.labels {
                if !existing.labels.contains(&label) {
                    existing.labels.push(label);
                }
            }
        } else {
            web_diags.push(converted);
        }
    }
    web_diags
}

/// Every web entry point uses the emitted module's target, including IDE
/// queries running in a native host process. Pointer width alone is not a
/// platform identity for compile-time branch selection.
fn configure_web_target(db: &mut DatabaseImpl) {
    db.set_target_config(DataLayout::ptr_width(4));
    db.set_target_identity(arandu_middle::db::TargetIdentity {
        os: "unknown".into(),
        arch: "wasm32".into(),
    });
}

/// Compile surface Arandu source code in memory targeting WebAssembly (wasm32).
#[must_use]
pub fn compile_source(source: &str) -> WebCompileResult {
    compile_source_with_limits(source, arandu_query::ctfe::CtfeLimits::default())
}

/// Compile using an explicit validated deterministic CTFE policy.
#[must_use]
pub fn compile_source_with_limits(
    source: &str,
    limits: arandu_query::ctfe::CtfeLimits,
) -> WebCompileResult {
    let line_index = arandu_base::LineIndex::new(source);
    let mut db = DatabaseImpl::new();
    configure_web_target(&mut db);
    db.set_ctfe_limits(limits);
    stdlib_core::register_embedded_core(&mut db);
    let file = db.new_file("playground.aru".into(), source.into());

    // 1. Parser pass
    let parsed = arandu_query::passes::parse(&db, file);
    if let Err(parse_err) = &**parsed {
        let diag = arandu_middle::Diagnostic::from(parse_err.clone());
        return WebCompileResult {
            success: false,
            wasm_bytes: None,
            diagnostics: vec![convert_diagnostic(&diag, &line_index)],
        };
    }

    // 2. Type-check pass and diagnostic accumulation
    let _ = arandu_query::passes::type_check(&db, file);
    let type_accumulated = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file);

    let mut diags = Vec::new();
    for acc in &type_accumulated {
        if !diags.contains(&acc.0) {
            diags.push(acc.0.clone());
        }
    }

    // 3. Lower AMIR pass and diagnostic accumulation
    let lowered = arandu_query::passes::lower_amir(&db, file);
    let lower_accumulated = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file);

    for acc in &lower_accumulated {
        if !diags.contains(&acc.0) {
            diags.push(acc.0.clone());
        }
    }

    let web_diags = collect_web_diagnostics(&db, file, &diags, &line_index);
    let has_errors = web_diags
        .iter()
        .any(|d| matches!(d.severity, WebSeverity::Error));

    if has_errors {
        return WebCompileResult {
            success: false,
            wasm_bytes: None,
            diagnostics: web_diags,
        };
    }

    // 4. Emit WebAssembly bytecode
    let wasm_res = arandu_backend_wasm::emit_wasm(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        DataLayout::ptr_width(4),
    );

    match wasm_res {
        Ok(bytes) => WebCompileResult {
            success: true,
            wasm_bytes: Some(bytes),
            diagnostics: web_diags,
        },
        Err(err) => {
            let ice_diag = arandu_middle::Diagnostic::ice(
                arandu_middle::DiagCode::ICEGEN001,
                format!("Wasm codegen error: {err}"),
                arandu_middle::Span::new(0, 0, 0),
            );
            let mut web_diags = web_diags;
            web_diags.push(convert_diagnostic(&ice_diag, &line_index));
            WebCompileResult {
                success: false,
                wasm_bytes: None,
                diagnostics: web_diags,
            }
        }
    }
}

/// Semantic completion items for the in-browser editor at a byte `offset`.
///
/// Runs the same query engine as the language server over a fresh in-memory
/// database, so the playground and the VS Code extension share one brain.
#[must_use]
pub fn completion_source(source: &str, offset: u32) -> Vec<arandu_ide::CompletionItem> {
    let mut host = arandu_query::AnalysisHost::new();
    configure_web_target(host.db_mut());
    stdlib_core::register_embedded_core(host.db_mut());
    let file = host.new_file("playground.aru".into(), source.into());
    let snapshot = host.snapshot();
    arandu_ide::completions(&snapshot, file, source, offset)
}

fn comptime_markdown(value: &arandu_ide::comptime::ComptimePresentation) -> String {
    let values = value
        .values
        .iter()
        .map(|v| format!("= {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("```arandu\n{values}\n```\n\n*(evaluated at compile time)*")
}

/// Semantic hover information for the in-browser editor at a byte `offset`.
#[must_use]
pub fn hover_source(source: &str, offset: u32) -> Option<WebHover> {
    let line_index = arandu_base::LineIndex::new(source);
    let mut host = arandu_query::AnalysisHost::new();
    configure_web_target(host.db_mut());
    stdlib_core::register_embedded_core(host.db_mut());
    let file = host.new_file("playground.aru".into(), source.into());
    let snapshot = host.snapshot();

    let tc = arandu_ide::typecheck(&snapshot, file);
    let parsed = arandu_query::passes::parse(&snapshot.db, file);
    let program = (**parsed).as_ref().ok();
    if let Some(value) =
        program.and_then(|program| arandu_ide::comptime::value_at(program, &tc, offset))
    {
        let (line, column) = line_index.line_col(value.span.start);
        let (end_line, end_column) = line_index.line_col(value.span.end);
        return Some(WebHover {
            contents: comptime_markdown(&value),
            signature: None,
            documentation: None,
            start: value.span.start,
            end: value.span.end,
            line,
            column,
            end_line,
            end_column,
        });
    }

    let sym = arandu_ide::symbol_at(&tc, offset)
        .or_else(|| arandu_ide::expr_symbol_at(program?, &tc, offset))?;
    let symbol = tc.symbols.try_get(sym)?;
    let presentation = arandu_ide::symbol_presentation(&snapshot, file, &tc, symbol);
    let mut md = format!("```arandu\n{}\n```", presentation.signature);
    if let Some(documentation) = &presentation.documentation {
        md.push_str("\n\n");
        md.push_str(documentation);
    }
    let extra =
        program.and_then(|program| arandu_ide::comptime::arguments_at(program, &tc, offset));
    if let Some(extra) = &extra {
        md.push_str("\n\n");
        md.push_str(&comptime_markdown(extra));
    }
    let tree = arandu_query::passes::syntax_tree(&snapshot.db, file);
    let occurrence = tree
        .tokens()
        .iter()
        .find(|token| token.start <= offset && offset < token.start.saturating_add(token.len))?;
    let span = extra.as_ref().map_or_else(
        || occurrence.span(*file.file_id(&snapshot.db)),
        |extra| extra.span,
    );
    let (line, column) = line_index.line_col(span.start);
    let (end_line, end_column) = line_index.line_col(span.end);
    Some(WebHover {
        contents: md,
        signature: Some(presentation.signature),
        documentation: presentation.documentation,
        start: span.start,
        end: span.end,
        line,
        column,
        end_line,
        end_column,
    })
}

/// Interactive parameter and signature help for the in-browser editor at a byte `offset`.
#[must_use]
pub fn signature_help_source(source: &str, offset: u32) -> Option<WebSignatureHelp> {
    let mut host = arandu_query::AnalysisHost::new();
    configure_web_target(host.db_mut());
    stdlib_core::register_embedded_core(host.db_mut());
    let file = host.new_file("playground.aru".into(), source.into());
    let snapshot = host.snapshot();

    let context = arandu_ide::signature_help::call_context(&snapshot, file, offset)?;
    let active_param = context.active_parameter;

    let tc = arandu_ide::typecheck(&snapshot, file);
    let sym = arandu_ide::signature_help::callee_symbol(&snapshot, file, &tc, &context)?;
    let symbol = tc.symbols.try_get(sym)?;
    if !matches!(
        symbol.kind,
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
    ) {
        return None;
    }
    let presentation = arandu_ide::symbol_presentation(&snapshot, file, &tc, symbol);
    let active_parameter = (!presentation.parameters.is_empty()).then(|| {
        active_param.min(u32::try_from(presentation.parameters.len() - 1).unwrap_or(u32::MAX))
    });
    let parameters = presentation
        .parameters
        .into_iter()
        .map(|parameter| WebParameterInformation {
            label: parameter.label,
        })
        .collect();
    Some(WebSignatureHelp {
        signatures: vec![WebSignatureInformation {
            label: presentation.signature,
            documentation: presentation.documentation,
            parameters,
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

/// Format surface Arandu source code according to official formatter rules.
#[must_use]
pub fn format_source(source: &str) -> String {
    arandu_fmt::format_source(source)
}

// ── C-ABI Exports for In-Browser WebAssembly Host ─────────────────────────────

/// Linear memory allocation helper for the JavaScript host.
#[unsafe(no_mangle)]
pub extern "C" fn arandu_alloc(size: usize) -> *mut u8 {
    let boxed = vec![0u8; size].into_boxed_slice();
    Box::into_raw(boxed) as *mut u8
}

/// Linear memory deallocation helper for the JavaScript host.
///
/// # Safety
/// `ptr` must have been returned by `arandu_alloc` or `Box::into_raw` with matching `size`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_free(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        let slice_ptr = std::ptr::slice_from_raw_parts_mut(ptr, size);
        unsafe {
            drop(Box::from_raw(slice_ptr));
        }
    }
}

/// Raw response header struct returned to JavaScript.
#[repr(C)]
pub struct RawCompileResponse {
    pub success: usize,
    pub wasm_ptr: usize,
    pub wasm_len: usize,
    pub json_ptr: usize,
    pub json_len: usize,
}

/// Compile source code passed from JavaScript and return a pointer to `RawCompileResponse`.
///
/// # Safety
/// `source_ptr` must point to `source_len` valid UTF-8 bytes in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_compile(
    source_ptr: *const u8,
    source_len: usize,
) -> *mut RawCompileResponse {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = if source_ptr.is_null() || source_len == 0 {
            ""
        } else {
            let slice = unsafe { std::slice::from_raw_parts(source_ptr, source_len) };
            std::str::from_utf8(slice).unwrap_or("")
        };

        compile_source(source)
    }));

    let result = match result {
        Ok(res) => res,
        Err(_) => WebCompileResult {
            success: false,
            wasm_bytes: None,
            diagnostics: vec![WebDiagnostic {
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 2,
                length: 1,
                file_id: 0,
                severity: WebSeverity::Error,
                code: Some("ICEGEN001".to_string()),
                message: "Internal compiler error during WebAssembly compilation".to_string(),
                primary_label: None,
                labels: Vec::new(),
                notes: Vec::new(),
                hints: Vec::<WebHint>::new(),
                replacements: Vec::<WebReplacement>::new(),
            }],
        },
    };

    let json_str = serde_json::to_string(&result.diagnostics).unwrap_or_else(|_| "[]".to_string());
    let json_boxed = json_str.into_bytes().into_boxed_slice();
    let json_len = json_boxed.len();
    let json_ptr = Box::into_raw(json_boxed) as *mut u8 as usize;

    let (wasm_ptr, wasm_len) = if let Some(wb) = result.wasm_bytes {
        let boxed = wb.into_boxed_slice();
        let len = boxed.len();
        let ptr = Box::into_raw(boxed) as *mut u8 as usize;
        (ptr, len)
    } else {
        (0, 0)
    };

    let resp = Box::new(RawCompileResponse {
        success: if result.success { 1 } else { 0 },
        wasm_ptr,
        wasm_len,
        json_ptr,
        json_len,
    });
    Box::into_raw(resp)
}

/// Free the `RawCompileResponse` and its contained buffers.
///
/// # Safety
/// `resp_ptr` must have been returned by `arandu_compile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_free_response(resp_ptr: *mut RawCompileResponse) {
    if !resp_ptr.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let resp = unsafe { Box::from_raw(resp_ptr) };
            if resp.wasm_ptr != 0 && resp.wasm_len != 0 {
                unsafe {
                    arandu_free(resp.wasm_ptr as *mut u8, resp.wasm_len);
                }
            }
            if resp.json_ptr != 0 && resp.json_len != 0 {
                unsafe {
                    arandu_free(resp.json_ptr as *mut u8, resp.json_len);
                }
            }
        }));
    }
}

// ── C-ABI Export for Semantic Completion, Hover, and Signature Help ──────────

/// Raw JSON buffer returned to JavaScript by IDE C-ABI functions.
#[repr(C)]
pub struct RawJsonResponse {
    pub ptr: usize,
    pub len: usize,
}

fn into_raw_json_response(json_str: String) -> *mut RawJsonResponse {
    let json_boxed = json_str.into_bytes().into_boxed_slice();
    let json_len = json_boxed.len();
    let json_ptr = Box::into_raw(json_boxed) as *mut u8 as usize;

    Box::into_raw(Box::new(RawJsonResponse {
        ptr: json_ptr,
        len: json_len,
    }))
}

/// Compute semantic completion items for `source` at byte `offset`.
///
/// Returns a pointer to a [`RawJsonResponse`] whose buffer holds a JSON array of
/// completion items. Call [`arandu_free_json`] to release it.
///
/// # Safety
/// `source_ptr` must point to `source_len` valid UTF-8 bytes in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_complete(
    source_ptr: *const u8,
    source_len: usize,
    offset: u32,
) -> *mut RawJsonResponse {
    let json_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = if source_ptr.is_null() || source_len == 0 {
            ""
        } else {
            let slice = unsafe { std::slice::from_raw_parts(source_ptr, source_len) };
            std::str::from_utf8(slice).unwrap_or("")
        };

        let items = completion_source(source, offset);
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
    }))
    .unwrap_or_else(|_| "[]".to_string());

    into_raw_json_response(json_str)
}

/// Compute hover information for `source` at byte `offset`.
///
/// Returns a pointer to a [`RawJsonResponse`] whose buffer holds a serialized
/// [`WebHover`] or `null`. Call [`arandu_free_json`] to release it.
///
/// # Safety
/// `source_ptr` must point to `source_len` valid UTF-8 bytes in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_hover(
    source_ptr: *const u8,
    source_len: usize,
    offset: u32,
) -> *mut RawJsonResponse {
    let json_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = if source_ptr.is_null() || source_len == 0 {
            ""
        } else {
            let slice = unsafe { std::slice::from_raw_parts(source_ptr, source_len) };
            std::str::from_utf8(slice).unwrap_or("")
        };

        let hover = hover_source(source, offset);
        serde_json::to_string(&hover).unwrap_or_else(|_| "null".to_string())
    }))
    .unwrap_or_else(|_| "null".to_string());

    into_raw_json_response(json_str)
}

/// Compute signature help for `source` at byte `offset`.
///
/// Returns a pointer to a [`RawJsonResponse`] whose buffer holds a serialized
/// [`WebSignatureHelp`] or `null`. Call [`arandu_free_json`] to release it.
///
/// # Safety
/// `source_ptr` must point to `source_len` valid UTF-8 bytes in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_signature_help(
    source_ptr: *const u8,
    source_len: usize,
    offset: u32,
) -> *mut RawJsonResponse {
    let json_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = if source_ptr.is_null() || source_len == 0 {
            ""
        } else {
            let slice = unsafe { std::slice::from_raw_parts(source_ptr, source_len) };
            std::str::from_utf8(slice).unwrap_or("")
        };

        let sig_help = signature_help_source(source, offset);
        serde_json::to_string(&sig_help).unwrap_or_else(|_| "null".to_string())
    }))
    .unwrap_or_else(|_| "null".to_string());

    into_raw_json_response(json_str)
}

/// Free the [`RawJsonResponse`] and its JSON buffer.
///
/// # Safety
/// `resp_ptr` must have been returned by [`arandu_complete`], [`arandu_hover`],
/// [`arandu_signature_help`], or [`arandu_format`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_free_json(resp_ptr: *mut RawJsonResponse) {
    if !resp_ptr.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let resp = unsafe { Box::from_raw(resp_ptr) };
            if resp.ptr != 0 && resp.len != 0 {
                unsafe {
                    arandu_free(resp.ptr as *mut u8, resp.len);
                }
            }
        }));
    }
}

// ── C-ABI Export for Source Code Formatting ───────────────────────────────────

/// Format source code passed from JavaScript and return a pointer to [`RawJsonResponse`].
///
/// The JSON payload is a serialized string containing the formatted source.
/// Call [`arandu_free_json`] to release it.
///
/// # Safety
/// `source_ptr` must point to `source_len` valid UTF-8 bytes in memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arandu_format(
    source_ptr: *const u8,
    source_len: usize,
) -> *mut RawJsonResponse {
    let json_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = if source_ptr.is_null() || source_len == 0 {
            ""
        } else {
            let slice = unsafe { std::slice::from_raw_parts(source_ptr, source_len) };
            std::str::from_utf8(slice).unwrap_or("")
        };

        let formatted = format_source(source);
        serde_json::to_string(&formatted).unwrap_or_else(|_| "\"\"".to_string())
    }))
    .unwrap_or_else(|_| "\"\"".to_string());

    into_raw_json_response(json_str)
}
