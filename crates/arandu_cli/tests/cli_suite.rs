//! Consolidated integration test suite for Arandu CLI commands and workflows.
//!
//! Preserves standalone executables for CI process/determinism tests
//! (`cache_process_concurrency`, `cli_project_concurrency`, `cli_project_determinism`,
//! `cli_project_adversarial`) while consolidating non-conflicting CLI tests into a
//! single binary to save dozens of linker invocations.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "common/mod.rs"]
mod common;

#[path = "common/wasm_host.rs"]
mod wasm_host;

#[path = "cli_suite/cli_archive.rs"]
mod cli_archive;

#[path = "cli_suite/cli_autoref.rs"]
mod cli_autoref;

#[path = "cli_suite/cli_ownership_regressions.rs"]
mod cli_ownership_regressions;

#[path = "cli_suite/cli_bench_slt4.rs"]
mod cli_bench_slt4;

#[path = "cli_suite/cli_borrow_safety.rs"]
mod cli_borrow_safety;

#[path = "cli_suite/cli_borrowed_views.rs"]
mod cli_borrowed_views;

#[path = "cli_suite/cli_cgu_incremental.rs"]
mod cli_cgu_incremental;

#[path = "cli_suite/cli_daemon.rs"]
mod cli_daemon;

#[path = "cli_suite/cli_color_handling.rs"]
mod cli_color_handling;

#[path = "cli_suite/cli_comptime.rs"]
mod cli_comptime;

#[path = "cli_suite/cli_decimal.rs"]
mod cli_decimal;

#[path = "cli_suite/cli_core_foundation.rs"]
mod cli_core_foundation;

#[path = "cli_suite/cli_doc_stdlib_types.rs"]
mod cli_doc_stdlib_types;

#[path = "cli_suite/cli_ergonomics.rs"]
mod cli_ergonomics;

#[path = "cli_suite/cli_elf_in_place_linker.rs"]
mod cli_elf_in_place_linker;

#[path = "cli_suite/cli_incremental_session.rs"]
mod cli_incremental_session;

#[path = "cli_suite/cli_minimal_gold.rs"]
mod cli_minimal_gold;

#[path = "cli_suite/cli_module_symbols.rs"]
mod cli_module_symbols;

#[path = "cli_suite/cli_opt_await_suspend.rs"]
mod cli_opt_await_suspend;

#[path = "cli_suite/cli_opt_while_backedge.rs"]
mod cli_opt_while_backedge;

#[path = "cli_suite/cli_backend_parity.rs"]
mod cli_backend_parity;

#[path = "cli_suite/cli_bounds_check_elim.rs"]
mod cli_bounds_check_elim;

#[path = "cli_suite/cli_emit_c_locals.rs"]
mod cli_emit_c_locals;

#[path = "cli_suite/cli_inspection_opt_level.rs"]
mod cli_inspection_opt_level;

#[path = "cli_suite/cli_loop_deletion.rs"]
mod cli_loop_deletion;

#[path = "cli_suite/cli_range_index.rs"]
mod cli_range_index;

#[path = "cli_suite/cli_soundness_regressions.rs"]
mod cli_soundness_regressions;

#[path = "cli_suite/cli_option_nil.rs"]
mod cli_option_nil;

#[path = "cli_suite/cli_parallel_order.rs"]
mod cli_parallel_order;

#[path = "cli_suite/cli_project_gold.rs"]
mod cli_project_gold;

#[path = "cli_suite/cli_rfc0021_stdlib_facade.rs"]
mod cli_rfc0021_stdlib_facade;

#[path = "cli_suite/cli_slt5_product.rs"]
mod cli_slt5_product;

#[path = "cli_suite/cli_smoke.rs"]
mod cli_smoke;

#[path = "cli_suite/cli_structured_job.rs"]
mod cli_structured_job;

#[path = "cli_suite/cli_syn1_match_join.rs"]
mod cli_syn1_match_join;

#[path = "cli_suite/cli_syn2_syn4.rs"]
mod cli_syn2_syn4;

#[path = "cli_suite/cli_test_adversarial.rs"]
mod cli_test_adversarial;

#[path = "cli_suite/cli_test_list.rs"]
mod cli_test_list;

#[path = "cli_suite/cli_test_slt3_expect.rs"]
mod cli_test_slt3_expect;

#[path = "cli_suite/cli_torture_cascade.rs"]
mod cli_torture_cascade;

#[path = "cli_suite/cli_typ2_sls.rs"]
mod cli_typ2_sls;

#[path = "cli_suite/cli_vec_defaults.rs"]
mod cli_vec_defaults;

#[path = "cli_suite/cli_wasm_build.rs"]
mod cli_wasm_build;

#[path = "cli_suite/cli_while_mut_backedge.rs"]
mod cli_while_mut_backedge;

#[path = "cli_suite/cli_byte_literal.rs"]
mod cli_byte_literal;

#[path = "cli_suite/cli_time_wide.rs"]
mod cli_time_wide;

#[path = "cli_suite/cli_llvm_backend.rs"]
mod cli_llvm_backend;
#[path = "cli_suite/cli_llvm_freestanding_abi.rs"]
mod cli_llvm_freestanding_abi;

#[path = "cli_suite/cli_math.rs"]
mod cli_math;

#[path = "cli_suite/cli_alloc_structures.rs"]
mod cli_alloc_structures;

#[path = "cli_suite/cli_native_thread.rs"]
mod cli_native_thread;

#[path = "cli_suite/cli_big_simd.rs"]
mod cli_big_simd;

#[path = "cli_suite/cli_bigint_oracle.rs"]
mod cli_bigint_oracle;
