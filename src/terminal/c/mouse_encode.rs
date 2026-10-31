//! Port of `src/terminal/c/mouse_encode.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `terminal.mouse.Event`/`Format` are `lib.Enum`s
//! (`c_int` in C ABI builds) but `u8`-backed in the Rust core, so
//! [`TrackingMode`] and [`Format`] are their C mirrors. Option values are
//! read through `c_int` pointers and validated like Zig's runtime check.

use std::ffi::{c_int, c_void};

use super::mouse_event::Event;
use super::result::Result;
use super::terminal::Terminal;
use super::types::{encode_to_buffer, HandleAllocator};
use crate::input::mouse_encode as input_mouse_encode;
use crate::lib::allocator::Allocator as CAllocator;
use crate::renderer::size as renderer_size;
use crate::terminal::mouse as terminal_mouse;
use crate::terminal::point;

const LOG_TARGET: &str = "mouse_encode";

/// Wrapper around mouse encoding options that tracks the allocator for C
/// API usage. The options are stored without `last_cell`, which points at
/// this wrapper's `last_cell` while encoding.
pub struct MouseEncoderWrapper {
    pub opts: input_mouse_encode::Options<'static>,
    pub track_last_cell: bool,
    pub last_cell: std::option::Option<point::Coordinate>,
    alloc: HandleAllocator,
}

/// C: GhosttyMouseEncoder
pub type Encoder = *mut MouseEncoderWrapper;

crate::c_api_enum! {
    /// C: GhosttyMouseTrackingMode (Zig: `terminal.mouse.Event`).
    pub enum TrackingMode {
        None = 0,
        X10 = 1,
        Normal = 2,
        Button = 3,
        Any = 4,
    }
}

impl TrackingMode {
    pub fn to_zig(self) -> terminal_mouse::Event {
        match self {
            TrackingMode::None => terminal_mouse::Event::None,
            TrackingMode::X10 => terminal_mouse::Event::X10,
            TrackingMode::Normal => terminal_mouse::Event::Normal,
            TrackingMode::Button => terminal_mouse::Event::Button,
            TrackingMode::Any => terminal_mouse::Event::Any,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyMouseFormat (Zig: `terminal.mouse.Format`).
    pub enum Format {
        X10 = 0,
        Utf8 = 1,
        Sgr = 2,
        Urxvt = 3,
        SgrPixels = 4,
    }
}

impl Format {
    pub fn to_zig(self) -> terminal_mouse::Format {
        match self {
            Format::X10 => terminal_mouse::Format::X10,
            Format::Utf8 => terminal_mouse::Format::Utf8,
            Format::Sgr => terminal_mouse::Format::Sgr,
            Format::Urxvt => terminal_mouse::Format::Urxvt,
            Format::SgrPixels => terminal_mouse::Format::SgrPixels,
        }
    }
}

/// C: GhosttyMouseEncoderSize
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub size: usize,
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

impl Size {
    fn to_renderer(self) -> std::option::Option<renderer_size::Size> {
        if self.cell_width == 0 || self.cell_height == 0 {
            return None;
        }
        Some(renderer_size::Size {
            screen: renderer_size::ScreenSize {
                width: self.screen_width,
                height: self.screen_height,
            },
            cell: renderer_size::CellSize {
                width: self.cell_width,
                height: self.cell_height,
            },
            padding: renderer_size::Padding {
                top: self.padding_top,
                bottom: self.padding_bottom,
                right: self.padding_right,
                left: self.padding_left,
            },
        })
    }
}

crate::c_api_enum! {
    /// C: GhosttyMouseEncoderOption
    ///
    /// Input type expected for setting each option (Zig: `Option.InType`):
    /// `GhosttyMouseTrackingMode` for event, `GhosttyMouseFormat` for
    /// format, `GhosttyMouseEncoderSize` for size and `bool` for
    /// any_button_pressed/track_last_cell.
    pub enum Option {
        Event = 0,
        Format = 1,
        Size = 2,
        AnyButtonPressed = 3,
        TrackLastCell = 4,
    }
}

/// C: ghostty_mouse_encoder_new
#[export_name = "ghostty_mouse_encoder_new"]
pub extern "C" fn new(alloc_: std::option::Option<&CAllocator>, result: &mut Encoder) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(MouseEncoderWrapper {
        opts: input_mouse_encode::Options {
            size: default_size(),
            ..Default::default()
        },
        track_last_cell: false,
        last_cell: None,
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_mouse_encoder_free
///
/// # Safety
/// `encoder_` must be NULL or an encoder from [`new`] not used afterwards.
#[export_name = "ghostty_mouse_encoder_free"]
pub unsafe extern "C" fn free(encoder_: Encoder) {
    let Some(wrapper) = encoder_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(encoder_);
}

/// C: ghostty_mouse_encoder_setopt
///
/// # Safety
/// `encoder_` must be a valid encoder; `value` must be NULL or point to the
/// option's input type.
#[export_name = "ghostty_mouse_encoder_setopt"]
pub unsafe extern "C" fn setopt(encoder_: Encoder, option: c_int, value: *const c_void) {
    let Some(option) = Option::from_int(option) else {
        log::warn!(target: LOG_TARGET, "setopt invalid option value={option}");
        return;
    };
    if value.is_null() {
        return;
    }
    setopt_typed(encoder_, option, value);
}

unsafe fn setopt_typed(encoder_: Encoder, option: Option, value: *const c_void) {
    let wrapper = &mut *encoder_;
    match option {
        Option::Event => {
            let raw = *(value as *const c_int);
            let Some(v) = TrackingMode::from_int(raw) else {
                log::warn!(target: LOG_TARGET, "setopt invalid TrackingMode value={raw}");
                return;
            };
            let v = v.to_zig();
            if wrapper.opts.event != v {
                wrapper.last_cell = None;
            }
            wrapper.opts.event = v;
        }

        Option::Format => {
            let raw = *(value as *const c_int);
            let Some(v) = Format::from_int(raw) else {
                log::warn!(target: LOG_TARGET, "setopt invalid Format value={raw}");
                return;
            };
            let v = v.to_zig();
            if wrapper.opts.format != v {
                wrapper.last_cell = None;
            }
            wrapper.opts.format = v;
        }

        Option::Size => {
            // Read the leading size field first: an older caller's struct
            // may be smaller than ours.
            let size = *(value as *const usize);
            if size < std::mem::size_of::<Size>() {
                log::warn!(
                    target: LOG_TARGET,
                    "setopt size struct too small size={} expected>={}",
                    size,
                    std::mem::size_of::<Size>(),
                );
                return;
            }

            let value = *(value as *const Size);
            let Some(s) = value.to_renderer() else {
                log::warn!(
                    target: LOG_TARGET,
                    "setopt invalid size values (cell width and height must be non-zero)"
                );
                return;
            };
            wrapper.opts.size = s;
            wrapper.last_cell = None;
        }

        Option::AnyButtonPressed => wrapper.opts.any_button_pressed = *(value as *const bool),

        Option::TrackLastCell => {
            let v = *(value as *const bool);
            wrapper.track_last_cell = v;
            if !v {
                wrapper.last_cell = None;
            }
        }
    }
}

/// C: ghostty_mouse_encoder_setopt_from_terminal
///
/// # Safety
/// `encoder_` and `terminal_` must each be NULL or valid.
#[export_name = "ghostty_mouse_encoder_setopt_from_terminal"]
pub unsafe extern "C" fn setopt_from_terminal(encoder_: Encoder, terminal_: Terminal) {
    let Some(wrapper) = encoder_.as_mut() else {
        return;
    };
    let Some(t) = terminal_.as_ref() else {
        return;
    };
    let t = t.terminal();
    wrapper.opts.event = t.flags.mouse_event;
    wrapper.opts.format = t.flags.mouse_format;
    wrapper.last_cell = None;
}

/// C: ghostty_mouse_encoder_reset
///
/// # Safety
/// `encoder_` must be NULL or a valid encoder.
#[export_name = "ghostty_mouse_encoder_reset"]
pub unsafe extern "C" fn reset(encoder_: Encoder) {
    let Some(wrapper) = encoder_.as_mut() else {
        return;
    };
    wrapper.last_cell = None;
}

/// The stored options with `last_cell` pointed at `last_cell`.
fn options_with<'a>(
    opts: &input_mouse_encode::Options<'static>,
    last_cell: std::option::Option<&'a mut std::option::Option<point::Coordinate>>,
) -> input_mouse_encode::Options<'a> {
    input_mouse_encode::Options {
        event: opts.event,
        format: opts.format,
        size: opts.size,
        any_button_pressed: opts.any_button_pressed,
        last_cell,
    }
}

/// C: ghostty_mouse_encoder_encode
///
/// # Safety
/// `encoder_`/`event_` must be NULL or valid; a non-null `out_` must be
/// valid for `out_len` writable bytes.
#[export_name = "ghostty_mouse_encoder_encode"]
pub unsafe extern "C" fn encode(
    encoder_: Encoder,
    event_: Event,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let Some(wrapper) = encoder_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(event) = event_.as_ref() else {
        return Result::InvalidValue;
    };
    let event = event.event;

    let prev_last_cell = wrapper.last_cell;

    // The first attempt writes into the caller's buffer and updates the
    // dedupe state; a retry only counts the required size.
    let mut first = true;
    let mut count_last_cell = prev_last_cell;
    let track = wrapper.track_last_cell;
    let result = encode_to_buffer(out_, out_len, out_written, |w| {
        if first {
            first = false;
            let opts = options_with(&wrapper.opts, if track { Some(&mut wrapper.last_cell) } else { None });
            input_mouse_encode::encode(w, event, opts)
        } else {
            // Failed writes should not mutate motion dedupe state because
            // no complete sequence was produced.
            wrapper.last_cell = prev_last_cell;

            // Use a counting writer to count how much space we would have
            // needed.
            let opts = options_with(&wrapper.opts, if track { Some(&mut count_last_cell) } else { None });
            input_mouse_encode::encode(w, event, opts)
        }
    });

    result
}

fn default_size() -> renderer_size::Size {
    renderer_size::Size {
        screen: renderer_size::ScreenSize { width: 1, height: 1 },
        cell: renderer_size::CellSize { width: 1, height: 1 },
        padding: renderer_size::Padding::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::mouse::{Action, Button};
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::{mouse_event, terminal as terminal_c};

    fn ptr<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    fn test_size() -> Size {
        Size {
            size: std::mem::size_of::<Size>(),
            screen_width: 1_000,
            screen_height: 1_000,
            cell_width: 1,
            cell_height: 1,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }

    unsafe fn setup_sgr_any(encoder: Encoder) {
        let event_mode = TrackingMode::Any;
        setopt(encoder, Option::Event as c_int, ptr(&event_mode));
        let format_mode = Format::Sgr;
        setopt(encoder, Option::Format as c_int, ptr(&format_mode));
        let size = test_size();
        setopt(encoder, Option::Size as c_int, ptr(&size));
    }

    #[test]
    fn alloc() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
        unsafe { free(e) };
    }

    #[test]
    fn setopt_test() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            let event_mode = TrackingMode::Any;
            setopt(e, Option::Event as c_int, ptr(&event_mode));
            assert_eq!(terminal_mouse::Event::Any, (*e).opts.event);

            let format_mode = Format::Sgr;
            setopt(e, Option::Format as c_int, ptr(&format_mode));
            assert_eq!(terminal_mouse::Format::Sgr, (*e).opts.format);

            let size = test_size();
            setopt(e, Option::Size as c_int, ptr(&size));
            assert_eq!(size.screen_width, (*e).opts.size.screen.width);
            assert_eq!(size.screen_height, (*e).opts.size.screen.height);

            let any_button_pressed = true;
            setopt(e, Option::AnyButtonPressed as c_int, ptr(&any_button_pressed));
            assert!((*e).opts.any_button_pressed);

            let track_last_cell = true;
            setopt(e, Option::TrackLastCell as c_int, ptr(&track_last_cell));
            assert!((*e).track_last_cell);

            free(e);
        }
    }

    #[test]
    fn setopt_from_terminal_test() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 80, 24));

        unsafe {
            let event_mode = TrackingMode::Any;
            setopt(e, Option::Event as c_int, ptr(&event_mode));
            let format_mode = Format::Sgr;
            setopt(e, Option::Format as c_int, ptr(&format_mode));

            setopt_from_terminal(e, t);
            assert_eq!(terminal_mouse::Event::None, (*e).opts.event);
            assert_eq!(terminal_mouse::Format::X10, (*e).opts.format);

            terminal_c::free(t);
            free(e);
        }
    }

    #[test]
    fn setopt_from_terminal_null() {
        unsafe {
            setopt_from_terminal(std::ptr::null_mut(), std::ptr::null_mut());

            let mut t: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 80, 24));
            setopt_from_terminal(std::ptr::null_mut(), t);

            let mut e: Encoder = std::ptr::null_mut();
            assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
            setopt_from_terminal(e, std::ptr::null_mut());

            free(e);
            terminal_c::free(t);
        }
    }

    #[test]
    fn encode_sgr_press_left() {
        let mut encoder: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut encoder));

        unsafe {
            setup_sgr_any(encoder);

            let mut event: Event = std::ptr::null_mut();
            assert_eq!(Result::Success, mouse_event::new(Some(&C_ALLOCATOR), &mut event));

            mouse_event::set_action(event, Action::Press as c_int);
            mouse_event::set_button(event, Button::Left as c_int);
            mouse_event::set_position(event, mouse_event::Position { x: 0.0, y: 0.0 });

            let mut required: usize = 0;
            assert_eq!(
                Result::OutOfSpace,
                encode(encoder, event, std::ptr::null_mut(), 0, &mut required)
            );
            assert_eq!(9, required);

            let mut buf = [0u8; 32];
            let mut written: usize = 0;
            assert_eq!(
                Result::Success,
                encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
            );
            assert_eq!(required, written);
            assert_eq!(b"\x1b[<0;1;1M", &buf[..written]);

            mouse_event::free(event);
            free(encoder);
        }
    }

    #[test]
    fn encode_motion_dedupe_and_reset() {
        let mut encoder: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut encoder));

        unsafe {
            setup_sgr_any(encoder);
            let track_last_cell = true;
            setopt(encoder, Option::TrackLastCell as c_int, ptr(&track_last_cell));

            let mut event: Event = std::ptr::null_mut();
            assert_eq!(Result::Success, mouse_event::new(Some(&C_ALLOCATOR), &mut event));

            mouse_event::set_action(event, Action::Motion as c_int);
            mouse_event::set_button(event, Button::Left as c_int);
            mouse_event::set_position(event, mouse_event::Position { x: 5.0, y: 6.0 });

            {
                let mut buf = [0u8; 32];
                let mut written: usize = 0;
                assert_eq!(
                    Result::Success,
                    encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
                );
                assert!(written > 0);
            }

            {
                let mut buf = [0u8; 32];
                let mut written: usize = 0;
                assert_eq!(
                    Result::Success,
                    encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
                );
                assert_eq!(0, written);
            }

            reset(encoder);

            {
                let mut buf = [0u8; 32];
                let mut written: usize = 0;
                assert_eq!(
                    Result::Success,
                    encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
                );
                assert!(written > 0);
            }

            mouse_event::free(event);
            free(encoder);
        }
    }

    #[test]
    fn encode_querying_required_size_doesnt_update_dedupe_state() {
        let mut encoder: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut encoder));

        unsafe {
            setup_sgr_any(encoder);
            let track_last_cell = true;
            setopt(encoder, Option::TrackLastCell as c_int, ptr(&track_last_cell));

            let mut event: Event = std::ptr::null_mut();
            assert_eq!(Result::Success, mouse_event::new(Some(&C_ALLOCATOR), &mut event));

            mouse_event::set_action(event, Action::Motion as c_int);
            mouse_event::set_button(event, Button::Left as c_int);
            mouse_event::set_position(event, mouse_event::Position { x: 5.0, y: 6.0 });

            let mut required: usize = 0;
            assert_eq!(
                Result::OutOfSpace,
                encode(encoder, event, std::ptr::null_mut(), 0, &mut required)
            );
            assert!(required > 0);

            let mut buf = [0u8; 32];
            let mut written: usize = 0;
            assert_eq!(
                Result::Success,
                encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
            );
            assert!(written > 0);

            mouse_event::free(event);
            free(encoder);
        }
    }
}
