//! Port of `src/renderer/cursor.zig`.
//!
//! STATUS: ported

use crate::terminal::cursor as terminal_cursor;
use crate::terminal::render::RenderState;

/// Available cursor styles for drawing that renderers must support.
/// This is a superset of terminal cursor styles since the renderer supports
/// some additional cursor states such as the hollow block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    // Typical cursor input styles
    Block,
    BlockHollow,
    Bar,
    Underline,

    // Special cursor styles
    Lock,
}

impl Style {
    /// Create a cursor style from the terminal style request.
    pub fn from_terminal(term: terminal_cursor::Style) -> Style {
        match term {
            terminal_cursor::Style::Bar => Style::Bar,
            terminal_cursor::Style::Block => Style::Block,
            terminal_cursor::Style::BlockHollow => Style::BlockHollow,
            terminal_cursor::Style::Underline => Style::Underline,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleOptions {
    pub preedit: bool,
    pub focused: bool,
    pub blink_visible: bool,
}

/// Returns the cursor style to use for the current render state or null
/// if a cursor should not be rendered at all.
pub fn style(state: &RenderState, opts: StyleOptions) -> Option<Style> {
    // Note the order of conditionals below is important. It represents
    // a priority system of how we determine what state overrides cursor
    // visibility and style.

    // The cursor must be visible in the viewport to be rendered.
    state.cursor.viewport?;

    // If we are in preedit, then we always show the block cursor. We do
    // this even if the cursor is explicitly not visible because it shows
    // an important editing state to the user.
    if opts.preedit {
        return Some(Style::Block);
    }

    // If we're at a password input its always a lock.
    if state.cursor.password_input {
        return Some(Style::Lock);
    }

    // If the cursor is explicitly not visible by terminal mode, we don't render.
    if !state.cursor.visible {
        return None;
    }

    // If we're not focused, our cursor is always visible so that
    // we can show the hollow box.
    if !opts.focused {
        return Some(Style::BlockHollow);
    }

    // If the cursor is blinking and our blink state is not visible,
    // then we don't show the cursor.
    if state.cursor.blinking && !opts.blink_visible {
        return None;
    }

    // Otherwise, we use whatever style the terminal wants.
    Some(Style::from_terminal(state.cursor.visual_style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::modes::Mode;
    use crate::terminal::terminal::{Options, ScrollViewport, Terminal};

    fn opts(preedit: bool, focused: bool, blink_visible: bool) -> StyleOptions {
        StyleOptions {
            preedit,
            focused,
            blink_visible,
        }
    }

    /// Zig: `.{ .focused = .., .blink_visible = .. }` (preedit defaults false).
    fn o(focused: bool, blink_visible: bool) -> StyleOptions {
        StyleOptions {
            focused,
            blink_visible,
            ..Default::default()
        }
    }

    #[test]
    fn cursor_default_uses_configured_style() {
        let mut term = Terminal::init(Options::new(10, 10)).unwrap();

        term.screens.active.cursor.cursor_style = terminal_cursor::Style::Bar;
        term.modes.set(Mode::CursorBlinking, true);

        let mut state = RenderState::empty();
        state.update(&mut term);

        assert!(style(&state, opts(false, true, true)) == Some(Style::Bar));
        assert!(style(&state, opts(false, false, true)) == Some(Style::BlockHollow));
        assert!(style(&state, opts(false, false, false)) == Some(Style::BlockHollow));
        assert!(style(&state, opts(false, true, false)).is_none());
    }

    #[test]
    fn cursor_blinking_disabled() {
        let mut term = Terminal::init(Options::new(10, 10)).unwrap();

        term.screens.active.cursor.cursor_style = terminal_cursor::Style::Bar;
        term.modes.set(Mode::CursorBlinking, false);

        let mut state = RenderState::empty();
        state.update(&mut term);

        assert!(style(&state, o(true, true)) == Some(Style::Bar));
        assert!(style(&state, o(true, false)) == Some(Style::Bar));
        assert!(style(&state, o(false, true)) == Some(Style::BlockHollow));
        assert!(style(&state, o(false, false)) == Some(Style::BlockHollow));
    }

    #[test]
    fn cursor_explicitly_not_visible() {
        let mut term = Terminal::init(Options::new(10, 10)).unwrap();

        term.screens.active.cursor.cursor_style = terminal_cursor::Style::Bar;
        term.modes.set(Mode::CursorVisible, false);
        term.modes.set(Mode::CursorBlinking, false);

        let mut state = RenderState::empty();
        state.update(&mut term);

        assert!(style(&state, o(true, true)).is_none());
        assert!(style(&state, o(true, false)).is_none());
        assert!(style(&state, o(false, true)).is_none());
        assert!(style(&state, o(false, false)).is_none());
    }

    #[test]
    fn cursor_always_block_with_preedit() {
        let mut term = Terminal::init(Options::new(10, 10)).unwrap();

        let mut state = RenderState::empty();
        state.update(&mut term);

        // In any bool state
        assert!(style(&state, opts(true, false, false)) == Some(Style::Block));
        assert!(style(&state, opts(true, true, false)) == Some(Style::Block));
        assert!(style(&state, opts(true, true, true)) == Some(Style::Block));
        assert!(style(&state, opts(true, false, true)) == Some(Style::Block));

        // If we're scrolled though, then we don't show the cursor.
        for _ in 0..100 {
            term.index().unwrap();
        }
        term.scroll_viewport(ScrollViewport::Top);
        state.update(&mut term);

        // In any bool state
        assert!(style(&state, opts(true, false, false)).is_none());
        assert!(style(&state, opts(true, true, false)).is_none());
        assert!(style(&state, opts(true, true, true)).is_none());
        assert!(style(&state, opts(true, false, true)).is_none());
    }
}
