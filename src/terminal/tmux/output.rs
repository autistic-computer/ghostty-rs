//! Port of `src/terminal/tmux/output.zig`.
//!
//! STATUS: ported
//!
//! The Zig code builds format structs at comptime (`FormatStruct(vars)`).
//! In Rust, [`format_struct!`] declares such a struct from a list of variable
//! names; each field gets the type of its variable ([`VariableType`]). The
//! struct implements [`FormatStruct`], which [`parse_format_struct`] uses.
//! Deviation: `comptime_format` returns an owned `String` (computed at
//! runtime) instead of a comptime string.

use std::fmt;
use std::io::Write;

/// `ParseError`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    MissingEntry,
    ExtraEntry,
    FormatError,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

/// Errors of `std.fmt.parseInt` plus the variable format error, returned
/// by [`Variable::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableParseError {
    InvalidCharacter,
    Overflow,
    FormatError,
}

impl fmt::Display for VariableParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for VariableParseError {}

/// `std.fmt.ParseIntError`
pub use crate::lib::parse_int::ParseIntError;

impl From<ParseIntError> for VariableParseError {
    fn from(e: ParseIntError) -> Self {
        match e {
            ParseIntError::InvalidCharacter => VariableParseError::InvalidCharacter,
            ParseIntError::Overflow => VariableParseError::Overflow,
        }
    }
}

/// Parse a base-10 `usize` with the semantics of Zig's
/// `std.fmt.parseInt(usize, buf, 10)`: an optional leading `+` or `-`
/// (a negative non-zero value overflows), `_` digit separators allowed
/// between digits, `InvalidCharacter` for anything else (including an
/// empty string) and `Overflow` when the value doesn't fit.
pub(crate) fn parse_int_usize(buf: &[u8]) -> Result<usize, ParseIntError> {
    crate::lib::parse_int::std_parse_int(buf, 10)
}

/// Parse the output from a command with the given format struct
/// (declared usually by `format_struct!`). The format struct is expected
/// to be in the order of the variables used in the format string and
/// the variables are expected to be plain variables (no conditionals,
/// extra formatting, etc.). Each variable is expected to be separated
/// by a single `delimiter` character.
pub fn parse_format_struct<'a, T: FormatStruct<'a>>(
    str: &'a [u8],
    delimiter: u8,
) -> Result<T, ParseError> {
    // Parse all our fields
    let mut it = str.split(move |&c| c == delimiter);
    let result = T::from_parts(&mut it)?;

    // We should have consumed all parts now.
    if it.next().is_some() {
        return Err(ParseError::ExtraEntry);
    }

    Ok(result)
}

/// The format string for the given variables (`comptimeFormat`).
pub fn comptime_format(vars: &[Variable], delimiter: u8) -> String {
    let mut buf = Vec::new();
    format(&mut buf, vars, delimiter).expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("variable names are ASCII")
}

/// Format a set of variables into the proper format string for tmux
/// that we can handle with `parse_format_struct`.
pub fn format<W: Write + ?Sized>(
    writer: &mut W,
    vars: &[Variable],
    delimiter: u8,
) -> std::io::Result<()> {
    for (i, variable) in vars.iter().enumerate() {
        if i != 0 {
            writer.write_all(&[delimiter])?;
        }
        write!(writer, "#{{{}}}", variable.as_str())?;
    }
    Ok(())
}

/// A struct with one field per format variable (`FormatStruct(vars)`).
/// Implemented by structs declared with [`format_struct!`].
pub trait FormatStruct<'a>: Sized {
    /// The variables, in field order.
    const VARS: &'static [Variable];

    /// Parse each field from the next part of `it`.
    fn from_parts(it: &mut dyn Iterator<Item = &'a [u8]>) -> Result<Self, ParseError>;

    /// The format string for this struct (`comptimeFormat`).
    fn comptime_format(delimiter: u8) -> String {
        comptime_format(Self::VARS, delimiter)
    }
}

/// Declare a struct that contains fields for each of the given format
/// variables (`FormatStruct` in Zig). This can be used with
/// `parse_format_struct` to parse an output string into a format struct.
///
/// ```ignore
/// format_struct!(pub struct ListWindows { session_id, window_id, window_layout });
/// let r: ListWindows = parse_format_struct(b"$1 @2 abcd,80x24,0,0,1", b' ')?;
/// ```
///
/// The struct has a lifetime parameter `'a` for the string fields, which
/// borrow from the parsed input.
#[allow(unused_macros)]
macro_rules! format_struct {
    ($(#[$m:meta])* $vis:vis struct $name:ident { $($field:ident),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis struct $name<'a> {
            $(
                pub $field: <<$crate::terminal::tmux::output::vars::$field
                    as $crate::terminal::tmux::output::VariableKind>::Kind
                    as $crate::terminal::tmux::output::ValueKind>::Type<'a>,
            )*
            #[doc(hidden)]
            pub _marker: ::core::marker::PhantomData<&'a [u8]>,
        }

        impl<'a> $crate::terminal::tmux::output::FormatStruct<'a> for $name<'a> {
            const VARS: &'static [$crate::terminal::tmux::output::Variable] = &[
                $(
                    <$crate::terminal::tmux::output::vars::$field
                        as $crate::terminal::tmux::output::VariableKind>::VARIABLE,
                )*
            ];

            #[allow(unused_variables)]
            fn from_parts(
                it: &mut dyn Iterator<Item = &'a [u8]>,
            ) -> ::core::result::Result<Self, $crate::terminal::tmux::output::ParseError> {
                $(
                    let $field = {
                        let part = it
                            .next()
                            .ok_or($crate::terminal::tmux::output::ParseError::MissingEntry)?;
                        let value = <$crate::terminal::tmux::output::vars::$field
                            as $crate::terminal::tmux::output::VariableKind>::VARIABLE
                            .parse(part)
                            .map_err(|_| $crate::terminal::tmux::output::ParseError::FormatError)?;
                        <<$crate::terminal::tmux::output::vars::$field
                            as $crate::terminal::tmux::output::VariableKind>::Kind
                            as $crate::terminal::tmux::output::ValueKind>::from_value(value)
                    };
                )*
                Ok(Self { $($field,)* _marker: ::core::marker::PhantomData })
            }
        }
    };
}
pub(crate) use format_struct;

/// The parsed value of a variable (`Variable.Type()` as a value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableValue<'a> {
    Bool(bool),
    Usize(usize),
    String(&'a [u8]),
}

/// The type of the parsed value for a variable (`Variable.Type()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableType {
    Bool,
    Usize,
    String,
}

/// Type-level value kinds, mapping a [`VariableType`] to a Rust type.
pub mod kinds {
    /// `bool`
    pub struct Bool;
    /// `usize`
    pub struct Usize;
    /// `[]const u8`
    pub struct String;
}

/// A type-level value kind (see [`kinds`]).
pub trait ValueKind {
    type Type<'a>: fmt::Debug + Clone + Copy + PartialEq + Eq;
    fn from_value(v: VariableValue<'_>) -> Self::Type<'_>;
}

impl ValueKind for kinds::Bool {
    type Type<'a> = bool;
    fn from_value(v: VariableValue<'_>) -> bool {
        match v {
            VariableValue::Bool(b) => b,
            _ => unreachable!(),
        }
    }
}

impl ValueKind for kinds::Usize {
    type Type<'a> = usize;
    fn from_value(v: VariableValue<'_>) -> usize {
        match v {
            VariableValue::Usize(n) => n,
            _ => unreachable!(),
        }
    }
}

impl ValueKind for kinds::String {
    type Type<'a> = &'a [u8];
    fn from_value(v: VariableValue<'_>) -> &[u8] {
        match v {
            VariableValue::String(s) => s,
            _ => unreachable!(),
        }
    }
}

/// A type-level variable (see [`vars`]).
pub trait VariableKind {
    const VARIABLE: Variable;
    type Kind: ValueKind;
}

macro_rules! define_variables {
    ($( $(#[$doc:meta])* $variant:ident, $name:ident, $kind:ident; )*) => {
        /// Possible variables in a tmux format string that we support.
        ///
        /// Tmux supports a large number of variables, but we only implement
        /// a subset of them here that are relevant to the use case of implementing
        /// control mode for terminal emulators.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Variable {
            $( $(#[$doc])* $variant, )*
        }

        impl Variable {
            /// All variables in declaration order.
            pub const ALL: &'static [Variable] = &[$(Variable::$variant,)*];

            /// The tmux variable name (the Zig tag name).
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Variable::$variant => stringify!($name), )*
                }
            }

            /// The type of the parsed value for this variable type.
            pub const fn value_type(self) -> VariableType {
                match self {
                    $( Variable::$variant => VariableType::$kind, )*
                }
            }
        }

        /// Type-level markers for each variable, named like the variable,
        /// used by `format_struct!`.
        #[allow(non_camel_case_types)]
        pub mod vars {
            $(
                pub struct $name;
                impl super::VariableKind for $name {
                    const VARIABLE: super::Variable = super::Variable::$variant;
                    type Kind = super::kinds::$kind;
                }
            )*
        }
    };
}

define_variables! {
    /// 1 if pane is in alternate screen.
    AlternateOn, alternate_on, Bool;
    /// Saved cursor X in alternate screen.
    AlternateSavedX, alternate_saved_x, Usize;
    /// Saved cursor Y in alternate screen.
    AlternateSavedY, alternate_saved_y, Usize;
    /// 1 if bracketed paste mode is enabled.
    BracketedPaste, bracketed_paste, Bool;
    /// 1 if the cursor is blinking.
    CursorBlinking, cursor_blinking, Bool;
    /// Cursor colour in pane. Possible formats:
    /// - Named colors: `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
    ///   `cyan`, `white`, `default`, `terminal`, or bright variants.
    /// - 256 colors: `colour<N>` where N is 0-255 (e.g., `colour100`).
    /// - RGB hex: `#RRGGBB` (e.g., `#ff0000`).
    /// - Empty string if unset.
    CursorColour, cursor_colour, String;
    /// Pane cursor flag.
    CursorFlag, cursor_flag, Bool;
    /// Cursor shape in pane. Possible values: `block`, `underline`, `bar`,
    /// or `default`.
    CursorShape, cursor_shape, String;
    /// Cursor X position in pane.
    CursorX, cursor_x, Usize;
    /// Cursor Y position in pane.
    CursorY, cursor_y, Usize;
    /// 1 if focus reporting is enabled.
    FocusFlag, focus_flag, Bool;
    /// Pane insert flag.
    InsertFlag, insert_flag, Bool;
    /// Pane keypad cursor flag.
    KeypadCursorFlag, keypad_cursor_flag, Bool;
    /// Pane keypad flag.
    KeypadFlag, keypad_flag, Bool;
    /// Pane mouse all flag.
    MouseAllFlag, mouse_all_flag, Bool;
    /// Pane mouse button flag.
    MouseButtonFlag, mouse_button_flag, Bool;
    /// Pane mouse SGR flag.
    MouseSgrFlag, mouse_sgr_flag, Bool;
    /// Pane mouse standard flag.
    MouseStandardFlag, mouse_standard_flag, Bool;
    /// Pane mouse UTF-8 flag.
    MouseUtf8Flag, mouse_utf8_flag, Bool;
    /// Pane origin flag.
    OriginFlag, origin_flag, Bool;
    /// Unique pane ID prefixed with `%` (e.g., `%0`, `%42`).
    PaneId, pane_id, Usize;
    /// Pane tab positions as a comma-separated list of 0-indexed column
    /// numbers (e.g., `8,16,24,32`). Empty string if no tabs are set.
    PaneTabs, pane_tabs, String;
    /// Bottom of scroll region in pane.
    ScrollRegionLower, scroll_region_lower, Usize;
    /// Top of scroll region in pane.
    ScrollRegionUpper, scroll_region_upper, Usize;
    /// Unique session ID prefixed with `$` (e.g., `$0`, `$42`).
    SessionId, session_id, Usize;
    /// Server version (e.g., `3.5a`).
    Version, version, String;
    /// Unique window ID prefixed with `@` (e.g., `@0`, `@42`).
    WindowId, window_id, Usize;
    /// Width of window.
    WindowWidth, window_width, Usize;
    /// Height of window.
    WindowHeight, window_height, Usize;
    /// Window layout description, ignoring zoomed window panes. Format is
    /// `<checksum>,<layout>` where checksum is a 4-digit hex CRC16 and layout
    /// encodes pane dimensions as `WxH,X,Y[,ID]` with `{...}` for horizontal
    /// splits and `[...]` for vertical splits.
    WindowLayout, window_layout, String;
    /// Pane wrap flag.
    WrapFlag, wrap_flag, Bool;
}

impl Variable {
    /// Parse the given string value into the appropriate resulting
    /// type for this variable.
    pub fn parse(self, value: &[u8]) -> Result<VariableValue<'_>, VariableParseError> {
        use Variable::*;
        Ok(match self {
            AlternateOn | BracketedPaste | CursorBlinking | CursorFlag | FocusFlag
            | InsertFlag | KeypadCursorFlag | KeypadFlag | MouseAllFlag | MouseButtonFlag
            | MouseSgrFlag | MouseStandardFlag | MouseUtf8Flag | OriginFlag | WrapFlag => {
                VariableValue::Bool(value == b"1")
            }
            AlternateSavedX | AlternateSavedY | CursorX | CursorY | ScrollRegionLower
            | ScrollRegionUpper => VariableValue::Usize(parse_int_usize(value)?),
            SessionId => {
                if value.len() >= 2 && value[0] == b'$' {
                    VariableValue::Usize(parse_int_usize(&value[1..])?)
                } else {
                    return Err(VariableParseError::FormatError);
                }
            }
            WindowId => {
                if value.len() >= 2 && value[0] == b'@' {
                    VariableValue::Usize(parse_int_usize(&value[1..])?)
                } else {
                    return Err(VariableParseError::FormatError);
                }
            }
            PaneId => {
                if value.len() >= 2 && value[0] == b'%' {
                    VariableValue::Usize(parse_int_usize(&value[1..])?)
                } else {
                    return Err(VariableParseError::FormatError);
                }
            }
            WindowWidth => VariableValue::Usize(parse_int_usize(value)?),
            WindowHeight => VariableValue::Usize(parse_int_usize(value)?),
            CursorColour | CursorShape | PaneTabs | Version | WindowLayout => {
                VariableValue::String(value)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R<'a> = Result<VariableValue<'a>, VariableParseError>;

    fn b(v: bool) -> R<'static> {
        Ok(VariableValue::Bool(v))
    }
    fn u(v: usize) -> R<'static> {
        Ok(VariableValue::Usize(v))
    }
    fn s(v: &[u8]) -> R<'_> {
        Ok(VariableValue::String(v))
    }
    const INVALID: R<'static> = Err(VariableParseError::InvalidCharacter);
    const FORMAT: R<'static> = Err(VariableParseError::FormatError);
    const OVERFLOW: R<'static> = Err(VariableParseError::Overflow);

    fn check_flag(v: Variable) {
        assert_eq!(b(true), v.parse(b"1"));
        assert_eq!(b(false), v.parse(b"0"));
        assert_eq!(b(false), v.parse(b""));
        assert_eq!(b(false), v.parse(b"true"));
    }

    fn check_usize(v: Variable, second: usize) {
        assert_eq!(u(0), v.parse(b"0"));
        assert_eq!(u(second), v.parse(second.to_string().as_bytes()));
        assert_eq!(INVALID, v.parse(b"abc"));
    }

    #[test]
    fn parse_alternate_on() {
        assert_eq!(b(true), Variable::AlternateOn.parse(b"1"));
        assert_eq!(b(false), Variable::AlternateOn.parse(b"0"));
        assert_eq!(b(false), Variable::AlternateOn.parse(b""));
        assert_eq!(b(false), Variable::AlternateOn.parse(b"true"));
        assert_eq!(b(false), Variable::AlternateOn.parse(b"yes"));
    }

    #[test]
    fn parse_alternate_saved_x() {
        check_usize(Variable::AlternateSavedX, 42);
    }

    #[test]
    fn parse_alternate_saved_y() {
        check_usize(Variable::AlternateSavedY, 42);
    }

    #[test]
    fn parse_cursor_x() {
        check_usize(Variable::CursorX, 79);
    }

    #[test]
    fn parse_cursor_y() {
        check_usize(Variable::CursorY, 23);
    }

    #[test]
    fn parse_scroll_region_upper() {
        check_usize(Variable::ScrollRegionUpper, 5);
    }

    #[test]
    fn parse_scroll_region_lower() {
        check_usize(Variable::ScrollRegionLower, 23);
    }

    #[test]
    fn parse_session_id() {
        let v = Variable::SessionId;
        assert_eq!(u(42), v.parse(b"$42"));
        assert_eq!(u(0), v.parse(b"$0"));
        assert_eq!(FORMAT, v.parse(b"0"));
        assert_eq!(FORMAT, v.parse(b"@0"));
        assert_eq!(FORMAT, v.parse(b"$"));
        assert_eq!(FORMAT, v.parse(b""));
        assert_eq!(INVALID, v.parse(b"$abc"));
    }

    #[test]
    fn parse_window_id() {
        let v = Variable::WindowId;
        assert_eq!(u(42), v.parse(b"@42"));
        assert_eq!(u(0), v.parse(b"@0"));
        assert_eq!(u(12345), v.parse(b"@12345"));
        assert_eq!(FORMAT, v.parse(b"0"));
        assert_eq!(FORMAT, v.parse(b"$0"));
        assert_eq!(FORMAT, v.parse(b"@"));
        assert_eq!(FORMAT, v.parse(b""));
        assert_eq!(INVALID, v.parse(b"@abc"));
    }

    #[test]
    fn parse_window_width() {
        let v = Variable::WindowWidth;
        assert_eq!(u(80), v.parse(b"80"));
        assert_eq!(u(0), v.parse(b"0"));
        assert_eq!(u(12345), v.parse(b"12345"));
        assert_eq!(INVALID, v.parse(b"abc"));
        assert_eq!(INVALID, v.parse(b"80px"));
        assert_eq!(OVERFLOW, v.parse(b"-1"));
    }

    #[test]
    fn parse_window_height() {
        let v = Variable::WindowHeight;
        assert_eq!(u(24), v.parse(b"24"));
        assert_eq!(u(0), v.parse(b"0"));
        assert_eq!(u(12345), v.parse(b"12345"));
        assert_eq!(INVALID, v.parse(b"abc"));
        assert_eq!(INVALID, v.parse(b"24px"));
        assert_eq!(OVERFLOW, v.parse(b"-1"));
    }

    #[test]
    fn parse_window_layout() {
        let v = Variable::WindowLayout;
        assert_eq!(s(b"abc123"), v.parse(b"abc123"));
        assert_eq!(s(b""), v.parse(b""));
        assert_eq!(s(b"a]b,c{d}e(f)"), v.parse(b"a]b,c{d}e(f)"));
    }

    #[test]
    fn parse_cursor_flag() {
        check_flag(Variable::CursorFlag);
    }

    #[test]
    fn parse_insert_flag() {
        check_flag(Variable::InsertFlag);
    }

    #[test]
    fn parse_keypad_cursor_flag() {
        check_flag(Variable::KeypadCursorFlag);
    }

    #[test]
    fn parse_keypad_flag() {
        check_flag(Variable::KeypadFlag);
    }

    #[test]
    fn parse_mouse_button_flag() {
        check_flag(Variable::MouseButtonFlag);
    }

    #[test]
    fn parse_mouse_sgr_flag() {
        check_flag(Variable::MouseSgrFlag);
    }

    #[test]
    fn parse_mouse_standard_flag() {
        check_flag(Variable::MouseStandardFlag);
    }

    #[test]
    fn parse_mouse_utf8_flag() {
        check_flag(Variable::MouseUtf8Flag);
    }

    #[test]
    fn parse_wrap_flag() {
        check_flag(Variable::WrapFlag);
    }

    #[test]
    fn parse_bracketed_paste() {
        check_flag(Variable::BracketedPaste);
    }

    #[test]
    fn parse_cursor_blinking() {
        check_flag(Variable::CursorBlinking);
    }

    #[test]
    fn parse_focus_flag() {
        check_flag(Variable::FocusFlag);
    }

    #[test]
    fn parse_mouse_all_flag() {
        check_flag(Variable::MouseAllFlag);
    }

    #[test]
    fn parse_origin_flag() {
        check_flag(Variable::OriginFlag);
    }

    #[test]
    fn parse_pane_id() {
        let v = Variable::PaneId;
        assert_eq!(u(42), v.parse(b"%42"));
        assert_eq!(u(0), v.parse(b"%0"));
        assert_eq!(FORMAT, v.parse(b"0"));
        assert_eq!(FORMAT, v.parse(b"@0"));
        assert_eq!(FORMAT, v.parse(b"%"));
        assert_eq!(FORMAT, v.parse(b""));
        assert_eq!(INVALID, v.parse(b"%abc"));
    }

    #[test]
    fn parse_cursor_colour() {
        let v = Variable::CursorColour;
        assert_eq!(s(b"red"), v.parse(b"red"));
        assert_eq!(s(b"#ff0000"), v.parse(b"#ff0000"));
        assert_eq!(s(b""), v.parse(b""));
    }

    #[test]
    fn parse_cursor_shape() {
        let v = Variable::CursorShape;
        assert_eq!(s(b"block"), v.parse(b"block"));
        assert_eq!(s(b"underline"), v.parse(b"underline"));
        assert_eq!(s(b"bar"), v.parse(b"bar"));
        assert_eq!(s(b""), v.parse(b""));
    }

    #[test]
    fn parse_pane_tabs() {
        let v = Variable::PaneTabs;
        assert_eq!(s(b"0,8,16,24"), v.parse(b"0,8,16,24"));
        assert_eq!(s(b""), v.parse(b""));
        assert_eq!(s(b"0"), v.parse(b"0"));
    }

    #[test]
    fn parse_version() {
        let v = Variable::Version;
        assert_eq!(s(b"3.5a"), v.parse(b"3.5a"));
        assert_eq!(s(b"3.5"), v.parse(b"3.5"));
        assert_eq!(s(b"next-3.5"), v.parse(b"next-3.5"));
        assert_eq!(s(b""), v.parse(b""));
    }

    #[test]
    fn parse_int_usize_zig_semantics() {
        assert_eq!(Ok(5), parse_int_usize(b"+5"));
        assert_eq!(Ok(0), parse_int_usize(b"-0"));
        assert_eq!(Ok(1000), parse_int_usize(b"1_000"));
        assert_eq!(Err(ParseIntError::InvalidCharacter), parse_int_usize(b"_1"));
        assert_eq!(Err(ParseIntError::InvalidCharacter), parse_int_usize(b"1_"));
        assert_eq!(Err(ParseIntError::InvalidCharacter), parse_int_usize(b"+"));
        assert_eq!(Err(ParseIntError::InvalidCharacter), parse_int_usize(b""));
        assert_eq!(
            Err(ParseIntError::Overflow),
            parse_int_usize(b"99999999999999999999999")
        );
    }

    format_struct!(struct SessionOnly { session_id });
    format_struct!(struct Multi { session_id, window_id, window_width, window_height });
    format_struct!(struct WindowIdLayout { window_id, window_layout });
    format_struct!(struct WidthHeight { window_width, window_height });
    format_struct!(struct SessionWindow { session_id, window_id });
    format_struct!(struct SessionLayout { session_id, window_layout });

    #[test]
    fn parse_format_struct_single_field() {
        let result: SessionOnly = parse_format_struct(b"$42", b' ').unwrap();
        assert_eq!(42, result.session_id);
    }

    #[test]
    fn parse_format_struct_multiple_fields() {
        let result: Multi = parse_format_struct(b"$1 @2 80 24", b' ').unwrap();
        assert_eq!(1, result.session_id);
        assert_eq!(2, result.window_id);
        assert_eq!(80, result.window_width);
        assert_eq!(24, result.window_height);
    }

    #[test]
    fn parse_format_struct_with_string_field() {
        let result: WindowIdLayout = parse_format_struct(b"@5,abc123", b',').unwrap();
        assert_eq!(5, result.window_id);
        assert_eq!(b"abc123", result.window_layout);
    }

    #[test]
    fn parse_format_struct_different_delimiter() {
        let result: WidthHeight = parse_format_struct(b"120\t40", b'\t').unwrap();
        assert_eq!(120, result.window_width);
        assert_eq!(40, result.window_height);
    }

    #[test]
    fn parse_format_struct_missing_entry() {
        assert_eq!(
            Err(ParseError::MissingEntry),
            parse_format_struct::<SessionWindow>(b"$1", b' ')
        );
    }

    #[test]
    fn parse_format_struct_extra_entry() {
        assert_eq!(
            Err(ParseError::ExtraEntry),
            parse_format_struct::<SessionOnly>(b"$1 @2", b' ')
        );
    }

    #[test]
    fn parse_format_struct_format_error() {
        assert_eq!(
            Err(ParseError::FormatError),
            parse_format_struct::<SessionOnly>(b"42", b' ')
        );
        assert_eq!(
            Err(ParseError::FormatError),
            parse_format_struct::<SessionOnly>(b"@42", b' ')
        );
        assert_eq!(
            Err(ParseError::FormatError),
            parse_format_struct::<SessionOnly>(b"$abc", b' ')
        );
    }

    #[test]
    fn parse_format_struct_empty_string() {
        assert_eq!(
            Err(ParseError::FormatError),
            parse_format_struct::<SessionOnly>(b"", b' ')
        );
    }

    #[test]
    fn parse_format_struct_with_empty_layout_field() {
        let result: SessionLayout = parse_format_struct(b"$1,", b',').unwrap();
        assert_eq!(1, result.session_id);
        assert_eq!(b"", result.window_layout);
    }

    fn test_format(vars: &[Variable], delimiter: u8, expected: &str) {
        let comptime_result = comptime_format(vars, delimiter);
        assert_eq!(expected, comptime_result);

        let mut buf = [0u8; 256];
        let mut writer: &mut [u8] = &mut buf;
        format(&mut writer, vars, delimiter).unwrap();
        let n = 256 - writer.len();
        assert_eq!(expected.as_bytes(), &buf[..n]);
    }

    #[test]
    fn format_single_variable() {
        test_format(&[Variable::SessionId], b' ', "#{session_id}");
        assert_eq!("#{session_id}", SessionOnly::comptime_format(b' '));
    }

    #[test]
    fn format_multiple_variables() {
        test_format(
            &[
                Variable::SessionId,
                Variable::WindowId,
                Variable::WindowWidth,
                Variable::WindowHeight,
            ],
            b' ',
            "#{session_id} #{window_id} #{window_width} #{window_height}",
        );
    }

    #[test]
    fn format_with_comma_delimiter() {
        test_format(
            &[Variable::WindowId, Variable::WindowLayout],
            b',',
            "#{window_id},#{window_layout}",
        );
    }

    #[test]
    fn format_with_tab_delimiter() {
        test_format(
            &[Variable::WindowWidth, Variable::WindowHeight],
            b'\t',
            "#{window_width}\t#{window_height}",
        );
    }

    #[test]
    fn format_empty_variables() {
        test_format(&[], b' ', "");
    }

    #[test]
    fn format_all_variables() {
        test_format(
            &[
                Variable::SessionId,
                Variable::WindowId,
                Variable::WindowWidth,
                Variable::WindowHeight,
                Variable::WindowLayout,
            ],
            b' ',
            "#{session_id} #{window_id} #{window_width} #{window_height} #{window_layout}",
        );
    }
}
