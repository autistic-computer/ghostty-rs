//! Port of `src/benchmark/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "apc_parser.rs"]
pub mod apc_parser;
#[path = "benchmark.rs"]
pub mod benchmark;
#[path = "c_api.rs"]
pub mod c_api;
#[path = "cli.rs"]
pub mod cli;
#[path = "codepoint_width.rs"]
pub mod codepoint_width;
#[path = "grapheme_break.rs"]
pub mod grapheme_break;
#[path = "hyperlink_map.rs"]
pub mod hyperlink_map;
#[path = "is_symbol.rs"]
pub mod is_symbol;
#[path = "options.rs"]
pub mod options;
#[path = "osc_parser.rs"]
pub mod osc_parser;
#[path = "page_compression.rs"]
pub mod page_compression;
#[path = "screen_clone.rs"]
pub mod screen_clone;
#[path = "scrollback_compression.rs"]
pub mod scrollback_compression;
#[path = "terminal_formatter.rs"]
pub mod terminal_formatter;
#[path = "terminal_parser.rs"]
pub mod terminal_parser;
#[path = "terminal_resize.rs"]
pub mod terminal_resize;
#[path = "terminal_snapshot.rs"]
pub mod terminal_snapshot;
#[path = "terminal_stream.rs"]
pub mod terminal_stream;
// @generated-mods-end

pub use self::benchmark::Benchmark;
pub use self::codepoint_width::CodepointWidth;
pub use self::grapheme_break::GraphemeBreak;
pub use self::hyperlink_map::HyperlinkMap;
pub use self::is_symbol::IsSymbol;
pub use self::page_compression::PageCompression;
pub use self::screen_clone::ScreenClone;
pub use self::scrollback_compression::ScrollbackCompression;
pub use self::terminal_formatter::TerminalFormatter;
pub use self::terminal_parser::TerminalParser;
pub use self::terminal_resize::TerminalResize;
pub use self::terminal_snapshot::TerminalSnapshot;
pub use self::terminal_stream::TerminalStream;
