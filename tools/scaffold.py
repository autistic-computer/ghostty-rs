#!/usr/bin/env python3
"""Generate the Rust module skeleton that mirrors ghostty's Zig `src/` tree.

Every `ghostty/src/**/*.zig` file gets a Rust counterpart under `src/` with a
snake_case name (e.g. `src/terminal/PageList.zig` -> `src/terminal/page_list.rs`).
Module declarations live in a `// @generated-mods` block at the top of each
module file so ports can fill in the rest of the file freely.

Re-running is safe: existing files are never overwritten, only the generated
mods block is refreshed. Also writes `PORTING_STATUS.tsv`.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# (zig tree, rust tree, root module file name, excluded top-level dirs)
# `src/build` and `pkg/apple-sdk` are Zig build-system code; they map to
# Cargo/build.rs instead of modules. `build.zig` files are excluded too.
TREES = [
    ("src", "src", "root.rs", {"build"}),
    ("pkg", "pkg", "mod.rs", {"apple-sdk"}),
]

SPECIAL = {"OpenGL": "opengl", "TinyIo": "tinyio", "CApi": "c_api"}
KEYWORDS = {
    "as", "break", "const", "continue", "crate", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    "async", "await", "dyn", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield",
    "try", "gen",
}


def snake(name: str) -> str:
    if name in SPECIAL:
        return SPECIAL[name]
    name = name.replace("-", "_")
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    s = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", s)
    return s.lower()


def ident(mod: str) -> str:
    return f"r#{mod}" if mod in KEYWORDS else mod


def scaffold(tree, rs_tree, root_file, EXCLUDE_DIRS, status):
    ZIG_SRC = os.path.join(ROOT, "ghostty", tree)
    RS_SRC = os.path.join(ROOT, rs_tree)
    zig_files = []
    for dirpath, dirnames, filenames in os.walk(ZIG_SRC):
        rel = os.path.relpath(dirpath, ZIG_SRC)
        top = rel.split(os.sep)[0]
        if top in EXCLUDE_DIRS:
            dirnames[:] = []
            continue
        for f in filenames:
            if f.endswith(".zig") and f != "build.zig":
                zig_files.append(os.path.normpath(os.path.join(rel, f)))
    zig_files.sort()

    # Directories that contain zig files (transitively).
    dirs = set()
    for z in zig_files:
        d = os.path.dirname(z)
        while d:
            dirs.add(d)
            d = os.path.dirname(d)

    def rs_dir(zdir: str) -> str:
        return os.path.join(*[snake(p) for p in zdir.split(os.sep)]) if zdir else ""

    # module file (rust relative path) for each zig dir, plus the zig file
    # that defines it (if any).
    dir_mod = {}
    for d in sorted(dirs):
        parent, base = os.path.split(d)
        sib = os.path.join(parent, base + ".zig")
        main_zig = os.path.join(d, "main.zig")
        collide = [z for z in zig_files if os.path.dirname(z) == parent
                   and snake(os.path.basename(z)[:-4]) == snake(base)
                   and z != sib]
        if sib in zig_files:
            dir_mod[d] = (os.path.join(rs_dir(parent), snake(base) + ".rs"), sib)
        elif main_zig in zig_files:
            dir_mod[d] = (os.path.join(rs_dir(d), "main.rs"), main_zig)
        elif collide:
            dir_mod[d] = (os.path.join(rs_dir(parent), snake(base) + ".rs"), collide[0])
        else:
            dir_mod[d] = (os.path.join(rs_dir(d), "mod.rs"), None)

    zig_to_rs = {}
    for z in zig_files:
        d = os.path.dirname(z)
        base = os.path.basename(z)[:-4]
        zig_to_rs[z] = os.path.join(rs_dir(d), snake(base) + ".rs")
    for d, (rs, z) in dir_mod.items():
        if z:
            zig_to_rs[z] = rs

    # collisions
    seen = {}
    for z, r in zig_to_rs.items():
        if r in seen and seen[r] != z:
            sys.exit(f"collision: {z} and {seen[r]} -> {r}")
        seen[r] = z

    # children of each module file: list of (ident, path attr or None)
    children = {root_file: []}
    for d, (rs, _) in dir_mod.items():
        children.setdefault(rs, [])

    def module_file_of_dir(d):
        return root_file if d == "" else dir_mod[d][0]

    def add_child(parent_dir, name, rs_path):
        pmod = module_file_of_dir(parent_dir)
        # #[path] on a non-inline module is relative to the directory of the
        # declaring file, so always emit it explicitly.
        rel = os.path.relpath(rs_path, os.path.dirname(pmod) or ".")
        children[pmod].append((name, rel))

    # files
    for z in zig_files:
        d = os.path.dirname(z)
        rs = zig_to_rs[z]
        if any(dm[1] == z for dm in dir_mod.values()):
            continue  # this file is a dir module; added via dir below
        if z == "main.zig" and tree == "src":
            continue  # binary entry point
        add_child(d, snake(os.path.basename(z)[:-4]), rs)
    for d, (rs, _) in dir_mod.items():
        parent = os.path.dirname(d)
        add_child(parent, snake(os.path.basename(d)), rs)

    # Write files
    def gen_block(mods):
        lines = ["// @generated-mods-begin (tools/scaffold.py; do not edit by hand)"]
        for name, rel in sorted(mods):
            if rel:
                lines.append(f'#[path = "{rel}"]')
            lines.append(f"pub mod {ident(name)};")
        lines.append("// @generated-mods-end")
        return "\n".join(lines) + "\n"

    zig_of_rs = {v: k for k, v in zig_to_rs.items()}
    all_rs = set(zig_to_rs.values()) | set(children.keys())
    for rs in sorted(all_rs):
        path = os.path.join(RS_SRC, rs)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        mods = children.get(rs, [])
        block = gen_block(mods) if mods else ""
        if os.path.exists(path):
            text = open(path).read()
            if block:
                if "// @generated-mods-begin" in text:
                    text = re.sub(r"// @generated-mods-begin.*?// @generated-mods-end\n",
                                  lambda _: block, text, flags=re.S)
                    open(path, "w").write(text)
            continue
        z = zig_of_rs.get(rs)
        if rs == root_file and tree == "src":
            header = ("//! Crate root for the Rust port of Ghostty.\n"
                      "//!\n//! The module tree mirrors ghostty's `src/` directory. See PORTING.md.\n\n"
                      "#![allow(dead_code, unused_imports, unused_variables, clippy::all)]\n\n")
        elif z:
            header = f"//! Port of `src/{z}`.\n//!\n//! STATUS: not yet ported.\n\n"
        else:
            header = "//! Module grouping (no Zig counterpart file; mirrors a directory).\n\n"
        open(path, "w").write(header + block)

    for z in zig_files:
        n = sum(1 for _ in open(os.path.join(ZIG_SRC, z)))
        status.append(f"{tree}/{z}\t{rs_tree}/{zig_to_rs[z]}\t{n}\n")


def main():
    status = []
    for t in TREES:
        scaffold(*t, status)
    with open(os.path.join(ROOT, "PORTING_STATUS.tsv"), "w") as f:
        f.write("zig\trust\tlines\n")
        f.writelines(status)


if __name__ == "__main__":
    main()
