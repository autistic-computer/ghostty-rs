//! Port of `src/cli/list_colors.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, IsTerminal, Write};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::tui::{self, vaxis};
use crate::terminal::x11_color;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If `true`, print without formatting even if printing to a tty
        pub plain: bool = false,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"list-colors"`).
pub const HELP: &str = "The `list-colors` command is used to list all the named RGB colors in
Ghostty.

Flags:

  * `--plain`: will disable formatting and make the output more
    friendly for Unix tooling. This is default when not printing to a tty.";

/// Zig `std.ascii.orderIgnoreCase`.
fn order_ignore_case(lhs: &str, rhs: &str) -> std::cmp::Ordering {
    lhs.bytes()
        .map(|b| b.to_ascii_lowercase())
        .cmp(rhs.bytes().map(|b| b.to_ascii_lowercase()))
}

/// The sorted list of color names (Zig: the keys of `x11_color.map`).
fn sorted_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = x11_color::entries()
        .iter()
        .map(|e| e.name.to_str().unwrap())
        .collect();
    keys.sort_unstable_by(|a, b| order_ignore_case(a, b));
    keys
}

/// The `list-colors` command is used to list all the named RGB colors in
/// Ghostty.
///
/// Flags:
///
///   * `--plain`: will disable formatting and make the output more
///     friendly for Unix tooling. This is default when not printing to a tty.
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let keys = sorted_keys();

    let stdout = io::stdout();
    if tui::CAN_PRETTY_PRINT && !opts.plain && stdout.is_terminal() {
        return pretty_print(&keys);
    }

    let mut writer = BufWriter::with_capacity(4096, stdout.lock());
    for name in &keys {
        let rgb = x11_color::MAP.get(name.as_bytes()).unwrap();
        writeln!(writer, "{} = #{:02x}{:02x}{:02x}", name, rgb.r, rgb.g, rgb.b)?;
    }
    writer.flush()?;

    Ok(0)
}

fn pretty_print(keys: &[&str]) -> RunResult {
    // Set up vaxis
    let mut tty = vaxis::Tty::init()?;
    let mut vx = vaxis::Vaxis::init();

    // We know we are ghostty, so let's enable mode 2027. Vaxis normally does this but you need an
    // event loop to auto-enable it.
    vx.caps.unicode = true;
    tty.writer().write_all(vaxis::ctlseqs::UNICODE_SET.as_bytes())?;

    let result = pretty_print_inner(&mut tty, &mut vx, keys);

    let _ = tty.writer().write_all(vaxis::ctlseqs::UNICODE_RESET.as_bytes());
    vx.deinit(tty.writer());
    result
}

fn pretty_print_inner(tty: &mut vaxis::Tty, vx: &mut vaxis::Vaxis, keys: &[&str]) -> RunResult {
    let winsize = tty.get_winsize()?;
    vx.resize(tty.writer(), winsize)?;

    let mut max_name_len: usize = 0;
    for name in keys {
        max_name_len = max_name_len.max(name.len());
    }

    // max name length plus " = #RRGGBB XX" plus "  " gutter between columns
    let column_size = max_name_len + 15;
    // add two to take into account lack of gutter after last column
    let width = vx.window().width as usize;
    let columns: usize = (width + 2) / column_size;
    if columns == 0 {
        return Ok(0);
    }

    let step = keys.len() / columns + 1;
    for i in 0..step {
        {
            let win = vx.window();
            win.clear();

            let seg = |text: &'static str| vaxis::Segment {
                text,
                ..Default::default()
            };
            let at = |col: u16| vaxis::PrintOptions {
                col_offset: col,
                ..Default::default()
            };

            let mut result = vaxis::PrintResult::default();

            for j in 0..columns {
                let k = i + step * j;
                if k >= keys.len() {
                    continue;
                }

                let name = keys[k];
                let rgb = x11_color::MAP.get(name.as_bytes()).unwrap();

                let style1 = vaxis::Style {
                    fg: vaxis::Color::Rgb([rgb.r, rgb.g, rgb.b]),
                    ..Default::default()
                };
                let style2 = vaxis::Style {
                    fg: vaxis::Color::Rgb([rgb.r, rgb.g, rgb.b]),
                    bg: vaxis::Color::Rgb([rgb.r, rgb.g, rgb.b]),
                    ..Default::default()
                };

                // name of the color
                result = win.print_segment(
                    vaxis::Segment {
                        text: name,
                        ..Default::default()
                    },
                    at(result.col),
                );
                // push the color data to the end of the column
                for _ in 0..max_name_len - name.len() {
                    result = win.print_segment(seg(" "), at(result.col));
                }
                result = win.print_segment(seg(" = "), at(result.col));
                // rgb triple
                let hex = format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b);
                result = win.print_segment(
                    vaxis::Segment {
                        text: &hex,
                        style: style1,
                        ..Default::default()
                    },
                    at(result.col),
                );
                result = win.print_segment(seg(" "), at(result.col));
                // colored block
                result = win.print_segment(
                    vaxis::Segment {
                        text: "  ",
                        style: style2,
                        ..Default::default()
                    },
                    at(result.col),
                );
                // add the gutter if needed
                if j + 1 < columns {
                    result = win.print_segment(seg("  "), at(result.col));
                }
            }

            // clear the rest of the line
            while result.col != 0 {
                result = win.print_segment(seg(" "), at(result.col));
            }
        }

        // output the data
        vx.pretty_print(tty.writer())?;
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sorted_case_insensitive() {
        let keys = sorted_keys();
        assert!(keys.len() > 700);
        for w in keys.windows(2) {
            assert_ne!(std::cmp::Ordering::Greater, order_ignore_case(w[0], w[1]));
        }
    }
}
