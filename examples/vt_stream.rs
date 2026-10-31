//! Port of `example/zig-vt-stream/src/main.zig`: parsing VT sequences into
//! a terminal with the libghostty-vt Rust API.
//!
//! Run with `cargo run --example vt_stream`.

use ghostty::lib_vt as ghostty_vt;
use ghostty::terminal::terminal::Options as TerminalOptions;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut t = ghostty_vt::Terminal::init(TerminalOptions::new(80, 24))?;

    {
        // Create a read-only VT stream for parsing terminal sequences
        let mut stream = t.vt_stream();

        // Basic text with newline
        stream.next_slice(b"Hello, World!\r\n");

        // ANSI color codes: ESC[1;32m = bold green, ESC[0m = reset
        stream.next_slice(b"\x1b[1;32mGreen Text\x1b[0m\r\n");

        // Cursor positioning: ESC[1;1H = move to row 1, column 1
        stream.next_slice(b"\x1b[1;1HTop-left corner\r\n");

        // Cursor movement: ESC[5B = move down 5 lines
        stream.next_slice(b"\x1b[5B");
        stream.next_slice(b"Moved down!\r\n");

        // Erase line: ESC[2K = clear entire line
        stream.next_slice(b"\x1b[2K");
        stream.next_slice(b"New content\r\n");

        // Multiple lines
        stream.next_slice(b"Line A\r\nLine B\r\nLine C\r\n");
    }

    // Get the final terminal state as a plain string
    let str = t.plain_string()?;
    eprintln!("{str}");
    Ok(())
}
