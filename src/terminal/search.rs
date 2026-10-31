//! Port of `src/terminal/search.zig`.
//!
//! STATUS: ported
//!
//! Search functionality for the terminal.
//!
//! Notes on the port:
//! - `options` is [`crate::terminal::build_options`].
//! - The Zig file re-exports each searcher under a short name
//!   (`search.Screen`, `search.Viewport`, ...). Rust re-exports them under
//!   their full names ([`ScreenSearch`], [`ViewportSearch`], ...) plus the
//!   short aliases so both spellings resolve.
//! - `Thread` is only available for the Ghostty application artifact in
//!   Zig (libghostty lacks the xev dependency). This crate always builds the
//!   application, so the module is always available.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "search/active.rs"]
pub mod active;
#[path = "search/pagelist.rs"]
pub mod pagelist;
#[path = "search/screen.rs"]
pub mod screen;
#[path = "search/sliding_window.rs"]
pub mod sliding_window;
#[path = "search/terminal.rs"]
pub mod terminal;
#[path = "search/thread.rs"]
pub mod thread;
#[path = "search/viewport.rs"]
pub mod viewport;
// @generated-mods-end

pub use crate::terminal::build_options as options;

pub use self::active::ActiveSearch;
pub use self::pagelist::PageListSearch;
pub use self::screen::ScreenSearch;
pub use self::terminal::TerminalSearch;
pub use self::thread::Thread;
pub use self::viewport::ViewportSearch;

/// Zig: `search.Active`.
pub type Active = ActiveSearch;
/// Zig: `search.PageList`.
pub type PageList = PageListSearch;
/// Zig: `search.Screen`.
pub type Screen = ScreenSearch;
/// Zig: `search.Terminal`.
pub type Terminal = TerminalSearch;
/// Zig: `search.Viewport`.
pub type Viewport = ViewportSearch;

/// Test helpers shared by the search submodules.
#[cfg(test)]
pub(crate) mod testing {
    use crate::terminal::point::{Coordinate, Point};
    use crate::terminal::terminal::{Options, Terminal};

    /// Create a terminal with the given size and scrollback limit.
    pub fn terminal(cols: u16, rows: u16, max_scrollback_bytes: Option<usize>) -> Terminal {
        Terminal::init(Options {
            cols,
            rows,
            max_scrollback_bytes,
            ..Default::default()
        })
        .unwrap()
    }

    /// Create a terminal with the default scrollback limit.
    pub fn terminal_default(cols: u16, rows: u16) -> Terminal {
        Terminal::init(Options {
            cols,
            rows,
            ..Default::default()
        })
        .unwrap()
    }

    pub fn active(x: u16, y: u32) -> Option<Point> {
        Some(Point::Active(Coordinate { x, y }))
    }

    pub fn screen(x: u16, y: u32) -> Option<Point> {
        Some(Point::Screen(Coordinate { x, y }))
    }

    /// Zig `t.vtStream().nextSlice(..)` in one call.
    pub fn next_slice(t: &mut Terminal, input: &[u8]) {
        let mut s = t.vt_stream();
        s.next_slice(input);
        s.deinit();
    }
}
