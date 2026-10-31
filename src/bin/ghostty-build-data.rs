//! The `ghostty-build-data` binary (Zig: `main_build_data.zig`, built by
//! `src/build/GhosttyResources.zig`). It prints generated resource files
//! (terminfo, shell completions, editor syntax) to stdout; see
//! `tools/install_resources.sh`.

fn main() {
    if let Err(err) = ghostty::main_build_data::main() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
