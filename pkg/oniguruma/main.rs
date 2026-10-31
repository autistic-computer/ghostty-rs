//! Port of `pkg/oniguruma/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
#[path = "errors.rs"]
pub mod errors;
#[path = "init.rs"]
pub mod init;
#[path = "match_param.rs"]
pub mod match_param;
#[path = "regex.rs"]
pub mod regex;
#[path = "region.rs"]
pub mod region;
#[path = "testing.rs"]
pub mod testing;
#[path = "types.rs"]
pub mod types;
// @generated-mods-end

pub use init::{deinit, init};
pub use match_param::MatchParam;
pub use regex::Regex;
pub use region::Region;
pub use types::{Encoding, Syntax};
