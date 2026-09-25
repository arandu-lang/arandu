#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    arandu_fuzz_support::run(arandu_fuzz_support::Target::EmiCorpus, data);
});
