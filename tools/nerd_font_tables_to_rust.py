#!/usr/bin/env python3
"""Convert ghostty's generated `src/font/nerd_font_tables.zig` to Rust.

The Zig file is itself generated (by nerd_font_codegen.py) and contains a
generic `Tables(comptime Elem: type)` with three arrays: `stage1` and
`stage2` (plain `u16` data) and `stage3` (`[N]Elem`, where each element is
`null` or an anonymous struct literal that coerces to
`?font.Glyph.RenderOptions.Constraint`).

The Rust output exposes `STAGE1`/`STAGE2` as statics and `stage3` as a
`macro_rules!` that expands to an `[Option<Constraint>; N]` expression for a
caller-provided `Constraint` type (and its `Size`/`Align`/`Height` enums),
mirroring the comptime generic: omitted fields come from `Constraint::NONE`.

Usage:
    tools/nerd_font_tables_to_rust.py [ghostty/src/font/nerd_font_tables.zig] \
        > src/font/nerd_font_tables.rs
"""

import re
import sys

ENUM_FIELDS = {
    "size": "$Size",
    "height": "$Height",
    "align_horizontal": "$Align",
    "align_vertical": "$Align",
}
FLOAT_FIELDS = {
    "pad_left", "pad_right", "pad_top", "pad_bottom",
    "relative_width", "relative_height", "relative_x", "relative_y",
}
OPTIONAL_FLOAT_FIELDS = {"max_xy_ratio"}
INT_FIELDS = {"max_constraint_width"}


def camel(tag):
    return "".join(p[:1].upper() + p[1:] for p in tag.split("_"))


def float_lit(v):
    float(v)  # validate
    if not re.search(r"[.eE]", v):
        v += ".0"
    return v


def parse_u16_array(src, name):
    m = re.search(r"pub const %s: \[(\d+)\]u16 = \.\{([^}]*)\};" % name, src)
    if not m:
        raise SystemExit("could not find %s" % name)
    n = int(m.group(1))
    vals = [int(v) for v in m.group(2).replace("\n", " ").split(",") if v.strip()]
    if len(vals) != n:
        raise SystemExit("%s: expected %d values, got %d" % (name, n, len(vals)))
    return vals


def parse_stage3(src):
    m = re.search(r"pub const stage3: \[(\d+)\]Elem = \.\{\n(.*?)\n        \};", src, re.S)
    if not m:
        raise SystemExit("could not find stage3")
    n = int(m.group(1))
    body = m.group(2)
    entries = []
    pos = 0
    item_re = re.compile(r"\s*(null|\.\{(.*?)\n\s*\}),", re.S)
    while pos < len(body):
        im = item_re.match(body, pos)
        if not im:
            if body[pos:].strip() == "":
                break
            raise SystemExit("unexpected stage3 content at: %r" % body[pos:pos + 80])
        pos = im.end()
        if im.group(1) == "null":
            entries.append(None)
            continue
        fields = []
        for line in im.group(2).strip().split("\n"):
            fm = re.match(r"\s*\.(\w+) = (.*),$", line)
            if not fm:
                raise SystemExit("bad field line: %r" % line)
            fields.append((fm.group(1), fm.group(2)))
        entries.append(fields)
    if len(entries) != n:
        raise SystemExit("stage3: expected %d entries, got %d" % (n, len(entries)))
    return entries


def field_expr(name, value):
    if name in ENUM_FIELDS:
        em = re.match(r"\.(\w+)$", value)
        if not em:
            raise SystemExit("bad enum value for %s: %s" % (name, value))
        return "%s::%s" % (ENUM_FIELDS[name], camel(em.group(1)))
    if name in FLOAT_FIELDS:
        return float_lit(value)
    if name in OPTIONAL_FLOAT_FIELDS:
        return "Some(%s)" % float_lit(value)
    if name in INT_FIELDS:
        return str(int(value))
    raise SystemExit("unknown field %s" % name)


def fmt_u16_array(vals, indent="    ", per_line=24):
    lines = []
    for i in range(0, len(vals), per_line):
        lines.append(indent + ", ".join(str(v) for v in vals[i:i + per_line]) + ",")
    return "\n".join(lines)


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "ghostty/src/font/nerd_font_tables.zig"
    src = open(path, encoding="utf-8").read()
    stage1 = parse_u16_array(src, "stage1")
    stage2 = parse_u16_array(src, "stage2")
    stage3 = parse_stage3(src)

    out = []
    w = out.append
    w("//! Port of `src/font/nerd_font_tables.zig`.")
    w("//!")
    w("//! STATUS: ported")
    w("//!")
    w("//! This is a generated file, produced by tools/nerd_font_tables_to_rust.py")
    w("//! from the Zig file (itself produced by nerd_font_codegen.py).")
    w("//! DO NOT EDIT BY HAND!")
    w("//!")
    w("//! 3-level lookup from a codepoint to a Nerd Font glyph constraint.")
    w("//! The layout matches src/unicode/lut.zig. stage3 index 0 is null.")
    w("//! Stage2 offset 0 is the shared page with no constrained glyphs.")
    w("//!")
    w("//! The Zig `Tables(comptime Elem: type)` is generic so the stage3 struct")
    w("//! literals coerce to `?Constraint`; here `STAGE1`/`STAGE2` are plain")
    w("//! statics and [`stage3!`] expands to the `[Option<Constraint>; %d]`" % len(stage3))
    w("//! array for the given `Constraint` type, with omitted fields taken")
    w("//! from `Constraint::NONE`.")
    w("")
    w("/// Length of `STAGE1`: one entry for every high byte of a u21.")
    w("pub const STAGE1_LEN: usize = %d;" % len(stage1))
    w("/// Length of `STAGE2`.")
    w("pub const STAGE2_LEN: usize = %d;" % len(stage2))
    w("/// Length of the array produced by [`stage3!`].")
    w("pub const STAGE3_LEN: usize = %d;" % len(stage3))
    w("")
    w("#[rustfmt::skip]")
    w("pub static STAGE1: [u16; STAGE1_LEN] = [")
    w(fmt_u16_array(stage1))
    w("];")
    w("")
    w("#[rustfmt::skip]")
    w("pub static STAGE2: [u16; STAGE2_LEN] = [")
    w(fmt_u16_array(stage2))
    w("];")
    w("")
    w("/// Expands to the stage3 array (`[Option<$Constraint>; STAGE3_LEN]`).")
    w("///")
    w("/// `$Constraint` must be a struct type with a `NONE` constant (all")
    w("/// defaults) and the fields used by the table; `$Size`, `$Align` and")
    w("/// `$Height` are its enum types (Zig `Constraint.Size` etc).")
    w("#[rustfmt::skip]")
    w("#[allow(unused_macros)] // used by nerd_font_attributes")
    w("macro_rules! stage3 {")
    w("    ($Constraint:ident, $Size:ident, $Align:ident, $Height:ident) => {")
    w("        [")
    for entry in stage3:
        if entry is None:
            w("            None,")
            continue
        w("            Some($Constraint {")
        for name, value in entry:
            w("                %s: %s," % (name, field_expr(name, value)))
        w("                ..$Constraint::NONE")
        w("            }),")
    w("        ]")
    w("    };")
    w("}")
    w("#[allow(unused_imports)]")
    w("pub(crate) use stage3;")
    w(TESTS.replace("@N@", str(len(stage3))).rstrip("\n"))
    sys.stdout.write("\n".join(out) + "\n")


# The Zig file has no tests; these check the invariants that
# nerd_font_attributes.zig asserts at comptime, and that the macro
# expands against a Constraint shaped like `Glyph.RenderOptions.Constraint`.
TESTS = r'''
#[cfg(test)]
mod tests {
    use super::*;

    use crate::font::glyph::{
        Constraint, ConstraintAlign as Align, ConstraintHeight as Height, ConstraintSize as Size,
    };

    static STAGE3: [Option<Constraint>; STAGE3_LEN] = stage3!(Constraint, Size, Align, Height);

    #[test]
    fn layout_invariants() {
        // One stage1 entry for every high byte of a u21.
        assert_eq!(0x2000, STAGE1.len());
        // stage3[0] must be null.
        assert!(!STAGE3.is_empty());
        assert_eq!(None, STAGE3[0]);
        // The empty nerd-font page must be stage2 offset 0.
        assert!(STAGE2[0..256].iter().all(|&e| e == 0));
        // Every stage1 entry is a page offset, every stage2 entry a stage3 index.
        assert!(STAGE1.iter().all(|&o| o as usize % 256 == 0 && (o as usize) < STAGE2.len()));
        assert!(STAGE2.iter().all(|&i| (i as usize) < STAGE3.len()));
        assert_eq!(@N@, STAGE3_LEN);
    }

    #[test]
    fn stage3_entries() {
        assert_eq!(
            Some(Constraint {
                size: Size::FitCover1,
                height: Height::Icon,
                align_horizontal: Align::Center1,
                align_vertical: Align::Center1,
                ..Constraint::NONE
            }),
            STAGE3[1],
        );
        assert_eq!(
            Some(Constraint {
                size: Size::Cover,
                height: Height::Icon,
                max_constraint_width: 1,
                align_horizontal: Align::Center1,
                align_vertical: Align::Center1,
                pad_left: 0.05,
                pad_right: 0.05,
                pad_top: 0.05,
                pad_bottom: 0.05,
                ..Constraint::NONE
            }),
            STAGE3[2],
        );
        // All entries but the first are constraints.
        assert!(STAGE3[1..].iter().all(|c| c.is_some()));
        // Every enum variant used by the generator is reachable.
        let _ = (Size::Fit, Align::Start, Align::End, Align::Center, Size::Stretch);
    }
}
'''


if __name__ == "__main__":
    main()
