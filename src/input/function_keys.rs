//! Port of `src/input/function_keys.zig`.
//!
//! STATUS: ported
//!
//! This is the list of "PC style function keys" that xterm supports for
//! the legacy keyboard protocols. These always take priority since at the
//! time of writing this, even the most modern keyboard protocols still
//! are backwards compatible with regards to these sequences.
//!
//! This is based on a variety of sources cross-referenced but mostly
//! based on foot's keymap.h: https://codeberg.org/dnkl/foot/src/branch/master/keymap.h

use super::key::{Key, Mods};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorMode {
    Any,
    Normal,
    Application,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeypadMode {
    Any,
    Normal,
    Application,
}

/// A bit confusing so I'll document this one: this is the "modify other keys"
/// setting. We only change behavior for "set_other" which is ESC [ > 4; 2 m.
/// So this can be "any" which means we don't care what's going on. Or it
/// can be "set" which means modify keys must be set EXCEPT FOR "other keys"
/// mode, and "set_other" which means modify keys must be set to "other keys"
/// mode.
///
/// See: https://invisible-island.net/xterm/modified-keys.html
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifyKeys {
    Any,
    Set,
    SetOther,
}

/// A single entry in the table of keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The exact set of modifiers that must be active for this entry to match.
    /// If mods_empty_is_any is true then empty mods means any set of mods can
    /// match. Otherwise, empty mods means no mods must be active.
    pub mods: Mods,
    pub mods_empty_is_any: bool,

    /// The state required for cursor/keypad mode.
    pub cursor: CursorMode,
    pub keypad: KeypadMode,

    /// Whether or not this entry should be used
    pub modify_other_keys: ModifyKeys,

    /// The sequence to send to the pty if this entry matches.
    pub sequence: &'static [u8],

    /// Sequence to send to the PTY if DECBKM is set.
    pub sequence_decbkm: Option<&'static [u8]>,
}

impl Entry {
    /// The Zig field defaults. `sequence` has no default in Zig; it is
    /// empty here and always overridden.
    pub const DEFAULT: Entry = Entry {
        mods: Mods::EMPTY,
        mods_empty_is_any: true,
        cursor: CursorMode::Any,
        keypad: KeypadMode::Any,
        modify_other_keys: ModifyKeys::Any,
        sequence: b"",
        sequence_decbkm: None,
    };
}

/// Builds a [`Mods`] with the listed fields set to true.
macro_rules! m {
    ($($field:ident),* $(,)?) => {
        Mods { $($field: true,)* ..Mods::EMPTY }
    };
}

/// Builds an [`Entry`] from the listed fields and the defaults.
macro_rules! e {
    ($($field:ident : $val:expr),* $(,)?) => {
        Entry { $($field: $val,)* ..Entry::DEFAULT }
    };
}

/// The list of modifier combinations for modify other key sequences.
/// The mode value is index + 2.
pub const MODIFIERS: &[Mods] = &[
    m!(shift),
    m!(alt),
    m!(shift, alt),
    m!(ctrl),
    m!(shift, ctrl),
    m!(alt, ctrl),
    m!(shift, alt, ctrl),
    m!(super_),
    m!(shift, super_),
    m!(alt, super_),
    m!(shift, alt, super_),
    m!(ctrl, super_),
    m!(shift, ctrl, super_),
    m!(alt, ctrl, super_),
    m!(shift, alt, ctrl, super_),
];

/// This is the array of entries for the PC style function keys as mapped to
/// our set of possible key codes. Not every key has any entries.
///
/// (Zig: `std.EnumArray(key.Key, []const Entry)`.)
#[derive(Debug, Clone, Copy)]
pub struct KeyEntryArray {
    pub values: [&'static [Entry]; Key::COUNT],
}

impl KeyEntryArray {
    pub const fn init_fill(value: &'static [Entry]) -> KeyEntryArray {
        KeyEntryArray {
            values: [value; Key::COUNT],
        }
    }

    #[inline]
    pub const fn get(&self, key: Key) -> &'static [Entry] {
        self.values[key as usize]
    }

    #[inline]
    pub const fn set(&mut self, key: Key, value: &'static [Entry]) {
        self.values[key as usize] = value;
    }
}

/// Total length of a list of entry slices (helper for `entries!`).
const fn total_len(parts: &[&[Entry]]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < parts.len() {
        n += parts[i].len();
        i += 1;
    }
    n
}

/// Concatenates entry slices at compile time (Zig's comptime `++`).
const fn concat<const N: usize>(parts: &[&[Entry]]) -> [Entry; N] {
    let mut result = [Entry::DEFAULT; N];
    let mut n = 0;
    let mut i = 0;
    while i < parts.len() {
        let mut j = 0;
        while j < parts[i].len() {
            result[n] = parts[i][j];
            n += 1;
            j += 1;
        }
        i += 1;
    }
    assert!(n == N);
    result
}

/// Concatenates the given entry arrays into a `&'static [Entry]`.
macro_rules! entries {
    [$($part:expr),* $(,)?] => {{
        const PARTS: &[&[Entry]] = &[$(&$part),*];
        const N: usize = total_len(PARTS);
        const ARR: [Entry; N] = concat::<N>(PARTS);
        &ARR
    }};
}

/// Constructs a set of pcStyle function keys using the given format. The
/// format should have exactly one "hole" for the mods code.
/// Example: "\x1b[11;{}~" for F1.
///
/// (The Zig format string `"<pre>{}<post>"` is given here as the two
/// literal halves `"<pre>", "<post>"`.)
macro_rules! pc_style {
    ($pre:literal, $post:literal) => {
        pc_style!(@keypad KeypadMode::Any, $pre, $post)
    };
    (@keypad $kp:expr, $pre:literal, $post:literal) => {
        [
            pc_style!(@entry $kp, 0, $pre, "2", $post),
            pc_style!(@entry $kp, 1, $pre, "3", $post),
            pc_style!(@entry $kp, 2, $pre, "4", $post),
            pc_style!(@entry $kp, 3, $pre, "5", $post),
            pc_style!(@entry $kp, 4, $pre, "6", $post),
            pc_style!(@entry $kp, 5, $pre, "7", $post),
            pc_style!(@entry $kp, 6, $pre, "8", $post),
            pc_style!(@entry $kp, 7, $pre, "9", $post),
            pc_style!(@entry $kp, 8, $pre, "10", $post),
            pc_style!(@entry $kp, 9, $pre, "11", $post),
            pc_style!(@entry $kp, 10, $pre, "12", $post),
            pc_style!(@entry $kp, 11, $pre, "13", $post),
            pc_style!(@entry $kp, 12, $pre, "14", $post),
            pc_style!(@entry $kp, 13, $pre, "15", $post),
            pc_style!(@entry $kp, 14, $pre, "16", $post),
        ]
    };
    (@entry $kp:expr, $i:literal, $pre:literal, $code:literal, $post:literal) => {
        Entry {
            mods: MODIFIERS[$i],
            keypad: $kp,
            sequence: concat!($pre, $code, $post).as_bytes(),
            ..Entry::DEFAULT
        }
    };
}

/// Returns entries that are dependent on cursor key settings.
macro_rules! cursor_key {
    ($normal:literal, $application:literal) => {
        [
            e!(cursor: CursorMode::Normal, sequence: $normal.as_bytes()),
            e!(cursor: CursorMode::Application, sequence: $application.as_bytes()),
        ]
    };
}

/// Returns the default keypad application mode entry.
macro_rules! kp_default {
    ($suffix:literal) => {
        [e!(
            mods_empty_is_any: false,
            keypad: KeypadMode::Application,
            sequence: concat!("\x1bO", $suffix).as_bytes(),
        )]
    };
}

/// Returns the entries for a keypad key. The suffix is the final character
/// of the sent sequence, such as "r" for kp_2.
macro_rules! kp_keys {
    ($suffix:literal, $normal:literal) => {
        entries![
            kp_default!($suffix),
            pc_style!(@keypad KeypadMode::Application, "\x1bO", $suffix),
            [e!(keypad: KeypadMode::Normal, sequence: $normal.as_bytes())],
        ]
    };
}

// The list of keys that we support and their entry values. It is expected
// that you'll just use a for loop through the entry values, there are never
// more than a couple dozen or so.
pub static KEYS: KeyEntryArray = {
    let mut result = KeyEntryArray::init_fill(&[]);

    result.set(
        Key::ArrowUp,
        entries![pc_style!("\x1b[1;", "A"), cursor_key!("\x1b[A", "\x1bOA")],
    );
    result.set(
        Key::ArrowDown,
        entries![pc_style!("\x1b[1;", "B"), cursor_key!("\x1b[B", "\x1bOB")],
    );
    result.set(
        Key::ArrowRight,
        entries![pc_style!("\x1b[1;", "C"), cursor_key!("\x1b[C", "\x1bOC")],
    );
    result.set(
        Key::ArrowLeft,
        entries![pc_style!("\x1b[1;", "D"), cursor_key!("\x1b[D", "\x1bOD")],
    );
    result.set(
        Key::Home,
        entries![pc_style!("\x1b[1;", "H"), cursor_key!("\x1b[H", "\x1bOH")],
    );
    result.set(
        Key::End,
        entries![pc_style!("\x1b[1;", "F"), cursor_key!("\x1b[F", "\x1bOF")],
    );
    result.set(
        Key::Insert,
        entries![pc_style!("\x1b[2;", "~"), [e!(sequence: b"\x1B[2~")]],
    );
    result.set(
        Key::Delete,
        entries![pc_style!("\x1b[3;", "~"), [e!(sequence: b"\x1B[3~")]],
    );
    result.set(
        Key::PageUp,
        entries![pc_style!("\x1b[5;", "~"), [e!(sequence: b"\x1B[5~")]],
    );
    result.set(
        Key::PageDown,
        entries![pc_style!("\x1b[6;", "~"), [e!(sequence: b"\x1B[6~")]],
    );
    result.set(
        Key::Help,
        entries![pc_style!("\x1b[28;", "~"), [e!(sequence: b"\x1B[28~")]],
    );
    result.set(
        Key::ContextMenu,
        entries![pc_style!("\x1b[29;", "~"), [e!(sequence: b"\x1B[29~")]],
    );

    // Function Keys. todo: f26-f35 but we need to add to input.Key
    result.set(
        Key::F1,
        entries![pc_style!("\x1b[1;", "P"), [e!(sequence: b"\x1BOP")]],
    );
    result.set(
        Key::F2,
        entries![pc_style!("\x1b[1;", "Q"), [e!(sequence: b"\x1BOQ")]],
    );
    result.set(
        Key::F3,
        entries![pc_style!("\x1b[13;", "~"), [e!(sequence: b"\x1BOR")]],
    );
    result.set(
        Key::F4,
        entries![pc_style!("\x1b[1;", "S"), [e!(sequence: b"\x1BOS")]],
    );
    result.set(
        Key::F5,
        entries![pc_style!("\x1b[15;", "~"), [e!(sequence: b"\x1B[15~")]],
    );
    result.set(
        Key::F6,
        entries![pc_style!("\x1b[17;", "~"), [e!(sequence: b"\x1B[17~")]],
    );
    result.set(
        Key::F7,
        entries![pc_style!("\x1b[18;", "~"), [e!(sequence: b"\x1B[18~")]],
    );
    result.set(
        Key::F8,
        entries![pc_style!("\x1b[19;", "~"), [e!(sequence: b"\x1B[19~")]],
    );
    result.set(
        Key::F9,
        entries![pc_style!("\x1b[20;", "~"), [e!(sequence: b"\x1B[20~")]],
    );
    result.set(
        Key::F10,
        entries![pc_style!("\x1b[21;", "~"), [e!(sequence: b"\x1B[21~")]],
    );
    result.set(
        Key::F11,
        entries![pc_style!("\x1b[23;", "~"), [e!(sequence: b"\x1B[23~")]],
    );
    result.set(
        Key::F12,
        entries![pc_style!("\x1b[24;", "~"), [e!(sequence: b"\x1B[24~")]],
    );
    result.set(
        Key::F13,
        entries![pc_style!("\x1b[25;", "~"), [e!(sequence: b"\x1B[25~")]],
    );
    result.set(
        Key::F14,
        entries![pc_style!("\x1b[26;", "~"), [e!(sequence: b"\x1B[26~")]],
    );
    result.set(
        Key::F15,
        entries![pc_style!("\x1b[28;", "~"), [e!(sequence: b"\x1B[28~")]],
    );
    result.set(
        Key::F16,
        entries![pc_style!("\x1b[29;", "~"), [e!(sequence: b"\x1B[29~")]],
    );
    result.set(
        Key::F17,
        entries![pc_style!("\x1b[31;", "~"), [e!(sequence: b"\x1B[31~")]],
    );
    result.set(
        Key::F18,
        entries![pc_style!("\x1b[32;", "~"), [e!(sequence: b"\x1B[32~")]],
    );
    result.set(
        Key::F19,
        entries![pc_style!("\x1b[33;", "~"), [e!(sequence: b"\x1B[33~")]],
    );
    result.set(
        Key::F20,
        entries![pc_style!("\x1b[34;", "~"), [e!(sequence: b"\x1B[34~")]],
    );
    result.set(
        Key::F21,
        entries![pc_style!("\x1b[42;", "~"), [e!(sequence: b"\x1B[42~")]],
    );
    result.set(
        Key::F22,
        entries![pc_style!("\x1b[43;", "~"), [e!(sequence: b"\x1B[43~")]],
    );
    result.set(
        Key::F23,
        entries![pc_style!("\x1b[44;", "~"), [e!(sequence: b"\x1B[44~")]],
    );
    result.set(
        Key::F24,
        entries![pc_style!("\x1b[45;", "~"), [e!(sequence: b"\x1B[45~")]],
    );
    result.set(
        Key::F25,
        entries![pc_style!("\x1b[46;", "~"), [e!(sequence: b"\x1B[46~")]],
    );

    // Keypad keys
    result.set(Key::Numpad0, kp_keys!("p", "0"));
    result.set(Key::Numpad1, kp_keys!("q", "1"));
    result.set(Key::Numpad2, kp_keys!("r", "2"));
    result.set(Key::Numpad3, kp_keys!("s", "3"));
    result.set(Key::Numpad4, kp_keys!("t", "4"));
    result.set(Key::Numpad5, kp_keys!("u", "5"));
    result.set(Key::Numpad6, kp_keys!("v", "6"));
    result.set(Key::Numpad7, kp_keys!("w", "7"));
    result.set(Key::Numpad8, kp_keys!("x", "8"));
    result.set(Key::Numpad9, kp_keys!("y", "9"));
    result.set(Key::NumpadDecimal, kp_keys!("n", "."));
    result.set(Key::NumpadDivide, kp_keys!("o", "/"));
    result.set(Key::NumpadMultiply, kp_keys!("j", "*"));
    result.set(Key::NumpadSubtract, kp_keys!("m", "-"));
    result.set(Key::NumpadAdd, kp_keys!("k", "+"));
    result.set(Key::NumpadEnter, kp_keys!("M", "\r"));
    result.set(
        Key::NumpadUp,
        entries![pc_style!("\x1b[1;", "A"), cursor_key!("\x1b[A", "\x1bOA")],
    );
    result.set(
        Key::NumpadDown,
        entries![pc_style!("\x1b[1;", "B"), cursor_key!("\x1b[B", "\x1bOB")],
    );
    result.set(
        Key::NumpadRight,
        entries![pc_style!("\x1b[1;", "C"), cursor_key!("\x1b[C", "\x1bOC")],
    );
    result.set(
        Key::NumpadLeft,
        entries![pc_style!("\x1b[1;", "D"), cursor_key!("\x1b[D", "\x1bOD")],
    );
    result.set(
        Key::NumpadBegin,
        entries![pc_style!("\x1b[1;", "E"), cursor_key!("\x1b[E", "\x1bOE")],
    );
    result.set(
        Key::NumpadHome,
        entries![pc_style!("\x1b[1;", "H"), cursor_key!("\x1b[H", "\x1bOH")],
    );
    result.set(
        Key::NumpadEnd,
        entries![pc_style!("\x1b[1;", "F"), cursor_key!("\x1b[F", "\x1bOF")],
    );
    result.set(
        Key::NumpadInsert,
        entries![pc_style!("\x1b[2;", "~"), [e!(sequence: b"\x1B[2~")]],
    );
    result.set(
        Key::NumpadDelete,
        entries![pc_style!("\x1b[3;", "~"), [e!(sequence: b"\x1B[3~")]],
    );
    result.set(
        Key::NumpadPageUp,
        entries![pc_style!("\x1b[5;", "~"), [e!(sequence: b"\x1B[5~")]],
    );
    result.set(
        Key::NumpadPageDown,
        entries![pc_style!("\x1b[6;", "~"), [e!(sequence: b"\x1B[6~")]],
    );

    result.set(
        Key::Backspace,
        {
            const L: &[Entry] = &[
                // Modify Keys Normal
                e!(mods: m!(shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x7f"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x7f"),
                e!(mods: m!(alt, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x7f"),
                e!(mods: m!(ctrl, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x08"),
                e!(mods: m!(alt, ctrl), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x08"),
                e!(mods: m!(alt, shift, ctrl), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x08"),
                e!(mods: m!(super_), modify_other_keys: ModifyKeys::Set, sequence: b"\x7f"),
                e!(mods: m!(super_, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x7f"),
                e!(mods: m!(alt, super_), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x7f"),
                e!(mods: m!(alt, super_, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x7f"),
                e!(mods: m!(super_, ctrl), modify_other_keys: ModifyKeys::Set, sequence: b"\x08"),
                e!(mods: m!(super_, ctrl, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x08"),
                e!(mods: m!(alt, super_, ctrl), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x08"),
                e!(mods: m!(alt, super_, ctrl, shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x08"),

                // Modify Keys Other
                e!(mods: m!(shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;2;127~"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;3;127~"),
                e!(mods: m!(alt, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;4;127~"),
                e!(mods: m!(ctrl, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;6;127~"),
                e!(mods: m!(alt, ctrl), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;7;127~"),
                e!(mods: m!(alt, shift, ctrl), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;8;127~"),
                e!(mods: m!(super_), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;9;127~"),
                e!(mods: m!(super_, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;10;127~"),
                e!(mods: m!(alt, super_), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;11;127~"),
                e!(mods: m!(alt, super_, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;12;127~"),
                e!(mods: m!(super_, ctrl), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;13;127~"),
                e!(mods: m!(super_, ctrl, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;14;127~"),
                e!(mods: m!(alt, super_, ctrl), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;15;127~"),
                e!(mods: m!(alt, super_, ctrl, shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;16;127~"),

                e!(mods: m!(ctrl), sequence: b"\x08", sequence_decbkm: Some(b"\x7f")),
                e!(sequence: b"\x7f", sequence_decbkm: Some(b"\x08")),
            ];
            L
        },
    );

    result.set(Key::Tab, {
        const L: &[Entry] = &[
                // Modify Keys Normal
                e!(mods: m!(shift), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b[Z"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\t"),
                // Modify Keys Other
                e!(mods: m!(shift), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;2;9~"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;3;9~"),
                // Everything else
                e!(mods: m!(alt, shift), sequence: b"\x1b[27;4;9~"),
                e!(mods: m!(ctrl), sequence: b"\x1b[27;5;9~"),
                e!(mods: m!(ctrl, shift), sequence: b"\x1b[27;6;9~"),
                e!(mods: m!(alt, ctrl), sequence: b"\x1b[27;7;9~"),
                e!(mods: m!(alt, ctrl, shift), sequence: b"\x1b[27;8;9~"),
                e!(mods: m!(super_), sequence: b"\x1b[27;9;9~"),
                e!(mods: m!(super_, shift), sequence: b"\x1b[27;10;9~"),
                e!(mods: m!(alt, super_), sequence: b"\x1b[27;11;9~"),
                e!(mods: m!(alt, super_, shift), sequence: b"\x1b[27;12;9~"),
                e!(mods: m!(super_, ctrl), sequence: b"\x1b[27;13;9~"),
                e!(mods: m!(super_, ctrl, shift), sequence: b"\x1b[27;14;9~"),
                e!(mods: m!(alt, super_, ctrl), sequence: b"\x1b[27;15;9~"),
                e!(mods: m!(alt, super_, ctrl, shift), sequence: b"\x1b[27;16;9~"),
                e!(sequence: b"\t"),
        ];
        L
    });

    result.set(Key::Enter, {
        const L: &[Entry] = &[
                e!(mods: m!(shift), sequence: b"\x1b[27;2;13~"),
                // Modify Keys Normal
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\r"),
                // Modify Keys Other
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;3;13~"),
                // Everything else
                e!(mods: m!(alt, shift), sequence: b"\x1b[27;4;13~"),
                e!(mods: m!(ctrl), sequence: b"\x1b[27;5;13~"),
                e!(mods: m!(ctrl, shift), sequence: b"\x1b[27;6;13~"),
                e!(mods: m!(alt, ctrl), sequence: b"\x1b[27;7;13~"),
                e!(mods: m!(alt, ctrl, shift), sequence: b"\x1b[27;8;13~"),
                e!(mods: m!(super_), sequence: b"\x1b[27;9;13~"),
                e!(mods: m!(super_, shift), sequence: b"\x1b[27;10;13~"),
                e!(mods: m!(alt, super_), sequence: b"\x1b[27;11;13~"),
                e!(mods: m!(alt, super_, shift), sequence: b"\x1b[27;12;13~"),
                e!(mods: m!(super_, ctrl), sequence: b"\x1b[27;13;13~"),
                e!(mods: m!(super_, ctrl, shift), sequence: b"\x1b[27;14;13~"),
                e!(mods: m!(alt, super_, ctrl), sequence: b"\x1b[27;15;13~"),
                e!(mods: m!(alt, super_, ctrl, shift), sequence: b"\x1b[27;16;13~"),
                e!(sequence: b"\r"),
        ];
        L
    });

    result.set(Key::Escape, {
        const L: &[Entry] = &[
                e!(mods: m!(shift), sequence: b"\x1b[27;2;27~"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::Set, sequence: b"\x1b\x1b"),
                e!(mods: m!(alt), modify_other_keys: ModifyKeys::SetOther, sequence: b"\x1b[27;3;27~"),
                e!(mods: m!(alt, shift), sequence: b"\x1b[27;4;27~"),
                e!(mods: m!(ctrl), sequence: b"\x1b[27;5;27~"),
                e!(mods: m!(ctrl, shift), sequence: b"\x1b[27;6;27~"),
                e!(mods: m!(alt, ctrl), sequence: b"\x1b[27;7;27~"),
                e!(mods: m!(alt, ctrl, shift), sequence: b"\x1b[27;8;27~"),
                e!(mods: m!(super_), sequence: b"\x1b[27;9;27~"),
                e!(mods: m!(super_, shift), sequence: b"\x1b[27;10;27~"),
                e!(mods: m!(alt, super_), sequence: b"\x1b[27;11;27~"),
                e!(mods: m!(alt, super_, shift), sequence: b"\x1b[27;12;27~"),
                e!(mods: m!(super_, ctrl), sequence: b"\x1b[27;13;27~"),
                e!(mods: m!(super_, ctrl, shift), sequence: b"\x1b[27;14;27~"),
                e!(mods: m!(alt, super_, ctrl), sequence: b"\x1b[27;15;27~"),
                e!(mods: m!(alt, super_, ctrl, shift), sequence: b"\x1b[27;16;27~"),
                e!(sequence: b"\x1b"),
        ];
        L
    });

    result
};

#[cfg(test)]
mod tests {
    use super::*;

    use crate::terminal::build_options::{self, Artifact};

    /// `build_options.artifact == .ghostty`
    const ARTIFACT_IS_GHOSTTY: bool = matches!(build_options::ARTIFACT, Artifact::Ghostty);

    /// `termio.Message.WriteReq.Small.Max`
    const WRITE_REQ_SMALL_MAX: usize = crate::termio::message::WriteReqSmall::MAX;

    #[test]
    fn keys() {
        if !ARTIFACT_IS_GHOSTTY {
            // Don't want to bring in termio into libghostty-vt
            return;
        }

        // All key sequences must fit into a termio array.
        let max = WRITE_REQ_SMALL_MAX;
        for entries in KEYS.values.iter() {
            for entry in entries.iter() {
                assert!(entry.sequence.len() <= max);
            }
        }
    }

    // Port-specific: spot-check the compile-time table construction.
    #[test]
    fn keys_contents() {
        let up = KEYS.get(Key::ArrowUp);
        assert_eq!(up.len(), 17);
        assert_eq!(up[0].mods, m!(shift));
        assert_eq!(up[0].sequence, b"\x1b[1;2A");
        assert_eq!(up[14].sequence, b"\x1b[1;16A");
        assert_eq!(up[15].cursor, CursorMode::Normal);
        assert_eq!(up[16].sequence, b"\x1bOA");

        let kp2 = KEYS.get(Key::Numpad2);
        assert_eq!(kp2.len(), 17);
        assert!(!kp2[0].mods_empty_is_any);
        assert_eq!(kp2[0].keypad, KeypadMode::Application);
        assert_eq!(kp2[0].sequence, b"\x1bOr");
        assert_eq!(kp2[1].keypad, KeypadMode::Application);
        assert_eq!(kp2[1].sequence, b"\x1bO2r");
        assert_eq!(kp2[16].keypad, KeypadMode::Normal);
        assert_eq!(kp2[16].sequence, b"2");

        let bs = KEYS.get(Key::Backspace);
        assert_eq!(bs.last().unwrap().sequence_decbkm, Some(b"\x08".as_slice()));
        assert!(KEYS.get(Key::KeyA).is_empty());
    }
}
