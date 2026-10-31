//! Port of `pkg/fontconfig/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
#[path = "char_set.rs"]
pub mod char_set;
#[path = "common.rs"]
pub mod common;
#[path = "config.rs"]
pub mod config;
#[path = "error.rs"]
pub mod error;
#[path = "font_set.rs"]
pub mod font_set;
#[path = "init.rs"]
pub mod init;
#[path = "lang_set.rs"]
pub mod lang_set;
#[path = "matrix.rs"]
pub mod matrix;
#[path = "object_set.rs"]
pub mod object_set;
#[path = "pattern.rs"]
pub mod pattern;
#[path = "range.rs"]
pub mod range;
#[path = "test.rs"]
pub mod test;
#[path = "value.rs"]
pub mod value;
// @generated-mods-end

pub use char_set::CharSet;
pub use common::{MatchKind, Property, Result, Slant, Spacing, Weight};
pub use config::Config;
pub use error::Error;
pub use font_set::FontSet;
pub use init::{fini, init, init_load_config, init_load_config_and_fonts, version};
pub use lang_set::LangSet;
pub use matrix::Matrix;
pub use object_set::ObjectSet;
pub use pattern::Pattern;
pub use range::Range;
pub use value::{Type, Value, ValueBinding};
