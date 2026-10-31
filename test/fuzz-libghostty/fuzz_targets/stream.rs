//! libFuzzer entry point for the stream fuzz target (`cargo fuzz run stream`).

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    ghostty_fuzz::fuzz_stream::test(data);
});
