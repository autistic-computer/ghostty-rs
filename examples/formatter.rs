//! Port of `example/zig-formatter/src/main.zig`: render the VT input read
//! from stdin as HTML with the libghostty-vt Rust API.
//!
//! Run with e.g. `printf '\e[1;31mred\e[0m\n' | cargo run --example formatter`.

use std::io::{Read, Write};

use ghostty::lib_vt as ghostty_vt;
use ghostty::terminal::terminal::Options as TerminalOptions;
use ghostty_vt::formatter::{Format, Options as FormatterOptions, TerminalFormatter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a terminal
    let mut t = ghostty_vt::Terminal::init(TerminalOptions::new(150, 80))?;

    {
        // Create a read-only VT stream for parsing terminal sequences
        let mut stream = t.vt_stream();

        // Read from stdin
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }

            // Replace \n with \r\n
            for &byte in &buf[..n] {
                if byte == b'\n' {
                    stream.next(b'\r');
                }
                stream.next(byte);
            }
        }
    }

    // Use TerminalFormatter to emit HTML
    let mut formatter = TerminalFormatter::init(
        &t,
        FormatterOptions {
            palette: Some(&t.colors.palette.current),
            ..FormatterOptions::new(Format::Html)
        },
    );

    // Write to stdout
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    formatter.format(&mut stdout)?;
    stdout.flush()?;
    Ok(())
}
