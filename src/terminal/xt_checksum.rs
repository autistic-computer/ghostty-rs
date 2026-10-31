//! Port of `src/terminal/xt_checksum.zig`.
//!
//! STATUS: ported
//!
//! DECRQCRA (Request Checksum of Rectangular Area) and XTCHECKSUM
//! (select the checksum variant DECRQCRA computes).
//!
//! The checksum follows xterm's `xtermCheckRect`. No real VT420 was
//! available to compare against while this was written, so xterm is the
//! reference:
//!
//! - `xtermCheckRect`: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/screen.c#L3162-L3290
//! - `xtermCharSetDec`: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/charsets.c#L608

use std::io;

use crate::terminal::page::{Cell, Wide};
use crate::terminal::page_list::{CellSubset, Direction, PageList};
use crate::terminal::point::{Coordinate, Point};
use crate::terminal::screen::Screen;
use crate::terminal::selection::Selection;
use crate::terminal::sgr;
use crate::terminal::style::Style;
use crate::terminal::terminal::ScrollingRegion;

/// The XTCHECKSUM (CSI Ps # y) bits. The zero value is the DEC checksum,
/// which is also what a full reset restores.
///
/// These are xterm's `CSBITS`, which it also takes from the
/// `checksumExtension` resource:
///
/// - `CSBITS`: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/ptyx.h#L488-L496
/// - XTCHECKSUM: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/charproc.c#L6044-L6050
/// - Reset to `checksumExtension`: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/charproc.c#L14424-L14426
/// - `checksumExtension` docs: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/xterm.man#L2870-L2896
/// - XTCHECKSUM docs: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-411/ctlseqs.ms#L2562-L2569
///
/// xterm's documentation disagrees with its code in two places, and we
/// follow the code. The docs describe bit 3 as "omit checksum for cells
/// not explicitly initialized", but `csDRAWN` does the opposite: without
/// it those cells are skipped, and with it they count as spaces. The man
/// page also lists a bit 5, "do not mask cell value to 7 bits", which
/// `xtermCheckRect` never looks at.
///
/// `packed struct(u5)`: bit 0 is `positive`, then `no_attributes`,
/// `no_trim`, `undrawn`, `full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags {
    /// Don't negate the result.
    pub positive: bool,

    /// Don't add the VT100 video attributes to each cell.
    pub no_attributes: bool,

    /// Don't omit blanks. The DEC checksum only counts a plain space if it
    /// is the first cell of the rectangle.
    pub no_trim: bool,

    /// Count cells that were never written to as spaces instead of
    /// skipping them.
    pub undrawn: bool,

    /// Use the full codepoint rather than the DEC 8-bit value. Wide
    /// spacers are skipped and, as in xterm, combining marks are not
    /// counted in this mode.
    pub full: bool,
}

impl Flags {
    /// Unpack from the u5 backing integer (`@bitCast`); higher bits are
    /// ignored.
    pub const fn from_bits(v: u8) -> Flags {
        Flags {
            positive: v & 0b1 != 0,
            no_attributes: v & 0b10 != 0,
            no_trim: v & 0b100 != 0,
            undrawn: v & 0b1000 != 0,
            full: v & 0b10000 != 0,
        }
    }

    /// Pack into the u5 backing integer (`@bitCast`).
    pub const fn to_bits(self) -> u8 {
        (self.positive as u8)
            | (self.no_attributes as u8) << 1
            | (self.no_trim as u8) << 2
            | (self.undrawn as u8) << 3
            | (self.full as u8) << 4
    }
}

/// A DECRQCRA request as it arrives in `CSI Pi ; Pg ; Pt ; Pl ; Pb ; Pr * y`.
/// The coordinates are 1-based and zero means the parameter was omitted.
/// The page number (Pg) is ignored since we only have one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct Request {
    pub id: u16,
    pub top: u16,
    pub left: u16,
    pub bottom: u16,
    pub right: u16,
}

impl Request {
    /// Resolve the request to a rectangle selection of the active area.
    /// If `origin` is set (DECOM), coordinates are relative to its
    /// top-left, as in xterm. Coordinates are clamped to the screen.
    /// Returns null if the rectangle is empty.
    pub fn selection(self, pages: &PageList, origin: Option<ScrollingRegion>) -> Option<Selection> {
        let rows = pages.rows;
        let cols = pages.cols;
        if rows == 0 || cols == 0 {
            return None;
        }
        let top_margin: u16 = origin.map_or(0, |o| o.top);
        let left_margin: u16 = origin.map_or(0, |o| o.left);
        let top = Self::limit(self.top, 1, top_margin, rows);
        let left = Self::limit(self.left, 1, left_margin, cols);
        let bottom = Self::limit(self.bottom, rows, top_margin, rows);
        let right = Self::limit(self.right, cols, left_margin, cols);
        if top > bottom || left > right {
            return None;
        }
        Some(Selection::init(
            pages.pin(Point::Active(Coordinate {
                x: left - 1,
                y: (top - 1) as u32,
            }))?,
            pages.pin(Point::Active(Coordinate {
                x: right - 1,
                y: (bottom - 1) as u32,
            }))?,
            true,
        ))
    }

    fn limit(v: u16, default: u16, margin: u16, max: u16) -> u16 {
        let n = if v == 0 { default } else { v };
        n.saturating_add(margin).clamp(1, max)
    }
}

/// Compute the checksum of a rectangle selection.
pub fn compute(screen: &Screen, sel_: Option<Selection>, flags: Flags) -> u16 {
    // An empty rectangle still gets a reply, with a sum of zero.
    let Some(sel) = sel_ else { return 0 };
    let tl = sel.top_left(screen);
    let br = sel.bottom_right(screen);

    // Everything is summed modulo 2^16 since only 16 bits are reported.
    let mut sum: u16 = 0;

    // The DEC checksum omits plain spaces except for the very first cell
    // counted in the rectangle.
    let mut first = true;

    for row in tl.row_iterator(Direction::RightDown, Some(br)) {
        // Pages may be narrower than the rectangle, as in xterm, where
        // a line may be shorter than the screen.
        // SAFETY: the row iterator yields valid pins.
        let cells = unsafe { &*row.cells(CellSubset::All) };
        let left = (tl.x as usize).min(cells.len());
        let right = (br.x as usize + 1).min(cells.len());

        for cell in &cells[left..right] {
            let mut ch: u16 = match value(cell, flags) {
                Value::Skip => continue,
                Value::Undrawn => {
                    if flags.no_trim || flags.undrawn {
                        b' ' as u16
                    } else {
                        continue;
                    }
                }
                Value::Value(v) => v,
            };

            let s = row.style(cell);
            if !flags.no_attributes {
                ch = ch.wrapping_add(attributes(cell, s));
            }

            if flags.no_trim {
                sum = sum.wrapping_add(ch);

                // xterm adds combining marks only in the DEC mode, and
                // only to the untrimmed sum.
                if !flags.full && cell.has_grapheme() {
                    if let Some(cps) = row.grapheme(cell) {
                        for &cp in cps {
                            sum = sum.wrapping_add(cp as u16);
                        }
                    }
                }
            } else if first || ch != b' ' as u16 || extended(s) {
                sum = sum.wrapping_add(ch);
            }

            first = flags.no_trim;
        }

        if !flags.no_trim {
            first = false;
        }
    }

    if flags.positive {
        sum
    } else {
        0u16.wrapping_sub(sum)
    }
}

enum Value {
    /// Not counted at all.
    Skip,

    /// Never written to.
    Undrawn,

    /// The value of the character, before attributes.
    Value(u16),
}

fn value(cell: &Cell, flags: Flags) -> Value {
    // The right half of a wide character. xterm stores a placeholder
    // outside of 8 bits there, so the DEC checksum counts it as ESC.
    if cell.wide() == Wide::SpacerTail {
        return if flags.full { Value::Skip } else { Value::Value(0x1B) };
    }

    if !cell.has_text() {
        return Value::Undrawn;
    }

    let cp = cell.codepoint();
    if flags.full {
        return Value::Value(cp as u16);
    }

    // The DEC 8-bit value, as xterm's `xtermCharSetDec` produces it for
    // the ASCII character set. xterm works from the byte as it was
    // received, before character set translation, so it counts DEC line
    // drawing as the letter that was sent. We only have the translated
    // codepoint, so those count as characters outside of 8 bits.
    Value::Value(match cp {
        0x7F | 0xFF => 0,
        0x20..=0x7E | 0x80..=0x9F | 0xA1..=0xFE => (cp & 0x7F) as u16,
        _ => 0x1B,
    })
}

/// The VT100 video attributes xterm adds to each cell.
fn attributes(cell: &Cell, s: Style) -> u16 {
    let mut result: u16 = 0;
    if cell.protected() {
        result += 0x04;
    }
    if s.flags.invisible {
        result += 0x08;
    }
    if s.flags.underline != sgr::Underline::None {
        result += 0x10;
    }
    if s.flags.inverse {
        result += 0x20;
    }
    if s.flags.blink {
        result += 0x40;
    }
    if s.flags.bold {
        result += 0x80;
    }
    result
}

/// Attributes that xterm doesn't add to the sum but that still keep a
/// space from being trimmed.
fn extended(s: Style) -> bool {
    s.flags.faint || s.flags.italic || s.flags.strikethrough || s.flags.underline == sgr::Underline::Double
}

/// The largest reply `encode` writes.
pub const MAX_ENCODE_SIZE: usize = b"\x1bP65535!~FFFF\x1b\\".len();

/// Encode the DECRQCRA reply, `DCS Pi ! ~ XXXX ST`.
pub fn encode<W: io::Write + ?Sized>(writer: &mut W, id: u16, sum: u16) -> io::Result<()> {
    write!(writer, "\x1bP{}!~{:04X}\x1b\\", id, sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xt_checksum_encode() {
        let mut buf = [0u8; MAX_ENCODE_SIZE];
        let mut writer: &mut [u8] = &mut buf;
        encode(&mut writer, 65535, 0xFFFF).unwrap();
        let n = MAX_ENCODE_SIZE - writer.len();
        assert_eq!(b"\x1bP65535!~FFFF\x1b\\", &buf[..n]);

        let mut writer: &mut [u8] = &mut buf;
        encode(&mut writer, 1, 0x1A).unwrap();
        let n = MAX_ENCODE_SIZE - writer.len();
        assert_eq!(b"\x1bP1!~001A\x1b\\", &buf[..n]);
    }

    #[test]
    fn flags_bits_round_trip() {
        assert_eq!(Flags::default(), Flags::from_bits(0));
        assert_eq!(
            Flags {
                positive: true,
                no_trim: true,
                ..Default::default()
            },
            Flags::from_bits(5)
        );
        assert_eq!(
            Flags {
                full: true,
                ..Default::default()
            },
            Flags::from_bits(48 & 0x1F)
        );
        for v in 0..32u8 {
            assert_eq!(v, Flags::from_bits(v).to_bits());
        }
    }

    use crate::terminal::point::Tag;
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    fn term() -> Terminal {
        Terminal::init(TerminalOptions::new(10, 5)).unwrap()
    }

    fn active(x: u16, y: u32) -> Point {
        Point::Active(Coordinate { x, y })
    }

    fn req(top: u16, left: u16, bottom: u16, right: u16) -> Request {
        Request {
            id: 0,
            top,
            left,
            bottom,
            right,
        }
    }

    fn flags(f: impl FnOnce(&mut Flags)) -> Flags {
        let mut v = Flags::default();
        f(&mut v);
        v
    }

    #[test]
    fn xt_checksum_selection_defaults_and_clamping() {
        let t = term();
        let pages = &t.screens.active.pages;

        {
            let sel = Request::default().selection(pages, None).unwrap();
            assert!(sel.rectangle);
            assert_eq!(active(0, 0), pages.point_from_pin(Tag::Active, sel.start()).unwrap());
            assert_eq!(active(9, 4), pages.point_from_pin(Tag::Active, sel.end()).unwrap());
        }

        {
            let r = req(2, 3, 99, 99);
            let sel = r.selection(pages, None).unwrap();
            assert_eq!(active(2, 1), pages.point_from_pin(Tag::Active, sel.start()).unwrap());
            assert_eq!(active(9, 4), pages.point_from_pin(Tag::Active, sel.end()).unwrap());
        }

        assert!(req(3, 0, 2, 0).selection(pages, None).is_none());
        assert!(req(0, 3, 0, 2).selection(pages, None).is_none());
    }

    #[test]
    fn xt_checksum_selection_origin_mode() {
        let t = term();
        let pages = &t.screens.active.pages;
        let region = ScrollingRegion {
            top: 1,
            bottom: 3,
            left: 2,
            right: 8,
        };

        {
            let sel = Request::default().selection(pages, Some(region)).unwrap();
            assert_eq!(active(2, 1), pages.point_from_pin(Tag::Active, sel.start()).unwrap());
            assert_eq!(active(9, 4), pages.point_from_pin(Tag::Active, sel.end()).unwrap());
        }

        {
            let r = req(1, 1, 1, 1);
            let sel = r.selection(pages, Some(region)).unwrap();
            assert_eq!(active(2, 1), pages.point_from_pin(Tag::Active, sel.start()).unwrap());
            assert_eq!(active(2, 1), pages.point_from_pin(Tag::Active, sel.end()).unwrap());
        }
    }

    #[test]
    fn xt_checksum_dec_trims_blanks_and_skips_undrawn_cells() {
        let mut t = term();
        t.print_string("a b").unwrap();
        let s = &*t.screens.active;

        // Only the first row, the whole width. The space is omitted and the
        // unwritten cells are skipped.
        let sel = req(1, 0, 1, 0).selection(&s.pages, None);
        assert_eq!(
            0u16.wrapping_sub((b'a' + b'b') as u16),
            compute(s, sel, Flags::default())
        );
        assert_eq!((b'a' + b'b') as u16, compute(s, sel, flags(|f| f.positive = true)));

        // Untrimmed counts the space and the undrawn cells as spaces.
        assert_eq!(
            b'a' as u16 + b'b' as u16 + b' ' as u16 * 8,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.no_trim = true;
                })
            )
        );

        // Undrawn cells are spaces, which are then trimmed.
        assert_eq!(
            (b'a' + b'b') as u16,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.undrawn = true;
                })
            )
        );
    }

    #[test]
    fn xt_checksum_dec_counts_a_leading_space() {
        let mut t = term();
        t.print_string(" a\n b").unwrap();
        let s = &*t.screens.active;

        // The very first counted cell is kept even if it's a space, but not
        // the first cell of later rows.
        let sel = Request::default().selection(&s.pages, None);
        assert_eq!(
            b' ' as u16 + b'a' as u16 + b'b' as u16,
            compute(s, sel, flags(|f| f.positive = true))
        );
    }

    #[test]
    fn xt_checksum_empty_rectangle() {
        let mut t = term();
        t.print_string("abc").unwrap();
        let s = &*t.screens.active;
        let r = req(0, 3, 0, 2);
        assert_eq!(0, compute(s, r.selection(&s.pages, None), Flags::default()));
    }

    #[test]
    fn xt_checksum_rectangle_bounds() {
        let mut t = term();
        t.print_string("abc\ndef\nghi").unwrap();
        let s = &*t.screens.active;
        let sel = req(2, 2, 3, 3).selection(&s.pages, None);
        assert_eq!(
            b'e' as u16 + b'f' as u16 + b'h' as u16 + b'i' as u16,
            compute(s, sel, flags(|f| f.positive = true))
        );
    }

    #[test]
    fn xt_checksum_attributes() {
        let mut t = term();
        t.set_attribute(sgr::Attribute::Bold).unwrap();
        t.set_attribute(sgr::Attribute::Underline(sgr::Underline::Single))
            .unwrap();
        t.print_string("a").unwrap();
        t.set_attribute(sgr::Attribute::Unset).unwrap();
        t.set_attribute(sgr::Attribute::Inverse).unwrap();
        t.set_attribute(sgr::Attribute::Blink).unwrap();
        t.print_string(" ").unwrap();
        t.set_attribute(sgr::Attribute::Unset).unwrap();
        t.set_attribute(sgr::Attribute::Invisible).unwrap();
        t.set_protected_mode(crate::terminal::ansi::ProtectedMode::Dec);
        t.print_string("b").unwrap();
        let s = &*t.screens.active;

        let sel = req(1, 0, 1, 0).selection(&s.pages, None);
        assert_eq!(
            b'a' as u16 + 0x80 + 0x10 + b' ' as u16 + 0x20 + 0x40 + b'b' as u16 + 0x08 + 0x04,
            compute(s, sel, flags(|f| f.positive = true))
        );
        assert_eq!(
            (b'a' + b'b') as u16,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.no_attributes = true;
                })
            )
        );
    }

    #[test]
    fn xt_checksum_extended_attributes_keep_a_space() {
        let mut t = term();
        t.print_string("a").unwrap();
        t.set_attribute(sgr::Attribute::Italic).unwrap();
        t.print_string(" ").unwrap();
        let s = &*t.screens.active;

        let sel = req(1, 0, 1, 0).selection(&s.pages, None);
        assert_eq!((b'a' + b' ') as u16, compute(s, sel, flags(|f| f.positive = true)));
    }

    #[test]
    fn xt_checksum_dec_8_bit_values() {
        let mut t = term();

        // é is 0xE9, masked to 7 bits; the euro sign is outside of 8 bits.
        t.print_string("é€").unwrap();
        let s = &*t.screens.active;
        let sel = req(1, 0, 1, 0).selection(&s.pages, None);
        assert_eq!(0x69u16 + 0x1B, compute(s, sel, flags(|f| f.positive = true)));
        assert_eq!(
            0xE9u16 + 0x20AC,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.full = true;
                })
            )
        );
    }

    #[test]
    fn xt_checksum_wide_characters() {
        let mut t = term();
        t.print_string("橋").unwrap();
        let s = &*t.screens.active;

        let sel = req(1, 0, 1, 0).selection(&s.pages, None);
        assert_eq!(0x1Bu16 + 0x1B, compute(s, sel, flags(|f| f.positive = true)));
        assert_eq!(
            0x6A4Bu16,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.full = true;
                })
            )
        );
    }

    #[test]
    fn xt_checksum_combining_marks() {
        let mut t = term();
        t.modes.set(crate::terminal::modes::Mode::GraphemeCluster, true);
        t.print_string("e\u{301}").unwrap();
        let s = &*t.screens.active;

        let sel = req(1, 0, 1, 1).selection(&s.pages, None);
        assert_eq!(b'e' as u16, compute(s, sel, flags(|f| f.positive = true)));
        assert_eq!(
            b'e' as u16 + 0x301,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.no_trim = true;
                })
            )
        );
        assert_eq!(
            b'e' as u16,
            compute(
                s,
                sel,
                flags(|f| {
                    f.positive = true;
                    f.no_trim = true;
                    f.full = true;
                })
            )
        );
    }
}
