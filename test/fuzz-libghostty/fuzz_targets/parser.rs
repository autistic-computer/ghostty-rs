//! libFuzzer entry point for the parser fuzz target (`cargo fuzz run parser`).

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    ghostty_fuzz::fuzz_parser::test(data);
});
