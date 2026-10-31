//! libFuzzer entry point for the osc fuzz target (`cargo fuzz run osc`).

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    ghostty_fuzz::fuzz_osc::test(data);
});
