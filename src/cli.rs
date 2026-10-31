//! Port of `src/cli.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "cli/action.rs"]
pub mod action;
#[path = "cli/args.rs"]
pub mod args;
#[path = "cli/boo.rs"]
pub mod boo;
#[path = "cli/comma_splitter.rs"]
pub mod comma_splitter;
#[path = "cli/crash_report.rs"]
pub mod crash_report;
#[path = "cli/diagnostics.rs"]
pub mod diagnostics;
#[path = "cli/edit_config.rs"]
pub mod edit_config;
#[path = "cli/explain_config.rs"]
pub mod explain_config;
#[path = "cli/ghostty.rs"]
pub mod ghostty;
#[path = "cli/help.rs"]
pub mod help;
#[path = "cli/list_actions.rs"]
pub mod list_actions;
#[path = "cli/list_colors.rs"]
pub mod list_colors;
#[path = "cli/list_fonts.rs"]
pub mod list_fonts;
#[path = "cli/list_keybinds.rs"]
pub mod list_keybinds;
#[path = "cli/list_themes.rs"]
pub mod list_themes;
#[path = "cli/new_tab.rs"]
pub mod new_tab;
#[path = "cli/new_window.rs"]
pub mod new_window;
#[path = "cli/pager.rs"]
pub mod pager;
#[path = "cli/show_config.rs"]
pub mod show_config;
#[path = "cli/show_face.rs"]
pub mod show_face;
#[path = "cli/ssh.rs"]
pub mod ssh;
#[path = "cli/ssh_cache.rs"]
pub mod ssh_cache;
#[path = "cli/toggle_quick_terminal.rs"]
pub mod toggle_quick_terminal;
#[path = "cli/tui.rs"]
pub mod tui;
#[path = "cli/validate_config.rs"]
pub mod validate_config;
#[path = "cli/version.rs"]
pub mod version;
// @generated-mods-end

pub(crate) use self::args::compatibility_renamed;
pub use self::args::CompatibilityHandler;
pub use self::diagnostics::{Diagnostic, DiagnosticList, Location};
