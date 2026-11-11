//! Consolidated test runner for arandu_query compiler query tests.
//!
//! Consolidating isolated query tests into a single test runner binary reduces
//! cargo test link overhead significantly while discovering all test cases.

#[path = "query_suite/accumulator_cache.rs"]
mod accumulator_cache;

#[path = "query_suite/cst_pipeline.rs"]
mod cst_pipeline;

#[path = "query_suite/empty_struct_mf.rs"]
mod empty_struct_mf;

#[path = "query_suite/explain_rebuild.rs"]
mod explain_rebuild;

#[path = "query_suite/file_highlights.rs"]
mod file_highlights;

#[path = "query_suite/generic_cross_module.rs"]
mod generic_cross_module;

#[path = "query_suite/inference_vars.rs"]
mod inference_vars;

#[path = "query_suite/holes.rs"]
mod holes;

#[path = "query_suite/indirect_call.rs"]
mod indirect_call;

#[path = "query_suite/owned_text.rs"]
mod owned_text;

#[path = "query_suite/owning_closures.rs"]
mod owning_closures;

#[path = "query_suite/missing_import_cascade.rs"]
mod missing_import_cascade;

#[path = "query_suite/module_overview_fallback.rs"]
mod module_overview_fallback;

#[path = "query_suite/path_resolution.rs"]
mod path_resolution;

#[path = "query_suite/salsa_cutoff.rs"]
mod salsa_cutoff;

#[path = "query_suite/syntax_tree.rs"]
mod syntax_tree;

#[path = "query_suite/variant_sugar.rs"]
mod variant_sugar;
