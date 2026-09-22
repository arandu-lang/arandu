//! In-browser WebAssembly compiler bridge for Arandu.
//!
//! Provides both a pure Rust compilation API and a zero-dependency C-ABI export
//! allowing web browsers to compile and run Arandu code entirely client-side.

pub mod diagnostics;
pub mod stdlib_core;

use arandu_middle::layout::DataLayout;
use arandu_query::db::DatabaseImpl;
use diagnostics::{WebDiagnostic, WebSeverity, convert_diagnostic};
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

/// Compile surface Arandu source code in memory targeting WebAssembly (wasm32).
#[must_use]
pub fn compile_source(source: &str) -> WebCompileResult {
    let line_index = arandu_base::LineIndex::new(source);
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
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

    let has_errors = diags
        .iter()
        .any(|d| matches!(d.severity, arandu_middle::Severity::Error));

    if has_errors {
        let web_diags = diags
            .iter()
            .map(|d| convert_diagnostic(d, &line_index))
            .collect();
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
        Ok(bytes) => {
            let web_diags = diags
                .iter()
                .map(|d| convert_diagnostic(d, &line_index))
                .collect();
            WebCompileResult {
                success: true,
                wasm_bytes: Some(bytes),
                diagnostics: web_diags,
            }
        }
        Err(err) => {
            let ice_diag = arandu_middle::Diagnostic::ice(
                arandu_middle::DiagCode::ICEGEN001,
                format!("Wasm codegen error: {err}"),
                arandu_middle::Span::new(0, 0, 0),
            );
            diags.push(ice_diag);
            let web_diags = diags
                .iter()
                .map(|d| convert_diagnostic(d, &line_index))
                .collect();
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
    host.db_mut().set_target_config(DataLayout::ptr_width(4));
    stdlib_core::register_embedded_core(host.db_mut());
    let file = host.new_file("playground.aru".into(), source.into());
    let snapshot = host.snapshot();
    arandu_ide::completions(&snapshot, file, source, offset)
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
                length: 1,
                severity: WebSeverity::Error,
                code: Some("ICEGEN001".to_string()),
                message: "Internal compiler error during WebAssembly compilation".to_string(),
                notes: Vec::new(),
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

// ── C-ABI Export for Semantic Completion ──────────────────────────────────────

/// Raw JSON buffer returned to JavaScript by [`arandu_complete`].
#[repr(C)]
pub struct RawJsonResponse {
    pub ptr: usize,
    pub len: usize,
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

    let json_boxed = json_str.into_bytes().into_boxed_slice();
    let json_len = json_boxed.len();
    let json_ptr = Box::into_raw(json_boxed) as *mut u8 as usize;

    Box::into_raw(Box::new(RawJsonResponse {
        ptr: json_ptr,
        len: json_len,
    }))
}

/// Free the [`RawJsonResponse`] and its JSON buffer.
///
/// # Safety
/// `resp_ptr` must have been returned by [`arandu_complete`] or [`arandu_format`].
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

    let json_boxed = json_str.into_bytes().into_boxed_slice();
    let json_len = json_boxed.len();
    let json_ptr = Box::into_raw(json_boxed) as *mut u8 as usize;

    Box::into_raw(Box::new(RawJsonResponse {
        ptr: json_ptr,
        len: json_len,
    }))
}
