//! Port of `pkg/freetype/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
#[path = "computations.rs"]
pub mod computations;
#[path = "errors.rs"]
pub mod errors;
#[path = "face.rs"]
pub mod face;
#[path = "library.rs"]
pub mod library;
#[path = "tag.rs"]
pub mod tag;
#[path = "test.rs"]
pub mod test;
// @generated-mods-end

/// Zig: `pub const testing = @import("test.zig");`
pub use test as testing;

pub use computations::mul_fix;
pub use errors::Error;
pub use face::{Face, LoadFlags};
pub use library::Library;
pub use tag::Tag;
