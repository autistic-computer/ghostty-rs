//! Port of `src/terminal/c/key_encode.zig`.
//!
//! STATUS: ported

use std::ffi::{c_int, c_void};

use super::key_event::Event as KeyEvent;
use super::result::Result;
use super::terminal::Terminal;
use super::types::{encode_to_buffer, HandleAllocator};
use crate::input::config::OptionAsAlt;
use crate::input::key_encode;
use crate::lib::allocator::Allocator as CAllocator;
use crate::terminal::kitty::key::Flags as KittyFlags;

const LOG_TARGET: &str = "key_encode";

/// Wrapper around key encoding options that tracks the allocator for C API usage.
pub struct KeyEncoderWrapper {
    pub opts: key_encode::Options,
    alloc: HandleAllocator,
}

/// C: GhosttyKeyEncoder
pub type Encoder = *mut KeyEncoderWrapper;

/// C: ghostty_key_encoder_new
#[export_name = "ghostty_key_encoder_new"]
pub extern "C" fn new(alloc_: std::option::Option<&CAllocator>, result: &mut Encoder) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(KeyEncoderWrapper {
        opts: key_encode::Options::default(),
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_key_encoder_free
///
/// # Safety
/// `encoder_` must be NULL or an encoder from [`new`] not used afterwards.
#[export_name = "ghostty_key_encoder_free"]
pub unsafe extern "C" fn free(encoder_: Encoder) {
    let Some(wrapper) = encoder_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(encoder_);
}

crate::c_api_enum! {
    /// C: GhosttyKeyEncoderOption
    ///
    /// Input type expected for setting each option (Zig: `Option.InType`):
    /// `bool` for the mode flags, `uint8_t` for kitty_flags and
    /// `GhosttyOptionAsAlt` for macos_option_as_alt.
    pub enum Option {
        CursorKeyApplication = 0,
        KeypadKeyApplication = 1,
        IgnoreKeypadWithNumlock = 2,
        AltEscPrefix = 3,
        ModifyOtherKeysState2 = 4 => "modify_other_keys_state_2",
        KittyFlags = 5,
        MacosOptionAsAlt = 6,
        /// DEC Backarrow Key Mode (DECBKM)
        /// See https://vt100.net/dec/ek-vt3xx-tp-002.pdf page 170
        /// If `false` (the default), `backspace` emits 0x7f
        /// If `true`, `backspace` emits 0x08
        BackarrowKeyMode = 7,
    }
}

/// Convert a C `GhosttyOptionAsAlt`.
fn option_as_alt_from_int(v: c_int) -> std::option::Option<OptionAsAlt> {
    [OptionAsAlt::False, OptionAsAlt::True, OptionAsAlt::Left, OptionAsAlt::Right]
        .into_iter()
        .find(|o| *o as c_int == v)
}

/// C: ghostty_key_encoder_setopt
///
/// # Safety
/// `encoder_` must be a valid encoder; `value` must be NULL or point to
/// the option's input type.
#[export_name = "ghostty_key_encoder_setopt"]
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
    let opts = &mut (*encoder_).opts;
    let b = || *(value as *const bool);
    match option {
        Option::CursorKeyApplication => opts.cursor_key_application = b(),
        Option::KeypadKeyApplication => opts.keypad_key_application = b(),
        Option::IgnoreKeypadWithNumlock => opts.ignore_keypad_with_numlock = b(),
        Option::AltEscPrefix => opts.alt_esc_prefix = b(),
        Option::ModifyOtherKeysState2 => opts.modify_other_keys_state_2 = b(),
        Option::KittyFlags => {
            // Zig: `@truncate` to u5, then `@bitCast`.
            let bits = *(value as *const u8) & 0x1F;
            opts.kitty_flags = KittyFlags::from_int(bits);
        }
        Option::MacosOptionAsAlt => {
            let raw = *(value as *const c_int);
            let Some(v) = option_as_alt_from_int(raw) else {
                log::warn!(target: LOG_TARGET, "setopt invalid OptionAsAlt value={raw}");
                return;
            };
            opts.macos_option_as_alt = v;
        }
        Option::BackarrowKeyMode => opts.backarrow_key_mode = b(),
    }
}

/// C: ghostty_key_encoder_setopt_from_terminal
///
/// # Safety
/// `encoder_` and `terminal_` must each be NULL or valid.
#[export_name = "ghostty_key_encoder_setopt_from_terminal"]
pub unsafe extern "C" fn setopt_from_terminal(encoder_: Encoder, terminal_: Terminal) {
    let Some(wrapper) = encoder_.as_mut() else {
        return;
    };
    let Some(t) = terminal_.as_ref() else {
        return;
    };
    wrapper.opts = key_encode::Options::from_terminal(t.terminal());
}

/// C: ghostty_key_encoder_encode
///
/// # Safety
/// `encoder_` and `event_` must be valid; a non-null `out_` must be valid
/// for `out_len` writable bytes.
#[export_name = "ghostty_key_encoder_encode"]
pub unsafe extern "C" fn encode(
    encoder_: Encoder,
    event_: KeyEvent,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let event = (*event_).event;
    let opts = (*encoder_).opts;
    // Attempt to write to this buffer; if we don't have space, count how
    // much space we would have needed.
    encode_to_buffer(out_, out_len, out_written, |w| key_encode::encode(w, event, opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::key;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::{key_event, terminal as terminal_c};

    fn ptr<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    #[test]
    fn alloc() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
        unsafe { free(e) };
    }

    #[test]
    fn setopt_bool() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Test setting bool options
            let val_true = true;
            setopt(e, Option::CursorKeyApplication as c_int, ptr(&val_true));
            assert!((*e).opts.cursor_key_application);

            let val_false = false;
            setopt(e, Option::CursorKeyApplication as c_int, ptr(&val_false));
            assert!(!(*e).opts.cursor_key_application);

            setopt(e, Option::KeypadKeyApplication as c_int, ptr(&val_true));
            assert!((*e).opts.keypad_key_application);

            free(e);
        }
    }

    #[test]
    fn setopt_kitty_flags() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Test setting kitty flags
            let flags = KittyFlags {
                disambiguate: true,
                report_events: true,
                ..KittyFlags::DISABLED
            };
            let flags_int: u8 = flags.int();
            setopt(e, Option::KittyFlags as c_int, ptr(&flags_int));
            assert!((*e).opts.kitty_flags.disambiguate);
            assert!((*e).opts.kitty_flags.report_events);
            assert!(!(*e).opts.kitty_flags.report_alternates);

            free(e);
        }
    }

    #[test]
    fn setopt_macos_option_as_alt() {
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Test setting option as alt
            let opt_left = OptionAsAlt::Left;
            setopt(e, Option::MacosOptionAsAlt as c_int, ptr(&opt_left));
            assert_eq!(OptionAsAlt::Left, (*e).opts.macos_option_as_alt);

            let opt_true = OptionAsAlt::True;
            setopt(e, Option::MacosOptionAsAlt as c_int, ptr(&opt_true));
            assert_eq!(OptionAsAlt::True, (*e).opts.macos_option_as_alt);

            free(e);
        }
    }

    #[test]
    fn setopt_from_terminal_test() {
        // Create encoder
        let mut e: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        // Create terminal
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 80, 24));

        unsafe {
            // Apply terminal state to encoder
            setopt_from_terminal(e, t);

            // Options should reflect defaults from a fresh terminal
            assert!(!(*e).opts.cursor_key_application);
            assert!((*e).opts.alt_esc_prefix);
            assert_eq!(KittyFlags::DISABLED, (*e).opts.kitty_flags);
            assert_eq!(OptionAsAlt::False, (*e).opts.macos_option_as_alt);

            terminal_c::free(t);
            free(e);
        }
    }

    #[test]
    fn setopt_from_terminal_null() {
        unsafe {
            // Both null should be no-ops
            setopt_from_terminal(std::ptr::null_mut(), std::ptr::null_mut());

            // Encoder null with valid terminal
            let mut t: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 80, 24));
            setopt_from_terminal(std::ptr::null_mut(), t);

            // Valid encoder with null terminal
            let mut e: Encoder = std::ptr::null_mut();
            assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
            setopt_from_terminal(e, std::ptr::null_mut());

            free(e);
            terminal_c::free(t);
        }
    }

    #[test]
    fn encode_kitty_ctrl_release_with_ctrl_mod_set() {
        // Create encoder
        let mut encoder: Encoder = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut encoder));

        unsafe {
            // Set kitty flags with all features enabled
            {
                let flags = KittyFlags {
                    disambiguate: true,
                    report_events: true,
                    report_alternates: true,
                    report_all: true,
                    report_associated: true,
                };
                let flags_int: u8 = flags.int();
                setopt(encoder, Option::KittyFlags as c_int, ptr(&flags_int));
            }

            // Create key event
            let mut event: key_event::Event = std::ptr::null_mut();
            assert_eq!(Result::Success, key_event::new(Some(&C_ALLOCATOR), &mut event));

            // Set event properties: release action, ctrl key, ctrl modifier
            key_event::set_action(event, key::Action::Release as c_int);
            key_event::set_key(event, key::Key::ControlLeft as c_int);
            let mut mods = key::Mods::default();
            mods.ctrl = true;
            key_event::set_mods(event, mods.int());

            // Encode null should give us the length required
            let mut required: usize = 0;
            assert_eq!(
                Result::OutOfSpace,
                encode(encoder, event, std::ptr::null_mut(), 0, &mut required)
            );

            // Encode the key event
            let mut buf = [0u8; 128];
            let mut written: usize = 0;
            assert_eq!(
                Result::Success,
                encode(encoder, event, buf.as_mut_ptr(), buf.len(), &mut written)
            );
            assert_eq!(required, written);

            // Expected: ESC[57442;5:3u (ctrl key code with mods and release event)
            assert_eq!(b"\x1b[57442;5:3u", &buf[..written]);

            key_event::free(event);
            free(encoder);
        }
    }
}
