//! Port of `src/input/mouse_encode.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! - `*std.Io.Writer` is any `std::io::Write` and `std.Io.Writer.Error`
//!   is `std::io::Error`.
//! - `Options.last_cell` (`?*?point.Coordinate`) is an
//!   `Option<&mut Option<Coordinate>>`, so `Options` carries a lifetime
//!   and is not `Copy`; the private helpers take `&Options`.
//! - `Options` implements `Default` (Zig requires `size` to be given);
//!   the default `size` is `Size::default()`.

use std::io::{self, Write};

use crate::input::key::Mods;
use crate::input::mouse;
use crate::renderer::size as renderer_size;
use crate::terminal::mouse::{self as terminal_mouse, Event as MouseEvent, Format as MouseFormat};
use crate::terminal::point;
use crate::terminal::terminal::Terminal;

/// Options that affect mouse encoding behavior and provide runtime context.
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// Terminal mouse reporting mode (X10, normal, button, any).
    pub event: MouseEvent,

    /// Terminal mouse reporting format.
    pub format: MouseFormat,

    /// Full renderer size used to convert surface-space pixel positions
    /// into grid cell coordinates (for most formats) and terminal-space
    /// pixel coordinates (for SGR-Pixels), as well as to determine
    /// whether a position falls outside the visible viewport.
    pub size: renderer_size::Size,

    /// Whether any mouse button is currently pressed. When a motion
    /// event occurs outside the viewport, it is only reported if a
    /// button is held down and the event mode supports motion tracking.
    /// Without this, out-of-viewport motions are silently dropped.
    ///
    /// This should reflect the state of the current event as well, so
    /// if the encoded event is a button press, this should be true.
    pub any_button_pressed: bool,

    /// Last reported viewport cell for motion deduplication.
    /// If null, motion deduplication state is not tracked.
    pub last_cell: Option<&'a mut Option<point::Coordinate>>,
}

impl Options<'_> {
    /// Initialize from terminal and renderer state. The caller may still
    /// set any_button_pressed and last_cell on the returned value.
    pub fn from_terminal(t: &Terminal, size: renderer_size::Size) -> Options<'static> {
        Options {
            event: t.flags.mouse_event,
            format: t.flags.mouse_format,
            size,
            any_button_pressed: false,
            last_cell: None,
        }
    }
}

/// A normalized mouse event for protocol encoding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// The action of this mouse event.
    pub action: mouse::Action,

    /// The button involved in this event. This can be null in the
    /// case of a motion action with no pressed buttons.
    pub button: Option<mouse::Button>,

    /// Keyboard modifiers held during this event.
    pub mods: Mods,

    /// Mouse position in terminal-space pixels, with (0, 0) at the top-left
    /// of the terminal. Negative values are allowed and indicate positions
    /// above or to the left of the terminal. Values larger than the terminal
    /// size are also allowed and indicate right or below the terminal.
    pub pos: Pos,
}

impl Default for Event {
    fn default() -> Self {
        Event {
            action: mouse::Action::Press,
            button: None,
            mods: Mods::default(),
            pos: Pos::default(),
        }
    }
}

/// Mouse position in surface-space pixels.
///
/// (Zig: `Event.Pos`, an `extern struct`.)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

/// Encode the mouse event to the writer according to the options.
///
/// Not all events result in output.
pub fn encode<W: Write + ?Sized>(
    writer: &mut W,
    event: Event,
    opts: Options<'_>,
) -> io::Result<()> {
    let mut opts = opts;
    if !should_report(event, &opts) {
        return Ok(());
    }

    // Handle scenarios where the mouse position is outside the viewport.
    // We always report release events no matter where they happen.
    if event.action != mouse::Action::Release && pos_out_of_viewport(event.pos, opts.size) {
        // If we don't have a motion-tracking event mode, do nothing,
        // because events outside the viewport are never reported in
        // such cases.
        if !terminal_mouse::event_sends_motion(opts.event) {
            return Ok(());
        }

        // For motion modes, we only report if a button is currently pressed.
        // This lets a TUI detect a click over the surface + drag out
        // of the surface.
        if !opts.any_button_pressed {
            return Ok(());
        }
    }

    let cell = pos_to_cell(event.pos, opts.size);

    // We only send motion events when the cell changed unless
    // we're tracking raw pixels.
    if event.action == mouse::Action::Motion && opts.format != MouseFormat::SgrPixels {
        if let Some(last) = opts.last_cell.as_deref() {
            if let Some(last_cell) = *last {
                if last_cell.eql(cell) {
                    return Ok(());
                }
            }
        }
    }

    // Update the last reported cell if we are tracking it.
    if let Some(last) = opts.last_cell.as_deref_mut() {
        *last = Some(cell);
    }

    let Some(button_code) = button_code(event, &opts) else {
        return Ok(());
    };
    match opts.format {
        MouseFormat::X10 => {
            if cell.x > 222 || cell.y > 222 {
                log::info!(target: "mouse_encode", "X10 mouse format can only encode X/Y up to 223");
                return Ok(());
            }

            // + 1 because our x/y are zero-indexed and the protocol uses 1-indexing.
            writer.write_all(b"\x1B[M")?;
            writer.write_all(&[
                32 + button_code,
                32 + cell.x as u8 + 1,
                32 + cell.y as u8 + 1,
            ])?;
        }

        MouseFormat::Utf8 => {
            writer.write_all(b"\x1B[M")?;
            write_codepoint(writer, 32 + button_code as u32)?;

            let x_cp: u32 = cell.x as u32 + 33;
            let y_cp: u32 = cell.y + 33;

            write_codepoint(writer, x_cp)?;
            write_codepoint(writer, y_cp)?;
        }

        MouseFormat::Sgr => write!(
            writer,
            "\x1B[<{};{};{}{}",
            button_code,
            cell.x as u32 + 1,
            cell.y + 1,
            if event.action == mouse::Action::Release {
                'm'
            } else {
                'M'
            },
        )?,

        MouseFormat::Urxvt => write!(
            writer,
            "\x1B[{};{};{}M",
            32 + button_code as u32,
            cell.x as u32 + 1,
            cell.y + 1,
        )?,

        MouseFormat::SgrPixels => {
            let pixels = pos_to_pixels(event.pos, opts.size);
            write!(
                writer,
                "\x1B[<{};{};{}{}",
                button_code,
                pixels.x,
                pixels.y,
                if event.action == mouse::Action::Release {
                    'm'
                } else {
                    'M'
                },
            )?;
        }
    }

    Ok(())
}

/// Write a codepoint as UTF-8 (Zig: `printUnicodeCodepoint` /
/// `std.unicode.utf8Encode(...) catch unreachable`).
fn write_codepoint<W: Write + ?Sized>(writer: &mut W, cp: u32) -> io::Result<()> {
    let c = char::from_u32(cp).expect("valid codepoint");
    let mut buf = [0u8; 4];
    writer.write_all(c.encode_utf8(&mut buf).as_bytes())
}

/// Returns true if this event should be reported for the given mouse
/// event mode.
fn should_report(event: Event, opts: &Options<'_>) -> bool {
    match opts.event {
        MouseEvent::None => false,

        // X10 only reports button presses of left, middle, and right.
        MouseEvent::X10 => {
            event.action == mouse::Action::Press
                && matches!(
                    event.button,
                    Some(mouse::Button::Left | mouse::Button::Middle | mouse::Button::Right)
                )
        }

        // Normal mode does not report motion.
        MouseEvent::Normal => event.action != mouse::Action::Motion,

        // Button mode requires an active button for motion events.
        MouseEvent::Button => event.button.is_some(),

        // Any mode reports everything.
        MouseEvent::Any => true,
    }
}

fn button_code(event: Event, opts: &Options<'_>) -> Option<u8> {
    let mut acc: u8 = 'code: {
        let Some(button) = event.button else {
            // Null button means motion with no pressed button.
            break 'code 3;
        };

        if event.action == mouse::Action::Release
            && opts.format != MouseFormat::Sgr
            && opts.format != MouseFormat::SgrPixels
        {
            // Legacy releases are always encoded as button 3.
            break 'code 3;
        }

        match button {
            mouse::Button::Left => 0,
            mouse::Button::Middle => 1,
            mouse::Button::Right => 2,
            mouse::Button::Four => 64,
            mouse::Button::Five => 65,
            mouse::Button::Six => 66,
            mouse::Button::Seven => 67,
            mouse::Button::Eight => 128,
            mouse::Button::Nine => 129,
            _ => return None,
        }
    };

    // X10 does not include modifiers.
    if opts.event != MouseEvent::X10 {
        if event.mods.shift {
            acc += 4;
        }
        if event.mods.alt {
            acc += 8;
        }
        if event.mods.ctrl {
            acc += 16;
        }
    }

    // Motion adds another bit.
    if event.action == mouse::Action::Motion {
        acc += 32;
    }

    Some(acc)
}

/// Terminal-space pixel position for SGR pixel reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelPoint {
    x: i32,
    y: i32,
}

/// Returns true if the surface-space pixel position is outside the
/// visible viewport bounds (negative or beyond screen dimensions).
fn pos_out_of_viewport(pos: Pos, size: renderer_size::Size) -> bool {
    let max_x = size.screen.width as f32;
    let max_y = size.screen.height as f32;
    pos.x < 0.0 || pos.y < 0.0 || pos.x > max_x || pos.y > max_y
}

/// Converts a surface-space pixel position to a zero-based grid cell
/// coordinate (column, row) within the terminal viewport. Out-of-bounds
/// values are clamped to the valid grid range (0 to columns/rows - 1).
fn pos_to_cell(pos: Pos, size: renderer_size::Size) -> point::Coordinate {
    let coord = renderer_size::Coordinate::Surface(renderer_size::CoordinateSurface {
        x: pos.x as f64,
        y: pos.y as f64,
    });
    let renderer_size::Coordinate::Grid(grid) =
        coord.convert(renderer_size::CoordinateTag::Grid, size)
    else {
        unreachable!()
    };
    point::Coordinate {
        x: grid.x,
        y: grid.y as u32,
    }
}

/// Converts a surface-space pixel position to terminal-space pixel
/// coordinates (accounting for padding/scaling) used by SGR-Pixels mode.
/// Unlike grid conversion, terminal-space coordinates are not clamped
/// and may be negative or exceed the terminal dimensions.
fn pos_to_pixels(pos: Pos, size: renderer_size::Size) -> PixelPoint {
    let renderer_size::Coordinate::Terminal(coord) =
        renderer_size::Coordinate::Surface(renderer_size::CoordinateSurface {
            x: pos.x as f64,
            y: pos.y as f64,
        })
        .convert(renderer_size::CoordinateTag::Terminal, size)
    else {
        unreachable!()
    };

    PixelPoint {
        x: coord.x.round() as i32,
        y: coord.y.round() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::mouse::{Action, Button};

    fn test_size() -> renderer_size::Size {
        renderer_size::Size {
            screen: renderer_size::ScreenSize {
                width: 1_000,
                height: 1_000,
            },
            cell: renderer_size::CellSize {
                width: 1,
                height: 1,
            },
            padding: renderer_size::Padding::default(),
        }
    }

    fn ev(button: Option<Button>, action: Action) -> Event {
        Event {
            button,
            action,
            ..Default::default()
        }
    }

    fn opts(event: MouseEvent) -> Options<'static> {
        Options {
            event,
            size: test_size(),
            ..Default::default()
        }
    }

    const ACTIONS: [Action; 3] = [Action::Press, Action::Release, Action::Motion];

    #[test]
    fn should_report_none_mode_never_reports() {
        for action in ACTIONS {
            assert!(!should_report(
                ev(Some(Button::Left), action),
                &opts(MouseEvent::None)
            ));
        }
    }

    #[test]
    fn should_report_x10_reports_only_left_middle_right_press() {
        // Left, middle, right presses should report.
        for btn in [Button::Left, Button::Middle, Button::Right] {
            assert!(should_report(
                ev(Some(btn), Action::Press),
                &opts(MouseEvent::X10)
            ));
        }

        // Release is not reported.
        assert!(!should_report(
            ev(Some(Button::Left), Action::Release),
            &opts(MouseEvent::X10)
        ));

        // Motion is not reported.
        assert!(!should_report(
            ev(Some(Button::Left), Action::Motion),
            &opts(MouseEvent::X10)
        ));

        // Other buttons are not reported.
        assert!(!should_report(
            ev(Some(Button::Four), Action::Press),
            &opts(MouseEvent::X10)
        ));

        // Null button is not reported.
        assert!(!should_report(
            ev(None, Action::Press),
            &opts(MouseEvent::X10)
        ));
    }

    #[test]
    fn should_report_normal_reports_press_and_release_but_not_motion() {
        assert!(should_report(
            ev(Some(Button::Left), Action::Press),
            &opts(MouseEvent::Normal)
        ));

        assert!(should_report(
            ev(Some(Button::Left), Action::Release),
            &opts(MouseEvent::Normal)
        ));

        assert!(!should_report(
            ev(Some(Button::Left), Action::Motion),
            &opts(MouseEvent::Normal)
        ));
    }

    #[test]
    fn should_report_button_mode_requires_a_button() {
        // With a button, all actions report.
        for action in ACTIONS {
            assert!(should_report(
                ev(Some(Button::Left), action),
                &opts(MouseEvent::Button)
            ));
        }

        // Without a button (null), nothing reports.
        for action in ACTIONS {
            assert!(!should_report(ev(None, action), &opts(MouseEvent::Button)));
        }
    }

    #[test]
    fn should_report_any_mode_reports_everything() {
        for action in ACTIONS {
            assert!(should_report(
                ev(Some(Button::Left), action),
                &opts(MouseEvent::Any)
            ));
        }

        // Even null button + motion reports.
        assert!(should_report(
            ev(None, Action::Motion),
            &opts(MouseEvent::Any)
        ));
    }

    /// Encode with a fresh `last` cell and the test size, returning the
    /// written bytes (Zig: a `std.Io.Writer.fixed` over a 32-byte buffer).
    fn enc(event: Event, mev: MouseEvent, format: MouseFormat) -> Vec<u8> {
        let mut writer: Vec<u8> = Vec::new();
        let mut last: Option<point::Coordinate> = None;
        encode(
            &mut writer,
            event,
            Options {
                event: mev,
                format,
                size: test_size(),
                last_cell: Some(&mut last),
                ..Default::default()
            },
        )
        .unwrap();
        writer
    }

    fn at(button: Option<Button>, action: Action, x: f32, y: f32) -> Event {
        Event {
            button,
            action,
            pos: Pos { x, y },
            ..Default::default()
        }
    }

    #[test]
    fn x10_press_left() {
        let out = enc(
            Event {
                button: Some(Button::Left),
                action: Action::Press,
                mods: Mods {
                    shift: true,
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                },
                pos: Pos { x: 0.0, y: 0.0 },
            },
            MouseEvent::X10,
            MouseFormat::X10,
        );

        assert_eq!(&out[..], &[0x1B, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn x10_ignores_release() {
        let out = enc(
            ev(Some(Button::Left), Action::Release),
            MouseEvent::X10,
            MouseFormat::X10,
        );
        assert_eq!(0, out.len());
    }

    #[test]
    fn normal_ignores_motion() {
        let out = enc(
            ev(Some(Button::Left), Action::Motion),
            MouseEvent::Normal,
            MouseFormat::Sgr,
        );
        assert_eq!(0, out.len());
    }

    #[test]
    fn button_mode_requires_button() {
        let out = enc(
            ev(None, Action::Motion),
            MouseEvent::Button,
            MouseFormat::Sgr,
        );
        assert_eq!(0, out.len());
    }

    #[test]
    fn sgr_release_keeps_button_identity() {
        let out = enc(
            at(Some(Button::Right), Action::Release, 4.0, 5.0),
            MouseEvent::Any,
            MouseFormat::Sgr,
        );
        assert_eq!(&out[..], b"\x1B[<2;5;6m");
    }

    #[test]
    fn sgr_motion_with_no_button() {
        let out = enc(
            at(None, Action::Motion, 1.0, 2.0),
            MouseEvent::Any,
            MouseFormat::Sgr,
        );
        assert_eq!(&out[..], b"\x1B[<35;2;3M");
    }

    #[test]
    fn urxvt_with_modifiers() {
        let out = enc(
            Event {
                button: Some(Button::Left),
                action: Action::Press,
                mods: Mods {
                    shift: true,
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                },
                pos: Pos { x: 2.0, y: 3.0 },
            },
            MouseEvent::Any,
            MouseFormat::Urxvt,
        );
        assert_eq!(&out[..], b"\x1B[60;3;4M");
    }

    #[test]
    fn utf8_encodes_large_coordinates() {
        let out = enc(
            at(Some(Button::Left), Action::Press, 300.0, 400.0),
            MouseEvent::Any,
            MouseFormat::Utf8,
        );
        assert_eq!(&out[0..4], &[0x1B, b'[', b'M', 32]);

        let view = std::str::from_utf8(&out[4..]).unwrap();
        let mut it = view.chars();
        assert_eq!(Some(333u32), it.next().map(|c| c as u32));
        assert_eq!(Some(433u32), it.next().map(|c| c as u32));
        assert_eq!(None, it.next());
    }

    #[test]
    fn utf8_encodes_extended_mouse_buttons() {
        let mut writer: Vec<u8> = Vec::new();
        encode(
            &mut writer,
            at(Some(Button::Eight), Action::Press, 2.0, 3.0),
            Options {
                event: MouseEvent::Any,
                format: MouseFormat::Utf8,
                size: test_size(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(&writer[..], "\x1B[M\u{A0}#$".as_bytes());
    }

    #[test]
    fn x10_coordinate_limit() {
        let out = enc(
            at(Some(Button::Left), Action::Press, 223.0, 0.0),
            MouseEvent::X10,
            MouseFormat::X10,
        );
        assert_eq!(0, out.len());
    }

    #[test]
    fn sgr_wheel_button_mappings() {
        let cases: [(Button, u8); 4] = [
            (Button::Four, 64),
            (Button::Five, 65),
            (Button::Six, 66),
            (Button::Seven, 67),
        ];

        for (button, code) in cases {
            let out = enc(
                at(Some(button), Action::Press, 0.0, 0.0),
                MouseEvent::Any,
                MouseFormat::Sgr,
            );

            let want = format!("\x1B[<{};1;1M", code);
            assert_eq!(&out[..], want.as_bytes());
        }
    }

    #[test]
    fn urxvt_release_uses_legacy_button_3_encoding() {
        let out = enc(
            at(Some(Button::Right), Action::Release, 2.0, 3.0),
            MouseEvent::Any,
            MouseFormat::Urxvt,
        );
        assert_eq!(&out[..], b"\x1B[35;3;4M");
    }

    #[test]
    fn unsupported_button_is_ignored() {
        let out = enc(
            at(Some(Button::Ten), Action::Press, 1.0, 1.0),
            MouseEvent::Any,
            MouseFormat::Sgr,
        );
        assert_eq!(0, out.len());
    }

    #[test]
    fn sgr_pixels_uses_terminal_space_cursor_coordinates() {
        let out = enc(
            at(Some(Button::Left), Action::Press, 10.0, 20.0),
            MouseEvent::Any,
            MouseFormat::SgrPixels,
        );
        assert_eq!(&out[..], b"\x1B[<0;10;20M");
    }

    #[test]
    fn sgr_pixels_release_keeps_button_identity() {
        let out = enc(
            at(Some(Button::Right), Action::Release, 10.0, 20.0),
            MouseEvent::Any,
            MouseFormat::SgrPixels,
        );
        assert_eq!(&out[..], b"\x1B[<2;10;20m");
    }

    #[test]
    fn position_exactly_at_viewport_boundary_is_encoded_in_final_cell() {
        let size = renderer_size::Size {
            screen: renderer_size::ScreenSize {
                width: 10,
                height: 10,
            },
            cell: renderer_size::CellSize {
                width: 2,
                height: 2,
            },
            padding: renderer_size::Padding::default(),
        };

        let mut writer: Vec<u8> = Vec::new();
        let mut last: Option<point::Coordinate> = None;
        encode(
            &mut writer,
            at(Some(Button::Left), Action::Press, 10.0, 10.0),
            Options {
                event: MouseEvent::Any,
                format: MouseFormat::Sgr,
                size,
                last_cell: Some(&mut last),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(&writer[..], b"\x1B[<0;5;5M");
    }

    #[test]
    fn outside_viewport_motion_with_no_pressed_button_is_ignored() {
        let mut writer: Vec<u8> = Vec::new();
        let mut last: Option<point::Coordinate> = None;
        encode(
            &mut writer,
            at(Some(Button::Left), Action::Motion, -1.0, -1.0),
            Options {
                event: MouseEvent::Any,
                format: MouseFormat::Sgr,
                size: test_size(),
                any_button_pressed: false,
                last_cell: Some(&mut last),
            },
        )
        .unwrap();

        assert_eq!(0, writer.len());
    }

    #[test]
    fn outside_viewport_motion_with_pressed_button_is_reported() {
        let mut writer: Vec<u8> = Vec::new();
        let mut last: Option<point::Coordinate> = None;
        encode(
            &mut writer,
            at(Some(Button::Left), Action::Motion, -1.0, -1.0),
            Options {
                event: MouseEvent::Any,
                format: MouseFormat::Sgr,
                size: test_size(),
                any_button_pressed: true,
                last_cell: Some(&mut last),
            },
        )
        .unwrap();

        assert_eq!(&writer[..], b"\x1B[<32;1;1M");
    }

    #[test]
    fn motion_is_deduped_by_last_cell_except_sgr_pixels() {
        let mut last: Option<point::Coordinate> = None;

        {
            let mut writer: Vec<u8> = Vec::new();
            encode(
                &mut writer,
                at(Some(Button::Left), Action::Motion, 5.0, 6.0),
                Options {
                    event: MouseEvent::Any,
                    format: MouseFormat::Sgr,
                    size: test_size(),
                    last_cell: Some(&mut last),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(!writer.is_empty());
        }

        {
            let mut writer: Vec<u8> = Vec::new();
            encode(
                &mut writer,
                at(Some(Button::Left), Action::Motion, 5.0, 6.0),
                Options {
                    event: MouseEvent::Any,
                    format: MouseFormat::Sgr,
                    size: test_size(),
                    last_cell: Some(&mut last),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(0, writer.len());
        }

        {
            let mut writer: Vec<u8> = Vec::new();
            encode(
                &mut writer,
                at(Some(Button::Left), Action::Motion, 5.0, 6.0),
                Options {
                    event: MouseEvent::Any,
                    format: MouseFormat::SgrPixels,
                    size: test_size(),
                    last_cell: Some(&mut last),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(!writer.is_empty());
        }
    }
}
