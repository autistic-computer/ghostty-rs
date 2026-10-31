//! Port of `src/input/command.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: the comptime `defaults`/`defaultsC` tables are built
//! once, lazily ([`defaults`], [`defaults_c`]). Titles and descriptions are
//! NUL-terminated (`[:0]const u8` in Zig) so they are `Cow<'static, CStr>`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::LazyLock;

use super::binding::{
    Action, ActionKey, CloseTabMode, CopyToClipboard, GotoWindow, InspectorMode, NavigateSearch,
    OpenConfig, SplitDirection, SplitFocusDirection, WriteScreen, WriteScreenAction,
    WriteScreenFormat,
};

use crate::os::i18n::{gettext, N_ as n_};

/// A command is a named binding action that can be executed from
/// something like a command palette.
///
/// A command must be associated with a binding; all commands can be
/// mapped to traditional `keybind` configurations. This restriction
/// makes it so that there is nothing special about commands and likewise
/// it makes it trivial and consistent to define custom commands.
///
/// For apprt implementers: a command palette doesn't have to make use
/// of all the fields here. We try to provide as much information as
/// possible to make it easier to implement a command palette in the way
/// that makes the most sense for the application.
#[derive(Debug, Clone)]
pub struct Command {
    pub action: Action,
    pub title: Cow<'static, CStr>,
    pub description: Cow<'static, CStr>,
}

/// ghostty_command_s (`Command.C`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CommandC {
    pub action_key: *const c_char,
    pub action: *const c_char,
    pub title: *const c_char,
    pub description: *const c_char,
}

impl Command {
    /// A command with an empty description.
    pub fn new(action: Action, title: Cow<'static, CStr>) -> Command {
        Command {
            action,
            title,
            description: Cow::Borrowed(c""),
        }
    }

    pub fn equal(&self, other: &Command) -> bool {
        if self.action.hash() != other.action.hash() {
            return false;
        }
        if self.title.to_bytes() != other.title.to_bytes() {
            return false;
        }
        if self.description.to_bytes() != other.description.to_bytes() {
            return false;
        }
        true
    }

    /// Convert this command to a C struct for static command tables (Zig:
    /// at comptime).
    ///
    /// The formatted action string is leaked (like comptime data embedded
    /// in the binary), so this is only meant to be called once per entry of
    /// a `'static` table such as [`defaults_c`].
    pub fn comptime_cval(&'static self) -> CommandC {
        let action: &'static CStr = Box::leak(action_cstring(&self.action).into_boxed_c_str());
        CommandC {
            action_key: self.action.key().as_cstr().as_ptr(),
            action: action.as_ptr(),
            title: self.title.as_ptr(),
            description: self.description.as_ptr(),
        }
    }

    /// Convert this command to a C struct at runtime.
    ///
    /// This shares memory with the original command, which must outlive
    /// the returned value.
    ///
    /// The action string is allocated into `alloc` (Zig: the provided
    /// allocator; an arena is recommended), which must also outlive the
    /// returned value.
    pub fn cval(&self, alloc: &mut Vec<CString>) -> CommandC {
        let action = action_cstring(&self.action);
        let action_ptr = action.as_ptr();
        alloc.push(action);

        CommandC {
            action_key: self.action.key().as_cstr().as_ptr(),
            action: action_ptr,
            title: self.title.as_ptr(),
            description: self.description.as_ptr(),
        }
    }

    pub fn translated(&self) -> Command {
        Command {
            action: self.action.clone(),
            title: translate(&self.title),
            description: translate(&self.description),
        }
    }

    /// Implements a comparison function for sorting. The sorting is
    /// defined by Ghostty to be what we prefer. If a caller wants some
    /// other sorting, they should do it themselves.
    pub fn less_than(lhs: &Command, rhs: &Command) -> bool {
        order_ignore_case(lhs.title.to_bytes(), rhs.title.to_bytes()) == Ordering::Less
    }
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

/// Format an action as a C string (truncated at an interior NUL, like a
/// C consumer would read it).
fn action_cstring(action: &Action) -> CString {
    let mut buf = Vec::new();
    action.format_to(&mut buf);
    if let Some(nul) = memchr::memchr(0, &buf) {
        buf.truncate(nul);
    }
    CString::new(buf).expect("interior NUL was removed")
}

/// `std.mem.span(i18n._(s))`, keeping static strings borrowed.
fn translate(s: &Cow<'static, CStr>) -> Cow<'static, CStr> {
    match s {
        Cow::Borrowed(s) => Cow::Borrowed(gettext(s)),
        Cow::Owned(s) => Cow::Owned(gettext(s).to_owned()),
    }
}

/// `std.ascii.orderIgnoreCase`
fn order_ignore_case(lhs: &[u8], rhs: &[u8]) -> Ordering {
    for (a, b) in lhs.iter().zip(rhs) {
        match a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    lhs.len().cmp(&rhs.len())
}

/// The default commands, sorted by title.
pub fn defaults() -> &'static [Command] {
    static DEFAULTS: LazyLock<Vec<Command>> = LazyLock::new(|| {
        let mut result: Vec<Command> = Vec::new();
        for &action in ActionKey::ALL {
            result.extend(action_commands(action));
        }

        result.sort_unstable_by(|a, b| order_ignore_case(a.title.to_bytes(), b.title.to_bytes()));
        result
    });
    &DEFAULTS
}

/// The C commands table. The pointers only reference immutable `'static`
/// data, so sharing it between threads is fine.
struct DefaultsC(Vec<CommandC>);

// SAFETY: every pointer in the table points to immutable data with a
// 'static lifetime (`defaults()` and leaked action strings).
unsafe impl Send for DefaultsC {}
// SAFETY: see above; the data is never mutated.
unsafe impl Sync for DefaultsC {}

/// Defaults in C-compatible form.
pub fn defaults_c() -> &'static [CommandC] {
    static DEFAULTS_C: LazyLock<DefaultsC> =
        LazyLock::new(|| DefaultsC(defaults().iter().map(Command::comptime_cval).collect()));
    &DEFAULTS_C.0
}

fn cmd(action: Action, title: &'static CStr, description: &'static CStr) -> Command {
    Command {
        action,
        title: Cow::Borrowed(title),
        description: Cow::Borrowed(description),
    }
}

fn ws(action: WriteScreenAction, emit: WriteScreenFormat) -> WriteScreen {
    WriteScreen { action, emit }
}

/// Returns the set of commands associated with this action key by
/// default. Not all actions should have commands. As a general guideline,
/// an action should have a command only if it is useful and reasonable
/// to appear in a command palette.
fn action_commands(action: ActionKey) -> Vec<Command> {
    use WriteScreenAction as WA;
    use WriteScreenFormat as WF;

    // This is implemented as a function and match rather than a
    // flat const because we want to ensure we get a compiler
    // error when a new binding is added so that the contributor has
    // to consider whether that new binding should have commands or not.
    let result: Vec<Command> = match action {
        ActionKey::Reset => vec![cmd(
            Action::Reset,
            n_(c"Reset Terminal"),
            n_(c"Reset the terminal to a clean state."),
        )],

        ActionKey::CopyToClipboard => vec![
            cmd(
                Action::CopyToClipboard(CopyToClipboard::Mixed),
                n_(c"Copy to Clipboard"),
                n_(c"Copy the selected text to the clipboard in both plain and styled formats."),
            ),
            cmd(
                Action::CopyToClipboard(CopyToClipboard::Plain),
                n_(c"Copy Selection as Plain Text to Clipboard"),
                n_(c"Copy the selected text as plain text to the clipboard."),
            ),
            cmd(
                Action::CopyToClipboard(CopyToClipboard::Vt),
                n_(c"Copy Selection as ANSI Sequences to Clipboard"),
                n_(c"Copy the selected text as ANSI escape sequences to the clipboard."),
            ),
            cmd(
                Action::CopyToClipboard(CopyToClipboard::Html),
                n_(c"Copy Selection as HTML to Clipboard"),
                n_(c"Copy the selected text as HTML to the clipboard."),
            ),
        ],

        ActionKey::CopyUrlToClipboard => vec![cmd(
            Action::CopyUrlToClipboard,
            n_(c"Copy URL to Clipboard"),
            n_(c"Copy the URL under the cursor to the clipboard."),
        )],

        ActionKey::CopyTitleToClipboard => vec![cmd(
            Action::CopyTitleToClipboard,
            n_(c"Copy Terminal Title to Clipboard"),
            n_(c"Copy the terminal title to the clipboard. If the terminal title is not set this has no effect."),
        )],

        ActionKey::PasteFromClipboard => vec![cmd(
            Action::PasteFromClipboard,
            n_(c"Paste from Clipboard"),
            n_(c"Paste the contents of the main clipboard."),
        )],

        ActionKey::PasteFromSelection => vec![cmd(
            Action::PasteFromSelection,
            n_(c"Paste from Selection"),
            n_(c"Paste the contents of the selection clipboard."),
        )],

        ActionKey::StartSearch => vec![cmd(
            Action::StartSearch,
            n_(c"Start Search"),
            n_(c"Start a search if one isn't already active."),
        )],

        ActionKey::SearchSelection => vec![cmd(
            Action::SearchSelection,
            n_(c"Search Selection"),
            n_(c"Start a search for the current text selection."),
        )],

        ActionKey::EndSearch => vec![cmd(
            Action::EndSearch,
            n_(c"End Search"),
            n_(c"End the current search if any and hide any GUI elements."),
        )],

        ActionKey::NavigateSearch => vec![
            cmd(
                Action::NavigateSearch(NavigateSearch::Next),
                n_(c"Next Search Result"),
                n_(c"Navigate to the next search result, if any."),
            ),
            cmd(
                Action::NavigateSearch(NavigateSearch::Previous),
                n_(c"Previous Search Result"),
                n_(c"Navigate to the previous search result, if any."),
            ),
        ],

        ActionKey::IncreaseFontSize => vec![cmd(
            Action::IncreaseFontSize(1.0),
            n_(c"Increase Font Size"),
            n_(c"Increase the font size by 1 point."),
        )],

        ActionKey::DecreaseFontSize => vec![cmd(
            Action::DecreaseFontSize(1.0),
            n_(c"Decrease Font Size"),
            n_(c"Decrease the font size by 1 point."),
        )],

        ActionKey::ResetFontSize => vec![cmd(
            Action::ResetFontSize,
            n_(c"Reset Font Size"),
            n_(c"Reset the font size to the default."),
        )],

        ActionKey::ClearScreen => vec![cmd(
            Action::ClearScreen,
            n_(c"Clear Screen"),
            n_(c"Clear the screen and scrollback."),
        )],

        ActionKey::SelectAll => vec![cmd(
            Action::SelectAll,
            n_(c"Select All"),
            n_(c"Select all text on the screen."),
        )],

        ActionKey::ScrollToTop => vec![cmd(
            Action::ScrollToTop,
            n_(c"Scroll to Top"),
            n_(c"Scroll to the top of the screen."),
        )],

        ActionKey::ScrollToBottom => vec![cmd(
            Action::ScrollToBottom,
            n_(c"Scroll to Bottom"),
            n_(c"Scroll to the bottom of the screen."),
        )],

        ActionKey::ScrollToSelection => vec![cmd(
            Action::ScrollToSelection,
            n_(c"Scroll to Selection"),
            n_(c"Scroll to the selected text."),
        )],

        ActionKey::ScrollPageUp => vec![cmd(
            Action::ScrollPageUp,
            n_(c"Scroll Page Up"),
            n_(c"Scroll the screen up by a page."),
        )],

        ActionKey::ScrollPageDown => vec![cmd(
            Action::ScrollPageDown,
            n_(c"Scroll Page Down"),
            n_(c"Scroll the screen down by a page."),
        )],

        ActionKey::WriteScreenFile => vec![
            cmd(
                Action::WriteScreenFile(WriteScreen::COPY),
                n_(c"Copy Screen to Temporary File and Copy Path"),
                n_(c"Copy the screen contents to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteScreenFile(WriteScreen::PASTE),
                n_(c"Copy Screen to Temporary File and Paste Path"),
                n_(c"Copy the screen contents to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteScreenFile(WriteScreen::OPEN),
                n_(c"Copy Screen to Temporary File and Open"),
                n_(c"Copy the screen contents to a temporary file and open it."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Copy, WF::Html)),
                n_(c"Copy Screen as HTML to Temporary File and Copy Path"),
                n_(c"Copy the screen contents as HTML to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Paste, WF::Html)),
                n_(c"Copy Screen as HTML to Temporary File and Paste Path"),
                n_(c"Copy the screen contents as HTML to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Open, WF::Html)),
                n_(c"Copy Screen as HTML to Temporary File and Open"),
                n_(c"Copy the screen contents as HTML to a temporary file and open it."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Copy, WF::Vt)),
                n_(c"Copy Screen as ANSI Sequences to Temporary File and Copy Path"),
                n_(c"Copy the screen contents as ANSI escape sequences to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Paste, WF::Vt)),
                n_(c"Copy Screen as ANSI Sequences to Temporary File and Paste Path"),
                n_(c"Copy the screen contents as ANSI escape sequences to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteScreenFile(ws(WA::Open, WF::Vt)),
                n_(c"Copy Screen as ANSI Sequences to Temporary File and Open"),
                n_(c"Copy the screen contents as ANSI escape sequences to a temporary file and open it."),
            ),
        ],

        ActionKey::WriteSelectionFile => vec![
            cmd(
                Action::WriteSelectionFile(WriteScreen::COPY),
                n_(c"Copy Selection to Temporary File and Copy Path"),
                n_(c"Copy the selection contents to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteSelectionFile(WriteScreen::PASTE),
                n_(c"Copy Selection to Temporary File and Paste Path"),
                n_(c"Copy the selection contents to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteSelectionFile(WriteScreen::OPEN),
                n_(c"Copy Selection to Temporary File and Open"),
                n_(c"Copy the selection contents to a temporary file and open it."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Copy, WF::Html)),
                n_(c"Copy Selection as HTML to Temporary File and Copy Path"),
                n_(c"Copy the selection contents as HTML to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Paste, WF::Html)),
                n_(c"Copy Selection as HTML to Temporary File and Paste Path"),
                n_(c"Copy the selection contents as HTML to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Open, WF::Html)),
                n_(c"Copy Selection as HTML to Temporary File and Open"),
                n_(c"Copy the selection contents as HTML to a temporary file and open it."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Copy, WF::Vt)),
                n_(c"Copy Selection as ANSI Sequences to Temporary File and Copy Path"),
                n_(c"Copy the selection contents as ANSI escape sequences to a temporary file and copy the path to the clipboard."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Paste, WF::Vt)),
                n_(c"Copy Selection as ANSI Sequences to Temporary File and Paste Path"),
                n_(c"Copy the selection contents as ANSI escape sequences to a temporary file and paste the path to the file."),
            ),
            cmd(
                Action::WriteSelectionFile(ws(WA::Open, WF::Vt)),
                n_(c"Copy Selection as ANSI Sequences to Temporary File and Open"),
                n_(c"Copy the selection contents as ANSI escape sequences to a temporary file and open it."),
            ),
        ],

        ActionKey::NewWindow => vec![cmd(
            Action::NewWindow,
            n_(c"New Window"),
            n_(c"Open a new window."),
        )],

        ActionKey::NewTab => vec![cmd(
            Action::NewTab,
            n_(c"New Tab"),
            n_(c"Open a new tab."),
        )],

        ActionKey::MoveTab => vec![
            cmd(
                Action::MoveTab(-1),
                n_(c"Move Tab Left"),
                n_(c"Move the current tab to the left."),
            ),
            cmd(
                Action::MoveTab(1),
                n_(c"Move Tab Right"),
                n_(c"Move the current tab to the right."),
            ),
        ],

        ActionKey::MoveTabToNewWindow => vec![cmd(
            Action::MoveTabToNewWindow,
            n_(c"Move Tab to New Window"),
            n_(c"Move the current tab to a new window."),
        )],

        ActionKey::ToggleTabOverview => vec![cmd(
            Action::ToggleTabOverview,
            n_(c"Toggle Tab Overview"),
            n_(c"Toggle the tab overview."),
        )],

        ActionKey::PromptSurfaceTitle => vec![cmd(
            Action::PromptSurfaceTitle,
            n_(c"Change Terminal Title…"),
            n_(c"Prompt for a new title for the current terminal."),
        )],

        ActionKey::PromptTabTitle => vec![cmd(
            Action::PromptTabTitle,
            n_(c"Change Tab Title…"),
            n_(c"Prompt for a new title for the current tab."),
        )],

        ActionKey::PromptWindowTitle => vec![cmd(
            Action::PromptWindowTitle,
            c"Change Window Title…",
            c"Prompt for a new title for the current window.",
        )],

        ActionKey::NewSplit => vec![
            cmd(
                Action::NewSplit(SplitDirection::Left),
                n_(c"Split Left"),
                n_(c"Split the terminal to the left."),
            ),
            cmd(
                Action::NewSplit(SplitDirection::Right),
                n_(c"Split Right"),
                n_(c"Split the terminal to the right."),
            ),
            cmd(
                Action::NewSplit(SplitDirection::Up),
                n_(c"Split Up"),
                n_(c"Split the terminal up."),
            ),
            cmd(
                Action::NewSplit(SplitDirection::Down),
                n_(c"Split Down"),
                n_(c"Split the terminal down."),
            ),
        ],

        ActionKey::GotoSplit => vec![
            cmd(
                Action::GotoSplit(SplitFocusDirection::Previous),
                n_(c"Focus Split: Previous"),
                n_(c"Focus the previous split, if any."),
            ),
            cmd(
                Action::GotoSplit(SplitFocusDirection::Next),
                n_(c"Focus Split: Next"),
                n_(c"Focus the next split, if any."),
            ),
            cmd(
                Action::GotoSplit(SplitFocusDirection::Left),
                n_(c"Focus Split: Left"),
                n_(c"Focus the split to the left, if it exists."),
            ),
            cmd(
                Action::GotoSplit(SplitFocusDirection::Right),
                n_(c"Focus Split: Right"),
                n_(c"Focus the split to the right, if it exists."),
            ),
            cmd(
                Action::GotoSplit(SplitFocusDirection::Up),
                n_(c"Focus Split: Up"),
                n_(c"Focus the split above, if it exists."),
            ),
            cmd(
                Action::GotoSplit(SplitFocusDirection::Down),
                n_(c"Focus Split: Down"),
                n_(c"Focus the split below, if it exists."),
            ),
        ],

        ActionKey::GotoWindow => vec![
            cmd(
                Action::GotoWindow(GotoWindow::Previous),
                n_(c"Focus Window: Previous"),
                n_(c"Focus the previous window, if any."),
            ),
            cmd(
                Action::GotoWindow(GotoWindow::Next),
                n_(c"Focus Window: Next"),
                n_(c"Focus the next window, if any."),
            ),
        ],

        ActionKey::ToggleSplitZoom => vec![cmd(
            Action::ToggleSplitZoom,
            n_(c"Toggle Split Zoom"),
            n_(c"Toggle the zoom state of the current split."),
        )],

        ActionKey::ToggleReadonly => vec![cmd(
            Action::ToggleReadonly,
            n_(c"Toggle Read-Only Mode"),
            n_(c"Toggle read-only mode for the current surface."),
        )],

        ActionKey::EqualizeSplits => vec![cmd(
            Action::EqualizeSplits,
            n_(c"Equalize Splits"),
            n_(c"Equalize the size of all splits."),
        )],

        ActionKey::ResetWindowSize => vec![cmd(
            Action::ResetWindowSize,
            n_(c"Reset Window Size"),
            n_(c"Reset the window size to the default."),
        )],

        ActionKey::Inspector => vec![cmd(
            Action::Inspector(InspectorMode::Toggle),
            n_(c"Toggle Inspector"),
            n_(c"Toggle the inspector."),
        )],

        ActionKey::ShowGtkInspector => vec![cmd(
            Action::ShowGtkInspector,
            n_(c"Show the GTK Inspector"),
            n_(c"Show the GTK inspector."),
        )],

        ActionKey::ShowOnScreenKeyboard => vec![cmd(
            Action::ShowOnScreenKeyboard,
            n_(c"Show On-Screen Keyboard"),
            n_(c"Show the on-screen keyboard if present."),
        )],

        ActionKey::OpenConfig => vec![
            cmd(
                Action::OpenConfig(OpenConfig::OsOpen),
                n_(c"Open Config Using OS editor"),
                n_(c"Open the config file with the OS's default editor."),
            ),
            cmd(
                Action::OpenConfig(OpenConfig::NewWindow),
                n_(c"Open Config in New Terminal Window"),
                n_(c"Open the config file in a new window using $EDITOR or $VISUAL."),
            ),
        ],

        ActionKey::ReloadConfig => vec![cmd(
            Action::ReloadConfig,
            n_(c"Reload Config"),
            n_(c"Reload the config file."),
        )],

        ActionKey::CloseSurface => vec![cmd(
            Action::CloseSurface,
            n_(c"Close Terminal"),
            n_(c"Close the current terminal."),
        )],

        ActionKey::CloseTab => vec![
            cmd(
                Action::CloseTab(CloseTabMode::This),
                n_(c"Close Tab"),
                n_(c"Close the current tab."),
            ),
            cmd(
                Action::CloseTab(CloseTabMode::Other),
                n_(c"Close Other Tabs"),
                n_(c"Close all tabs in this window except the current one."),
            ),
            cmd(
                Action::CloseTab(CloseTabMode::Right),
                n_(c"Close Tabs to the Right"),
                n_(c"Close all tabs to the right of the current one."),
            ),
        ],

        ActionKey::CloseWindow => vec![cmd(
            Action::CloseWindow,
            n_(c"Close Window"),
            n_(c"Close the current window."),
        )],

        ActionKey::CloseAllWindows => vec![cmd(
            Action::CloseAllWindows,
            n_(c"Close All Windows"),
            n_(c"Close all windows."),
        )],

        ActionKey::ToggleMaximize => vec![cmd(
            Action::ToggleMaximize,
            n_(c"Toggle Maximize"),
            n_(c"Toggle the maximized state of the current window."),
        )],

        ActionKey::ToggleFullscreen => vec![cmd(
            Action::ToggleFullscreen,
            n_(c"Toggle Fullscreen"),
            n_(c"Toggle the fullscreen state of the current window."),
        )],

        ActionKey::ToggleWindowDecorations => vec![cmd(
            Action::ToggleWindowDecorations,
            n_(c"Toggle Window Decorations"),
            n_(c"Toggle the window decorations."),
        )],

        ActionKey::ToggleWindowFloatOnTop => vec![cmd(
            Action::ToggleWindowFloatOnTop,
            n_(c"Toggle Float on Top"),
            n_(c"Toggle the float on top state of the current window."),
        )],

        ActionKey::ToggleSecureInput => vec![cmd(
            Action::ToggleSecureInput,
            n_(c"Toggle Secure Input"),
            n_(c"Toggle secure input mode."),
        )],

        ActionKey::ToggleMouseReporting => vec![cmd(
            Action::ToggleMouseReporting,
            n_(c"Toggle Mouse Reporting"),
            n_(c"Toggle whether mouse events are reported to terminal applications."),
        )],

        ActionKey::ToggleBackgroundOpacity => vec![cmd(
            Action::ToggleBackgroundOpacity,
            n_(c"Toggle Background Opacity"),
            n_(c"Toggle the background opacity of a window that started transparent."),
        )],

        ActionKey::CheckForUpdates => vec![cmd(
            Action::CheckForUpdates,
            n_(c"Check for Updates"),
            n_(c"Check for updates to the application."),
        )],

        ActionKey::Undo => vec![cmd(
            Action::Undo,
            n_(c"Undo"),
            n_(c"Undo the last action."),
        )],

        ActionKey::Redo => vec![cmd(
            Action::Redo,
            n_(c"Redo"),
            n_(c"Redo the last undone action."),
        )],

        ActionKey::Quit => vec![cmd(
            Action::Quit,
            n_(c"Quit"),
            n_(c"Quit the application."),
        )],

        ActionKey::Text => vec![cmd(
            Action::Text("👻".as_bytes().to_vec()),
            c"Ghostty",
            n_(c"Put a little Ghostty in your terminal."),
        )],

        // No commands because they're parameterized and there
        // aren't obvious values users would use. It is possible that
        // these may have commands in the future if there are very
        // common values that users tend to use.
        ActionKey::Csi
        | ActionKey::Esc
        | ActionKey::CursorKey
        | ActionKey::SetFontSize
        | ActionKey::SetSurfaceTitle
        | ActionKey::SetTabTitle
        | ActionKey::SetWindowTitle
        | ActionKey::Search
        | ActionKey::ScrollToRow
        | ActionKey::ScrollPageFractional
        | ActionKey::ScrollPageLines
        | ActionKey::AdjustSelection
        | ActionKey::JumpToPrompt
        | ActionKey::WriteScrollbackFile
        | ActionKey::GotoTab
        | ActionKey::ResizeSplit
        | ActionKey::ActivateKeyTable
        | ActionKey::ActivateKeyTableOnce
        | ActionKey::DeactivateKeyTable
        | ActionKey::DeactivateAllKeyTables
        | ActionKey::EndKeySequence
        | ActionKey::Crash => vec![],

        // No commands because I'm not sure they make sense in a command
        // palette context.
        ActionKey::ToggleCommandPalette
        | ActionKey::ToggleQuickTerminal
        | ActionKey::ToggleVisibility
        | ActionKey::PreviousTab
        | ActionKey::NextTab
        | ActionKey::LastTab => vec![],

        // No commands for obvious reasons
        ActionKey::Ignore | ActionKey::Unbind => vec![],
    };

    // All generated commands should have the same action as the
    // action passed in.
    for cmd in &result {
        debug_assert_eq!(cmd.action.key(), action);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_defaults() {
        // This just ensures that defaults is analyzed and works.
        assert!(!defaults().is_empty());
        assert_eq!(defaults().len(), defaults_c().len());
    }

    // Port-specific checks of the lazily built tables.

    #[test]
    fn command_defaults_sorted_and_c_strings() {
        for pair in defaults().windows(2) {
            assert!(!Command::less_than(&pair[1], &pair[0]));
        }

        for (cmd, c) in defaults().iter().zip(defaults_c()) {
            // SAFETY: defaults_c points to valid NUL-terminated 'static strings.
            unsafe {
                assert_eq!(
                    cmd.action.key().as_str().as_bytes(),
                    CStr::from_ptr(c.action_key).to_bytes()
                );
                assert_eq!(cmd.action.to_string().as_bytes(), CStr::from_ptr(c.action).to_bytes());
                assert_eq!(cmd.title.to_bytes(), CStr::from_ptr(c.title).to_bytes());
            }
        }

        let mut arena = Vec::new();
        let first = &defaults()[0];
        let c = first.cval(&mut arena);
        // SAFETY: `arena` and `first` outlive `c`.
        unsafe {
            assert_eq!(first.action.to_string().as_bytes(), CStr::from_ptr(c.action).to_bytes());
        }
        assert!(first.equal(&first.translated()));
    }
}
