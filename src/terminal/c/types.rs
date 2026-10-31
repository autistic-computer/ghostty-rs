//! Port of `src/terminal/c/types.zig`.
//!
//! STATUS: ported
//!
//! Comptime-generated ABI metadata for the public libghostty-vt C types.
//!
//! The manifest is embedded in the library and returned by
//! `ghostty_type_json`. It is intended for FFI consumers that cannot use the
//! C headers directly, most notably WebAssembly hosts. Its format is defined
//! by `types.schema.json`.
//!
//! Notes on the port:
//! - Zig reflects every type with `@typeInfo`. Rust has no reflection, so
//!   each [`TypeDecl`] carries a description built from `offset_of!`,
//!   `size_of`/`align_of` and an explicit public type for each field
//!   (the explicit field types subsume Zig's `writeFieldType` overrides).
//!   Enum keys come from the Zig tag names ([`CApiEnum::zig_fields`] or the
//!   core enum's `as_str`) and go through the same
//!   [`Json::enum_object_field`] renames as Zig.
//! - The JSON is built once with `serde_json` (insertion order preserved)
//!   into a NUL-terminated static string.
//! - This module also hosts the small helpers the C API files share
//!   (Rust-only, no Zig counterpart): [`c_api_enum!`](crate::c_api_enum),
//!   [`HandleAllocator`] and [`encode_to_buffer`].

use std::ffi::{c_char, c_int, c_void};
use std::io;
use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use super::result::Result;
use crate::lib::c_abi;
use crate::lib::packed::{Encoding, PackedLayout};
use crate::lib::r#enum::LibEnum;
use crate::terminal::build_options;

// ---------------------------------------------------------------------------
// Rust port helpers shared by the C API files.

/// Declare a C ABI enum (Zig: `enum(c_int) { ... }` in the C API files).
///
/// Produces a `#[repr(C)]` enum with explicit values plus:
/// * `from_int(c_int) -> Option<Self>` (Zig: `std.enums.fromInt`), used to
///   validate values received from C before they are trusted;
/// * [`LibEnum`] (so [`crate::lib::check_ghostty_h_enum`] works) and
///   [`crate::lib::CRepr`];
/// * [`CApiEnum`] with the Zig tag names. The Zig name is derived from the
///   Rust variant name (`CamelCase` → `snake_case`) unless given explicitly
///   with `=> "zig_name"`.
///
/// Extern functions take enum arguments as `c_int` (or read them through a
/// `c_int` pointer) and convert with `from_int`: an out-of-range value in a
/// Rust enum would be undefined behavior, while Zig checks it at runtime.
#[macro_export]
macro_rules! c_api_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident = $value:expr $(=> $zig:literal)?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(C)]
        $vis enum $name {
            $($(#[$vmeta])* $variant = $value,)*
        }

        impl $name {
            /// Convert from the C integer value, or `None` if the value is
            /// not a valid key (Zig: `std.enums.fromInt`).
            #[allow(unreachable_patterns)]
            pub const fn from_int(v: ::std::ffi::c_int) -> ::core::option::Option<Self> {
                $(if v == ($value) {
                    return ::core::option::Option::Some($name::$variant);
                })*
                ::core::option::Option::None
            }
        }

        impl $crate::lib::r#enum::LibEnum for $name {
            type Backing = ::std::ffi::c_int;
            const TARGET: $crate::lib::target::Target = $crate::lib::target::Target::C;
            const KEYS_LEN: usize = [$(stringify!($variant)),*].len();
            const BACKING_BITS: u32 = ::std::ffi::c_int::BITS;
            const FIELDS: &'static [(&'static str, Self)] =
                &[$((stringify!($variant), $name::$variant),)*];

            fn backing(self) -> ::std::ffi::c_int {
                self as ::std::ffi::c_int
            }

            fn backing_i64(self) -> i64 {
                self as ::std::ffi::c_int as i64
            }

            fn from_backing(v: ::std::ffi::c_int) -> ::core::option::Option<Self> {
                Self::from_int(v)
            }
        }

        impl $crate::lib::union::CRepr for $name {
            type C = $name;
            fn cval(&self) -> $name {
                *self
            }
        }

        impl $crate::terminal::c::types::CApiEnum for $name {
            fn zig_fields() -> ::std::vec::Vec<(::std::string::String, i64)> {
                ::std::vec![$((
                    $crate::c_api_enum!(@zig $variant $($zig)?),
                    $name::$variant as ::std::ffi::c_int as i64,
                ),)*]
            }
        }
    };

    (@zig $variant:ident $zig:literal) => { ::std::string::String::from($zig) };
    (@zig $variant:ident) => { $crate::terminal::c::types::zig_name(stringify!($variant)) };
}

/// Reflection for enums declared with [`c_api_enum!`](crate::c_api_enum):
/// the Zig tag names and values (Zig: `@typeInfo(T).@"enum"`).
pub trait CApiEnum: LibEnum<Backing = c_int> {
    fn zig_fields() -> Vec<(String, i64)>;
}

/// Convert a Rust `CamelCase` variant name to the Zig `snake_case` tag
/// name. Digits are kept attached to the preceding word (`X10` → `x10`);
/// variants whose Zig name separates digits give it explicitly.
pub fn zig_name(rust: &str) -> String {
    let bytes = rust.as_bytes();
    let mut out = String::with_capacity(rust.len() + 4);
    for (i, &c) in bytes.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = bytes[i - 1];
            let next_lower = bytes.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase() as char);
    }
    out
}

/// The allocator retained by a C API handle (Zig: the `std.mem.Allocator`
/// stored in each wrapper struct, obtained with `lib.alloc.default`).
///
/// Like Zig, a caller-provided `GhosttyAllocator` is referenced, not
/// copied, so it must outlive the handle. A null allocator selects the
/// default (libc) allocator.
#[derive(Debug, Clone, Copy)]
pub struct HandleAllocator {
    c: *const crate::lib::allocator::Allocator,
}

impl HandleAllocator {
    /// `lib.alloc.default(alloc_)`.
    pub fn new(c: Option<&crate::lib::allocator::Allocator>) -> HandleAllocator {
        HandleAllocator {
            c: c.map_or(std::ptr::null(), |c| c as *const _),
        }
    }

    fn zig(&self) -> crate::lib::allocator::DefaultAllocator<'_> {
        // SAFETY: the C API contract requires the allocator to outlive
        // every handle created with it.
        crate::lib::allocator::default(unsafe { self.c.as_ref() })
    }

    /// Allocate `len` bytes with the given alignment (bytes). Returns null
    /// on failure.
    pub fn alloc_bytes(&self, len: usize, align: usize) -> *mut u8 {
        use crate::lib::allocator::ZigAllocator;
        self.zig().alloc(len, align.trailing_zeros() as u8, 0)
    }

    /// Free memory returned by [`HandleAllocator::alloc_bytes`].
    pub fn free_bytes(&self, ptr: *mut u8, len: usize, align: usize) {
        use crate::lib::allocator::ZigAllocator;
        self.zig().free(ptr, len, align.trailing_zeros() as u8, 0)
    }

    /// `alloc.create(T)` followed by initialization. Returns null when the
    /// allocation fails (Zig: `error.OutOfMemory`).
    pub fn create<T>(&self, value: T) -> *mut T {
        let ptr = self.alloc_bytes(std::mem::size_of::<T>().max(1), std::mem::align_of::<T>()) as *mut T;
        if !ptr.is_null() {
            // SAFETY: freshly allocated with T's size and alignment.
            unsafe { ptr.write(value) };
        }
        ptr
    }

    /// `alloc.destroy(ptr)`: drop the value and free its memory.
    ///
    /// # Safety
    /// `ptr` must come from [`HandleAllocator::create`] on an allocator
    /// referencing the same C allocator, and must not be used afterwards.
    pub unsafe fn destroy<T>(&self, ptr: *mut T) {
        std::ptr::drop_in_place(ptr);
        self.free_bytes(
            ptr as *mut u8,
            std::mem::size_of::<T>().max(1),
            std::mem::align_of::<T>(),
        );
    }
}

/// A fixed buffer writer (Zig: `std.Io.Writer.fixed`). Writes that do not
/// fit fail.
struct FixedWriter<'a> {
    buf: &'a mut [u8],
    end: usize,
}

impl io::Write for FixedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let avail = self.buf.len() - self.end;
        if avail == 0 && !data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "WriteFailed"));
        }
        let n = avail.min(data.len());
        self.buf[self.end..self.end + n].copy_from_slice(&data[..n]);
        self.end += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A writer that only counts bytes (Zig: `std.Io.Writer.Discarding`).
#[derive(Default)]
struct Discarding {
    count: u64,
}

impl io::Write for Discarding {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.count += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The common encode pattern of the C API: encode into the caller's
/// buffer; if it is too small (or null), encode again into a counting
/// writer, report the required size in `out_written` and return
/// `out_of_space`.
///
/// # Safety
/// `out` must be null or valid for `out_len` writable bytes.
pub unsafe fn encode_to_buffer(
    out: *mut u8,
    out_len: usize,
    out_written: &mut usize,
    mut encode: impl FnMut(&mut dyn io::Write) -> io::Result<()>,
) -> Result {
    let buf: &mut [u8] = if out.is_null() {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out, out_len)
    };
    let mut writer = FixedWriter { buf, end: 0 };
    if encode(&mut writer).is_err() {
        let mut discarding = Discarding::default();
        let _ = encode(&mut discarding);
        *out_written = discarding.count as usize;
        return Result::OutOfSpace;
    }

    *out_written = writer.end;
    Result::Success
}

/// A slice from a nullable C pointer and length (`ptr[0..len]`, or empty
/// for null).
///
/// # Safety
/// A non-null `ptr` must be valid for `len` reads for `'a`.
pub unsafe fn slice_or_empty<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

// ---------------------------------------------------------------------------
// Public value types.

/// C: GhosttySurfacePosition
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SurfacePosition {
    pub x: f64,
    pub y: f64,
}

/// C: GhosttyCodepoints
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Codepoints {
    pub ptr: *const u32,
    pub len: usize,
}

impl Default for Codepoints {
    fn default() -> Self {
        Codepoints {
            ptr: std::ptr::null(),
            len: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest type declarations.

/// The public type of a field (Zig: `writeFieldType`).
#[derive(Debug, Clone, Copy)]
pub enum FieldType {
    /// A builtin (`u8`, `bool`, ...) or public type name.
    Named(&'static str),
    Array {
        elem: &'static str,
        count: usize,
    },
    Pointer {
        elem: &'static str,
        is_const: bool,
        nullable: bool,
    },
}

/// The public name of `usize` (Zig: `intName(.unsigned, @bitSizeOf(usize))`).
pub const USIZE: &str = if cfg!(target_pointer_width = "64") { "u64" } else { "u32" };

/// The public name of `isize`.
pub const ISIZE: &str = if cfg!(target_pointer_width = "64") { "i64" } else { "i32" };

/// Shorthand for [`FieldType::Named`].
pub const fn named(name: &'static str) -> FieldType {
    FieldType::Named(name)
}

/// Shorthand for [`FieldType::Array`].
pub const fn array(elem: &'static str, count: usize) -> FieldType {
    FieldType::Array { elem, count }
}

/// Shorthand for [`FieldType::Pointer`].
pub const fn pointer(elem: &'static str, is_const: bool, nullable: bool) -> FieldType {
    FieldType::Pointer {
        elem,
        is_const,
        nullable,
    }
}

/// One field of a struct or union.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub offset: usize,
    pub size: usize,
    pub ty: FieldType,
}

/// Size of the pointee type of a raw pointer (helper for [`field!`]).
pub const fn size_of_pointee<T>(_: *const T) -> usize {
    std::mem::size_of::<T>()
}

/// Describe a field of a `#[repr(C)]` struct (`field!(T, name, type)`) or
/// union (`field!(union T, name, type)`, offset 0 like Zig).
#[macro_export]
macro_rules! c_api_field {
    ($T:ty, $f:ident, $ty:expr) => {{
        let uninit = ::core::mem::MaybeUninit::<$T>::uninit();
        let ptr = uninit.as_ptr();
        // SAFETY: only the field address is computed.
        let field_ptr = unsafe { ::core::ptr::addr_of!((*ptr).$f) };
        $crate::terminal::c::types::Field {
            name: stringify!($f),
            offset: ::core::mem::offset_of!($T, $f),
            size: $crate::terminal::c::types::size_of_pointee(field_ptr),
            ty: $ty,
        }
    }};
    (union $T:ty, $f:ident, $ty:expr) => {{
        let uninit = ::core::mem::MaybeUninit::<$T>::uninit();
        let ptr = uninit.as_ptr();
        // SAFETY: only the field address is computed.
        let field_ptr = unsafe { ::core::ptr::addr_of!((*ptr).$f) };
        $crate::terminal::c::types::Field {
            name: stringify!($f),
            offset: 0,
            size: $crate::terminal::c::types::size_of_pointee(field_ptr),
            ty: $ty,
        }
    }};
}
use crate::c_api_field as field;

/// Kind of a public type (Zig: `TypeDecl.Kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Struct,
    Union,
    Enum,
    Packed,
    Alias,
    Opaque,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Struct => "struct",
            Kind::Union => "union",
            Kind::Enum => "enum",
            Kind::Packed => "packed",
            Kind::Alias => "alias",
            Kind::Opaque => "opaque",
        }
    }
}

/// Where the arms of a tagged struct come from (Zig:
/// `TypeDecl.TaggedUnion.ArmSource`).
#[derive(Debug, Clone)]
pub enum ArmSource {
    /// Every tag whose name is a non-zero-sized field of the value union.
    Fields,
    /// `cFieldRename(tag)` of a `lib.TaggedUnion`: (Zig tag name, field).
    Generated(Vec<(String, Option<&'static str>)>),
}

/// Tagged-union metadata of a struct (Zig: `TypeDecl.TaggedUnion`).
#[derive(Debug, Clone)]
pub struct TaggedUnion {
    pub tag_field: &'static str,
    pub value_field: &'static str,
    pub arm_source: ArmSource,
}

/// The reflected description of a type.
#[derive(Debug, Clone)]
pub enum TypeInfo {
    Struct {
        size: usize,
        align: usize,
        fields: Vec<Field>,
        tagged_union: Option<TaggedUnion>,
    },
    Union {
        size: usize,
        align: usize,
        fields: Vec<Field>,
    },
    Enum {
        prefix: &'static str,
        sentinel_suffix: &'static str,
        /// (Zig tag name, value).
        values: Vec<(String, i64)>,
    },
    Packed {
        size: usize,
        align: usize,
        underlying: &'static str,
        layout: &'static PackedLayout,
    },
    Alias {
        size: usize,
        align: usize,
        alias_type: &'static str,
    },
    Opaque,
}

/// A public C type (Zig: `TypeDecl`).
#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: &'static str,
    pub info: TypeInfo,
}

impl TypeDecl {
    pub fn kind(&self) -> Kind {
        match self.info {
            TypeInfo::Struct { .. } => Kind::Struct,
            TypeInfo::Union { .. } => Kind::Union,
            TypeInfo::Enum { .. } => Kind::Enum,
            TypeInfo::Packed { .. } => Kind::Packed,
            TypeInfo::Alias { .. } => Kind::Alias,
            TypeInfo::Opaque => Kind::Opaque,
        }
    }

    fn init_struct<T>(name: &'static str, fields: Vec<Field>) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Struct {
                size: std::mem::size_of::<T>(),
                align: std::mem::align_of::<T>(),
                fields,
                tagged_union: None,
            },
        }
    }

    fn init_tagged_struct<T>(
        name: &'static str,
        fields: Vec<Field>,
        tag_field: &'static str,
        value_field: &'static str,
        arm_source: ArmSource,
    ) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Struct {
                size: std::mem::size_of::<T>(),
                align: std::mem::align_of::<T>(),
                fields,
                tagged_union: Some(TaggedUnion {
                    tag_field,
                    value_field,
                    arm_source,
                }),
            },
        }
    }

    fn init_union<T>(name: &'static str, fields: Vec<Field>) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Union {
                size: std::mem::size_of::<T>(),
                align: std::mem::align_of::<T>(),
                fields,
            },
        }
    }

    fn init_enum(name: &'static str, prefix: &'static str, values: Vec<(String, i64)>) -> TypeDecl {
        Self::init_enum_sentinel(name, prefix, "MAX_VALUE", values)
    }

    fn init_enum_sentinel(
        name: &'static str,
        prefix: &'static str,
        sentinel_suffix: &'static str,
        values: Vec<(String, i64)>,
    ) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Enum {
                prefix,
                sentinel_suffix,
                values,
            },
        }
    }

    fn init_alias<T>(name: &'static str, alias_type: &'static str) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Alias {
                size: std::mem::size_of::<T>(),
                align: std::mem::align_of::<T>(),
                alias_type,
            },
        }
    }

    fn init_opaque(name: &'static str) -> TypeDecl {
        TypeDecl {
            name,
            info: TypeInfo::Opaque,
        }
    }
}

/// `(zig name, value)` of a [`c_api_enum!`](crate::c_api_enum) enum.
fn enum_values<T: CApiEnum>() -> Vec<(String, i64)> {
    T::zig_fields()
}

/// `(zig name, value)` from an explicit list.
fn values_of<T: Copy>(items: &[T], name: impl Fn(T) -> String, value: impl Fn(T) -> i64) -> Vec<(String, i64)> {
    items.iter().map(|&v| (name(v), value(v))).collect()
}

/// The arms of a `lib.TaggedUnion` C struct: for every tag, the public
/// value-union field (Zig: `T.cFieldRename(tag)`).
fn generated_arms<T: CApiEnum>(rename: impl Fn(c_int) -> Option<&'static str>) -> ArmSource {
    ArmSource::Generated(
        T::zig_fields()
            .into_iter()
            .map(|(name, value)| (name, rename(value as c_int)))
            .collect(),
    )
}

/// Public C types. Names and enum prefixes are intentionally explicit because
/// Zig identifiers are not the C API contract.
pub fn type_decls() -> Vec<TypeDecl> {
    use super::{build_info, cell, color as color_c, io as io_c, key_encode, modes, mouse_encode};
    use super::{grid_ref, osc, paste, result, row, selection, sgr, style, sys, terminal as term};
    use super::{formatter, kitty_graphics as kitty, render, search, selection_gesture as gesture, snapshot};
    use crate::terminal::point;
    use crate::lib::allocator as alloc;
    use crate::terminal::color;

    let mut decls = vec![
        TypeDecl::init_struct::<alloc::Allocator>(
            "GhosttyAllocator",
            vec![
                field!(alloc::Allocator, ctx, pointer("opaque", false, false)),
                field!(alloc::Allocator, vtable, pointer("GhosttyAllocatorVtable", true, false)),
            ],
        ),
        TypeDecl::init_struct::<alloc::VTable>(
            "GhosttyAllocatorVtable",
            vec![
                field!(alloc::VTable, alloc, pointer("function", true, false)),
                field!(alloc::VTable, resize, pointer("function", true, false)),
                field!(alloc::VTable, remap, pointer("function", true, false)),
                field!(alloc::VTable, free, pointer("function", true, false)),
            ],
        ),
        TypeDecl::init_struct::<crate::lib::Buffer>(
            "GhosttyBuffer",
            vec![
                field!(crate::lib::Buffer, ptr, pointer("u8", false, true)),
                field!(crate::lib::Buffer, cap, named(USIZE)),
                field!(crate::lib::Buffer, len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<cell::CellsView>(
            "GhosttyCellsView",
            vec![
                field!(cell::CellsView, ptr, pointer("GhosttyCell", true, true)),
                field!(cell::CellsView, len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<term::ClipboardContent>(
            "GhosttyClipboardContent",
            vec![
                field!(term::ClipboardContent, mime, named("GhosttyString")),
                field!(term::ClipboardContent, data, named("GhosttyString")),
            ],
        ),
        TypeDecl::init_struct::<term::ClipboardRead>(
            "GhosttyClipboardRead",
            vec![
                field!(term::ClipboardRead, size, named(USIZE)),
                field!(term::ClipboardRead, location, named("GhosttyClipboardLocation")),
                field!(term::ClipboardRead, mimes, pointer("GhosttyString", true, true)),
                field!(term::ClipboardRead, mimes_len, named(USIZE)),
                field!(term::ClipboardRead, list, named("bool")),
                field!(term::ClipboardRead, name, named("GhosttyString")),
                field!(term::ClipboardRead, granted, named("bool")),
                field!(term::ClipboardRead, can_remember, named("bool")),
                field!(term::ClipboardRead, ctx, pointer("opaque", true, false)),
                field!(term::ClipboardRead, reply, pointer("function", true, false)),
            ],
        ),
        TypeDecl::init_struct::<term::ClipboardReadReply>(
            "GhosttyClipboardReadReply",
            vec![
                field!(term::ClipboardReadReply, size, named(USIZE)),
                field!(term::ClipboardReadReply, result, named("GhosttyClipboardReadResult")),
                field!(term::ClipboardReadReply, contents, pointer("GhosttyClipboardContent", true, true)),
                field!(term::ClipboardReadReply, contents_len, named(USIZE)),
                field!(term::ClipboardReadReply, available, pointer("GhosttyString", true, true)),
                field!(term::ClipboardReadReply, available_len, named(USIZE)),
                field!(term::ClipboardReadReply, remember, named("bool")),
            ],
        ),
        TypeDecl::init_struct::<term::ClipboardWrite>(
            "GhosttyClipboardWrite",
            vec![
                field!(term::ClipboardWrite, size, named(USIZE)),
                field!(term::ClipboardWrite, location, named("GhosttyClipboardLocation")),
                field!(term::ClipboardWrite, contents, pointer("GhosttyClipboardContent", true, true)),
                field!(term::ClipboardWrite, contents_len, named(USIZE)),
                field!(term::ClipboardWrite, name, named("GhosttyString")),
                field!(term::ClipboardWrite, granted, named("bool")),
                field!(term::ClipboardWrite, can_remember, named("bool")),
                field!(term::ClipboardWrite, ctx, pointer("opaque", true, false)),
                field!(term::ClipboardWrite, reply, pointer("function", true, false)),
            ],
        ),
        TypeDecl::init_struct::<term::ClipboardWriteReply>(
            "GhosttyClipboardWriteReply",
            vec![
                field!(term::ClipboardWriteReply, size, named(USIZE)),
                field!(term::ClipboardWriteReply, result, named("GhosttyClipboardWriteResult")),
                field!(term::ClipboardWriteReply, remember, named("bool")),
            ],
        ),
        TypeDecl::init_struct::<Codepoints>(
            "GhosttyCodepoints",
            vec![
                field!(Codepoints, ptr, pointer("u32", true, true)),
                field!(Codepoints, len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<color_c::PaletteMask>(
            "GhosttyColorPaletteMask",
            vec![field!(color_c::PaletteMask, bits, array("u64", 4))],
        ),
        TypeDecl::init_struct::<color::RgbC>(
            "GhosttyColorRgb",
            vec![
                field!(color::RgbC, r, named("u8")),
                field!(color::RgbC, g, named("u8")),
                field!(color::RgbC, b, named("u8")),
            ],
        ),
        TypeDecl::init_struct::<color_c::X11Entry>(
            "GhosttyColorX11Entry",
            vec![
                field!(color_c::X11Entry, name, pointer("u8", true, true)),
                field!(color_c::X11Entry, color, named("GhosttyColorRgb")),
            ],
        ),
        TypeDecl::init_struct::<term::DeviceAttributes>(
            "GhosttyDeviceAttributes",
            vec![
                field!(term::DeviceAttributes, primary, named("GhosttyDeviceAttributesPrimary")),
                field!(term::DeviceAttributes, secondary, named("GhosttyDeviceAttributesSecondary")),
                field!(term::DeviceAttributes, tertiary, named("GhosttyDeviceAttributesTertiary")),
            ],
        ),
        TypeDecl::init_struct::<term::DeviceAttributesPrimary>(
            "GhosttyDeviceAttributesPrimary",
            vec![
                field!(term::DeviceAttributesPrimary, conformance_level, named("u16")),
                field!(term::DeviceAttributesPrimary, features, array("u16", 64)),
                field!(term::DeviceAttributesPrimary, num_features, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<term::DeviceAttributesSecondary>(
            "GhosttyDeviceAttributesSecondary",
            vec![
                field!(term::DeviceAttributesSecondary, device_type, named("u16")),
                field!(term::DeviceAttributesSecondary, firmware_version, named("u16")),
                field!(term::DeviceAttributesSecondary, rom_cartridge, named("u16")),
            ],
        ),
        TypeDecl::init_struct::<term::DeviceAttributesTertiary>(
            "GhosttyDeviceAttributesTertiary",
            vec![field!(term::DeviceAttributesTertiary, unit_id, named("u32"))],
        ),
        TypeDecl::init_struct::<formatter::ScreenOptionsExtra>("GhosttyFormatterScreenExtra", {
            use formatter::ScreenOptionsExtra as E;
            vec![
                field!(E, size, named(USIZE)),
                field!(E, cursor, named("bool")),
                field!(E, style, named("bool")),
                field!(E, hyperlink, named("bool")),
                field!(E, protection, named("bool")),
                field!(E, kitty_keyboard, named("bool")),
                field!(E, charsets, named("bool")),
            ]
        }),
        TypeDecl::init_struct::<formatter::TerminalOptionsExtra>("GhosttyFormatterTerminalExtra", {
            use formatter::TerminalOptionsExtra as E;
            vec![
                field!(E, size, named(USIZE)),
                field!(E, palette, named("bool")),
                field!(E, modes, named("bool")),
                field!(E, scrolling_region, named("bool")),
                field!(E, tabstops, named("bool")),
                field!(E, pwd, named("bool")),
                field!(E, keyboard, named("bool")),
                field!(E, screen, named("GhosttyFormatterScreenExtra")),
            ]
        }),
        TypeDecl::init_struct::<formatter::TerminalOptions>("GhosttyFormatterTerminalOptions", {
            use formatter::TerminalOptions as O;
            vec![
                field!(O, size, named(USIZE)),
                field!(O, emit, named("GhosttyFormatterFormat")),
                field!(O, unwrap, named("bool")),
                field!(O, trim, named("bool")),
                field!(O, extra, named("GhosttyFormatterTerminalExtra")),
                field!(O, selection, pointer("GhosttySelection", true, true)),
            ]
        }),
        TypeDecl::init_struct::<grid_ref::CGridRef>(
            "GhosttyGridRef",
            vec![
                field!(grid_ref::CGridRef, size, named(USIZE)),
                field!(grid_ref::CGridRef, node, pointer("opaque", false, true)),
                field!(grid_ref::CGridRef, x, named("u16")),
                field!(grid_ref::CGridRef, y, named("u16")),
            ],
        ),
        TypeDecl::init_struct::<kitty::PlacementRenderInfo>("GhosttyKittyGraphicsPlacementRenderInfo", {
            use kitty::PlacementRenderInfo as I;
            vec![
                field!(I, size, named(USIZE)),
                field!(I, pixel_width, named("u32")),
                field!(I, pixel_height, named("u32")),
                field!(I, grid_cols, named("u32")),
                field!(I, grid_rows, named("u32")),
                field!(I, viewport_col, named("i32")),
                field!(I, viewport_row, named("i32")),
                field!(I, viewport_visible, named("bool")),
                field!(I, source_x, named("u32")),
                field!(I, source_y, named("u32")),
                field!(I, source_width, named("u32")),
                field!(I, source_height, named("u32")),
            ]
        }),
        TypeDecl::init_struct::<io_c::MimeReader>(
            "GhosttyMimeReader",
            vec![
                field!(io_c::MimeReader, read, pointer("function", true, true)),
                field!(io_c::MimeReader, userdata, pointer("opaque", false, true)),
            ],
        ),
        TypeDecl::init_struct::<mouse_encode::Size>(
            "GhosttyMouseEncoderSize",
            vec![
                field!(mouse_encode::Size, size, named(USIZE)),
                field!(mouse_encode::Size, screen_width, named("u32")),
                field!(mouse_encode::Size, screen_height, named("u32")),
                field!(mouse_encode::Size, cell_width, named("u32")),
                field!(mouse_encode::Size, cell_height, named("u32")),
                field!(mouse_encode::Size, padding_top, named("u32")),
                field!(mouse_encode::Size, padding_bottom, named("u32")),
                field!(mouse_encode::Size, padding_right, named("u32")),
                field!(mouse_encode::Size, padding_left, named("u32")),
            ],
        ),
        TypeDecl::init_struct::<super::mouse_event::Position>(
            "GhosttyMousePosition",
            vec![
                field!(super::mouse_event::Position, x, named("f32")),
                field!(super::mouse_event::Position, y, named("f32")),
            ],
        ),
        TypeDecl::init_struct::<paste::Request>(
            "GhosttyPaste",
            vec![
                field!(paste::Request, size, named(USIZE)),
                field!(paste::Request, location, named("GhosttyClipboardLocation")),
                field!(paste::Request, source, named("GhosttyPasteSource")),
                field!(paste::Request, mimes, pointer("GhosttyString", true, true)),
                field!(paste::Request, mimes_len, named(USIZE)),
                field!(paste::Request, reader, named("GhosttyMimeReader")),
                field!(paste::Request, allow_unsafe, named("bool")),
            ],
        ),
        TypeDecl::init_tagged_struct::<point::PointC>(
            "GhosttyPoint",
            vec![
                field!(point::PointC, tag, named("GhosttyPointTag")),
                field!(point::PointC, value, named("GhosttyPointValue")),
            ],
            "tag",
            "value",
            ArmSource::Generated(
                ["active", "viewport", "screen", "history"]
                    .into_iter()
                    .map(|n| (n.to_string(), Some(n)))
                    .collect(),
            ),
        ),
        TypeDecl::init_struct::<point::Coordinate>(
            "GhosttyPointCoordinate",
            vec![
                field!(point::Coordinate, x, named("u16")),
                field!(point::Coordinate, y, named("u32")),
            ],
        ),
        TypeDecl::init_union::<point::PointCValue>(
            "GhosttyPointValue",
            vec![
                field!(union point::PointCValue, active, named("GhosttyPointCoordinate")),
                field!(union point::PointCValue, viewport, named("GhosttyPointCoordinate")),
                field!(union point::PointCValue, screen, named("GhosttyPointCoordinate")),
                field!(union point::PointCValue, history, named("GhosttyPointCoordinate")),
                field!(union point::PointCValue, _padding, array("u64", 2)),
            ],
        ),
        TypeDecl::init_struct::<io_c::Reader>(
            "GhosttyReader",
            vec![
                field!(io_c::Reader, read, pointer("function", true, true)),
                field!(io_c::Reader, userdata, pointer("opaque", false, true)),
            ],
        ),
        TypeDecl::init_struct::<render::Colors>("GhosttyRenderStateColors", {
            use render::Colors as C;
            vec![
                field!(C, size, named(USIZE)),
                field!(C, background, named("GhosttyColorRgb")),
                field!(C, foreground, named("GhosttyColorRgb")),
                field!(C, cursor, named("GhosttyColorRgb")),
                field!(C, cursor_has_value, named("bool")),
                field!(C, palette, array("GhosttyColorRgb", 256)),
            ]
        }),
        TypeDecl::init_struct::<render::Cursor>("GhosttyRenderStateCursor", {
            use render::Cursor as C;
            vec![
                field!(C, size, named(USIZE)),
                field!(C, viewport_has_value, named("bool")),
                field!(C, viewport_x, named("u16")),
                field!(C, viewport_y, named("u16")),
                field!(C, wide_tail, named("bool")),
                field!(C, visible, named("bool")),
                field!(C, blinking, named("bool")),
                field!(C, password_input, named("bool")),
                field!(C, visual_style, named("GhosttyRenderStateCursorVisualStyle")),
            ]
        }),
        TypeDecl::init_struct::<render::Overscan>(
            "GhosttyRenderStateOverscan",
            vec![
                field!(render::Overscan, above, named("u16")),
                field!(render::Overscan, below, named("u16")),
            ],
        ),
        TypeDecl::init_struct::<render::RowId>(
            "GhosttyRenderStateRowId",
            vec![field!(render::RowId, bits, array("u64", 2))],
        ),
        TypeDecl::init_struct::<render::RowSelection>(
            "GhosttyRenderStateRowSelection",
            vec![
                field!(render::RowSelection, size, named(USIZE)),
                field!(render::RowSelection, start_x, named("u16")),
                field!(render::RowSelection, end_x, named("u16")),
            ],
        ),
        TypeDecl::init_struct::<selection::CSelection>(
            "GhosttySelection",
            vec![
                field!(selection::CSelection, size, named(USIZE)),
                field!(selection::CSelection, start, named("GhosttyGridRef")),
                field!(selection::CSelection, end, named("GhosttyGridRef")),
                field!(selection::CSelection, rectangle, named("bool")),
            ],
        ),
        TypeDecl::init_struct::<selection::CSelectionBuffer>(
            "GhosttySelectionBuffer",
            vec![
                field!(selection::CSelectionBuffer, ptr, pointer("GhosttySelection", false, true)),
                field!(selection::CSelectionBuffer, cap, named(USIZE)),
                field!(selection::CSelectionBuffer, len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<gesture::Behaviors>(
            "GhosttySelectionGestureBehaviors",
            vec![
                field!(gesture::Behaviors, single_click, named("GhosttySelectionGestureBehavior")),
                field!(gesture::Behaviors, double_click, named("GhosttySelectionGestureBehavior")),
                field!(gesture::Behaviors, triple_click, named("GhosttySelectionGestureBehavior")),
            ],
        ),
        TypeDecl::init_struct::<gesture::Geometry>(
            "GhosttySelectionGestureGeometry",
            vec![
                field!(gesture::Geometry, columns, named("u32")),
                field!(gesture::Geometry, cell_width, named("u32")),
                field!(gesture::Geometry, padding_left, named("u32")),
                field!(gesture::Geometry, screen_height, named("u32")),
            ],
        ),
        TypeDecl::init_tagged_struct::<sgr::AttributeC>(
            "GhosttySgrAttribute",
            vec![
                field!(sgr::AttributeC, tag, named("GhosttySgrAttributeTag")),
                field!(sgr::AttributeC, value, named("GhosttySgrAttributeValue")),
            ],
            "tag",
            "value",
            generated_arms::<sgr::AttributeTag>(|tag| {
                sgr::AttributeTag::from_int(tag).and_then(sgr::AttributeC::c_field_rename)
            }),
        ),
        TypeDecl::init_struct::<crate::terminal::sgr::UnknownC>(
            "GhosttySgrUnknown",
            vec![
                field!(crate::terminal::sgr::UnknownC, full_ptr, pointer("u16", true, false)),
                field!(crate::terminal::sgr::UnknownC, full_len, named(USIZE)),
                field!(crate::terminal::sgr::UnknownC, partial_ptr, pointer("u16", true, false)),
                field!(crate::terminal::sgr::UnknownC, partial_len, named(USIZE)),
            ],
        ),
        TypeDecl::init_union::<sgr::AttributeCValue>("GhosttySgrAttributeValue", sgr::AttributeCValue::fields()),
        TypeDecl::init_struct::<crate::terminal::size_report::Size>(
            "GhosttySizeReportSize",
            vec![
                field!(crate::terminal::size_report::Size, rows, named("u16")),
                field!(crate::terminal::size_report::Size, columns, named("u16")),
                field!(crate::terminal::size_report::Size, cell_width, named("u32")),
                field!(crate::terminal::size_report::Size, cell_height, named("u32")),
            ],
        ),
        TypeDecl::init_struct::<crate::lib::String>(
            "GhosttyString",
            vec![
                field!(crate::lib::String, ptr, pointer("u8", true, false)),
                field!(crate::lib::String, len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<SurfacePosition>(
            "GhosttySurfacePosition",
            vec![
                field!(SurfacePosition, x, named("f64")),
                field!(SurfacePosition, y, named("f64")),
            ],
        ),
        TypeDecl::init_struct::<style::Style>(
            "GhosttyStyle",
            vec![
                field!(style::Style, size, named(USIZE)),
                field!(style::Style, fg_color, named("GhosttyStyleColor")),
                field!(style::Style, bg_color, named("GhosttyStyleColor")),
                field!(style::Style, underline_color, named("GhosttyStyleColor")),
                field!(style::Style, bold, named("bool")),
                field!(style::Style, italic, named("bool")),
                field!(style::Style, faint, named("bool")),
                field!(style::Style, blink, named("bool")),
                field!(style::Style, inverse, named("bool")),
                field!(style::Style, invisible, named("bool")),
                field!(style::Style, strikethrough, named("bool")),
                field!(style::Style, overline, named("bool")),
                field!(style::Style, underline, named("i32")),
            ],
        ),
        TypeDecl::init_tagged_struct::<style::Color>(
            "GhosttyStyleColor",
            vec![
                field!(style::Color, tag, named("GhosttyStyleColorTag")),
                field!(style::Color, value, named("GhosttyStyleColorValue")),
            ],
            "tag",
            "value",
            ArmSource::Fields,
        ),
        TypeDecl::init_union::<style::ColorValue>(
            "GhosttyStyleColorValue",
            vec![
                field!(union style::ColorValue, palette, named("GhosttyColorPaletteIndex")),
                field!(union style::ColorValue, rgb, named("GhosttyColorRgb")),
                field!(union style::ColorValue, _padding, named("u64")),
            ],
        ),
        TypeDecl::init_struct::<sys::Image>(
            "GhosttySysImage",
            vec![
                field!(sys::Image, width, named("u32")),
                field!(sys::Image, height, named("u32")),
                field!(sys::Image, data, pointer("u8", false, true)),
                field!(sys::Image, data_len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<term::DesktopNotification>(
            "GhosttyTerminalDesktopNotification",
            vec![
                field!(term::DesktopNotification, size, named(USIZE)),
                field!(term::DesktopNotification, title, named("GhosttyString")),
                field!(term::DesktopNotification, body, named("GhosttyString")),
            ],
        ),
        TypeDecl::init_struct::<term::TerminalMemoryUsage>("GhosttyTerminalMemoryUsage", {
            use term::TerminalMemoryUsage as M;
            vec![
                field!(M, size, named(USIZE)),
                field!(M, compression_supported, named("bool")),
                field!(M, primary_pages, named("u64")),
                field!(M, primary_virtual_bytes, named("u64")),
                field!(M, primary_resident_bytes, named("u64")),
                field!(M, primary_compressed_pages, named("u64")),
                field!(M, primary_compressed_bytes, named("u64")),
                field!(M, primary_image_bytes, named("u64")),
                field!(M, alternate_pages, named("u64")),
                field!(M, alternate_virtual_bytes, named("u64")),
                field!(M, alternate_resident_bytes, named("u64")),
                field!(M, alternate_compressed_pages, named("u64")),
                field!(M, alternate_compressed_bytes, named("u64")),
                field!(M, alternate_image_bytes, named("u64")),
            ]
        }),
        TypeDecl::init_struct::<term::ModeConfig>(
            "GhosttyTerminalModeConfig",
            vec![
                field!(term::ModeConfig, mode, named("GhosttyMode")),
                field!(term::ModeConfig, value, named("bool")),
            ],
        ),
        TypeDecl::init_struct::<term::ProgramStatus>(
            "GhosttyTerminalProgramStatus",
            vec![
                field!(term::ProgramStatus, size, named(USIZE)),
                field!(term::ProgramStatus, state, named("GhosttyProgramStatusState")),
                field!(term::ProgramStatus, kind, named("GhosttyProgramStatusKind")),
                field!(term::ProgramStatus, progress, named("i8")),
                field!(term::ProgramStatus, id, named("GhosttyString")),
                field!(term::ProgramStatus, app, named("GhosttyString")),
                field!(term::ProgramStatus, title, named("GhosttyString")),
                field!(term::ProgramStatus, message, named("GhosttyString")),
            ],
        ),
        TypeDecl::init_struct::<term::ProgressReport>(
            "GhosttyTerminalProgressReport",
            vec![
                field!(term::ProgressReport, size, named(USIZE)),
                field!(term::ProgressReport, state, named("GhosttyTerminalProgressState")),
                field!(term::ProgressReport, progress, named("i8")),
            ],
        ),
        TypeDecl::init_tagged_struct::<term::ResizeWindow>(
            "GhosttyTerminalResizeWindow",
            vec![
                field!(term::ResizeWindow, tag, named("GhosttyTerminalResizeWindowTag")),
                field!(term::ResizeWindow, value, named("GhosttyTerminalResizeWindowValue")),
            ],
            "tag",
            "value",
            generated_arms::<term::ResizeWindowTag>(|tag| {
                term::ResizeWindowTag::from_int(tag).and_then(term::ResizeWindow::c_field_rename)
            }),
        ),
        TypeDecl::init_struct::<term::ResizeWindowGrid>(
            "GhosttyTerminalResizeWindowGrid",
            vec![
                field!(term::ResizeWindowGrid, rows, named("u16")),
                field!(term::ResizeWindowGrid, columns, named("u16")),
            ],
        ),
        TypeDecl::init_union::<term::ResizeWindowValue>(
            "GhosttyTerminalResizeWindowValue",
            vec![
                field!(union term::ResizeWindowValue, grid, named("GhosttyTerminalResizeWindowGrid")),
                field!(union term::ResizeWindowValue, _padding, array("u64", 2)),
            ],
        ),
        TypeDecl::init_struct::<term::TerminalScrollbar>(
            "GhosttyTerminalScrollbar",
            vec![
                field!(term::TerminalScrollbar, total, named("u64")),
                field!(term::TerminalScrollbar, offset, named("u64")),
                field!(term::TerminalScrollbar, len, named("u64")),
            ],
        ),
        TypeDecl::init_struct::<term::SemanticPrompt>(
            "GhosttyTerminalSemanticPrompt",
            vec![
                field!(term::SemanticPrompt, size, named(USIZE)),
                field!(term::SemanticPrompt, kind, named("GhosttySemanticPromptKind")),
                field!(term::SemanticPrompt, prompt_kind, named("GhosttySemanticPromptPromptKind")),
                field!(term::SemanticPrompt, has_exit_code, named("bool")),
                field!(term::SemanticPrompt, exit_code, named("i32")),
                field!(term::SemanticPrompt, command, named("GhosttyString")),
                field!(term::SemanticPrompt, error, named("GhosttyString")),
            ],
        ),
        TypeDecl::init_tagged_struct::<term::ScrollViewport>(
            "GhosttyTerminalScrollViewport",
            vec![
                field!(term::ScrollViewport, tag, named("GhosttyTerminalScrollViewportTag")),
                field!(term::ScrollViewport, value, named("GhosttyTerminalScrollViewportValue")),
            ],
            "tag",
            "value",
            ArmSource::Generated(vec![
                ("top".to_string(), None),
                ("bottom".to_string(), None),
                ("delta".to_string(), Some("delta")),
                ("row".to_string(), Some("row")),
                ("delta_prompt".to_string(), Some("delta_prompt")),
            ]),
        ),
        TypeDecl::init_union::<crate::terminal::terminal::ScrollViewportCValue>("GhosttyTerminalScrollViewportValue", {
            use crate::terminal::terminal::ScrollViewportCValue as V;
            vec![
                field!(union V, delta, named(ISIZE)),
                field!(union V, row, named(USIZE)),
                field!(union V, delta_prompt, named(ISIZE)),
                field!(union V, _padding, array("u64", 2)),
            ]
        }),
        TypeDecl::init_struct::<selection::SelectLineOptions>(
            "GhosttyTerminalSelectLineOptions",
            vec![
                field!(selection::SelectLineOptions, size, named(USIZE)),
                field!(selection::SelectLineOptions, r#ref, named("GhosttyGridRef")),
                field!(selection::SelectLineOptions, whitespace, pointer("u32", true, true)),
                field!(selection::SelectLineOptions, whitespace_len, named(USIZE)),
                field!(selection::SelectLineOptions, semantic_prompt_boundary, named("bool")),
            ],
        ),
        TypeDecl::init_struct::<selection::SelectWordBetweenOptions>(
            "GhosttyTerminalSelectWordBetweenOptions",
            vec![
                field!(selection::SelectWordBetweenOptions, size, named(USIZE)),
                field!(selection::SelectWordBetweenOptions, start, named("GhosttyGridRef")),
                field!(selection::SelectWordBetweenOptions, end, named("GhosttyGridRef")),
                field!(selection::SelectWordBetweenOptions, boundary_codepoints, pointer("u32", true, true)),
                field!(selection::SelectWordBetweenOptions, boundary_codepoints_len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<selection::SelectWordOptions>(
            "GhosttyTerminalSelectWordOptions",
            vec![
                field!(selection::SelectWordOptions, size, named(USIZE)),
                field!(selection::SelectWordOptions, r#ref, named("GhosttyGridRef")),
                field!(selection::SelectWordOptions, boundary_codepoints, pointer("u32", true, true)),
                field!(selection::SelectWordOptions, boundary_codepoints_len, named(USIZE)),
            ],
        ),
        TypeDecl::init_struct::<selection::FormatOptions>(
            "GhosttyTerminalSelectionFormatOptions",
            vec![
                field!(selection::FormatOptions, size, named(USIZE)),
                field!(selection::FormatOptions, emit, named("GhosttyFormatterFormat")),
                field!(selection::FormatOptions, unwrap, named("bool")),
                field!(selection::FormatOptions, trim, named("bool")),
                field!(selection::FormatOptions, selection, pointer("GhosttySelection", true, true)),
            ],
        ),
        TypeDecl::init_tagged_struct::<term::UnknownSequence>(
            "GhosttyTerminalUnknownSequence",
            vec![
                field!(term::UnknownSequence, tag, named("GhosttyTerminalUnknownSequenceTag")),
                field!(term::UnknownSequence, value, named("GhosttyTerminalUnknownSequenceValue")),
            ],
            "tag",
            "value",
            generated_arms::<term::UnknownSequenceTag>(|tag| {
                term::UnknownSequenceTag::from_int(tag).and_then(term::UnknownSequence::c_field_rename)
            }),
        ),
        TypeDecl::init_struct::<term::UnknownOscSequence>(
            "GhosttyTerminalUnknownOscSequence",
            vec![
                field!(term::UnknownOscSequence, truncated, named("bool")),
                field!(term::UnknownOscSequence, content, named("GhosttyString")),
                field!(term::UnknownOscSequence, terminator, named("GhosttyOscTerminator")),
            ],
        ),
        TypeDecl::init_struct::<term::UnknownStringSequence>(
            "GhosttyTerminalUnknownStringSequence",
            vec![
                field!(term::UnknownStringSequence, truncated, named("bool")),
                field!(term::UnknownStringSequence, content, named("GhosttyString")),
            ],
        ),
        TypeDecl::init_struct::<term::WriteStop>(
            "GhosttyTerminalWriteStop",
            vec![
                field!(term::WriteStop, size, named(USIZE)),
                field!(term::WriteStop, consumed, named(USIZE)),
                field!(term::WriteStop, reached, named("u32")),
                field!(term::WriteStop, kind, named("GhosttyTerminalStringKind")),
                field!(term::WriteStop, introducer, named("GhosttyTerminalStringIntroducer")),
                field!(term::WriteStop, end, named("GhosttyTerminalStringEnd")),
                field!(term::WriteStop, byte, named("u8")),
            ],
        ),
        TypeDecl::init_union::<term::UnknownSequenceValue>(
            "GhosttyTerminalUnknownSequenceValue",
            vec![
                field!(union term::UnknownSequenceValue, apc, named("GhosttyTerminalUnknownStringSequence")),
                field!(union term::UnknownSequenceValue, osc, named("GhosttyTerminalUnknownOscSequence")),
                field!(union term::UnknownSequenceValue, _padding, array("u64", 16)),
            ],
        ),
        TypeDecl::init_struct::<io_c::Writer>(
            "GhosttyWriter",
            vec![
                field!(io_c::Writer, write, pointer("function", true, true)),
                field!(io_c::Writer, userdata, pointer("opaque", false, true)),
            ],
        ),
        TypeDecl::init_enum_sentinel(
            "GhosttyResult",
            "GHOSTTY_",
            "RESULT_MAX_VALUE",
            enum_values::<result::Result>(),
        ),
        TypeDecl::init_enum(
            "GhosttyBuildInfo",
            "GHOSTTY_BUILD_INFO_",
            enum_values::<build_info::BuildInfo>(),
        ),
        TypeDecl::init_enum_sentinel(
            "GhosttyOptimizeMode",
            "GHOSTTY_OPTIMIZE_",
            "MODE_MAX_VALUE",
            enum_values::<build_info::OptimizeMode>(),
        ),
        TypeDecl::init_enum_sentinel(
            "GhosttyCellContentTag",
            "GHOSTTY_CELL_CONTENT_",
            "TAG_MAX_VALUE",
            enum_values::<cell::ContentTag>(),
        ),
        TypeDecl::init_enum("GhosttyCellData", "GHOSTTY_CELL_DATA_", enum_values::<cell::CellData>()),
        TypeDecl::init_enum(
            "GhosttyCellSemanticContent",
            "GHOSTTY_CELL_SEMANTIC_",
            enum_values::<cell::SemanticContent>(),
        ),
        TypeDecl::init_enum("GhosttyCellWide", "GHOSTTY_CELL_WIDE_", enum_values::<cell::Wide>()),
        TypeDecl::init_enum(
            "GhosttyClipboardLocation",
            "GHOSTTY_CLIPBOARD_LOCATION_",
            {
                use crate::terminal::clipboard::Location;
                values_of(
                    &[
                        ("standard", Location::STANDARD),
                        ("selection", Location::SELECTION),
                        ("primary", Location::PRIMARY),
                    ],
                    |(n, _)| n.to_string(),
                    |(_, v)| v.0 as i64,
                )
            },
        ),
        TypeDecl::init_enum("GhosttyClipboardReadResult", "GHOSTTY_CLIPBOARD_READ_RESULT_", {
            use crate::terminal::clipboard::ReadStatus as S;
            values_of(
                &[
                    ("success", S::SUCCESS),
                    ("denied", S::DENIED),
                    ("unsupported", S::UNSUPPORTED),
                    ("busy", S::BUSY),
                    ("io_error", S::IO_ERROR),
                ],
                |(n, _)| n.to_string(),
                |(_, v)| v.0 as i64,
            )
        }),
        TypeDecl::init_enum("GhosttyClipboardWriteResult", "GHOSTTY_CLIPBOARD_WRITE_RESULT_", {
            use crate::terminal::clipboard::WriteStatus as S;
            values_of(
                &[
                    ("success", S::SUCCESS),
                    ("denied", S::DENIED),
                    ("unsupported", S::UNSUPPORTED),
                    ("busy", S::BUSY),
                    ("invalid_data", S::INVALID_DATA),
                    ("io_error", S::IO_ERROR),
                ],
                |(n, _)| n.to_string(),
                |(_, v)| v.0 as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttyColorScheme",
            "GHOSTTY_COLOR_SCHEME_",
            enum_values::<super::color_scheme::ColorScheme>(),
        ),
        TypeDecl::init_enum("GhosttyFocusEvent", "GHOSTTY_FOCUS_", enum_values::<super::focus::Event>()),
        TypeDecl::init_enum("GhosttyFormatterFormat", "GHOSTTY_FORMATTER_FORMAT_", {
            use crate::terminal::formatter::Format;
            values_of(
                &[Format::Plain, Format::Vt, Format::Html],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum("GhosttyImageProtocol", "GHOSTTY_IMAGE_PROTOCOL_", enum_values::<render::ImageProtocol>()),
        TypeDecl::init_enum("GhosttyKey", "GHOSTTY_KEY_", {
            use crate::input::key::Key;
            values_of(&Key::ALL, |v| v.as_str().to_string(), |v| v as c_int as i64)
        }),
        TypeDecl::init_enum("GhosttyKeyAction", "GHOSTTY_KEY_ACTION_", {
            use crate::input::key::Action;
            values_of(
                &[Action::Release, Action::Press, Action::Repeat],
                |v| v.as_str().to_string(),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttyKeyEncoderOption",
            "GHOSTTY_KEY_ENCODER_OPT_",
            enum_values::<key_encode::Option>(),
        ),
        TypeDecl::init_enum("GhosttyKittyGraphicsData", "GHOSTTY_KITTY_GRAPHICS_DATA_", enum_values::<kitty::Data>()),
        TypeDecl::init_enum(
            "GhosttyKittyGraphicsImageData",
            "GHOSTTY_KITTY_IMAGE_DATA_",
            enum_values::<kitty::ImageData>(),
        ),
        TypeDecl::init_enum(
            "GhosttyKittyGraphicsPlacementData",
            "GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_",
            enum_values::<kitty::PlacementData>(),
        ),
        TypeDecl::init_enum(
            "GhosttyKittyGraphicsPlacementIteratorOption",
            "GHOSTTY_KITTY_GRAPHICS_PLACEMENT_ITERATOR_OPTION_",
            enum_values::<kitty::PlacementIteratorOption>(),
        ),
        TypeDecl::init_enum(
            "GhosttyKittyImageCompression",
            "GHOSTTY_KITTY_IMAGE_COMPRESSION_",
            enum_values::<kitty::ImageCompression>(),
        ),
        TypeDecl::init_enum(
            "GhosttyKittyImageFormat",
            "GHOSTTY_KITTY_IMAGE_FORMAT_",
            enum_values::<kitty::ImageFormat>(),
        ),
        TypeDecl::init_enum(
            "GhosttyKittyPlacementLayer",
            "GHOSTTY_KITTY_PLACEMENT_LAYER_",
            enum_values::<kitty::PlacementLayer>(),
        ),
        TypeDecl::init_enum(
            "GhosttyModeReportState",
            "GHOSTTY_MODE_REPORT_",
            enum_values::<modes::ReportState>(),
        ),
        TypeDecl::init_enum("GhosttyMouseAction", "GHOSTTY_MOUSE_ACTION_", {
            use crate::input::mouse::Action;
            values_of(
                &[Action::Press, Action::Release, Action::Motion],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum("GhosttyMouseButton", "GHOSTTY_MOUSE_BUTTON_", {
            use crate::input::mouse::Button;
            values_of(&Button::ALL, |v| zig_name(&format!("{v:?}")), |v| v as c_int as i64)
        }),
        TypeDecl::init_enum(
            "GhosttyMouseEncoderOption",
            "GHOSTTY_MOUSE_ENCODER_OPT_",
            enum_values::<mouse_encode::Option>(),
        ),
        TypeDecl::init_enum(
            "GhosttyMouseFormat",
            "GHOSTTY_MOUSE_FORMAT_",
            enum_values::<mouse_encode::Format>(),
        ),
        TypeDecl::init_enum("GhosttyMouseShape", "GHOSTTY_MOUSE_SHAPE_", {
            values_of(&MOUSE_SHAPES, |v| zig_name(&format!("{v:?}")), |v| v as c_int as i64)
        }),
        TypeDecl::init_enum(
            "GhosttyMouseTrackingMode",
            "GHOSTTY_MOUSE_TRACKING_",
            enum_values::<mouse_encode::TrackingMode>(),
        ),
        TypeDecl::init_enum("GhosttyOptionAsAlt", "GHOSTTY_OPTION_AS_ALT_", {
            use crate::input::config::OptionAsAlt;
            values_of(
                &[OptionAsAlt::False, OptionAsAlt::True, OptionAsAlt::Left, OptionAsAlt::Right],
                |v| v.as_str().to_string(),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum("GhosttyOscCommandData", "GHOSTTY_OSC_DATA_", enum_values::<osc::CommandData>()),
        TypeDecl::init_enum("GhosttyOscOption", "GHOSTTY_OSC_OPT_", enum_values::<osc::Option>()),
        TypeDecl::init_enum("GhosttyOscTerminator", "GHOSTTY_OSC_TERMINATOR_", {
            use crate::terminal::osc::TerminatorC;
            values_of(
                &[TerminatorC::St, TerminatorC::Bel],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum_sentinel(
            "GhosttyOscCommandType",
            "GHOSTTY_OSC_COMMAND_",
            "TYPE_MAX_VALUE",
            osc::command_type_values(),
        ),
        TypeDecl::init_enum("GhosttyPasteSource", "GHOSTTY_PASTE_SOURCE_", enum_values::<paste::Source>()),
        TypeDecl::init_enum("GhosttyPointTag", "GHOSTTY_POINT_TAG_", {
            values_of(
                &[point::Tag::Active, point::Tag::Viewport, point::Tag::Screen, point::Tag::History],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttyProgramStatusKind",
            "GHOSTTY_PROGRAM_STATUS_KIND_",
            enum_values::<term::ProgramStatusKind>(),
        ),
        TypeDecl::init_enum(
            "GhosttyProgramStatusState",
            "GHOSTTY_PROGRAM_STATUS_STATE_",
            enum_values::<term::ProgramStatusState>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateCursorVisualStyle",
            "GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_",
            enum_values::<render::CursorVisualStyle>(),
        ),
        TypeDecl::init_enum("GhosttyRenderStateData", "GHOSTTY_RENDER_STATE_DATA_", enum_values::<render::Data>()),
        TypeDecl::init_enum("GhosttyRenderStateDirty", "GHOSTTY_RENDER_STATE_DIRTY_", enum_values::<render::Dirty>()),
        TypeDecl::init_enum(
            "GhosttyRenderStateImageData",
            "GHOSTTY_RENDER_STATE_IMAGE_DATA_",
            enum_values::<render::ImageData>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateImageLayer",
            "GHOSTTY_RENDER_STATE_IMAGE_LAYER_",
            enum_values::<render::ImageLayer>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateKittyPlacementKind",
            "GHOSTTY_RENDER_STATE_KITTY_PLACEMENT_KIND_",
            enum_values::<render::KittyPlacementKind>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateOption",
            "GHOSTTY_RENDER_STATE_OPTION_",
            enum_values::<render::SetOption>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateRowCellsData",
            "GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_",
            enum_values::<render::RowCellsData>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateRowData",
            "GHOSTTY_RENDER_STATE_ROW_DATA_",
            enum_values::<render::RowData>(),
        ),
        TypeDecl::init_enum(
            "GhosttyRenderStateRowOption",
            "GHOSTTY_RENDER_STATE_ROW_OPTION_",
            enum_values::<render::RowOption>(),
        ),
        TypeDecl::init_enum("GhosttyRowData", "GHOSTTY_ROW_DATA_", enum_values::<row::RowData>()),
        TypeDecl::init_enum(
            "GhosttyRowSemanticPrompt",
            "GHOSTTY_ROW_SEMANTIC_",
            enum_values::<row::SemanticPrompt>(),
        ),
        TypeDecl::init_enum("GhosttySearchData", "GHOSTTY_SEARCH_DATA_", enum_values::<search::Data>()),
        TypeDecl::init_enum("GhosttySearchOption", "GHOSTTY_SEARCH_OPT_", enum_values::<search::Option>()),
        TypeDecl::init_enum("GhosttySearchScroll", "GHOSTTY_SEARCH_SCROLL_", enum_values::<search::Scroll>()),
        TypeDecl::init_enum("GhosttySearchStatus", "GHOSTTY_SEARCH_STATUS_", enum_values::<search::Status>()),
        TypeDecl::init_enum("GhosttySelectionAdjust", "GHOSTTY_SELECTION_ADJUST_", {
            use crate::terminal::selection::Adjustment::*;
            values_of(
                &[Left, Right, Up, Down, Home, End, PageUp, PageDown, BeginningOfLine, EndOfLine],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttySelectionGestureAutoscroll",
            "GHOSTTY_SELECTION_GESTURE_AUTOSCROLL_",
            enum_values::<gesture::Autoscroll>(),
        ),
        TypeDecl::init_enum(
            "GhosttySelectionGestureBehavior",
            "GHOSTTY_SELECTION_GESTURE_BEHAVIOR_",
            enum_values::<gesture::Behavior>(),
        ),
        TypeDecl::init_enum(
            "GhosttySelectionGestureData",
            "GHOSTTY_SELECTION_GESTURE_DATA_",
            enum_values::<gesture::Data>(),
        ),
        TypeDecl::init_enum(
            "GhosttySelectionGestureEventOption",
            "GHOSTTY_SELECTION_GESTURE_EVENT_OPT_",
            enum_values::<gesture::EventOption>(),
        ),
        TypeDecl::init_enum(
            "GhosttySelectionGestureEventType",
            "GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_",
            enum_values::<gesture::EventType>(),
        ),
        TypeDecl::init_enum("GhosttySelectionOrder", "GHOSTTY_SELECTION_ORDER_", {
            use crate::terminal::selection::Order::*;
            values_of(
                &[Forward, Reverse, MirroredForward, MirroredReverse],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttySemanticPromptKind",
            "GHOSTTY_SEMANTIC_PROMPT_",
            enum_values::<term::SemanticPromptKind>(),
        ),
        TypeDecl::init_enum(
            "GhosttySemanticPromptPromptKind",
            "GHOSTTY_SEMANTIC_PROMPT_PROMPT_",
            enum_values::<term::SemanticPromptPromptKind>(),
        ),
        TypeDecl::init_enum(
            "GhosttySgrAttributeTag",
            "GHOSTTY_SGR_ATTR_",
            enum_values::<sgr::AttributeTag>(),
        ),
        TypeDecl::init_enum("GhosttySgrUnderline", "GHOSTTY_SGR_UNDERLINE_", enum_values::<sgr::Underline>()),
        TypeDecl::init_enum_sentinel(
            "GhosttySizeReportStyle",
            "GHOSTTY_SIZE_REPORT_",
            "STYLE_MAX_VALUE",
            {
                use crate::terminal::size_report::Style;
                values_of(
                    &[Style::Mode2048, Style::Csi14T, Style::Csi16T, Style::Csi18T, Style::Csi19T],
                    |v| v.as_str().to_string(),
                    |v| v as c_int as i64,
                )
            },
        ),
        TypeDecl::init_enum(
            "GhosttySnapshotDecoderData",
            "GHOSTTY_SNAPSHOT_DECODER_DATA_",
            enum_values::<snapshot::DecoderData>(),
        ),
        TypeDecl::init_enum(
            "GhosttySnapshotDecoderOption",
            "GHOSTTY_SNAPSHOT_DECODER_OPT_",
            enum_values::<snapshot::DecoderOption>(),
        ),
        TypeDecl::init_enum(
            "GhosttySnapshotDecoderProgressKind",
            "GHOSTTY_SNAPSHOT_DECODER_PROGRESS_KIND_",
            enum_values::<snapshot::DecoderProgressKind>(),
        ),
        TypeDecl::init_enum_sentinel(
            "GhosttyStyleColorTag",
            "GHOSTTY_STYLE_COLOR_",
            "TAG_MAX_VALUE",
            enum_values::<style::ColorTag>(),
        ),
        TypeDecl::init_enum("GhosttySysLogLevel", "GHOSTTY_SYS_LOG_LEVEL_", enum_values::<sys::LogLevel>()),
        TypeDecl::init_enum("GhosttySysOption", "GHOSTTY_SYS_OPT_", enum_values::<sys::Option>()),
        TypeDecl::init_enum("GhosttyTerminalBoundary", "GHOSTTY_TERMINAL_BOUNDARY_", enum_values::<term::Boundary>()),
        TypeDecl::init_enum(
            "GhosttyTerminalCompressionMode",
            "GHOSTTY_TERMINAL_COMPRESSION_MODE_",
            enum_values::<term::CompressionMode>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalCompressionResult",
            "GHOSTTY_TERMINAL_COMPRESSION_RESULT_",
            enum_values::<term::CompressionResult>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalCursorStyle",
            "GHOSTTY_TERMINAL_CURSOR_STYLE_",
            enum_values::<term::TerminalCursorStyle>(),
        ),
        TypeDecl::init_enum("GhosttyTerminalData", "GHOSTTY_TERMINAL_DATA_", enum_values::<term::TerminalData>()),
        TypeDecl::init_enum("GhosttyTerminalOption", "GHOSTTY_TERMINAL_OPT_", enum_values::<term::Option>()),
        TypeDecl::init_enum(
            "GhosttyTerminalProgressState",
            "GHOSTTY_TERMINAL_PROGRESS_STATE_",
            enum_values::<term::ProgressState>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalScreen",
            "GHOSTTY_TERMINAL_SCREEN_",
            enum_values::<term::TerminalScreen>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalResizeWindowTag",
            "GHOSTTY_TERMINAL_RESIZE_WINDOW_",
            enum_values::<term::ResizeWindowTag>(),
        ),
        TypeDecl::init_enum("GhosttyTerminalScrollViewportTag", "GHOSTTY_SCROLL_VIEWPORT_", {
            use crate::terminal::terminal::ScrollViewportTag::*;
            values_of(
                &[Top, Bottom, Delta, Row, DeltaPrompt],
                |v| zig_name(&format!("{v:?}")),
                |v| v as c_int as i64,
            )
        }),
        TypeDecl::init_enum(
            "GhosttyTerminalStringEnd",
            "GHOSTTY_TERMINAL_STRING_END_",
            enum_values::<term::StringEnd>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalStringIntroducer",
            "GHOSTTY_TERMINAL_STRING_INTRODUCER_",
            enum_values::<term::StringIntroducer>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalStringKind",
            "GHOSTTY_TERMINAL_STRING_KIND_",
            enum_values::<term::StringKind>(),
        ),
        TypeDecl::init_enum(
            "GhosttyTerminalUnknownSequenceTag",
            "GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_",
            enum_values::<term::UnknownSequenceTag>(),
        ),
        TypeDecl {
            name: "GhosttyCell",
            info: TypeInfo::Packed {
                size: std::mem::size_of::<crate::terminal::page::Cell>(),
                align: std::mem::align_of::<crate::terminal::page::Cell>(),
                underlying: "u64",
                layout: &crate::terminal::page::cell_c_layout::LAYOUT,
            },
        },
        TypeDecl::init_alias::<u8>("GhosttyColorPaletteIndex", "u8"),
        TypeDecl::init_alias::<u8>("GhosttyKittyKeyFlags", "u8"),
        TypeDecl::init_alias::<u16>("GhosttyMode", "u16"),
        TypeDecl::init_alias::<u16>("GhosttyMods", "u16"),
        TypeDecl::init_alias::<u64>("GhosttyRow", "u64"),
        TypeDecl::init_alias::<u16>("GhosttyStyleId", "u16"),
    ];

    for name in [
        "GhosttyFormatter",
        "GhosttyKeyEncoder",
        "GhosttyKeyEvent",
        "GhosttyKittyGraphics",
        "GhosttyKittyGraphicsImage",
        "GhosttyKittyGraphicsPlacementIterator",
        "GhosttyMouseEncoder",
        "GhosttyMouseEvent",
        "GhosttyOscCommand",
        "GhosttyOscParser",
        "GhosttyRenderState",
        "GhosttyRenderStateImageIterator",
        "GhosttyRenderStateRowCells",
        "GhosttyRenderStateRowIterator",
        "GhosttySearch",
        "GhosttySelectionGesture",
        "GhosttySelectionGestureEvent",
        "GhosttySgrParser",
        "GhosttySnapshotDecoder",
        "GhosttyTerminal",
        "GhosttyTrackedGridRef",
    ] {
        decls.push(TypeDecl::init_opaque(name));
    }

    // comptime: duplicate public ABI type check.
    for (i, decl) in decls.iter().enumerate() {
        for previous in &decls[..i] {
            assert!(decl.name != previous.name, "duplicate public ABI type: {}", decl.name);
        }
    }

    decls
}

/// Every `GhosttyMouseShape` key in order (`terminal.mouse.Shape`).
const MOUSE_SHAPES: [crate::terminal::mouse::Shape; 34] = {
    use crate::terminal::mouse::Shape::*;
    [
        Default,
        ContextMenu,
        Help,
        Pointer,
        Progress,
        Wait,
        Cell,
        Crosshair,
        Text,
        VerticalText,
        Alias,
        Copy,
        Move,
        NoDrop,
        NotAllowed,
        Grab,
        Grabbing,
        AllScroll,
        ColResize,
        RowResize,
        NResize,
        EResize,
        SResize,
        WResize,
        NeResize,
        NwResize,
        SeResize,
        SwResize,
        EwResize,
        NsResize,
        NeswResize,
        NwseResize,
        ZoomIn,
        ZoomOut,
    ]
};

/// The NUL-terminated manifest (Zig: `json`, built at comptime).
pub static JSON: LazyLock<std::ffi::CString> = LazyLock::new(|| {
    let value = Json::write_all(&type_decls());
    std::ffi::CString::new(serde_json::to_string(&value).expect("manifest serializes"))
        .expect("manifest has no NUL bytes")
});

/// C: ghostty_type_json
#[export_name = "ghostty_type_json"]
pub extern "C" fn get_json() -> *const c_char {
    JSON.as_ptr()
}

/// The JSON writer (Zig: `Json`).
pub struct Json;

impl Json {
    fn write_all(decls: &[TypeDecl]) -> Value {
        let mut root = Map::new();
        root.insert("schema".into(), json!(1));

        let mut abi = Map::new();
        abi.insert("target".into(), json!(std::env::consts::ARCH));
        abi.insert("os".into(), json!(std::env::consts::OS));
        abi.insert("environment".into(), json!(target_environment()));
        abi.insert("pointer_size".into(), json!(std::mem::size_of::<*const c_void>()));
        abi.insert("usize_size".into(), json!(std::mem::size_of::<usize>()));
        abi.insert("max_alignment".into(), json!(c_abi::MAX_ALIGNMENT));
        abi.insert(
            "endian".into(),
            json!(if cfg!(target_endian = "little") { "little" } else { "big" }),
        );
        root.insert("abi".into(), Value::Object(abi));

        root.insert("library_version".into(), json!(build_options::VERSION_STRING));
        root.insert(
            "commit".into(),
            match build_options::VERSION_BUILD {
                Some(commit) => json!(commit),
                None => Value::Null,
            },
        );
        root.insert("dirty".into(), Value::Null);

        let mut types = Map::new();
        for decl in decls {
            types.insert(decl.name.into(), Self::write_type(decl, decls));
        }
        root.insert("types".into(), Value::Object(types));
        Value::Object(root)
    }

    fn write_type(decl: &TypeDecl, decls: &[TypeDecl]) -> Value {
        let mut obj = Map::new();
        obj.insert("kind".into(), json!(decl.kind().as_str()));

        match &decl.info {
            TypeInfo::Struct {
                size,
                align,
                fields,
                tagged_union,
            } => {
                Self::write_size_align(&mut obj, *size, *align);
                let mut out = Map::new();
                for f in fields {
                    out.insert(f.name.into(), Self::write_field(decl, tagged_union.as_ref(), f, decls));
                }
                obj.insert("fields".into(), Value::Object(out));
            }
            TypeInfo::Union { size, align, fields } => {
                Self::write_size_align(&mut obj, *size, *align);
                let mut out = Map::new();
                for f in fields {
                    out.insert(f.name.into(), Self::write_field(decl, None, f, decls));
                }
                obj.insert("fields".into(), Value::Object(out));
            }
            TypeInfo::Enum {
                prefix,
                sentinel_suffix,
                values,
            } => {
                Self::write_size_align(&mut obj, std::mem::size_of::<c_int>(), std::mem::align_of::<c_int>());
                obj.insert("underlying".into(), json!("i32"));
                obj.insert("prefix".into(), json!(prefix));
                let mut out = Map::new();
                for (field, value) in values {
                    out.insert(Self::enum_object_field(decl.name, field), json!(value));
                }
                out.insert((*sentinel_suffix).into(), json!(c_int::MAX));
                obj.insert("values".into(), Value::Object(out));
            }
            TypeInfo::Packed {
                size,
                align,
                underlying,
                layout,
            } => {
                Self::write_size_align(&mut obj, *size, *align);
                obj.insert("underlying".into(), json!(underlying));
                obj.insert("bits".into(), Value::Object(Self::write_packed_bits(layout)));
            }
            TypeInfo::Alias {
                size,
                align,
                alias_type,
            } => {
                Self::write_size_align(&mut obj, *size, *align);
                obj.insert("type".into(), json!(alias_type));
            }
            TypeInfo::Opaque => {
                Self::write_size_align(
                    &mut obj,
                    std::mem::size_of::<*mut c_void>(),
                    std::mem::align_of::<*mut c_void>(),
                );
            }
        }
        Value::Object(obj)
    }

    fn write_packed_bits(layout: &PackedLayout) -> Map<String, Value> {
        let mut bits = Map::new();
        for field in layout.fields {
            let Some(name) = layout.field_name(field.name) else {
                continue;
            };
            let options = field.options;

            let mut obj = Map::new();
            obj.insert("lsb".into(), json!(field.bit_offset));
            obj.insert("width".into(), json!(field.bit_width));
            match options.encoding {
                Encoding::Scalar => {
                    obj.insert("type".into(), json!(options.type_name.unwrap_or(field.ty)));
                }
                Encoding::Packed(nested) => {
                    obj.insert("kind".into(), json!("packed"));
                    obj.insert("bits".into(), Value::Object(Self::write_packed_bits(nested)));
                }
                Encoding::TaggedUnion(union_layout) => {
                    obj.insert("kind".into(), json!("union"));
                    obj.insert(
                        "tag".into(),
                        json!(layout.field_name(union_layout.tag_field).expect("tag field is public")),
                    );
                    let tag_options = layout.field_options(union_layout.tag_field);
                    let tag_type_name = tag_options.type_name.expect("packed union tags have public type names");
                    let mut arms = Map::new();
                    for (tag, arm) in union_layout.arms {
                        let key = Self::enum_object_field(tag_type_name, tag);
                        match arm {
                            None => {
                                arms.insert(key, Value::Null);
                            }
                            Some(arm) => {
                                let mut a = Map::new();
                                a.insert("kind".into(), json!("packed"));
                                a.insert("width".into(), json!(arm.layout.backing_bits));
                                a.insert("bits".into(), Value::Object(Self::write_packed_bits(arm.layout)));
                                arms.insert(key, Value::Object(a));
                            }
                        }
                    }
                    obj.insert("arms".into(), Value::Object(arms));
                }
            }
            bits.insert(name.into(), Value::Object(obj));
        }
        bits
    }

    fn write_field(decl: &TypeDecl, tagged: Option<&TaggedUnion>, f: &Field, decls: &[TypeDecl]) -> Value {
        let mut obj = Map::new();
        obj.insert("offset".into(), json!(f.offset));
        obj.insert("size".into(), json!(f.size));
        match f.ty {
            FieldType::Named(name) => {
                obj.insert("type".into(), json!(name));
            }
            FieldType::Array { elem, count } => {
                obj.insert("type".into(), json!("array"));
                obj.insert("elem".into(), json!(elem));
                obj.insert("count".into(), json!(count));
            }
            FieldType::Pointer {
                elem,
                is_const,
                nullable,
            } => {
                obj.insert("type".into(), json!("pointer"));
                obj.insert("elem".into(), json!(elem));
                obj.insert("const".into(), json!(is_const));
                if nullable {
                    obj.insert("nullable".into(), json!(true));
                }
            }
        }
        if let Some(tagged) = tagged {
            if f.name == tagged.value_field {
                obj.insert("tag".into(), json!(tagged.tag_field));
                obj.insert("arms".into(), Value::Object(Self::write_tagged_arms(decl, tagged, decls)));
            }
        }
        Value::Object(obj)
    }

    fn write_tagged_arms(decl: &TypeDecl, tagged: &TaggedUnion, decls: &[TypeDecl]) -> Map<String, Value> {
        let TypeInfo::Struct { fields, .. } = &decl.info else {
            unreachable!()
        };
        let field_type_name = |name: &str| -> &'static str {
            match fields.iter().find(|f| f.name == name).map(|f| f.ty) {
                Some(FieldType::Named(n)) => n,
                _ => unreachable!("tagged union fields have named types"),
            }
        };
        let tag_type = field_type_name(tagged.tag_field);
        let value_type = field_type_name(tagged.value_field);
        let find = |name: &str| decls.iter().find(|d| d.name == name);

        let mut arms = Map::new();
        match &tagged.arm_source {
            ArmSource::Generated(list) => {
                for (tag, arm) in list {
                    arms.insert(
                        Self::enum_object_field(tag_type, tag),
                        match arm {
                            Some(a) => json!(a),
                            None => Value::Null,
                        },
                    );
                }
            }
            ArmSource::Fields => {
                let Some(TypeInfo::Enum { values, .. }) = find(tag_type).map(|d| &d.info) else {
                    unreachable!("tag type is a registered enum")
                };
                let Some(TypeInfo::Union { fields: value_fields, .. }) = find(value_type).map(|d| &d.info) else {
                    unreachable!("value type is a registered union")
                };
                for (tag, _) in values {
                    let arm = value_fields
                        .iter()
                        .find(|f| f.name == tag)
                        .filter(|f| f.size != 0)
                        .map(|f| f.name);
                    arms.insert(
                        Self::enum_object_field(tag_type, tag),
                        match arm {
                            Some(a) => json!(a),
                            None => Value::Null,
                        },
                    );
                }
            }
        }
        arms
    }

    fn write_size_align(obj: &mut Map<String, Value>, size: usize, align: usize) {
        obj.insert("size".into(), json!(size));
        obj.insert("align".into(), json!(align));
    }

    fn upper(name: &str) -> String {
        name.to_ascii_uppercase()
    }

    /// The public key for enum tag `zig_name` of `enum_name`
    /// (Zig: `writeEnumObjectField`).
    pub fn enum_object_field(enum_name: &str, zig_name: &str) -> String {
        if enum_name == "GhosttyKey" {
            if let Some(rest) = zig_name.strip_prefix("key_") {
                return Self::upper(rest);
            }
        }
        if enum_name == "GhosttySgrAttributeTag" {
            match zig_name {
                "256_underline_color" => return "UNDERLINE_COLOR_256".into(),
                "8_bg" => return "BG_8".into(),
                "8_fg" => return "FG_8".into(),
                "8_bright_bg" => return "BRIGHT_BG_8".into(),
                "8_bright_fg" => return "BRIGHT_FG_8".into(),
                "256_bg" => return "BG_256".into(),
                "256_fg" => return "FG_256".into(),
                _ => {}
            }
        }
        if enum_name == "GhosttyTerminalOption" && zig_name == "size_cb" {
            return "SIZE".into();
        }
        Self::upper(zig_name)
    }

    /// Whether `name` is a builtin manifest type (Zig: `isBuiltinType`).
    pub fn is_builtin_type(name: &str) -> bool {
        const BUILTINS: [&str; 16] = [
            "array", "bool", "f32", "f64", "function", "i8", "i16", "i32", "i64", "opaque", "pointer", "u8",
            "u16", "u32", "u64", "void",
        ];
        if BUILTINS.contains(&name) {
            return true;
        }
        let b = name.as_bytes();
        if b.len() > 1 && (b[0] == b'i' || b[0] == b'u') && (b'1'..=b'9').contains(&b[1]) {
            return b[2..].iter().all(|c| c.is_ascii_digit());
        }
        false
    }
}

/// `@tagName(builtin.target.abi)`.
fn target_environment() -> &'static str {
    if cfg!(target_env = "gnu") {
        "gnu"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "msvc") {
        "msvc"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::{cell_c_layout, Cell, ContentTag as CellContentTag};

    fn parsed() -> Value {
        let s = JSON.to_str().unwrap();
        serde_json::from_str(s).unwrap()
    }

    fn obj(v: &Value) -> &Map<String, Value> {
        v.as_object().unwrap()
    }

    #[test]
    fn manifest_parses_and_is_versioned() {
        let parsed = parsed();
        let root = obj(&parsed);
        assert_eq!(1, root["schema"].as_i64().unwrap());
        assert!(root.contains_key("abi"));
        assert_eq!(
            c_abi::MAX_ALIGNMENT as i64,
            root["abi"]["max_alignment"].as_i64().unwrap()
        );
        assert!(root.contains_key("library_version"));
        let manifest_types = obj(&root["types"]);
        let decls = type_decls();
        assert_eq!(decls.len(), manifest_types.len());
        for decl in &decls {
            assert!(manifest_types.contains_key(decl.name));
        }

        // The C entry point returns the same static string every time.
        assert_eq!(get_json(), get_json());
    }

    #[test]
    fn manifest_parses_and_is_versioned_render_cursor() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);
        let cursor_fields = obj(&manifest_types["GhosttyRenderStateCursor"]["fields"]);
        for f in [
            "size",
            "viewport_has_value",
            "viewport_x",
            "viewport_y",
            "wide_tail",
            "visible",
            "blinking",
            "password_input",
            "visual_style",
        ] {
            assert!(cursor_fields.contains_key(f), "{f}");
        }
    }

    #[test]
    fn manifest_describes_enums_arrays_and_tagged_unions() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        // (Zig checks GhosttyRenderStateData and GhosttyRenderStateColors
        // here; see the render test below.)
        let mask = obj(&manifest_types["GhosttyColorPaletteMask"]);
        let bits = obj(&mask["fields"]["bits"]);
        assert_eq!("array", bits["type"].as_str().unwrap());
        assert_eq!("u64", bits["elem"].as_str().unwrap());
        assert_eq!(4, bits["count"].as_i64().unwrap());

        let style_color = obj(&manifest_types["GhosttyStyleColor"]);
        let value = obj(&style_color["fields"]["value"]);
        assert_eq!("GhosttyStyleColorValue", value["type"].as_str().unwrap());
        assert_eq!("tag", value["tag"].as_str().unwrap());
        assert_eq!("palette", value["arms"]["PALETTE"].as_str().unwrap());
        assert!(value["arms"]["NONE"].is_null());
    }

    #[test]
    fn manifest_describes_enums_arrays_and_tagged_unions_render() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        let data = obj(&manifest_types["GhosttyRenderStateData"]);
        assert_eq!("enum", data["kind"].as_str().unwrap());
        assert!(data["values"]["DIRTY"].is_i64());

        let colors = obj(&manifest_types["GhosttyRenderStateColors"]);
        let palette = obj(&colors["fields"]["palette"]);
        assert_eq!("array", palette["type"].as_str().unwrap());
        assert_eq!("GhosttyColorRgb", palette["elem"].as_str().unwrap());
        assert_eq!(256, palette["count"].as_i64().unwrap());
    }

    #[test]
    fn manifest_describes_render_state_overscan_types() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        let overscan = obj(&manifest_types["GhosttyRenderStateOverscan"]);
        assert_eq!(4, overscan["size"].as_i64().unwrap());
        let overscan_fields = obj(&overscan["fields"]);
        assert_eq!(0, overscan_fields["above"]["offset"].as_i64().unwrap());
        assert_eq!(2, overscan_fields["below"]["offset"].as_i64().unwrap());

        let row_id = obj(&manifest_types["GhosttyRenderStateRowId"]);
        assert_eq!(16, row_id["size"].as_i64().unwrap());
        let bits = obj(&row_id["fields"]["bits"]);
        assert_eq!(0, bits["offset"].as_i64().unwrap());
        assert_eq!("array", bits["type"].as_str().unwrap());
        assert_eq!(2, bits["count"].as_i64().unwrap());

        let data_values = obj(&manifest_types["GhosttyRenderStateData"]["values"]);
        assert_eq!(20, data_values["OVERSCAN"].as_i64().unwrap());
        assert_eq!(21, data_values["OVERSCAN_REQUEST"].as_i64().unwrap());

        let row_data_values = obj(&manifest_types["GhosttyRenderStateRowData"]["values"]);
        assert_eq!(6, row_data_values["VIEWPORT_Y"].as_i64().unwrap());
        assert_eq!(7, row_data_values["ID"].as_i64().unwrap());

        let option_values = obj(&manifest_types["GhosttyRenderStateOption"]["values"]);
        assert_eq!(1, option_values["OVERSCAN"].as_i64().unwrap());
    }

    #[test]
    fn manifest_describes_the_complete_packed_cell_layout() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);
        let descriptor = obj(&manifest_types["GhosttyCell"]);

        assert_eq!("packed", descriptor["kind"].as_str().unwrap());
        assert_eq!(std::mem::size_of::<Cell>() as i64, descriptor["size"].as_i64().unwrap());
        assert_eq!("u64", descriptor["underlying"].as_str().unwrap());

        let layout = &cell_c_layout::LAYOUT;
        let bits = obj(&descriptor["bits"]);
        for field in layout.fields {
            match layout.field_name(field.name) {
                Some(public_name) => {
                    let bit = obj(&bits[public_name]);
                    assert_eq!(field.bit_offset as i64, bit["lsb"].as_i64().unwrap());
                    assert_eq!(field.bit_width as i64, bit["width"].as_i64().unwrap());
                }
                None => assert!(!bits.contains_key(field.name)),
            }
        }

        let content = obj(&bits["content"]);
        assert_eq!("union", content["kind"].as_str().unwrap());
        assert_eq!("content_tag", content["tag"].as_str().unwrap());
        let arms = obj(&content["arms"]);

        let codepoint = obj(&arms["CODEPOINT"]);
        let grapheme = obj(&arms["CODEPOINT_GRAPHEME"]);
        expect_packed_arm_field(codepoint, "codepoint", &cell_c_layout::CODEPOINT, "data", "u21");
        expect_packed_arm_field(grapheme, "codepoint", &cell_c_layout::CODEPOINT, "data", "u21");
        expect_packed_arm_field(
            obj(&arms["BG_COLOR_PALETTE"]),
            "index",
            &cell_c_layout::COLOR_PALETTE,
            "data",
            "GhosttyColorPaletteIndex",
        );

        let rgb = obj(&arms["BG_COLOR_RGB"]);
        for field in ["r", "g", "b"] {
            expect_packed_arm_field(rgb, field, &cell_c_layout::COLOR_RGB, field, "u8");
        }
    }

    #[test]
    fn manifest_packed_cell_layouts_decode_real_values() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);
        let cell_bits = obj(&manifest_types["GhosttyCell"]["bits"]);

        let mut codepoint = Cell::init('A' as u32);
        expect_decoded_cell_content(manifest_types, cell_bits, codepoint.to_bits(), "codepoint", 'A' as u64);

        codepoint.set_content_tag(CellContentTag::CodepointGrapheme);
        expect_decoded_cell_content(manifest_types, cell_bits, codepoint.to_bits(), "codepoint", 'A' as u64);

        let mut palette = Cell::from_bits(0);
        palette.set_content_tag(CellContentTag::BgColorPalette);
        palette.set_content_color_palette(173);
        expect_decoded_cell_content(manifest_types, cell_bits, palette.to_bits(), "index", 173);

        let mut rgb = Cell::from_bits(0);
        rgb.set_content_tag(CellContentTag::BgColorRgb);
        rgb.set_content_color_rgb(crate::terminal::color::Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        });
        let rgb_arm = active_cell_content_arm(manifest_types, cell_bits, rgb.to_bits());
        let content = extract_manifest_bits(rgb.to_bits(), obj(&cell_bits["content"]));
        let arm_bits = obj(&rgb_arm["bits"]);
        assert_eq!(0x12, extract_manifest_bits(content, obj(&arm_bits["r"])));
        assert_eq!(0x34, extract_manifest_bits(content, obj(&arm_bits["g"])));
        assert_eq!(0x56, extract_manifest_bits(content, obj(&arm_bits["b"])));
    }

    #[test]
    fn manifest_uses_public_enum_names() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        let cell_content = obj(&manifest_types["GhosttyCellContentTag"]["values"]);
        assert!(cell_content.contains_key("TAG_MAX_VALUE"));
        assert!(!cell_content.contains_key("MAX_VALUE"));

        let sgr_values = obj(&manifest_types["GhosttySgrAttributeTag"]["values"]);
        assert_eq!(9, sgr_values["UNDERLINE_COLOR_256"].as_i64().unwrap());
        assert_eq!(23, sgr_values["BG_8"].as_i64().unwrap());
        assert!(!sgr_values.contains_key("256_UNDERLINE_COLOR"));

        let sgr_arms = obj(&manifest_types["GhosttySgrAttribute"]["fields"]["value"]["arms"]);
        assert_eq!("underline_color_256", sgr_arms["UNDERLINE_COLOR_256"].as_str().unwrap());
        assert_eq!("bg_8", sgr_arms["BG_8"].as_str().unwrap());

        let osc_values = obj(&manifest_types["GhosttyOscCommandType"]["values"]);
        assert_eq!(22, osc_values["KITTY_TEXT_SIZING"].as_i64().unwrap());
        assert_eq!(23, osc_values["KITTY_CLIPBOARD_PROTOCOL"].as_i64().unwrap());
        assert_eq!(24, osc_values["KITTY_DND_PROTOCOL"].as_i64().unwrap());
        assert_eq!(25, osc_values["HIERARCHICAL_CONTEXT"].as_i64().unwrap());
        assert_eq!(26, osc_values["KITTY_DESKTOP_NOTIFICATION"].as_i64().unwrap());
        assert!(osc_values.contains_key("TYPE_MAX_VALUE"));
        assert!(!osc_values.contains_key("MAX_VALUE"));

        let mouse_shape_values = obj(&manifest_types["GhosttyMouseShape"]["values"]);
        assert_eq!(8, mouse_shape_values["TEXT"].as_i64().unwrap());
        assert_eq!(33, mouse_shape_values["ZOOM_OUT"].as_i64().unwrap());
        assert!(mouse_shape_values.contains_key("MAX_VALUE"));

        let key_values = obj(&manifest_types["GhosttyKey"]["values"]);
        assert!(key_values.contains_key("A"));
        assert!(!key_values.contains_key("KEY_A"));

        let mode_values = obj(&manifest_types["GhosttyModeReportState"]["values"]);
        assert_eq!(0, mode_values["NOT_RECOGNIZED"].as_i64().unwrap());
        assert_eq!(4, mode_values["PERMANENTLY_RESET"].as_i64().unwrap());
    }

    #[test]
    fn manifest_uses_public_enum_names_terminal() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        let terminal_values = obj(&manifest_types["GhosttyTerminalOption"]["values"]);
        assert_eq!(6, terminal_values["SIZE"].as_i64().unwrap());
        assert!(!terminal_values.contains_key("SIZE_CB"));
        assert_eq!(42, terminal_values["SEMANTIC_PROMPT"].as_i64().unwrap());
        assert_eq!(43, terminal_values["RESET"].as_i64().unwrap());

        let semantic_prompt_values = obj(&manifest_types["GhosttySemanticPromptKind"]["values"]);
        assert_eq!(0, semantic_prompt_values["INVALID"].as_i64().unwrap());
        assert_eq!(4, semantic_prompt_values["COMMAND_END"].as_i64().unwrap());

        let prompt_kind_values = obj(&manifest_types["GhosttySemanticPromptPromptKind"]["values"]);
        assert_eq!(3, prompt_kind_values["SECONDARY"].as_i64().unwrap());

        let terminal_data_values = obj(&manifest_types["GhosttyTerminalData"]["values"]);
        assert_eq!(41, terminal_data_values["MOUSE_SHAPE"].as_i64().unwrap());
    }

    /// (The render half of Zig's "manifest uses public enum names" test.)
    #[test]
    fn manifest_uses_public_enum_names_render() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        let kitty_kind_values = obj(&manifest_types["GhosttyRenderStateKittyPlacementKind"]["values"]);
        assert_eq!(1, kitty_kind_values["RELATIVE"].as_i64().unwrap());
        assert_eq!(2, kitty_kind_values["FIXED"].as_i64().unwrap());
    }

    #[test]
    fn manifest_named_references_resolve() {
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);

        for descriptor in manifest_types.values() {
            let descriptor = obj(descriptor);
            if let Some(bits_value) = descriptor.get("bits") {
                expect_manifest_bits_valid(
                    manifest_types,
                    obj(bits_value),
                    (descriptor["size"].as_i64().unwrap() * 8) as usize,
                );
            }
            let Some(fields_value) = descriptor.get("fields") else {
                continue;
            };
            for field in obj(fields_value).values() {
                let field = obj(field);
                let field_type = field["type"].as_str().unwrap();
                if !Json::is_builtin_type(field_type) {
                    assert!(manifest_types.contains_key(field_type), "{field_type}");
                }

                if let Some(elem_value) = field.get("elem") {
                    let elem = elem_value.as_str().unwrap();
                    if !Json::is_builtin_type(elem) {
                        assert!(manifest_types.contains_key(elem), "{elem}");
                    }
                }

                if let Some(arms_value) = field.get("arms") {
                    let union_descriptor = obj(&manifest_types[field_type]);
                    let union_fields = obj(&union_descriptor["fields"]);
                    let tag_field_name = field["tag"].as_str().unwrap();
                    let tag_type_name = obj(fields_value)[tag_field_name]["type"].as_str().unwrap();
                    let tag_values = obj(&manifest_types[tag_type_name]["values"]);
                    for (arm_key, arm_value) in obj(arms_value) {
                        assert!(tag_values.contains_key(arm_key), "{arm_key}");
                        if arm_value.is_null() {
                            continue;
                        }
                        assert!(union_fields.contains_key(arm_value.as_str().unwrap()));
                    }
                }
            }
        }
    }

    /// Rust port: every registered enum key must be a constant of the C
    /// headers with the same value (Zig gets this from `checkGhosttyHEnum`
    /// per enum and from the headers being generated alongside).
    #[test]
    fn manifest_enum_values_match_headers() {
        let c = crate::lib::r#enum::parse_c_constants(&super::super::tests::all_headers());
        let parsed = parsed();
        let manifest_types = obj(&parsed["types"]);
        for (name, descriptor) in manifest_types {
            let descriptor = obj(descriptor);
            if descriptor["kind"].as_str().unwrap() != "enum" {
                continue;
            }
            let prefix = descriptor["prefix"].as_str().unwrap();
            for (key, value) in obj(&descriptor["values"]) {
                let value = value.as_i64().unwrap();
                if value == c_int::MAX as i64 {
                    continue;
                }
                let c_name = format!("{prefix}{key}");
                assert_eq!(Some(&value), c.get(&c_name), "{name}: {c_name}");
            }
        }
    }

    fn expect_packed_arm_field(
        arm: &Map<String, Value>,
        manifest_name: &str,
        layout: &PackedLayout,
        zig_name: &str,
        expected_type: &str,
    ) {
        assert_eq!("packed", arm["kind"].as_str().unwrap());
        assert_eq!(layout.backing_bits as i64, arm["width"].as_i64().unwrap());
        let bit = obj(&arm["bits"][manifest_name]);
        assert_eq!(layout.bit_offset(zig_name) as i64, bit["lsb"].as_i64().unwrap());
        assert_eq!(layout.bit_width(zig_name) as i64, bit["width"].as_i64().unwrap());
        assert_eq!(expected_type, bit["type"].as_str().unwrap());
    }

    fn extract_manifest_bits(value: u64, bit: &Map<String, Value>) -> u64 {
        let lsb = bit["lsb"].as_i64().unwrap() as u32;
        let width = bit["width"].as_i64().unwrap() as u32;
        let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
        (value >> lsb) & mask
    }

    fn active_cell_content_arm<'a>(
        manifest_types: &'a Map<String, Value>,
        cell_bits: &'a Map<String, Value>,
        raw: u64,
    ) -> &'a Map<String, Value> {
        let content_tag = obj(&cell_bits["content_tag"]);
        let value = extract_manifest_bits(raw, content_tag);
        let enum_values = obj(&manifest_types[content_tag["type"].as_str().unwrap()]["values"]);
        for (key, v) in enum_values {
            if v.as_i64().unwrap() as u64 == value {
                return obj(&cell_bits["content"]["arms"][key.as_str()]);
            }
        }
        unreachable!()
    }

    fn expect_decoded_cell_content(
        manifest_types: &Map<String, Value>,
        cell_bits: &Map<String, Value>,
        raw: u64,
        field_name: &str,
        expected: u64,
    ) {
        let arm = active_cell_content_arm(manifest_types, cell_bits, raw);
        let content = extract_manifest_bits(raw, obj(&cell_bits["content"]));
        assert_eq!(expected, extract_manifest_bits(content, obj(&arm["bits"][field_name])));
    }

    fn expect_manifest_bits_valid(
        manifest_types: &Map<String, Value>,
        bits: &Map<String, Value>,
        container_width: usize,
    ) {
        for bit in bits.values() {
            let bit = obj(bit);
            let lsb = bit["lsb"].as_i64().unwrap() as usize;
            let width = bit["width"].as_i64().unwrap() as usize;
            assert!(lsb + width <= container_width);

            if let Some(type_value) = bit.get("type") {
                let type_name = type_value.as_str().unwrap();
                assert!(Json::is_builtin_type(type_name) || manifest_types.contains_key(type_name));
                continue;
            }

            let kind = bit["kind"].as_str().unwrap();
            if kind == "packed" {
                expect_manifest_bits_valid(manifest_types, obj(&bit["bits"]), width);
                continue;
            }

            assert_eq!("union", kind);
            let tag_name = bit["tag"].as_str().unwrap();
            let tag = obj(&bits[tag_name]);
            let enum_values = obj(&manifest_types[tag["type"].as_str().unwrap()]["values"]);
            for (arm_key, arm_value) in obj(&bit["arms"]) {
                assert!(enum_values.contains_key(arm_key));
                if arm_value.is_null() {
                    continue;
                }
                let arm = obj(arm_value);
                let arm_width = arm["width"].as_i64().unwrap() as usize;
                assert!(arm_width <= width);
                expect_manifest_bits_valid(manifest_types, obj(&arm["bits"]), arm_width);
            }
        }
    }
}
