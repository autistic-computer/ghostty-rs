#!/usr/bin/env bash
# Build and install libghostty-vt, the standalone terminal C library.
#
# This is the cargo counterpart of `zig build -Demit-lib-vt`
# (`src/build/GhosttyLibVt.zig`, `src/build/GhosttyPkgConfig.zig`,
# `src/build/libghostty-vt.pc.in`). It installs, Zig-style:
#
#   <prefix>/lib/libghostty-vt.so.<major>.<minor>.<patch>  (soname libghostty-vt.so.<major>)
#   <prefix>/lib/libghostty-vt.so.<major> -> libghostty-vt.so.<major>.<minor>.<patch>
#   <prefix>/lib/libghostty-vt.so -> libghostty-vt.so.<major>
#   <prefix>/lib/libghostty-vt.a
#   <prefix>/include/ghostty/vt.h, <prefix>/include/ghostty/vt/**.h
#   <prefix>/share/pkgconfig/libghostty-vt.pc         (shared)
#   <prefix>/share/pkgconfig/libghostty-vt-static.pc  (static)
#
# Both libraries export ONLY the libghostty-vt C API: the `ghostty_*`
# functions defined in `src/terminal/c/**` and `src/lib_vt.rs` (declared in
# `include/ghostty/vt*.h`). How:
#
#  1. `cargo rustc --crate-type staticlib` builds the crate (no default
#     features, so no GTK) into a separate target dir (`target/lib-vt`), so
#     the normal `cargo build` is neither slowed down nor changed. With
#     `GHOSTTY_LIB_VT=1`, `build.rs` skips the app's C libraries (freetype,
#     harfbuzz, fontconfig, oniguruma, EGL, glad, stb): like the Zig lib-vt,
#     the result only needs libc (+ libgcc_s for Rust's unwinder).
#  2. `ld -r --gc-sections` partially links the archive into one relocatable
#     object rooted at the exported functions, dropping everything the C API
#     does not reach (the app, renderer, fonts, the `ghostty_app_*`/
#     `ghostty_surface_*`/`ghostty_config_*` embedding API, ...).
#  3. `objcopy --keep-global-symbols` makes every other symbol (Rust std,
#     compiler builtins, crate internals) local, so the static library
#     can't clash with other Rust code linked into the same program.
#  4. That object becomes `libghostty-vt.a` and, linked with a version
#     script listing the same symbols, `libghostty-vt.so`.
#
# Usage: tools/build_lib_vt.sh [--debug] [--target <rust-triple>] [prefix]
#
#   prefix              install prefix (default: target/lib-vt/out)
#   --debug             build with the dev profile (Zig's -Doptimize=Debug);
#                       the default is the release profile (ReleaseFast)
#   --target <triple>   cargo target triple (Linux/ELF targets only); uses
#                       $CC, $LD and $OBJCOPY for the link steps
#
# Environment:
#   GHOSTTY_LIB_VT_VERSION     library version (Zig: -Dlib-version-string),
#                              default 0.1.0-dev as in build.zig
#   GHOSTTY_LIB_VT_TARGET_DIR  cargo target dir (default: target/lib-vt)
#   GHOSTTY_LIB_VT_CARGO_FLAGS extra flags for `cargo rustc`
#   CARGO                      cargo binary (default: cargo)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

profile=release
target=""
prefix=""
while [ $# -gt 0 ]; do
    case "$1" in
    --debug) profile=dev ;;
    --release) profile=release ;;
    --target)
        target="$2"
        shift
        ;;
    --target=*) target="${1#--target=}" ;;
    -h | --help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    -*)
        echo "build_lib_vt.sh: unknown option $1" >&2
        exit 2
        ;;
    *) prefix="$1" ;;
    esac
    shift
done

version="${GHOSTTY_LIB_VT_VERSION:-0.1.0-dev}"
if ! [[ "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "build_lib_vt.sh: GHOSTTY_LIB_VT_VERSION must be a semantic version: $version" >&2
    exit 2
fi
major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"

target_dir="${GHOSTTY_LIB_VT_TARGET_DIR:-$ROOT/target/lib-vt}"
prefix="${prefix:-$target_dir/out}"
mkdir -p "$prefix"
prefix="$(cd "$prefix" && pwd)"
CARGO="${CARGO:-cargo}"
CC="${CC:-cc}"
LD="${LD:-ld}"
OBJCOPY="${OBJCOPY:-objcopy}"
NM="${NM:-nm}"
AR="${AR:-ar}"

# 1. The crate as a static library.
cargo_args=(rustc --manifest-path "$ROOT/Cargo.toml" --lib --crate-type staticlib
    --no-default-features --profile "$profile" --target-dir "$target_dir")
out_dir="$target_dir"
if [ -n "$target" ]; then
    cargo_args+=(--target "$target")
    out_dir="$out_dir/$target"
fi
[ "$profile" = dev ] && out_dir="$out_dir/debug" || out_dir="$out_dir/$profile"
# shellcheck disable=SC2086
GHOSTTY_LIB_VT=1 GHOSTTY_LIB_VT_VERSION="$version" "$CARGO" "${cargo_args[@]}" ${GHOSTTY_LIB_VT_CARGO_FLAGS:-}
archive="$out_dir/libghostty.a"

work="$out_dir/libghostty-vt"
rm -rf "$work"
mkdir -p "$work"

# The exported API: every `ghostty_*` C function of src/terminal/c and
# src/lib_vt.rs that this target defines (the Wasm conveniences are
# wasm32-only, as in Zig).
grep -rhoE 'export_name = "ghostty_[a-z0-9_]+"' "$ROOT/src/terminal/c" "$ROOT/src/lib_vt.rs" |
    sed -E 's/.*"(.*)"/\1/' | sort -u >"$work/api.txt"
"$NM" --defined-only -g "$archive" 2>/dev/null | awk 'NF == 3 { print $3 }' | sort -u >"$work/defined.txt"
comm -12 "$work/api.txt" "$work/defined.txt" >"$work/exports.txt"
if [ ! -s "$work/exports.txt" ]; then
    echo "build_lib_vt.sh: no ghostty_* C API symbols found in $archive" >&2
    exit 1
fi

# Every exported function must be declared (`GHOSTTY_API`) by the public
# headers, and every declared function must be exported (the Wasm
# conveniences only exist on wasm32).
find "$ROOT/include/ghostty/vt.h" "$ROOT/include/ghostty/vt" -name '*.h' -exec cat {} + | tr '\n' ' ' |
    grep -oE 'GHOSTTY_API[^;(]*\bghostty_[a-z0-9_]+[[:space:]]*\(' |
    grep -oE 'ghostty_[a-z0-9_]+[[:space:]]*\($' | grep -oE 'ghostty_[a-z0-9_]+' |
    sort -u >"$work/header.txt"
undeclared="$(comm -23 "$work/exports.txt" "$work/header.txt")"
if [ -n "$undeclared" ]; then
    echo "build_lib_vt.sh: exported but not declared in include/ghostty/vt*.h:" >&2
    echo "$undeclared" >&2
    exit 1
fi
missing="$(comm -13 "$work/exports.txt" "$work/header.txt" | grep -v '^ghostty_wasm_' || true)"
if [ -n "$missing" ]; then
    echo "build_lib_vt.sh: declared in include/ghostty/vt*.h but not exported:" >&2
    echo "$missing" >&2
    exit 1
fi

# 2. + 3. One relocatable object holding only what the C API reaches, with
# only the C API global.
"$LD" -r --gc-sections $(sed 's/^/--require-defined=/' "$work/exports.txt") \
    -o "$work/ghostty-vt.o" "$archive"
"$OBJCOPY" --keep-global-symbols="$work/exports.txt" "$work/ghostty-vt.o"

# 4a. The static library.
"$AR" rcsD "$work/libghostty-vt.a" "$work/ghostty-vt.o"

# 4b. The shared library, with the soname/versioning scheme of Zig's
# `addLibrary(.{ .version = ... })`.
{
    echo "{"
    echo "  global:"
    sed 's/^/    /; s/$/;/' "$work/exports.txt"
    echo "  local: *;"
    echo "};"
} >"$work/libghostty-vt.map"
real_name="libghostty-vt.so.$major.$minor.$patch"
soname="libghostty-vt.so.$major"
# Rust's std needs these on Linux (`rustc --print native-static-libs`);
# --as-needed keeps only the ones the C API actually uses.
sys_libs=(-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc)
"$CC" -shared -o "$work/$real_name" "$work/ghostty-vt.o" \
    -Wl,-soname,"$soname" \
    -Wl,--version-script="$work/libghostty-vt.map" \
    -Wl,--gc-sections -Wl,--as-needed -Wl,-z,relro,-z,now -Wl,-z,noexecstack \
    -Wl,--no-undefined \
    "${sys_libs[@]}"

# Libs.private of the static library: what the shared one ended up needing.
libs_private=""
for needed in $(readelf -d "$work/$real_name" | sed -nE 's/.*\(NEEDED\).*\[lib([^].]+)\.so.*\]/\1/p'); do
    case "$needed" in
    c | ld-linux*) ;;
    *) libs_private="$libs_private -l$needed" ;;
    esac
done
libs_private="${libs_private# }"

# Install.
mkdir -p "$prefix/lib" "$prefix/include" "$prefix/share/pkgconfig"
install -m 0755 "$work/$real_name" "$prefix/lib/$real_name"
ln -sf "$real_name" "$prefix/lib/$soname"
ln -sf "$soname" "$prefix/lib/libghostty-vt.so"
install -m 0644 "$work/libghostty-vt.a" "$prefix/lib/libghostty-vt.a"
rm -rf "$prefix/include/ghostty"
mkdir -p "$prefix/include/ghostty"
(cd "$ROOT/include/ghostty" && find . -name '*.h' | while read -r h; do
    install -D -m 0644 "$h" "$prefix/include/ghostty/$h"
done)

# pkg-config (src/build/libghostty-vt.pc.in / src/extra/pkg-config.zig).
pc() { # name description libs
    cat <<EOF
prefix=$prefix
includedir=\${prefix}/include
libdir=\${prefix}/lib

Name: $1
URL: https://github.com/ghostty-org/ghostty
Description: $2
Version: $version
Cflags: -I\${includedir}
Libs: $3
Libs.private: $libs_private
Requires.private:
EOF
}
pc ghostty-vt "Ghostty VT library" "-L\${libdir} -lghostty-vt" \
    >"$prefix/share/pkgconfig/libghostty-vt.pc"
pc ghostty-vt-static "Ghostty VT library (static)" "\${libdir}/libghostty-vt.a" \
    >"$prefix/share/pkgconfig/libghostty-vt-static.pc"

echo "libghostty-vt $version: exports $(wc -l <"$work/exports.txt") functions (the headers declare $(grep -vc '^ghostty_wasm_' "$work/header.txt") + $(grep -c '^ghostty_wasm_' "$work/header.txt") wasm32-only) -> $prefix"
