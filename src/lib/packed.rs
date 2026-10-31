//! Port of `src/lib/packed.zig`.
//!
//! STATUS: ported
//!
//! Metadata describing the bit layout of packed (bit-field) structs for
//! public C API metadata.
//!
//! # Usage for later ports
//!
//! Zig reflects bit offsets and widths from a `packed struct(uN)` type.
//! Rust has no bit-packed structs; ports represent them with explicit bit
//! packing on an integer (see PORTING.md). The physical layout is
//! therefore declared once as a list of [`FieldSpec`]s in declaration
//! order (least significant bits first, like Zig) and [`packed_fields`]
//! computes each field's bit offset at compile time. [`PackedLayout::new`]
//! and [`PackedTaggedUnion::new`] are `const fn`s that perform the same
//! validation as the Zig `comptime` blocks; a violation fails compilation
//! when used to initialize a `static`/`const`.
//!
//! ```ignore
//! static FIELDS: [PackedField; 2] = packed_fields([
//!     FieldSpec::scalar("enabled", "bool", 1),
//!     FieldSpec::scalar("_pad", "u7", 7).omit(),
//! ]);
//! pub static LAYOUT: PackedLayout = PackedLayout::new("Flags", 8, &FIELDS);
//! ```
//!
//! Fields are identified by their Zig/Rust field name string (Zig uses
//! `std.meta.FieldEnum`); unknown names panic.

/// How the field is represented in public metadata.
#[derive(Debug, Clone, Copy)]
pub enum Encoding {
    Scalar,
    Packed(&'static PackedLayout),
    TaggedUnion(&'static PackedTaggedUnion),
}

/// Metadata for a field in a packed layout.
#[derive(Debug, Clone, Copy)]
pub struct FieldOptions {
    /// Public name for this field. The field name is used by default.
    pub name: Option<&'static str>,

    /// Public type name for scalar fields. The field type is used by default.
    pub type_name: Option<&'static str>,

    /// Omit this field from public metadata. This is intended for padding.
    pub omit: bool,

    /// How the field is represented in public metadata.
    pub encoding: Encoding,
}

impl FieldOptions {
    pub const DEFAULT: FieldOptions = FieldOptions {
        name: None,
        type_name: None,
        omit: false,
        encoding: Encoding::Scalar,
    };
}

impl Default for FieldOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Physical description of one field of a packed struct, in declaration
/// order, plus its public options.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    /// The field name in the packed struct.
    pub name: &'static str,
    /// The field's type name (used to match nested layouts).
    pub ty: &'static str,
    /// Bit width of the field (`@bitSizeOf`).
    pub bits: usize,
    /// True for `bool`, integer and enum fields, which can be encoded as
    /// scalars without an explicit encoding.
    pub scalar: bool,
    pub options: FieldOptions,
}

impl FieldSpec {
    /// A `bool`/integer/enum field.
    pub const fn scalar(name: &'static str, ty: &'static str, bits: usize) -> FieldSpec {
        FieldSpec {
            name,
            ty,
            bits,
            scalar: true,
            options: FieldOptions::DEFAULT,
        }
    }

    /// A field whose type is a nested packed struct or packed union.
    pub const fn compound(name: &'static str, ty: &'static str, bits: usize) -> FieldSpec {
        FieldSpec {
            name,
            ty,
            bits,
            scalar: false,
            options: FieldOptions::DEFAULT,
        }
    }

    pub const fn with_options(mut self, options: FieldOptions) -> FieldSpec {
        self.options = options;
        self
    }

    pub const fn name(mut self, name: &'static str) -> FieldSpec {
        self.options.name = Some(name);
        self
    }

    pub const fn type_name(mut self, type_name: &'static str) -> FieldSpec {
        self.options.type_name = Some(type_name);
        self
    }

    pub const fn omit(mut self) -> FieldSpec {
        self.options.omit = true;
        self
    }

    pub const fn encoding(mut self, encoding: Encoding) -> FieldSpec {
        self.options.encoding = encoding;
        self
    }
}

/// A field with its computed bit offset.
#[derive(Debug, Clone, Copy)]
pub struct PackedField {
    pub name: &'static str,
    pub ty: &'static str,
    pub bit_offset: usize,
    pub bit_width: usize,
    pub scalar: bool,
    pub options: FieldOptions,
}

/// Compute bit offsets for fields declared in order (first field in the
/// least significant bits, like Zig packed structs).
pub const fn packed_fields<const N: usize>(specs: [FieldSpec; N]) -> [PackedField; N] {
    let mut out = [PackedField {
        name: "",
        ty: "",
        bit_offset: 0,
        bit_width: 0,
        scalar: true,
        options: FieldOptions::DEFAULT,
    }; N];
    let mut offset = 0;
    let mut i = 0;
    while i < N {
        out[i] = PackedField {
            name: specs[i].name,
            ty: specs[i].ty,
            bit_offset: offset,
            bit_width: specs[i].bits,
            scalar: specs[i].scalar,
            options: specs[i].options,
        };
        offset += specs[i].bits;
        i += 1;
    }
    out
}

const fn str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Metadata for a packed struct. Physical layout information comes from
/// the field specs; options only provide public names and relationships.
#[derive(Debug)]
pub struct PackedLayout {
    /// The packed struct type name (Zig: `Zig`).
    pub zig: &'static str,
    /// Width of the backing integer (Zig: `Backing`).
    pub backing_bits: usize,
    pub fields: &'static [PackedField],
}

impl PackedLayout {
    /// Create and validate a layout. I know this is an insane amount of
    /// validation but getting this correct is critical to C APIs working
    /// so we go overboard.
    pub const fn new(
        zig: &'static str,
        backing_bits: usize,
        fields: &'static [PackedField],
    ) -> PackedLayout {
        let mut total = 0;
        let mut i = 0;
        while i < fields.len() {
            let field = &fields[i];
            total += field.bit_width;
            let options = &field.options;
            i += 1;

            if options.omit {
                if options.name.is_some()
                    || options.type_name.is_some()
                    || !matches!(options.encoding, Encoding::Scalar)
                {
                    panic!("omitted packed field has other options");
                }
                continue;
            }

            if let Some(name) = options.name {
                if name.is_empty() {
                    panic!("packed field name cannot be empty");
                }
            }

            match options.encoding {
                Encoding::Scalar => {
                    if !field.scalar {
                        panic!("packed field requires an explicit encoding");
                    }
                }
                Encoding::Packed(layout) => {
                    if !str_eq(layout.zig, field.ty) {
                        panic!("nested packed layout has the wrong Zig type");
                    }
                    if options.type_name.is_some() {
                        panic!("nested packed field cannot have a scalar type name");
                    }
                }
                Encoding::TaggedUnion(layout) => {
                    if !str_eq(layout.owner, zig)
                        || !str_eq(layout.union_ty, field.ty)
                        || !str_eq(layout.union_field, field.name)
                    {
                        panic!("tagged union layout does not match packed field");
                    }
                    if options.type_name.is_some() {
                        panic!("tagged union field cannot have a scalar type name");
                    }
                }
            }

            let public_name = match options.name {
                Some(n) => n,
                None => field.name,
            };
            let mut j = 0;
            while j < i - 1 {
                let previous = &fields[j];
                j += 1;
                if previous.options.omit {
                    continue;
                }
                let previous_name = match previous.options.name {
                    Some(n) => n,
                    None => previous.name,
                };
                if str_eq(public_name, previous_name) {
                    panic!("duplicate public packed field name");
                }
            }
        }

        if total != backing_bits {
            panic!("Packed requires an integer-backed packed struct");
        }

        PackedLayout {
            zig,
            backing_bits,
            fields,
        }
    }

    fn field(&self, field: &str) -> &PackedField {
        self.fields
            .iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("unknown packed field: {field}"))
    }

    pub fn field_options(&self, field: &str) -> FieldOptions {
        self.field(field).options
    }

    pub fn field_name(&self, field: &str) -> Option<&'static str> {
        let f = self.field(field);
        if f.options.omit {
            return None;
        }
        Some(f.options.name.unwrap_or(f.name))
    }

    pub fn bit_offset(&self, field: &str) -> usize {
        self.field(field).bit_offset
    }

    pub fn bit_width(&self, field: &str) -> usize {
        self.field(field).bit_width
    }
}

/// One arm of a packed tagged union: the source union field and the
/// reflected packed layout of that arm.
#[derive(Debug, Clone, Copy)]
pub struct Arm {
    /// The union field this arm reads.
    pub source: &'static str,
    /// The union field's type name.
    pub source_ty: &'static str,
    pub layout: &'static PackedLayout,
}

/// Describe a packed union field whose active arm is selected by another
/// field in the containing packed struct. Each tag maps to a reflected
/// packed arm layout. Multiple tags may map to the same source union field.
#[derive(Debug)]
pub struct PackedTaggedUnion {
    /// The packed struct containing both fields (Zig: `Owner`).
    pub owner: &'static str,
    /// The packed union type (Zig: `Union`).
    pub union_ty: &'static str,
    /// Bit width of the packed union.
    pub union_bits: usize,
    pub union_field: &'static str,
    pub tag_field: &'static str,
    /// For every tag name (in tag order), its arm if any.
    pub arms: &'static [(&'static str, Option<Arm>)],
}

impl PackedTaggedUnion {
    pub const fn new(
        owner: &'static str,
        union_ty: &'static str,
        union_bits: usize,
        union_field: &'static str,
        tag_field: &'static str,
        arms: &'static [(&'static str, Option<Arm>)],
    ) -> PackedTaggedUnion {
        let mut i = 0;
        while i < arms.len() {
            if let Some(arm) = arms[i].1 {
                if !str_eq(arm.layout.zig, arm.source_ty) {
                    panic!("packed union arm layout has the wrong Zig type for tag");
                }
                if arm.layout.backing_bits > union_bits {
                    panic!("packed union arm is wider than its union");
                }
            }
            i += 1;
        }

        PackedTaggedUnion {
            owner,
            union_ty,
            union_bits,
            union_field,
            tag_field,
            arms,
        }
    }

    pub fn arm(&self, tag: &str) -> Option<Arm> {
        self.arms
            .iter()
            .find(|(t, _)| *t == tag)
            .unwrap_or_else(|| panic!("unknown tag: {tag}"))
            .1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // const Tag = enum(u2) { first, second, third };
    // const Value = packed union {
    //     number: packed struct(u4) { data: u3, _pad: u1 },
    //     flags: packed struct(u4) { a: bool, b: bool, _pad: u2 },
    // };
    // const Nested = packed struct(u1) { enabled: bool };
    // const Subject = packed struct(u8) {
    //     tag: Tag, value: Value, nested: Nested, _padding: u1,
    // };

    static NUMBER_FIELDS: [PackedField; 2] = packed_fields([
        FieldSpec::scalar("data", "u3", 3).name("number"),
        FieldSpec::scalar("_pad", "u1", 1).omit(),
    ]);
    static NUMBER: PackedLayout = PackedLayout::new("Number", 4, &NUMBER_FIELDS);

    static FLAGS_FIELDS: [PackedField; 3] = packed_fields([
        FieldSpec::scalar("a", "bool", 1),
        FieldSpec::scalar("b", "bool", 1),
        FieldSpec::scalar("_pad", "u2", 2).omit(),
    ]);
    static FLAGS: PackedLayout = PackedLayout::new("Flags", 4, &FLAGS_FIELDS);

    static NESTED_FIELDS: [PackedField; 1] =
        packed_fields([FieldSpec::scalar("enabled", "bool", 1)]);
    static NESTED_LAYOUT: PackedLayout = PackedLayout::new("Nested", 1, &NESTED_FIELDS);

    static VALUE_LAYOUT: PackedTaggedUnion = PackedTaggedUnion::new(
        "Subject",
        "Value",
        4,
        "value",
        "tag",
        &[
            (
                "first",
                Some(Arm {
                    source: "number",
                    source_ty: "Number",
                    layout: &NUMBER,
                }),
            ),
            (
                "second",
                Some(Arm {
                    source: "number",
                    source_ty: "Number",
                    layout: &NUMBER,
                }),
            ),
            (
                "third",
                Some(Arm {
                    source: "flags",
                    source_ty: "Flags",
                    layout: &FLAGS,
                }),
            ),
        ],
    );

    static SUBJECT_FIELDS: [PackedField; 4] = packed_fields([
        FieldSpec::scalar("tag", "Tag", 2),
        FieldSpec::compound("value", "Value", 4).encoding(Encoding::TaggedUnion(&VALUE_LAYOUT)),
        FieldSpec::compound("nested", "Nested", 1).encoding(Encoding::Packed(&NESTED_LAYOUT)),
        FieldSpec::scalar("_padding", "u1", 1).omit(),
    ]);
    static LAYOUT: PackedLayout = PackedLayout::new("Subject", 8, &SUBJECT_FIELDS);

    #[test]
    fn packed_reflected_fields_and_nested_tagged_union() {
        assert_eq!(2, LAYOUT.bit_offset("value"));
        assert_eq!(4, LAYOUT.bit_width("value"));
        assert_eq!("number", NUMBER.field_name("data").unwrap());
        assert!(NUMBER.field_name("_pad").is_none());
        match VALUE_LAYOUT.arm("first").unwrap() {
            Arm {
                source: "number",
                layout,
                ..
            } => assert!(std::ptr::eq(layout, &NUMBER)),
            _ => panic!("TestUnexpectedResult"),
        }
        match VALUE_LAYOUT.arm("second").unwrap() {
            Arm {
                source: "number",
                layout,
                ..
            } => assert!(std::ptr::eq(layout, &NUMBER)),
            _ => panic!("TestUnexpectedResult"),
        }
        assert_eq!(1, NESTED_LAYOUT.bit_width("enabled"));
        assert_eq!(6, LAYOUT.bit_offset("nested"));
        assert!(matches!(
            LAYOUT.field_options("nested").encoding,
            Encoding::Packed(_)
        ));
    }
}
