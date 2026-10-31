//! Port of `src/terminal/size_report.zig`.
//!
//! STATUS: ported

use std::io::Write;

use crate::terminal::size::CellCountInt;

/// Output formats for terminal size reports written to the PTY.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    /// In-band size reports (mode 2048)
    Mode2048 = 0,
    /// XTWINOPS: report text area size in pixels
    Csi14T = 1,
    /// XTWINOPS: report cell size in pixels
    Csi16T = 2,
    /// XTWINOPS: report text area size in characters
    Csi18T = 3,
    /// XTWINOPS: report screen size in characters
    Csi19T = 4,
}

impl Style {
    /// The Zig tag name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Style::Mode2048 => "mode_2048",
            Style::Csi14T => "csi_14_t",
            Style::Csi16T => "csi_16_t",
            Style::Csi18T => "csi_18_t",
            Style::Csi19T => "csi_19_t",
        }
    }
}

/// Runtime size values used to encode terminal size reports.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Terminal row count in cells.
    pub rows: CellCountInt,

    /// Terminal column count in cells.
    pub columns: CellCountInt,

    /// Width of a single terminal cell in pixels.
    pub cell_width: u32,

    /// Height of a single terminal cell in pixels.
    pub cell_height: u32,
}

fn width_pixels(s: Size) -> u64 {
    s.columns as u64 * s.cell_width as u64
}

fn height_pixels(s: Size) -> u64 {
    s.rows as u64 * s.cell_height as u64
}

/// Encode a terminal size report sequence.
pub fn encode<W: Write + ?Sized>(writer: &mut W, style: Style, size: Size) -> std::io::Result<()> {
    match style {
        Style::Mode2048 => write!(
            writer,
            "\x1B[48;{};{};{};{}t",
            size.rows,
            size.columns,
            height_pixels(size),
            width_pixels(size),
        ),

        Style::Csi14T => write!(
            writer,
            "\x1b[4;{};{}t",
            height_pixels(size),
            width_pixels(size),
        ),

        Style::Csi16T => write!(writer, "\x1b[6;{};{}t", size.cell_height, size.cell_width),

        Style::Csi18T => write!(writer, "\x1b[8;{};{}t", size.rows, size.columns),

        // We have no knowledge of the physical screen size so, like VTE,
        // we report the size of the text area in characters.
        Style::Csi19T => write!(writer, "\x1b[9;{};{}t", size.rows, size.columns),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_size() -> Size {
        Size {
            rows: 24,
            columns: 80,
            cell_width: 9,
            cell_height: 18,
        }
    }

    fn encoded(style: Style, size: Size) -> String {
        let mut buf: Vec<u8> = Vec::new();
        encode(&mut buf, style, size).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn encode_mode_2048() {
        assert_eq!("\x1B[48;24;80;432;720t", encoded(Style::Mode2048, test_size()));
    }

    #[test]
    fn encode_csi_14_t() {
        assert_eq!("\x1b[4;432;720t", encoded(Style::Csi14T, test_size()));
    }

    #[test]
    fn encode_csi_16_t() {
        assert_eq!("\x1b[6;18;9t", encoded(Style::Csi16T, test_size()));
    }

    #[test]
    fn encode_csi_18_t() {
        assert_eq!("\x1b[8;24;80t", encoded(Style::Csi18T, test_size()));
    }

    #[test]
    fn encode_csi_19_t() {
        assert_eq!("\x1b[9;24;80t", encoded(Style::Csi19T, test_size()));
    }

    #[test]
    fn encode_max_values_for_all_fields() {
        let max_size = Size {
            rows: CellCountInt::MAX,
            columns: CellCountInt::MAX,
            cell_width: u32::MAX,
            cell_height: u32::MAX,
        };

        let cases: [(Style, &str); 5] = [
            (
                Style::Mode2048,
                "\x1B[48;65535;65535;281470681677825;281470681677825t",
            ),
            (Style::Csi14T, "\x1b[4;281470681677825;281470681677825t"),
            (Style::Csi16T, "\x1b[6;4294967295;4294967295t"),
            (Style::Csi18T, "\x1b[8;65535;65535t"),
            (Style::Csi19T, "\x1b[9;65535;65535t"),
        ];

        for (style, expected) in cases {
            assert_eq!(expected, encoded(style, max_size));
        }
    }
}
