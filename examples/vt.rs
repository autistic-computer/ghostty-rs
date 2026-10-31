//! Port of `example/zig-vt/src/main.zig`: the libghostty-vt Rust API.
//!
//! Run with `cargo run --example vt`.

use ghostty::lib_vt as ghostty_vt;
use ghostty::terminal::terminal::Options as TerminalOptions;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize a terminal.
    let mut t = ghostty_vt::Terminal::init(TerminalOptions {
        cols: 6,
        rows: 40,
        ..Default::default()
    })?;

    // Write some text. It'll wrap because this is too long for our
    // columns size above (6).
    t.print_string("Hello, World!")?;

    // Get the plain string view of the terminal screen.
    let str = t.plain_string()?;
    eprintln!("{str}");
    Ok(())
}
