//! Port of `src/terminal/c/osc.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: Zig's `GhosttyOscCommand` is a pointer to the command
//! stored inside the parser. The Rust parser returns the command by value
//! (borrowing the parser), so the handle wraps the parser together with the
//! last command returned by `end`; the command handle points at that slot.
//! The slot is cleared before the parser is used again, matching the
//! documented lifetime of the command.

use std::ffi::{c_char, c_int, c_void};

use super::result::Result;
use super::types::HandleAllocator;
use crate::lib::allocator::Allocator as CAllocator;
use crate::terminal::osc;

const LOG_TARGET: &str = "osc";

/// The parser behind a `GhosttyOscParser` handle.
pub struct ParserWrapper {
    pub parser: osc::Parser,
    /// The command returned by the last `end` call. It borrows the
    /// parser's buffers (the `'static` lifetime is a lie upheld by clearing
    /// this before the parser is mutated again).
    command: std::option::Option<osc::Command<'static>>,
    alloc: HandleAllocator,
}

impl ParserWrapper {
    /// Forget the last command before the parser changes.
    fn parser_mut(&mut self) -> &mut osc::Parser {
        self.command = None;
        &mut self.parser
    }
}

/// C: GhosttyOscParser
pub type Parser = *mut ParserWrapper;

/// C: GhosttyOscCommand
pub type Command = *mut osc::Command<'static>;

/// C: GhosttyOscCommandType
pub type CommandType = osc::Key;

/// C: GhosttyOscTerminator
pub type Terminator = osc::TerminatorC;

/// C: ghostty_osc_new
#[export_name = "ghostty_osc_new"]
pub extern "C" fn new(alloc_: std::option::Option<&CAllocator>, result: &mut Parser) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(ParserWrapper {
        parser: osc::Parser::init(Some(osc::Allocator::GLOBAL)),
        command: None,
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_osc_free
///
/// # Safety
/// `parser_` must be NULL or a parser from [`new`] that is not used
/// afterwards.
#[export_name = "ghostty_osc_free"]
pub unsafe extern "C" fn free(parser_: Parser) {
    // C-built parsers always have an associated allocator.
    let Some(parser) = parser_.as_mut() else {
        return;
    };
    let alloc = parser.alloc;
    parser.parser_mut().deinit();
    alloc.destroy(parser_);
}

/// C: ghostty_osc_reset
///
/// # Safety
/// `parser_` must be a valid parser.
#[export_name = "ghostty_osc_reset"]
pub unsafe extern "C" fn reset(parser_: Parser) {
    (*parser_).parser_mut().reset();
}

/// C: ghostty_osc_next
///
/// # Safety
/// `parser_` must be a valid parser.
#[export_name = "ghostty_osc_next"]
pub unsafe extern "C" fn next(parser_: Parser, byte: u8) {
    (*parser_).parser_mut().next(byte);
}

/// C: ghostty_osc_end
///
/// # Safety
/// `parser_` must be a valid parser. The returned command is valid until
/// the next call on the parser.
#[export_name = "ghostty_osc_end"]
pub unsafe extern "C" fn end(parser_: Parser, terminator: u8) -> Command {
    let wrapper = &mut *parser_;
    let command = wrapper.parser_mut().end(Some(terminator));
    // SAFETY: see `ParserWrapper::command`.
    let command: std::option::Option<osc::Command<'static>> = std::mem::transmute(command);
    wrapper.command = command;
    match wrapper.command.as_mut() {
        Some(c) => c as *mut osc::Command<'static>,
        None => std::ptr::null_mut(),
    }
}

crate::c_api_enum! {
    /// C: GhosttyOscOption
    pub enum Option {
        UnknownMaxBytes = 0,
    }
}

/// C: ghostty_osc_set
///
/// # Safety
/// `parser_` must be NULL or a valid parser; `value` must be NULL or of
/// the option's input type (`usize` for `unknown_max_bytes`).
#[export_name = "ghostty_osc_set"]
pub unsafe extern "C" fn set(parser_: Parser, option: c_int, value: *const c_void) -> Result {
    let Some(option) = Option::from_int(option) else {
        log::warn!(target: LOG_TARGET, "osc_set invalid option value={option}");
        return Result::InvalidValue;
    };

    let Some(parser) = parser_.as_mut() else {
        return Result::InvalidValue;
    };
    match option {
        Option::UnknownMaxBytes => {
            let ptr = value as *const usize;
            parser.parser_mut().unknown_max_bytes = if ptr.is_null() { 0 } else { *ptr };
        }
    }

    Result::Success
}

/// C: ghostty_osc_command_type
///
/// # Safety
/// `command_` must be NULL or a command returned by [`end`] that is still
/// valid.
#[export_name = "ghostty_osc_command_type"]
pub unsafe extern "C" fn command_type(command_: Command) -> CommandType {
    match command_.as_ref() {
        Some(command) => command.key(),
        None => osc::Key::Invalid,
    }
}

crate::c_api_enum! {
    /// C: GhosttyOscCommandData
    ///
    /// Output type expected for querying the data of the given kind
    /// (Zig: `CommandData.OutType`): `const char*` for
    /// change_window_title_str, `GhosttyString` for unknown_content, `bool`
    /// for unknown_truncated and `GhosttyOscTerminator` for
    /// unknown_terminator.
    pub enum CommandData {
        Invalid = 0,
        ChangeWindowTitleStr = 1,
        UnknownContent = 2,
        UnknownTruncated = 3,
        UnknownTerminator = 4,
    }
}

/// C: ghostty_osc_command_data
///
/// # Safety
/// `command_` must be NULL or a valid command; `out` must point to storage
/// of the output type for `data`.
#[export_name = "ghostty_osc_command_data"]
pub unsafe extern "C" fn command_data(command_: Command, data: c_int, out: *mut c_void) -> bool {
    let Some(data) = CommandData::from_int(data) else {
        log::warn!(target: LOG_TARGET, "commandData invalid data value={data}");
        return false;
    };

    match data {
        CommandData::Invalid => false,
        _ => command_data_typed(command_, data, out),
    }
}

unsafe fn command_data_typed(command_: Command, data: CommandData, out: *mut c_void) -> bool {
    let Some(command) = command_.as_ref() else {
        return false;
    };
    match data {
        CommandData::Invalid => return false,
        CommandData::ChangeWindowTitleStr => match command {
            // The parser NUL-terminates the title in its buffer.
            osc::Command::ChangeWindowTitle(v) => *(out as *mut *const c_char) = v.as_ptr() as *const c_char,
            _ => return false,
        },
        CommandData::UnknownContent => match command {
            osc::Command::Unknown(v) => *(out as *mut crate::lib::String) = crate::lib::String::init(v.content),
            _ => return false,
        },
        CommandData::UnknownTruncated => match command {
            osc::Command::Unknown(v) => *(out as *mut bool) = v.truncated,
            _ => return false,
        },
        CommandData::UnknownTerminator => match command {
            osc::Command::Unknown(v) => *(out as *mut Terminator) = v.terminator.cval(),
            _ => return false,
        },
    }

    true
}

/// `(zig name, value)` of every `GhosttyOscCommandType` key, for the type
/// manifest.
pub fn command_type_values() -> Vec<(String, i64)> {
    use osc::Key::*;
    [
        Invalid,
        ChangeWindowTitle,
        ChangeWindowIcon,
        SemanticPrompt,
        ClipboardContents,
        ReportPwd,
        MouseShape,
        ColorOperation,
        KittyColorProtocol,
        ShowDesktopNotification,
        HyperlinkStart,
        HyperlinkEnd,
        ConemuSleep,
        ConemuShowMessageBox,
        ConemuChangeTabTitle,
        ConemuProgressReport,
        ConemuWaitInput,
        ConemuGuimacro,
        ConemuRunProcess,
        ConemuOutputEnvironmentVariable,
        ConemuXtermEmulation,
        ConemuComment,
        KittyTextSizing,
        KittyClipboardProtocol,
        KittyDndProtocol,
        HierarchicalContext,
        KittyDesktopNotification,
        Unknown,
        ProgramStatus,
    ]
    .into_iter()
    .map(|k| (super::types::zig_name(&format!("{k:?}")), k as c_int as i64))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;

    fn out<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn alloc() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut p));
        unsafe { free(p) };
    }

    #[test]
    fn command_type_null() {
        assert_eq!(osc::Key::Invalid, unsafe { command_type(std::ptr::null_mut()) });
    }

    #[test]
    fn command_data_null() {
        let mut title: *const c_char = std::ptr::null();
        assert!(!unsafe {
            command_data(
                std::ptr::null_mut(),
                CommandData::ChangeWindowTitleStr as c_int,
                out(&mut title),
            )
        });
    }

    #[test]
    fn change_window_title() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut p));

        unsafe {
            // Parse it
            next(p, b'0');
            next(p, b';');
            next(p, b'a');
            let cmd = end(p, 0);
            assert_eq!(osc::Key::ChangeWindowTitle, command_type(cmd));

            // Extract the title
            let mut title: *const c_char = std::ptr::null();
            assert!(command_data(cmd, CommandData::ChangeWindowTitleStr as c_int, out(&mut title)));
            assert_eq!(b"a", std::ffi::CStr::from_ptr(title).to_bytes());

            free(p);
        }
    }

    #[test]
    fn unknown() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut p));

        unsafe {
            let max_bytes: usize = 8;
            assert_eq!(
                Result::Success,
                set(p, Option::UnknownMaxBytes as c_int, &max_bytes as *const usize as *const c_void)
            );

            // Parse it
            for &ch in b"7400;x" {
                next(p, ch);
            }
            let cmd = end(p, 0x07);
            assert_eq!(osc::Key::Unknown, command_type(cmd));

            let mut content = crate::lib::String::init(b"");
            assert!(command_data(cmd, CommandData::UnknownContent as c_int, out(&mut content)));
            assert_eq!(b"7400;x", std::slice::from_raw_parts(content.ptr, content.len));

            let mut truncated = true;
            assert!(command_data(cmd, CommandData::UnknownTruncated as c_int, out(&mut truncated)));
            assert!(!truncated);

            let mut terminator = osc::TerminatorC::St;
            assert!(command_data(cmd, CommandData::UnknownTerminator as c_int, out(&mut terminator)));
            assert_eq!(osc::TerminatorC::Bel, terminator);

            // Other commands can't read unknown data.
            reset(p);
            for &ch in b"0;a" {
                next(p, ch);
            }
            let title_cmd = end(p, 0x07);
            assert_eq!(osc::Key::ChangeWindowTitle, command_type(title_cmd));
            assert!(!command_data(title_cmd, CommandData::UnknownContent as c_int, out(&mut content)));

            // A NULL value disables capture again.
            assert_eq!(
                Result::Success,
                set(p, Option::UnknownMaxBytes as c_int, std::ptr::null())
            );
            reset(p);
            for &ch in b"7400;x" {
                next(p, ch);
            }
            assert_eq!(osc::Key::Invalid, command_type(end(p, 0x07)));

            free(p);
        }
    }
}
