//! Port of `src/terminal/lib.zig`.
//!
//! STATUS: ported
//!
//! Helpers for the terminal package's C ABI (`libghostty-vt`).
//!
//! In Zig, `lib.Enum(lib.target, &.{...})` creates an enum whose backing
//! type is `c_int` when building the C ABI and the smallest unsigned integer
//! otherwise. The Rust port is always the Ghostty application build
//! (`build_options::C_ABI == false`), so such enums are written as plain Rust
//! enums with explicit `#[repr(u8)]` discriminants equal to the key index.
//!
//! Rust notes: Zig's comptime type generators `lib.Struct` and
//! `lib.TaggedUnion` are the `#[macro_export]` macros [`crate::lib_struct!`]
//! and [`crate::lib_tagged_union!`] (re-exported here as [`Struct`] and
//! [`TaggedUnion`]); `std.lang.CallingConvention` has no Rust value
//! counterpart, so [`CallingConvention`] is a local enum.

use super::build_options;
use crate::lib;

/// The target for the terminal lib in particular.
pub const TARGET: lib::Target = if build_options::C_ABI {
    lib::Target::C
} else {
    lib::Target::Zig
};

/// A calling convention (`std.lang.CallingConvention`), restricted to what
/// the terminal package uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallingConvention {
    /// The C calling convention (`extern "C"` in Rust).
    C,
}

/// The calling convention to use for C APIs.
///
/// This is always .c for now. I want to make this "Zig" when we're not
/// building the C ABI but there are bigger issues we need to resolve to
/// make that possible (change it and see for yourself).
///
/// In Rust, functions exported with this convention are declared
/// `extern "C" fn`.
pub const CALLING_CONV: CallingConvention = CallingConvention::C;

// Forwarded decls from lib that are used.
pub use crate::lib::allocator as alloc;
pub use crate::lib::check_ghostty_h_enum;
pub use crate::lib::parse_int;
pub use crate::lib::struct_sized_field_fits;
pub use crate::lib::Buffer;
pub use crate::lib::Enum;
pub use crate::lib::String;
pub use crate::lib::Struct;
pub use crate::lib::TaggedUnion;
pub use crate::lib::TinyIo;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_zig_for_app_build() {
        assert_eq!(TARGET, lib::Target::Zig);
        assert_eq!(CALLING_CONV, CallingConvention::C);
    }
}
