//! Port of `src/lib/main.zig`.
//!
//! STATUS: ported
//!
//! Shared library helpers. The Zig comptime type generators map to:
//!
//! | Zig | Rust |
//! | --- | --- |
//! | `lib.Enum(target, keys)` | [`crate::lib_enum!`] + [`Enum`] trait |
//! | `lib.Struct(target, T)` | [`crate::lib_struct!`] |
//! | `lib.structSizedFieldFits` | [`crate::struct_sized_field_fits!`] |
//! | `lib.TaggedUnion(target, U, opts)` | [`crate::lib_tagged_union!`] + [`CRepr`] |
//! | `lib.Packed` / `lib.PackedTaggedUnion` | [`Packed`] / [`PackedTaggedUnion`] const metadata |
//! | `lib.checkGhosttyHEnum` | [`check_ghostty_h_enum`] |
//!
//! See each module's documentation for how later ports should use them.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "allocator.rs"]
pub mod allocator;
#[path = "c_abi.rs"]
pub mod c_abi;
#[path = "compat/mod.rs"]
pub mod compat;
#[path = "enum.rs"]
pub mod r#enum;
#[path = "packed.rs"]
pub mod packed;
#[path = "parse_int.rs"]
pub mod parse_int;
#[path = "struct.rs"]
pub mod r#struct;
#[path = "target.rs"]
pub mod target;
#[path = "tinyio.rs"]
pub mod tinyio;
#[path = "types.rs"]
pub mod types;
#[path = "union.rs"]
pub mod union;
#[path = "windows_dll.rs"]
pub mod windows_dll;
// @generated-mods-end

pub use self::packed::{PackedLayout as Packed, PackedTaggedUnion};
pub use self::parse_int::parse_int;
pub use self::r#enum::{check_ghostty_h_enum, LibEnum as Enum};
pub use self::r#struct::sized_field_fits_raw as struct_sized_field_fits;
pub use self::target::Target;
pub use self::tinyio::TinyIo;
pub use self::types::{Buffer, String};
pub use self::union::CRepr;
/// `lib.Struct` (a `#[macro_export]` macro, see [`r#struct`]).
pub use crate::lib_struct as Struct;
/// `lib.TaggedUnion` (a `#[macro_export]` macro, see [`union`]).
pub use crate::lib_tagged_union as TaggedUnion;
