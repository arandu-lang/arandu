//! Small deterministic, type-directed program synthesis for compiler fuzzing.
//!
//! The bounded grammar composes scalar expressions into a helper call,
//! fixed-size array, aggregate and payload enum, then reads indexed elements,
//! fields, and enum payloads through control flow. This exercises function ABI,
//! layout, and access paths across the compiler pipeline and backends.

pub mod artifact;
pub mod emi;
pub mod oracle;
pub mod process;
pub mod synth;
pub mod types;
pub mod writer;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(dead_code)]
#[must_use]
pub fn synthesize(seed: u64) -> String {
    synth::synthesize(seed)
}

pub(super) fn run(data: &[u8]) {
    oracle::run(data);
}

pub(super) fn run_c(data: &[u8]) {
    oracle::run_c(data);
}

pub(super) fn run_wasm(data: &[u8]) {
    oracle::run_wasm(data);
}

pub(super) fn run_all_backends(data: &[u8]) {
    oracle::run_all_backends(data);
}

pub(super) fn run_emi_corpus(data: &[u8]) {
    emi::run_emi_corpus(data);
}

pub(super) fn verify_emi_regression_source(source: &str, seed: u64) -> Result<(), String> {
    emi::verify_emi_regression_source(source, seed)
}
