//! Port of `src/terminal/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "ansi.rs"]
pub mod ansi;
#[path = "apc.rs"]
pub mod apc;
#[path = "bitmap_allocator.rs"]
pub mod bitmap_allocator;
#[path = "build_options.rs"]
pub mod build_options;
#[path = "c/main.rs"]
pub mod c;
#[path = "charsets.rs"]
pub mod charsets;
#[path = "clipboard.rs"]
pub mod clipboard;
#[path = "color.rs"]
pub mod color;
#[path = "compress.rs"]
pub mod compress;
#[path = "csi.rs"]
pub mod csi;
#[path = "cursor.rs"]
pub mod cursor;
#[path = "dcs.rs"]
pub mod dcs;
#[path = "device_attributes.rs"]
pub mod device_attributes;
#[path = "device_status.rs"]
pub mod device_status;
#[path = "focus.rs"]
pub mod focus;
#[path = "formatter.rs"]
pub mod formatter;
#[path = "fraction.rs"]
pub mod fraction;
#[path = "hash_map.rs"]
pub mod hash_map;
#[path = "highlight.rs"]
pub mod highlight;
#[path = "hyperlink.rs"]
pub mod hyperlink;
#[path = "kitty.rs"]
pub mod kitty;
#[path = "lib.rs"]
pub mod lib;
#[path = "mem.rs"]
pub mod mem;
#[path = "modes.rs"]
pub mod modes;
#[path = "mouse.rs"]
pub mod mouse;
#[path = "osc.rs"]
pub mod osc;
#[path = "page.rs"]
pub mod page;
#[path = "page_list.rs"]
pub mod page_list;
#[path = "parse_table.rs"]
pub mod parse_table;
#[path = "parser.rs"]
pub mod parser;
#[path = "paste.rs"]
pub mod paste;
#[path = "point.rs"]
pub mod point;
#[path = "ref_counted_set.rs"]
pub mod ref_counted_set;
#[path = "render.rs"]
pub mod render;
#[path = "screen.rs"]
pub mod screen;
#[path = "screen_set.rs"]
pub mod screen_set;
#[path = "search.rs"]
pub mod search;
#[path = "selection.rs"]
pub mod selection;
#[path = "selection_codepoints.rs"]
pub mod selection_codepoints;
#[path = "selection_gesture.rs"]
pub mod selection_gesture;
#[path = "sgr.rs"]
pub mod sgr;
#[path = "size.rs"]
pub mod size;
#[path = "size_report.rs"]
pub mod size_report;
#[path = "snapshot/main.rs"]
pub mod snapshot;
#[path = "stream.rs"]
pub mod stream;
#[path = "stream_continuation.rs"]
pub mod stream_continuation;
#[path = "stream_terminal.rs"]
pub mod stream_terminal;
#[path = "string_map.rs"]
pub mod string_map;
#[path = "style.rs"]
pub mod style;
#[path = "sys.rs"]
pub mod sys;
#[path = "tabstops.rs"]
pub mod tabstops;
#[path = "terminal.rs"]
pub mod terminal;
#[path = "tmux.rs"]
pub mod tmux;
#[path = "utf8_decoder.rs"]
pub mod utf8_decoder;
#[path = "x11_color.rs"]
pub mod x11_color;
#[path = "xt_checksum.rs"]
pub mod xt_checksum;
// @generated-mods-end

// Re-exports (Zig: the `pub const` declarations of `terminal/main.zig`).
// Modules (`apc`, `dcs`, `osc`, `point`, `color`, ...) are the generated
// `pub mod`s above.

pub use self::charsets::table as charset_table;
pub use self::charsets::ActiveSlot as CharsetActiveSlot;
pub use self::charsets::Charset;
pub use self::charsets::Slots as CharsetSlot;
pub use self::mouse::Event as MouseEvent;
pub use self::mouse::Format as MouseFormat;
pub use self::mouse::Shape as MouseShape;
pub use self::page::Cell;
pub use self::page::Page;
pub use self::page_list::PageList;
pub use self::page_list::Pin;
pub use self::page_list::Scrollbar;
pub use self::render::RenderState;
pub use self::parser::Csi as CSI;
pub use self::parser::Dcs as DCS;
pub use self::parser::Parser;
pub use self::point::Coordinate;
pub use self::point::Point;
pub use self::screen::Screen;
pub use self::screen_set::ScreenSet;
pub use self::selection::Selection;
pub use self::selection_gesture::SelectionGesture;
pub use self::string_map::StringMap;
pub use self::style::Style;
pub use self::terminal::Terminal;
pub use self::stream_terminal::Stream as TerminalStream;
pub use self::stream::Stream;
pub use self::stream::Action as StreamAction;
pub use self::stream::Boundary as StreamBoundary;
pub use self::stream_terminal::SemanticPrompt;
pub use self::stream_terminal::UnknownSequence;
pub use self::paste::Request as Paste;
pub use self::paste::Contents as PasteContents;
pub use self::paste::Source as PasteSource;
pub use self::clipboard::MimeReader;
pub use self::stream_terminal::PasteError;
pub use self::screen::Cursor;
pub use self::cursor::Style as CursorStyle;
pub use self::ansi::CursorStyle as CursorStyleReq;
pub use self::device_attributes::Req as DeviceAttributeReq;
pub use self::modes::Mode;
pub use self::modes::ModePacked;
pub use self::ansi::ModifyKeyFormat;
pub use self::ansi::ProtectedMode;
pub use self::ansi::StatusLineType;
pub use self::ansi::StatusDisplay;
pub use self::csi::EraseDisplay;
pub use self::csi::EraseLine;
pub use self::csi::SizeReportStyle;
pub use self::csi::TabClear;
pub use self::sgr::Attribute;

pub use self::build_options::Options;
/// (Zig: `options`, the `terminal_options` build module.)
pub use self::build_options as options;

// (Zig: `c_api` is the `c` module.)

/// Whether this target supports terminal page compression.
pub const COMPRESSION_ENABLED: bool = self::mem::can_reclaim(self::mem::DecommitMode::Strict);
