//! Port of `src/terminal/modes.zig`.
//!
//! STATUS: ported
//!
//! This file contains all the terminal modes that we support
//! and various support types for them: an enum of supported modes,
//! a packed struct to store mode values, a more generalized state
//! struct to store values plus handle save/restore, and much more.
//!
//! The Zig version generates the `Mode` enum and the `ModePacked` struct at
//! comptime from the `entries` table. Here the `define_modes!` macro does the
//! same from a single table, so the enum values, defaults and the packed bit
//! layout (bit `i` is entry `i`, exactly like the Zig packed struct) stay in
//! sync.

use std::io::Write;

use crate::datastruct::ComptimeIntSet;

/// A struct that maintains the state of all the settable modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeState {
    /// The values of the current modes.
    pub values: ModePacked,

    /// The saved values. We only allow saving each mode once.
    /// This is in line with other terminals that implement XTSAVE
    /// and XTRESTORE. We can improve this in the future if it becomes
    /// a real-world issue but we need to be aware of a DoS vector.
    pub saved: ModePacked,

    /// The default values for the modes. This is used to reset
    /// the modes to their default values during reset.
    pub default: ModePacked,
}

impl ModeState {
    /// Reset the modes to their default values. This also clears the
    /// saved state.
    pub fn reset(&mut self) {
        self.values = self.default;
        self.saved = ModePacked::default();
    }

    /// Set a mode to a value.
    pub fn set(&mut self, mode: Mode, value: bool) {
        set_packed(&mut self.values, mode, value);
    }

    /// Set the reset default and current value for a mode.
    pub fn set_default(&mut self, mode: Mode, value: bool) {
        set_packed(&mut self.values, mode, value);
        set_packed(&mut self.default, mode, value);
    }

    /// Get the value of a mode.
    pub fn get(&self, mode: Mode) -> bool {
        get_packed(&self.values, mode)
    }

    /// Get the value a mode is reset to.
    pub fn get_default(&self, mode: Mode) -> bool {
        get_packed(&self.default, mode)
    }

    /// Save the state of the given mode. This can then be restored
    /// with restore. This will only be accurate if the previous
    /// mode was saved exactly once and not restored. Otherwise this
    /// will just keep restoring the last stored value in memory.
    pub fn save(&mut self, mode: Mode) {
        let v = self.values.get(mode);
        self.saved.set(mode, v);
    }

    /// See save. This will return the restored value.
    pub fn restore(&mut self, mode: Mode) -> bool {
        let v = self.saved.get(mode);
        self.values.set(mode, v);
        v
    }

    /// Return a DECRPM report for the given mode tag. If the tag does
    /// not correspond to a known mode, the report state is .not_recognized,
    /// except for the permanently-reset modes below.
    pub fn get_report(&self, tag: ReportTag) -> Report {
        // DECECM (Erase Color Mode, DEC private mode 117) controls whether erasing
        // and scrolling use the default background or the active background color.
        // Ghostty's behavior is fixed equivalent to DECECM reset, and DECRQM has a
        // "permanently reset" response for recognized modes that cannot be changed.
        // Report that instead of "not recognized" so applications can query and adapt
        // to Ghostty's erase-color behavior.
        //
        // See VT520/VT525 Programmer Information, "Erase Color" and DECRQM/DECRPM:
        // https://web.mit.edu/dosathena/doc/www/ek-vt520-rm.pdf

        if !tag.ansi && tag.value == 117 {
            return Report {
                tag,
                state: ReportState::PermanentlyReset,
            };
        }

        // These unimplemented ANSI modes report fixed reset, matching
        // xterm: GATM, SRTM, VEM, HEM, PUM, FEAM, FETM, MATM, TTM, SATM,
        // TSM, EBM. Report permanently_reset instead of not_recognized so
        // DECRQM callers hear "unsupported" rather than "unknown".
        if tag.ansi && PERMANENTLY_RESET_ANSI.contains(&tag.value) {
            return Report {
                tag,
                state: ReportState::PermanentlyReset,
            };
        }

        let Some(mode) = mode_from_int(tag.value, tag.ansi) else {
            return Report {
                tag,
                state: ReportState::NotRecognized,
            };
        };
        Report {
            tag,
            state: if self.get(mode) {
                ReportState::Set
            } else {
                ReportState::Reset
            },
        }
    }
}

/// ANSI modes that are reported as permanently reset (see `get_report`).
const PERMANENTLY_RESET_ANSI: [u16; 12] = [1, 5, 7, 10, 11, 13, 14, 15, 16, 17, 18, 19];

// Compile-time check: none of the permanently reset ANSI modes may be in
// the mode table.
const _: () = {
    let mut i = 0;
    while i < PERMANENTLY_RESET_ANSI.len() {
        let v = PERMANENTLY_RESET_ANSI[i];
        let mut j = 0;
        while j < ENTRIES.len() {
            if ENTRIES[j].ansi && ENTRIES[j].value == v {
                panic!("ANSI mode is in the mode table and must not be reported as permanently reset");
            }
            j += 1;
        }
        i += 1;
    }
};

fn set_packed(values: &mut ModePacked, mode: Mode, value: bool) {
    values.set(mode, value);
}

fn get_packed(values: &ModePacked, mode: Mode) -> bool {
    values.get(mode)
}

/// A packed struct of all the settable modes. This shouldn't
/// be used directly but rather through the ModeState struct.
///
/// Bit `i` holds the value of `ENTRIES[i]` (Zig packed struct layout).
/// Per-mode getters and `set_*` setters are generated from the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ModePacked {
    pub bits: u64,
}

impl Default for ModePacked {
    fn default() -> Self {
        ModePacked {
            bits: DEFAULT_BITS,
        }
    }
}

impl ModePacked {
    /// Get the value of a mode.
    #[inline]
    pub const fn get(&self, mode: Mode) -> bool {
        self.bits & (1u64 << mode.index()) != 0
    }

    /// Set the value of a mode.
    #[inline]
    pub fn set(&mut self, mode: Mode, value: bool) {
        let mask = 1u64 << mode.index();
        if value {
            self.bits |= mask;
        } else {
            self.bits &= !mask;
        }
    }
}

/// The tag type for our enum is a u16 but we use a packed struct
/// in order to pack the ansi bit into the tag.
///
/// Layout (`packed struct(u16)`): bits 0..15 are `value` (u15), bit 15 is
/// `ansi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModeTag {
    /// A u15 value.
    pub value: u16,
    pub ansi: bool,
}

/// `ModeTag.Backing`
pub type ModeTagBacking = u16;

impl ModeTag {
    pub const fn new(value: u16, ansi: bool) -> ModeTag {
        ModeTag { value, ansi }
    }

    /// Pack into the backing integer (`@bitCast`).
    pub const fn to_bits(self) -> ModeTagBacking {
        (self.value & 0x7FFF) | ((self.ansi as u16) << 15)
    }

    /// Unpack from the backing integer (`@bitCast`).
    pub const fn from_bits(bits: ModeTagBacking) -> ModeTag {
        ModeTag {
            value: bits & 0x7FFF,
            ansi: bits & 0x8000 != 0,
        }
    }

    pub const fn from_mode(mode: Mode) -> ModeTag {
        ModeTag::from_bits(mode as u16)
    }
}

/// Returns the mode for a mode number as it appears in a control
/// sequence, or null if the mode is unknown or disabled.
///
/// ANSI and DEC private modes are numbered separately, so the same
/// number can mean two different modes. `ansi` selects which one. For
/// example, `CSI ? 4 h` is `mode_from_int(4, false)` (slow scroll) and
/// `CSI 4 h` is `mode_from_int(4, true)` (insert).
pub fn mode_from_int(v: u16, ansi: bool) -> Option<Mode> {
    // ModeTag stores the number in 15 bits. A larger number can't be a
    // known mode, and packing it below would spill into the ansi bit.
    if v > 0x7FFF {
        return None;
    }

    // This runs for every mode that is set, reset, or queried, so we use
    // a sorted lookup instead of comparing against each entry in turn.
    let int = ModeTag::new(v, ansi).to_bits();
    let idx = ENTRY_SET.index_of(int)?;
    if ENTRIES_ANY_DISABLED && ENTRIES[idx].disabled {
        return None;
    }

    Some(ENTRIES[idx].mode)
}

/// The tag of every entry, in the same order as `ENTRIES`. Looking up
/// a tag here returns the index of its entry.
static ENTRY_SET: ComptimeIntSet<ModeTagBacking, ENTRY_COUNT> = {
    let mut keys = [0 as ModeTagBacking; ENTRY_COUNT];
    let mut i = 0;
    while i < ENTRY_COUNT {
        keys[i] = ModeTag::new(ENTRIES[i].value, ENTRIES[i].ansi).to_bits();
        i += 1;
    }
    ComptimeIntSet::<ModeTagBacking, ENTRY_COUNT>::new(&keys)
};

/// True if any entry is disabled. When false, mode_from_int skips the
/// disabled check entirely.
const ENTRIES_ANY_DISABLED: bool = {
    let mut any = false;
    let mut i = 0;
    while i < ENTRY_COUNT {
        if ENTRIES[i].disabled {
            any = true;
        }
        i += 1;
    }
    any
};

/// The packed default values of every mode.
const DEFAULT_BITS: u64 = {
    let mut bits = 0u64;
    let mut i = 0;
    while i < ENTRY_COUNT {
        if ENTRIES[i].default {
            bits |= 1u64 << i;
        }
        i += 1;
    }
    bits
};

const ENTRY_COUNT: usize = ENTRIES.len();
const _: () = assert!(ENTRY_COUNT <= 64);

/// A DECRPM mode report response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub tag: ReportTag,
    pub state: ReportState,
}

/// A query identifier can use the full parser parameter range. Keep it
/// separate from ModeTag, which packs supported modes and the ANSI flag
/// into a u16 and is also used by the public C API.
///
/// (`Report.Tag`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportTag {
    pub value: u16,
    pub ansi: bool,
}

impl ReportTag {
    pub const fn from_mode(mode: Mode) -> ReportTag {
        let tag = ModeTag::from_mode(mode);
        ReportTag {
            value: tag.value,
            ansi: tag.ansi,
        }
    }
}

/// The state of a mode as reported in a DECRPM response (`Report.State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ReportState {
    NotRecognized = 0,
    Set = 1,
    Reset = 2,
    PermanentlySet = 3,
    PermanentlyReset = 4,
}

impl Report {
    /// The largest possible encoded report (`Report.max_size`): the
    /// largest tag value, DEC (with `?`), and a single-digit state.
    pub const MAX_SIZE: usize = b"\x1B[?65535;4$y".len();

    /// Encode the DECRPM report sequence.
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        write!(
            writer,
            "\x1B[{}{};{}$y",
            if self.tag.ansi { "" } else { "?" },
            self.tag.value,
            self.state as u8,
        )
    }
}

/// A single entry of a possible mode we support. This is used to
/// dynamically define the enum and other tables.
#[derive(Debug, Clone, Copy)]
struct ModeEntry {
    mode: Mode,
    name: &'static str,
    value: u16,
    default: bool,

    /// True if this is an ANSI mode, false if its a DEC mode (?-prefixed).
    ansi: bool,

    /// If true, this mode is disabled and Ghostty will not allow it to be
    /// set or queried. The mode enum still has it, allowing Ghostty developers
    /// to develop a mode without exposing it to real users.
    disabled: bool,

    /// Whether an embedder may safely configure this bit as reset policy.
    /// Modes that perform a transition or mirror additional terminal state
    /// must use their semantic configuration API instead.
    default_configurable: bool,
}

fn entry_for_mode(mode: Mode) -> &'static ModeEntry {
    &ENTRIES[mode.index() as usize]
}

/// Return whether a mode can safely be configured as a reset default by an
/// embedder. This excludes modes whose set/reset operations have side effects
/// beyond changing the mode bit.
pub fn default_configurable(mode: Mode) -> bool {
    entry_for_mode(mode).default_configurable
}

impl Mode {
    /// The Zig tag name of this mode (e.g. `"132_column"`).
    pub fn name(self) -> &'static str {
        entry_for_mode(self).name
    }

    /// Look up a mode by its Zig tag name.
    pub fn from_name(name: &str) -> Option<Mode> {
        ENTRIES.iter().find(|e| e.name == name).map(|e| e.mode)
    }

    /// The packed tag of this mode.
    pub const fn tag(self) -> ModeTag {
        ModeTag::from_mode(self)
    }

    /// All modes in table order.
    pub fn all() -> impl Iterator<Item = Mode> {
        ENTRIES.iter().map(|e| e.mode)
    }
}

macro_rules! define_modes {
    ($(
        $variant:ident, $getter:ident, $setter:ident, $name:literal, $value:literal,
        ansi = $ansi:literal, default = $default:literal,
        disabled = $disabled:literal, default_configurable = $dc:literal;
    )*) => {
        /// An enum(u16) of the available modes. See entries for available values.
        /// The discriminant is the packed `ModeTag`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u16)]
        pub enum Mode {
            $( $variant = ModeTag::new($value, $ansi).to_bits(), )*
        }

        /// Sequential index of each mode in the entries table.
        #[allow(dead_code)]
        #[repr(u8)]
        enum ModeIndex {
            $( $variant, )*
        }

        impl Mode {
            /// The index of this mode in the entries table, which is also
            /// its bit position in `ModePacked`.
            #[inline]
            pub const fn index(self) -> u32 {
                match self {
                    $( Mode::$variant => ModeIndex::$variant as u32, )*
                }
            }
        }

        impl ModePacked {
            $(
                #[doc = concat!("The value of mode `", $name, "`.")]
                #[inline]
                pub const fn $getter(&self) -> bool {
                    self.get(Mode::$variant)
                }

                #[doc = concat!("Set the value of mode `", $name, "`.")]
                #[inline]
                pub fn $setter(&mut self, v: bool) {
                    self.set(Mode::$variant, v)
                }
            )*
        }

        /// The full list of available entries. For documentation see how
        /// they're used within Ghostty or google their values. It is not
        /// valuable to redocument them all here.
        ///
        /// NOTE: When adding a new mode entry, also add a corresponding
        /// GHOSTTY_MODE_* macro in include/ghostty/vt/modes.h.
        const ENTRIES: &[ModeEntry] = &[
            $(
                ModeEntry {
                    mode: Mode::$variant,
                    name: $name,
                    value: $value,
                    default: $default,
                    ansi: $ansi,
                    disabled: $disabled,
                    default_configurable: $dc,
                },
            )*
        ];
    };
}

#[rustfmt::skip]
define_modes! {
    // ANSI
    DisableKeyboard, disable_keyboard, set_disable_keyboard, "disable_keyboard", 2, ansi = true, default = false, disabled = false, default_configurable = true; // KAM
    Insert, insert, set_insert, "insert", 4, ansi = true, default = false, disabled = false, default_configurable = true;
    SendReceiveMode, send_receive_mode, set_send_receive_mode, "send_receive_mode", 12, ansi = true, default = true, disabled = false, default_configurable = true; // SRM
    Linefeed, linefeed, set_linefeed, "linefeed", 20, ansi = true, default = false, disabled = false, default_configurable = true;

    // DEC
    CursorKeys, cursor_keys, set_cursor_keys, "cursor_keys", 1, ansi = false, default = false, disabled = false, default_configurable = true; // DECCKM
    Column132, column_132, set_column_132, "132_column", 3, ansi = false, default = false, disabled = false, default_configurable = false;
    SlowScroll, slow_scroll, set_slow_scroll, "slow_scroll", 4, ansi = false, default = false, disabled = false, default_configurable = true;
    ReverseColors, reverse_colors, set_reverse_colors, "reverse_colors", 5, ansi = false, default = false, disabled = false, default_configurable = true;
    Origin, origin, set_origin, "origin", 6, ansi = false, default = false, disabled = false, default_configurable = false;
    Wraparound, wraparound, set_wraparound, "wraparound", 7, ansi = false, default = true, disabled = false, default_configurable = true;
    Autorepeat, autorepeat, set_autorepeat, "autorepeat", 8, ansi = false, default = false, disabled = false, default_configurable = true;
    MouseEventX10, mouse_event_x10, set_mouse_event_x10, "mouse_event_x10", 9, ansi = false, default = false, disabled = false, default_configurable = false;
    CursorBlinking, cursor_blinking, set_cursor_blinking, "cursor_blinking", 12, ansi = false, default = false, disabled = false, default_configurable = false;
    CursorVisible, cursor_visible, set_cursor_visible, "cursor_visible", 25, ansi = false, default = true, disabled = false, default_configurable = true;
    EnableMode3, enable_mode_3, set_enable_mode_3, "enable_mode_3", 40, ansi = false, default = false, disabled = false, default_configurable = true;
    ReverseWrap, reverse_wrap, set_reverse_wrap, "reverse_wrap", 45, ansi = false, default = false, disabled = false, default_configurable = true;
    AltScreenLegacy, alt_screen_legacy, set_alt_screen_legacy, "alt_screen_legacy", 47, ansi = false, default = false, disabled = false, default_configurable = false;
    KeypadKeys, keypad_keys, set_keypad_keys, "keypad_keys", 66, ansi = false, default = false, disabled = false, default_configurable = true;
    // DEC Backarrow Key Mode (DECBKM)
    // See https://vt100.net/dec/ek-vt3xx-tp-002.pdf page 170
    // If `false` (the default), `backspace` emits 0x7f
    // If `true`, `backspace` emits 0x08
    BackarrowKeyMode, backarrow_key_mode, set_backarrow_key_mode, "backarrow_key_mode", 67, ansi = false, default = false, disabled = false, default_configurable = true;
    EnableLeftAndRightMargin, enable_left_and_right_margin, set_enable_left_and_right_margin, "enable_left_and_right_margin", 69, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseEventNormal, mouse_event_normal, set_mouse_event_normal, "mouse_event_normal", 1000, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseEventButton, mouse_event_button, set_mouse_event_button, "mouse_event_button", 1002, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseEventAny, mouse_event_any, set_mouse_event_any, "mouse_event_any", 1003, ansi = false, default = false, disabled = false, default_configurable = false;
    FocusEvent, focus_event, set_focus_event, "focus_event", 1004, ansi = false, default = false, disabled = false, default_configurable = true;
    MouseFormatUtf8, mouse_format_utf8, set_mouse_format_utf8, "mouse_format_utf8", 1005, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseFormatSgr, mouse_format_sgr, set_mouse_format_sgr, "mouse_format_sgr", 1006, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseAlternateScroll, mouse_alternate_scroll, set_mouse_alternate_scroll, "mouse_alternate_scroll", 1007, ansi = false, default = true, disabled = false, default_configurable = true;
    MouseFormatUrxvt, mouse_format_urxvt, set_mouse_format_urxvt, "mouse_format_urxvt", 1015, ansi = false, default = false, disabled = false, default_configurable = false;
    MouseFormatSgrPixels, mouse_format_sgr_pixels, set_mouse_format_sgr_pixels, "mouse_format_sgr_pixels", 1016, ansi = false, default = false, disabled = false, default_configurable = false;
    IgnoreKeypadWithNumlock, ignore_keypad_with_numlock, set_ignore_keypad_with_numlock, "ignore_keypad_with_numlock", 1035, ansi = false, default = true, disabled = false, default_configurable = true;
    AltEscPrefix, alt_esc_prefix, set_alt_esc_prefix, "alt_esc_prefix", 1036, ansi = false, default = true, disabled = false, default_configurable = true;
    AltSendsEscape, alt_sends_escape, set_alt_sends_escape, "alt_sends_escape", 1039, ansi = false, default = false, disabled = false, default_configurable = true;
    ReverseWrapExtended, reverse_wrap_extended, set_reverse_wrap_extended, "reverse_wrap_extended", 1045, ansi = false, default = false, disabled = false, default_configurable = true;
    AltScreen, alt_screen, set_alt_screen, "alt_screen", 1047, ansi = false, default = false, disabled = false, default_configurable = false;
    SaveCursor, save_cursor, set_save_cursor, "save_cursor", 1048, ansi = false, default = false, disabled = false, default_configurable = false;
    AltScreenSaveCursorClearEnter, alt_screen_save_cursor_clear_enter, set_alt_screen_save_cursor_clear_enter, "alt_screen_save_cursor_clear_enter", 1049, ansi = false, default = false, disabled = false, default_configurable = false;
    BracketedPaste, bracketed_paste, set_bracketed_paste, "bracketed_paste", 2004, ansi = false, default = false, disabled = false, default_configurable = true;
    SynchronizedOutput, synchronized_output, set_synchronized_output, "synchronized_output", 2026, ansi = false, default = false, disabled = false, default_configurable = false;
    GraphemeCluster, grapheme_cluster, set_grapheme_cluster, "grapheme_cluster", 2027, ansi = false, default = false, disabled = false, default_configurable = true;
    ReportColorScheme, report_color_scheme, set_report_color_scheme, "report_color_scheme", 2031, ansi = false, default = false, disabled = false, default_configurable = true;
    ReportVisibility, report_visibility, set_report_visibility, "report_visibility", 2033, ansi = false, default = false, disabled = false, default_configurable = false;
    InBandSizeReports, in_band_size_reports, set_in_band_size_reports, "in_band_size_reports", 2048, ansi = false, default = false, disabled = false, default_configurable = true;
    // Kitty clipboard protocol paste events. When set, a user-initiated
    // paste sends an unsolicited OSC 5522 targets listing with a
    // one-time password instead of pasting the text.
    // See https://sw.kovidgoyal.net/kitty/clipboard/
    KittyPasteEvents, kitty_paste_events, set_kitty_paste_events, "kitty_paste_events", 5522, ansi = false, default = false, disabled = false, default_configurable = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_report(report: Report) -> (Vec<u8>, usize) {
        let mut buf = [0u8; Report::MAX_SIZE];
        let mut writer: &mut [u8] = &mut buf;
        report.encode(&mut writer).unwrap();
        let n = Report::MAX_SIZE - writer.len();
        (buf[..n].to_vec(), buf.len())
    }

    #[test]
    fn mode_packed_size() {
        // We have this here so that we explicitly fail when we change the
        // size of modes. The size of modes is NOT particularly important,
        // we just want to be mentally aware when it happens.
        assert_eq!(8, std::mem::size_of::<ModePacked>());
        assert_eq!(43, ENTRY_COUNT);
    }

    #[test]
    fn mode_tag_order() {
        let t = ModeTag {
            value: 1,
            ansi: false,
        };
        let int: ModeTagBacking = t.to_bits();
        assert_eq!(1u16, int);
    }

    #[test]
    fn mode_table_consistency() {
        for (i, entry) in ENTRIES.iter().enumerate() {
            assert_eq!(entry.mode.index() as usize, i);
            assert_eq!(entry.mode.tag(), ModeTag::new(entry.value, entry.ansi));
            assert_eq!(Mode::from_name(entry.name), Some(entry.mode));
            assert_eq!(ModePacked::default().get(entry.mode), entry.default);
        }
        assert_eq!(Mode::Column132.name(), "132_column");
        assert!(ModePacked::default().wraparound());
        assert!(!ModePacked::default().insert());
    }

    #[test]
    fn mode_from_int_test() {
        assert!(mode_from_int(4, true).unwrap() == Mode::Insert);
        assert!(mode_from_int(9, true).is_none());
        assert!(mode_from_int(9, false).unwrap() == Mode::MouseEventX10);
        assert!(mode_from_int(14, true).is_none());

        // Numbers too large for the tag must not be mistaken for an ANSI mode.
        assert!(mode_from_int(4 | 0x8000, false).is_none());
        assert!(mode_from_int(u16::MAX, true).is_none());
    }

    #[test]
    fn mode_from_int_matches_entries_exhaustively() {
        // Compare every possible input against a plain scan of the entries.
        for ansi in [false, true] {
            for v in 0..=u16::MAX as usize {
                let expected = ENTRIES
                    .iter()
                    .find(|e| !e.disabled && e.value as usize == v && e.ansi == ansi)
                    .map(|e| e.mode);
                assert_eq!(expected, mode_from_int(v as u16, ansi));
            }
        }
    }

    #[test]
    fn mode_state() {
        let mut state = ModeState::default();

        // Normal set/get
        assert!(!state.get(Mode::CursorKeys));
        state.set(Mode::CursorKeys, true);
        assert!(state.get(Mode::CursorKeys));

        // Save/restore
        state.save(Mode::CursorKeys);
        state.set(Mode::CursorKeys, false);
        assert!(!state.get(Mode::CursorKeys));
        assert!(state.restore(Mode::CursorKeys));
        assert!(state.get(Mode::CursorKeys));
    }

    #[test]
    fn mode_state_set_default_updates_current_and_reset_value() {
        let mut state = ModeState::default();

        state.set_default(Mode::GraphemeCluster, true);
        assert!(state.get(Mode::GraphemeCluster));
        assert!(state.default.grapheme_cluster());

        state.set(Mode::GraphemeCluster, false);
        assert!(!state.get(Mode::GraphemeCluster));
        assert!(state.default.grapheme_cluster());

        state.set_default(Mode::GraphemeCluster, true);
        assert!(state.get(Mode::GraphemeCluster));

        state.set(Mode::GraphemeCluster, false);
        state.reset();
        assert!(state.get(Mode::GraphemeCluster));
        assert!(state.get_default(Mode::GraphemeCluster));
    }

    #[test]
    fn default_configurable_modes() {
        assert!(default_configurable(Mode::GraphemeCluster));
        assert!(default_configurable(Mode::Wraparound));
        assert!(!default_configurable(Mode::AltScreen));
        assert!(!default_configurable(Mode::CursorBlinking));
    }

    #[test]
    fn get_report_known_dec_mode() {
        let mut state = ModeState::default();
        let report = state.get_report(ReportTag {
            value: 1,
            ansi: false,
        });
        assert_eq!(ReportState::Reset, report.state);
        assert!(!report.tag.ansi);
        assert_eq!(1u16, report.tag.value);

        state.set(Mode::CursorKeys, true);
        let report2 = state.get_report(ReportTag {
            value: 1,
            ansi: false,
        });
        assert_eq!(ReportState::Set, report2.state);
    }

    #[test]
    fn get_report_known_ansi_mode() {
        let mut state = ModeState::default();
        state.set(Mode::Insert, true);
        let report = state.get_report(ReportTag {
            value: 4,
            ansi: true,
        });
        assert_eq!(ReportState::Set, report.state);
        assert!(report.tag.ansi);
    }

    #[test]
    fn get_report_dececm_permanently_reset() {
        let state = ModeState::default();
        let report = state.get_report(ReportTag {
            value: 117,
            ansi: false,
        });
        assert_eq!(ReportState::PermanentlyReset, report.state);
        assert!(!report.tag.ansi);
    }

    #[test]
    fn get_report_unsupported_ansi_modes_permanently_reset() {
        let state = ModeState::default();
        // GATM, SRTM, VEM, HEM, PUM, FEAM, FETM, MATM, TTM, SATM, TSM, EBM.
        for value in [1u16, 5, 7, 10, 11, 13, 14, 15, 16, 17, 18, 19] {
            // None of these may gain a settable mode table entry without
            // leaving this list first.
            assert!(mode_from_int(value, true).is_none());
            let report = state.get_report(ReportTag { value, ansi: true });
            assert_eq!(ReportState::PermanentlyReset, report.state);
            assert!(report.tag.ansi);
            assert_eq!(value, report.tag.value);
        }

        // An unlisted ANSI mode is still unknown, not permanently reset.
        let unknown = state.get_report(ReportTag {
            value: 9,
            ansi: true,
        });
        assert_eq!(ReportState::NotRecognized, unknown.state);
    }

    #[test]
    fn get_report_unknown_mode() {
        let state = ModeState::default();
        let report = state.get_report(ReportTag {
            value: 9999,
            ansi: false,
        });
        assert_eq!(ReportState::NotRecognized, report.state);
    }

    #[test]
    fn report_encode_dec_mode_set() {
        let report = Report {
            tag: ReportTag {
                value: 1,
                ansi: false,
            },
            state: ReportState::Set,
        };
        assert_eq!(b"\x1B[?1;1$y".as_slice(), encode_report(report).0.as_slice());
    }

    #[test]
    fn report_encode_dec_mode_reset() {
        let report = Report {
            tag: ReportTag {
                value: 1,
                ansi: false,
            },
            state: ReportState::Reset,
        };
        assert_eq!(b"\x1B[?1;2$y".as_slice(), encode_report(report).0.as_slice());
    }

    #[test]
    fn report_encode_ansi_mode() {
        let report = Report {
            tag: ReportTag {
                value: 4,
                ansi: true,
            },
            state: ReportState::Set,
        };
        assert_eq!(b"\x1B[4;1$y".as_slice(), encode_report(report).0.as_slice());
    }

    #[test]
    fn report_encode_not_recognized() {
        let report = Report {
            tag: ReportTag {
                value: 9999,
                ansi: false,
            },
            state: ReportState::NotRecognized,
        };
        assert_eq!(b"\x1B[?9999;0$y".as_slice(), encode_report(report).0.as_slice());
    }

    #[test]
    fn get_report_large_unknown_modes() {
        let state = ModeState::default();
        // These would alias ANSI insert mode and fixed-status DEC mode 117.
        for tag in [
            ReportTag {
                value: 32772,
                ansi: true,
            },
            ReportTag {
                value: 32885,
                ansi: false,
            },
        ] {
            let report = state.get_report(tag);
            assert_eq!(ReportState::NotRecognized, report.state);
            assert_eq!(tag, report.tag);
        }
    }

    #[test]
    fn report_encode_maximum_size() {
        let report = Report {
            tag: ReportTag {
                value: 65535,
                ansi: false,
            },
            state: ReportState::PermanentlyReset,
        };
        let (out, len) = encode_report(report);
        assert_eq!(b"\x1b[?65535;4$y".as_slice(), out.as_slice());
        assert_eq!(len, out.len());
    }
}
