//! Port of `src/lib/target.zig`.
//!
//! STATUS: ported

/// The target for ABI generation. The detection of this is left to the
/// caller since there are multiple ways to do that.
///
/// In the Rust port the `C` target means "also generate the `#[repr(C)]`
/// companion types / `c_int` backing" in the `lib` helper macros, while
/// `Zig` means "only the native Rust representation" (see
/// [`crate::lib::r#enum`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    C,
    Zig,
}
