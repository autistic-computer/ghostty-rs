#!/usr/bin/env bash
# Regenerates the bindgen FFI bindings (`pkg/<lib>/c_bindings.rs`) for the C
# library wrappers in pkg/ (the Rust counterpart of the Zig packages'
# translate-c / @cImport step). Requires `bindgen` (cargo install bindgen-cli),
# libclang and the system development headers (via pkg-config).
#
# usage: tools/gen_pkg_bindings.sh [all|freetype|harfbuzz|fontconfig|oniguruma|opengl]
set -euo pipefail
cd "$(dirname "$0")/.."
BG="${BINDGEN:-bindgen}"
COMMON=(--no-layout-tests --no-prepend-enum-name --no-doc-comments --merge-extern-blocks)
which="${1:-all}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [[ $which == all || $which == freetype ]]; then
  # Same includes as ghostty/pkg/freetype/freetype-zig.h
  printf '%s\n' '#include <ft2build.h>' '#include FT_FREETYPE_H' \
    '#include FT_TRUETYPE_TABLES_H' '#include <freetype/ftmm.h>' \
    '#include <freetype/ftoutln.h>' '#include <freetype/ftsnames.h>' \
    '#include <freetype/ttnameid.h>' '#include <freetype/ftbitmap.h>' \
    '#include <freetype/ftbbox.h>' > "$tmp/ft.h"
  "$BG" "$tmp/ft.h" "${COMMON[@]}" \
    --allowlist-item 'FT_.*|TT_.*|FREETYPE_.*|ft_.*|TTAG_.*' \
    --default-macro-constant-type signed \
    -o pkg/freetype/c_bindings.rs -- $(pkg-config --cflags freetype2)
fi

if [[ $which == all || $which == harfbuzz ]]; then
  printf '%s\n' '#include <hb.h>' '#include <hb-ft.h>' > "$tmp/hb.h"
  "$BG" "$tmp/hb.h" "${COMMON[@]}" \
    --allowlist-item 'hb_.*|HB_.*' \
    --blocklist-item 'FT_.*' --blocklist-type 'FT_.*' \
    --raw-line 'use crate::pkg::freetype::c::FT_Face;' \
    -o pkg/harfbuzz/c_bindings.rs -- $(pkg-config --cflags harfbuzz freetype2)
fi

if [[ $which == all || $which == fontconfig ]]; then
  printf '%s\n' '#include <fontconfig/fontconfig.h>' > "$tmp/fc.h"
  "$BG" "$tmp/fc.h" "${COMMON[@]}" \
    --allowlist-item 'Fc.*|FC_.*|_Fc.*' \
    -o pkg/fontconfig/c_bindings.rs -- $(pkg-config --cflags fontconfig)
fi

if [[ $which == all || $which == oniguruma ]]; then
  printf '%s\n' '#include <oniguruma.h>' > "$tmp/onig.h"
  "$BG" "$tmp/onig.h" "${COMMON[@]}" \
    --allowlist-item 'onig_.*|Onig.*|ONIG.*|OnigEnc.*' \
    --default-macro-constant-type signed \
    -o pkg/oniguruma/c_bindings.rs -- $(pkg-config --cflags oniguruma)
fi

if [[ $which == all || $which == opengl ]]; then
  printf '%s\n' '#include <glad/gl.h>' '#include <glad/glad_egl.h>' > "$tmp/gl.h"
  "$BG" "$tmp/gl.h" "${COMMON[@]}" \
    --allowlist-item 'GL.*|gl.*|Glad.*|glad.*|GLAD.*|EGL.*|egl.*|PFN.*|khronos_.*|__egl.*' \
    -o pkg/opengl/c_bindings.rs -- -Ivendor/glad/include
fi
