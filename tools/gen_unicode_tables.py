#!/usr/bin/env python3
"""Generate the Unicode lookup tables used by `src/unicode/`.

This replaces two build-time steps of the Zig build:

1. The `uucode` package (jacobsandlund/uucode, the revision pinned in
   `ghostty/build.zig.zon`) which parses the Unicode Character Database and
   computes per-codepoint fields, configured by
   `ghostty/src/build/uucode_config.zig` (which adds Ghostty's own `width` and
   `is_symbol` fields).
2. `src/unicode/props_uucode.zig` / `src/unicode/symbols_uucode.zig`, which
   turn those fields into the 3-level lookup tables of `src/unicode/lut.zig`.

The output is committed (generated files are never produced at cargo build
time):

* `src/unicode/uucode_tables.rs`      — the uucode fields Ghostty reads.
* `src/unicode/props_table_data.rs`   — `props_table.rs` (Properties LUT).
* `src/unicode/symbols_table_data.rs` — `symbols_table.rs` (is_symbol LUT).
* `src/unicode/testdata/GraphemeBreakTest.txt` — conformance test data.

Usage:

    tools/gen_unicode_tables.py [--ucd-dir DIR]

UCD files are downloaded from unicode.org into DIR (default: a temporary
directory) unless they already exist there.
"""

import argparse
import os
import shutil
import sys
import tempfile
import urllib.request

UNICODE_VERSION = "18.0.0"
UCD_URL = f"https://www.unicode.org/Public/{UNICODE_VERSION}/ucd/"

UCD_FILES = [
    "UnicodeData.txt",
    "DerivedCoreProperties.txt",
    "Blocks.txt",
    "CaseFolding.txt",
    "extracted/DerivedEastAsianWidth.txt",
    "auxiliary/GraphemeBreakProperty.txt",
    "auxiliary/GraphemeBreakTest.txt",
    "emoji/emoji-data.txt",
    "emoji/emoji-variation-sequences.txt",
]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT_DIR = os.path.join(ROOT, "src", "unicode")

MAX_CODE_POINT = 0x10FFFF
NUM_CODE_POINTS = MAX_CODE_POINT + 1
# lut.zig iterates every u21 value.
LUT_END = 0x1FFFFF + 1

ZERO_WIDTH_NON_JOINER = 0x200C
ZERO_WIDTH_JOINER = 0x200D

# uucode `types.GraphemeBreak`, in declaration order.
GRAPHEME_BREAK = [
    "other",
    "control",
    "prepend",
    "cr",
    "lf",
    "regional_indicator",
    "spacing_mark",
    "l",
    "v",
    "t",
    "lv",
    "lvt",
    "zwj",
    "zwnj",
    "extended_pictographic",
    "emoji_modifier_base",
    "emoji_modifier",
    "indic_conjunct_break_extend",
    "indic_conjunct_break_linker_extend",
    "indic_conjunct_break_linker_other",
    "indic_conjunct_break_consonant",
]

# uucode `types.GraphemeBreakNoControl`, in declaration order.
GRAPHEME_BREAK_NO_CONTROL = [
    g for g in GRAPHEME_BREAK if g not in ("control", "cr", "lf")
]

ORIGINAL_GRAPHEME_BREAK_MAP = {
    "Prepend": "prepend",
    "CR": "cr",
    "LF": "lf",
    "Control": "control",
    "Extend": "extend",
    "Regional_Indicator": "regional_indicator",
    "SpacingMark": "spacing_mark",
    "L": "l",
    "V": "v",
    "T": "t",
    "LV": "lv",
    "LVT": "lvt",
    "ZWJ": "zwj",
}

EAW_MAP = {
    "F": "fullwidth",
    "H": "halfwidth",
    "W": "wide",
    "Na": "narrow",
    "A": "ambiguous",
    "N": "neutral",
}

SYMBOL_BLOCKS = {
    "Arrows",
    "Dingbats",
    "Emoticons",
    "Miscellaneous Symbols",
    "Enclosed Alphanumerics",
    "Enclosed Alphanumeric Supplement",
    "Miscellaneous Symbols and Pictographs",
    "Transport and Map Symbols",
}


def camel(name):
    return "".join(p[:1].upper() + p[1:] for p in name.split("_"))


# ---------------------------------------------------------------------------
# UCD parsing (port of the relevant uucode `src/components.zig` parts)


def fetch(ucd_dir):
    for f in UCD_FILES:
        path = os.path.join(ucd_dir, f)
        if os.path.exists(path):
            continue
        os.makedirs(os.path.dirname(path), exist_ok=True)
        url = UCD_URL + f
        print(f"downloading {url}", file=sys.stderr)
        with urllib.request.urlopen(url) as r, open(path + ".tmp", "wb") as out:
            shutil.copyfileobj(r, out)
        os.replace(path + ".tmp", path)


def read_lines(ucd_dir, name):
    with open(os.path.join(ucd_dir, name), encoding="utf-8") as f:
        return f.read().split("\n")


def trim(line):
    idx = line.find("#")
    if idx >= 0:
        line = line[:idx]
    return line.strip(" \t\r")


def parse_range(s):
    if ".." in s:
        a, b = s.split("..")
        return int(a, 16), int(b, 16) + 1
    cp = int(s, 16)
    return cp, cp + 1


def check_version(ucd_dir, name):
    with open(os.path.join(ucd_dir, name), encoding="utf-8") as f:
        head = f.read(200)
    base = os.path.basename(name)[: -len(".txt")]
    if base in ("emoji-data", "emoji-variation-sequences", "UnicodeData"):
        return
    if f"{base}-{UNICODE_VERSION}" not in head:
        sys.exit(f"{name}: expected Unicode {UNICODE_VERSION}, got: {head!r}")


def parse_unicode_data(ucd_dir):
    """General category; default `Cn` (uucode `other_not_assigned`)."""
    gc = ["Cn"] * NUM_CODE_POINTS
    range_gc = None
    range_start = None
    for line in read_lines(ucd_dir, "UnicodeData.txt"):
        t = trim(line)
        if not t:
            continue
        parts = t.split(";")
        cp = int(parts[0], 16)
        if range_gc is not None:
            assert parts[1].endswith("Last>")
            for c in range(range_start, cp + 1):
                gc[c] = range_gc
            range_gc = None
            continue
        gc[cp] = parts[2]
        if parts[1].endswith("First>"):
            range_gc = parts[2]
            range_start = cp
    return gc


def parse_derived_core(ucd_dir):
    default_ignorable = [False] * NUM_CODE_POINTS
    incb = ["none"] * NUM_CODE_POINTS
    for line in read_lines(ucd_dir, "DerivedCoreProperties.txt"):
        t = trim(line)
        if not t:
            continue
        parts = [p.strip(" \t\r") for p in t.split(";")]
        start, end = parse_range(parts[0])
        prop = parts[1]
        if prop == "Default_Ignorable_Code_Point":
            for c in range(start, end):
                default_ignorable[c] = True
        elif prop == "InCB":
            value = {"Linker": "linker", "Consonant": "consonant", "Extend": "extend"}[
                parts[2]
            ]
            for c in range(start, end):
                incb[c] = value
    return default_ignorable, incb


def parse_east_asian_width(ucd_dir):
    eaw = ["neutral"] * NUM_CODE_POINTS
    for line in read_lines(ucd_dir, "extracted/DerivedEastAsianWidth.txt"):
        t = line.strip(" \t\r")
        if not t:
            continue
        if t.startswith("# @missing:"):
            parts = [p.strip(" \t\r") for p in t[len("# @missing:") :].split(";")]
            start, end = parse_range(parts[0])
            if parts[1] == "Neutral":
                continue
            assert parts[1] == "Wide", parts[1]
            for c in range(start, end):
                eaw[c] = "wide"
            continue
        d = trim(t)
        if not d:
            continue
        parts = [p.strip(" \t\r") for p in d.split(";")]
        start, end = parse_range(parts[0])
        value = EAW_MAP[parts[1]]
        for c in range(start, end):
            eaw[c] = value
    return eaw


def parse_grapheme_break_property(ucd_dir):
    gb = ["other"] * NUM_CODE_POINTS
    for line in read_lines(ucd_dir, "auxiliary/GraphemeBreakProperty.txt"):
        t = trim(line)
        if not t:
            continue
        parts = [p.strip(" \t\r") for p in t.split(";")]
        start, end = parse_range(parts[0])
        value = ORIGINAL_GRAPHEME_BREAK_MAP.get(parts[1], "other")
        for c in range(start, end):
            gb[c] = value
    return gb


def parse_emoji_data(ucd_dir):
    props = {
        "Emoji": [False] * NUM_CODE_POINTS,
        "Emoji_Presentation": [False] * NUM_CODE_POINTS,
        "Emoji_Modifier": [False] * NUM_CODE_POINTS,
        "Emoji_Modifier_Base": [False] * NUM_CODE_POINTS,
        "Emoji_Component": [False] * NUM_CODE_POINTS,
        "Extended_Pictographic": [False] * NUM_CODE_POINTS,
    }
    for line in read_lines(ucd_dir, "emoji/emoji-data.txt"):
        t = trim(line)
        if not t:
            continue
        parts = [p.strip(" \t\r") for p in t.split(";")]
        start, end = parse_range(parts[0])
        arr = props[parts[1]]
        for c in range(start, end):
            arr[c] = True
    return props


def parse_emoji_vs(ucd_dir):
    vs_base = [False] * NUM_CODE_POINTS
    for line in read_lines(ucd_dir, "emoji/emoji-variation-sequences.txt"):
        t = trim(line)
        if not t:
            continue
        parts = t.split(" ")
        cp = int(parts[0], 16)
        vs = int(parts[1], 16)
        # Only "text style" lines count; the "emoji style" lines are 1:1.
        if vs == 0xFE0E:
            vs_base[cp] = True
        else:
            assert vs == 0xFE0F and vs_base[cp]
    return vs_base


def parse_blocks(ucd_dir):
    block = [None] * NUM_CODE_POINTS
    for line in read_lines(ucd_dir, "Blocks.txt"):
        t = trim(line)
        if not t:
            continue
        parts = [p.strip(" \t\r") for p in t.split(";")]
        start, end = parse_range(parts[0])
        for c in range(start, end):
            block[c] = parts[1]
    return block


def parse_case_folding(ucd_dir):
    """Returns {cp: mapping} for `case_folding_full` where mapping != [cp]."""
    common = {}
    full = {}
    for line in read_lines(ucd_dir, "CaseFolding.txt"):
        t = trim(line)
        if not t:
            continue
        parts = [p.strip(" \t\r") for p in t.split(";")]
        cp = int(parts[0], 16)
        status = parts[1]
        mapping = [int(x, 16) for x in parts[2].split(" ") if x]
        if status == "C":
            assert len(mapping) == 1
            common[cp] = mapping
        elif status == "F":
            full[cp] = mapping
    result = {}
    for cp in sorted(set(common) | set(full)):
        mapping = full.get(cp) or common[cp]
        if mapping != [cp]:
            result[cp] = mapping
    return result


# ---------------------------------------------------------------------------
# Derived fields (port of uucode `GraphemeBreakDerived`, `Wcwidth`, and
# Ghostty's `uucode_config.zig` `WidthComponent` / `IsSymbolComponent`).


def derive(ucd_dir):
    for f in UCD_FILES:
        if f.endswith(".txt"):
            check_version(ucd_dir, f)

    gc = parse_unicode_data(ucd_dir)
    default_ignorable, incb = parse_derived_core(ucd_dir)
    eaw = parse_east_asian_width(ucd_dir)
    original_gb = parse_grapheme_break_property(ucd_dir)
    emoji = parse_emoji_data(ucd_dir)
    vs_base = parse_emoji_vs(ucd_dir)
    block = parse_blocks(ucd_dir)
    case_folding = parse_case_folding(ucd_dir)

    is_emoji_modifier = emoji["Emoji_Modifier"]
    is_emoji_modifier_base = emoji["Emoji_Modifier_Base"]
    is_extended_pictographic = emoji["Extended_Pictographic"]
    is_emoji_component = emoji["Emoji_Component"]

    grapheme_break = [None] * NUM_CODE_POINTS
    for cp in range(NUM_CODE_POINTS):
        o = original_gb[cp]
        if is_emoji_modifier[cp]:
            assert o == "extend"
            assert not is_extended_pictographic[cp] and is_emoji_component[cp]
            g = "emoji_modifier"
        elif is_emoji_modifier_base[cp]:
            assert o == "other"
            assert is_extended_pictographic[cp]
            g = "emoji_modifier_base"
        elif is_extended_pictographic[cp]:
            assert o == "other"
            g = "extended_pictographic"
        else:
            i = incb[cp]
            if i == "none":
                if o == "extend":
                    assert cp == ZERO_WIDTH_NON_JOINER, hex(cp)
                    g = "zwnj"
                else:
                    g = o
            elif i == "extend":
                if cp == ZERO_WIDTH_JOINER:
                    assert o == "zwj"
                    g = "zwj"
                else:
                    assert o == "extend"
                    g = "indic_conjunct_break_extend"
            elif i == "linker":
                g = {
                    "extend": "indic_conjunct_break_linker_extend",
                    "other": "indic_conjunct_break_linker_other",
                }[o]
            else:
                assert i == "consonant" and o == "other"
                g = "indic_conjunct_break_consonant"
        grapheme_break[cp] = g

    grapheme_break_no_control = [
        "other" if g in ("control", "cr", "lf") else g for g in grapheme_break
    ]

    wcwidth_standalone = [0] * NUM_CODE_POINTS
    wcwidth_zero_in_grapheme = [False] * NUM_CODE_POINTS
    for cp in range(NUM_CODE_POINTS):
        c = gc[cp]
        if c in ("Cc", "Cs", "Zl", "Zp"):
            width = 0
        elif cp == 0x00AD:
            width = 1
        elif default_ignorable[cp]:
            width = 0
        elif cp == 0x2E3A:
            width = 2
        elif cp == 0x2E3B:
            width = 3
        elif eaw[cp] in ("wide", "fullwidth"):
            width = 2
        elif grapheme_break[cp] == "regional_indicator":
            width = 2
        else:
            width = 1

        wcwidth_standalone[cp] = 2 if cp == 0x20E3 else width
        wcwidth_zero_in_grapheme[cp] = (
            width == 0
            or is_emoji_modifier[cp]
            or c == "Mn"
            or c == "Me"
            or grapheme_break[cp] in ("v", "t", "prepend")
        )

    # Ghostty's `width` field (uucode_config.zig WidthComponent).
    ghostty_width = [0] * NUM_CODE_POINTS
    for cp in range(NUM_CODE_POINTS):
        if (
            wcwidth_zero_in_grapheme[cp]
            and not is_emoji_modifier[cp]
            and grapheme_break_no_control[cp] != "prepend"
        ):
            ghostty_width[cp] = 0
        else:
            ghostty_width[cp] = min(2, wcwidth_standalone[cp])

    # Ghostty's `is_symbol` field (uucode_config.zig IsSymbolComponent).
    is_symbol = [gc[cp] == "Co" or block[cp] in SYMBOL_BLOCKS for cp in range(NUM_CODE_POINTS)]

    return {
        "width": ghostty_width,
        "wcwidth_zero_in_grapheme": wcwidth_zero_in_grapheme,
        "grapheme_break": grapheme_break,
        "grapheme_break_no_control": grapheme_break_no_control,
        "is_emoji_vs_base": vs_base,
        "is_symbol": is_symbol,
        "is_emoji_presentation": emoji["Emoji_Presentation"],
        "case_folding_full": case_folding,
    }


# ---------------------------------------------------------------------------
# lut.zig Generator


def generate_lut(get):
    """Port of `lut.Generator.generate`. `get` returns a hashable elem."""
    blocks_map = {}
    stage1 = []
    stage2 = []
    stage3 = []
    stage3_index = {}
    block = []
    for cp in range(LUT_END):
        elem = get(cp)
        idx = stage3_index.get(elem)
        if idx is None:
            idx = len(stage3)
            stage3.append(elem)
            stage3_index[elem] = idx
        assert idx <= 0xFFFF, "BlockTooLarge"
        block.append(idx)
        if len(block) < 256 and cp != LUT_END - 1:
            continue
        block_len = len(block)
        key = tuple(block + [0] * (256 - block_len))
        found = blocks_map.get(key)
        if found is None:
            found = len(stage2)
            assert found <= 0xFFFF, "Stage2TooLarge"
            blocks_map[key] = found
            stage2.extend(block)
        stage1.append(found)
        block = []
    assert len(stage1) <= 0xFFFF
    assert len(stage2) <= 0xFFFF
    assert len(stage3) <= 0xFFFF
    return stage1, stage2, stage3


def fmt_u16_array(name, values, per_line=24):
    out = [f"pub static {name}: [u16; {len(values)}] = ["]
    for i in range(0, len(values), per_line):
        out.append("    " + ", ".join(str(v) for v in values[i : i + per_line]) + ",")
    out.append("];")
    return "\n".join(out)


HEADER = """\
// @generated by tools/gen_unicode_tables.py from the Unicode {version}
// Character Database. DO NOT EDIT.
//
// {desc}
"""


def write_lut(path, desc, elem_type, tables, fmt_elem, prefix=""):
    stage1, stage2, stage3 = tables
    parts = [HEADER.format(version=UNICODE_VERSION, desc=desc)]
    parts.append(fmt_u16_array(f"{prefix}STAGE1", stage1))
    parts.append("")
    parts.append(fmt_u16_array(f"{prefix}STAGE2", stage2))
    parts.append("")
    parts.append(f"pub static {prefix}STAGE3: [{elem_type}; {len(stage3)}] = [")
    for e in stage3:
        parts.append(f"    {fmt_elem(e)},")
    parts.append("];")
    parts.append("")
    with open(path, "w") as f:
        f.write("\n".join(parts))
    print(
        f"wrote {os.path.relpath(path, ROOT)}: stage1={len(stage1)} "
        f"stage2={len(stage2)} stage3={len(stage3)}",
        file=sys.stderr,
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ucd-dir", help="directory holding (or receiving) UCD files")
    args = ap.parse_args()

    tmp = None
    ucd_dir = args.ucd_dir
    if ucd_dir is None:
        tmp = tempfile.mkdtemp(prefix="ucd-")
        ucd_dir = tmp
    try:
        fetch(ucd_dir)
        d = derive(ucd_dir)

        width = d["width"]
        zig = d["wcwidth_zero_in_grapheme"]
        gb = d["grapheme_break"]
        gbnc = d["grapheme_break_no_control"]
        vs = d["is_emoji_vs_base"]
        sym = d["is_symbol"]
        pres = d["is_emoji_presentation"]

        # uucode fields Ghostty reads (the "buildtime" and "runtime" tables
        # of uucode_config.zig, minus case folding which is stored below).
        def uucode_get(cp):
            if cp > MAX_CODE_POINT:
                return (0, False, "other", False, False, False)
            return (width[cp], zig[cp], gb[cp], vs[cp], sym[cp], pres[cp])

        def fmt_row(r):
            return (
                f"Row {{ width: {r[0]}, wcwidth_zero_in_grapheme: {str(r[1]).lower()}, "
                f"grapheme_break: GraphemeBreak::{camel(r[2])}, "
                f"is_emoji_vs_base: {str(r[3]).lower()}, is_symbol: {str(r[4]).lower()}, "
                f"is_emoji_presentation: {str(r[5]).lower()} }}"
            )

        uu_path = os.path.join(OUT_DIR, "uucode_tables.rs")
        write_lut(
            uu_path,
            "uucode per-codepoint fields used by Ghostty (see uucode.rs).",
            "Row",
            generate_lut(uucode_get),
            fmt_row,
        )
        cf = d["case_folding_full"]
        with open(uu_path, "a") as f:
            f.write(
                "/// `case_folding_full` for every codepoint whose full case folding is\n"
                "/// not the codepoint itself, sorted by codepoint. Unused slots are 0.\n"
            )
            f.write(f"pub static CASE_FOLDING_FULL: [(u32, u8, [u32; 3]); {len(cf)}] = [\n")
            for cp, m in cf.items():
                assert 1 <= len(m) <= 3
                padded = m + [0] * (3 - len(m))
                f.write(
                    f"    (0x{cp:X}, {len(m)}, [{', '.join(f'0x{x:X}' for x in padded)}]),\n"
                )
            f.write("];\n")

        # props_uucode.zig `get`.
        def props_get(cp):
            if cp > MAX_CODE_POINT:
                return (1, True, "other", False)
            return (width[cp], zig[cp], gbnc[cp], vs[cp])

        def fmt_props(p):
            return (
                f"Properties {{ width: {p[0]}, width_zero_in_grapheme: {str(p[1]).lower()}, "
                f"grapheme_break: GraphemeBreakNoControl::{camel(p[2])}, "
                f"emoji_vs_base: {str(p[3]).lower()} }}"
            )

        write_lut(
            os.path.join(OUT_DIR, "props_table_data.rs"),
            "Output of the props_uucode.rs generator (Properties lookup table).",
            "Properties",
            generate_lut(props_get),
            fmt_props,
        )

        # symbols_uucode.zig `get`.
        def symbols_get(cp):
            return False if cp > MAX_CODE_POINT else sym[cp]

        write_lut(
            os.path.join(OUT_DIR, "symbols_table_data.rs"),
            "Output of the symbols_uucode.rs generator (is_symbol lookup table).",
            "bool",
            generate_lut(symbols_get),
            lambda b: str(b).lower(),
        )

        testdata = os.path.join(OUT_DIR, "testdata")
        os.makedirs(testdata, exist_ok=True)
        shutil.copyfile(
            os.path.join(ucd_dir, "auxiliary/GraphemeBreakTest.txt"),
            os.path.join(testdata, "GraphemeBreakTest.txt"),
        )
    finally:
        if tmp is not None:
            shutil.rmtree(tmp)


if __name__ == "__main__":
    main()
