#!/usr/bin/env bash
# Build macos/GhosttyKit.xcframework from the Rust port, mirroring
# `src/build/GhosttyXCFramework.zig` + `GhosttyLib.zig` (the Zig build's
# `zig build -Demit-macos-app=false`). Run on macOS with Xcode installed.
#
# usage: tools/build_xcframework.sh [universal|native] [debug|release]
#   universal (default): aarch64 + x86_64, combined with lipo.
#   native: only the host architecture (faster for development).
#
# Native C dependencies (freetype, harfbuzz, fontconfig, oniguruma) come
# from pkg-config, e.g. `brew install pkg-config freetype harfbuzz
# fontconfig oniguruma`. They are linked statically (PKG_CONFIG_ALL_STATIC)
# so rustc bundles them into the static library, like the Zig build
# combines its dependency archives with libtool.
set -euo pipefail
cd "$(dirname "$0")/.."

[ "$(uname -s)" = Darwin ] || { echo "error: building GhosttyKit requires macOS" >&2; exit 1; }

mode="${1:-universal}"
profile="${2:-release}"
case "$mode" in
  universal) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  native) targets=("$(uname -m | sed 's/arm64/aarch64/')-apple-darwin") ;;
  *) echo "usage: $0 [universal|native] [debug|release]" >&2; exit 2 ;;
esac
case "$profile" in
  release) cargo_profile=(--release); dir=release ;;
  debug) cargo_profile=(); dir=debug ;;
  *) echo "usage: $0 [universal|native] [debug|release]" >&2; exit 2 ;;
esac

# `Config.osVersionMin(.macos)`.
export MACOSX_DEPLOYMENT_TARGET=13.0
export PKG_CONFIG_ALL_STATIC=1

libs=()
for target in "${targets[@]}"; do
  rustup target add "$target" >/dev/null
  # libghostty for the embedded apprt; the GTK runtime is not used on macOS.
  cargo rustc --lib "${cargo_profile[@]}" --target "$target" \
    --no-default-features --crate-type staticlib \
    -- --print native-static-libs 2> "target/$target-native-libs.txt" || {
      cat "target/$target-native-libs.txt" >&2; exit 1; }
  libs+=("target/$target/$dir/libghostty.a")
done

out=target/xcframework
rm -rf "$out"
mkdir -p "$out/headers"
if [ "${#libs[@]}" -gt 1 ]; then
  lipo -create "${libs[@]}" -output "$out/ghostty-internal.a"
else
  cp "${libs[0]}" "$out/ghostty-internal.a"
fi

# Only ghostty.h and the module map: include/ also holds the libghostty-vt
# headers, which would trip Clang's "umbrella header does not include
# header" warning.
cp include/ghostty.h include/module.modulemap "$out/headers/"

rm -rf macos/GhosttyKit.xcframework
xcodebuild -create-xcframework \
  -library "$out/ghostty-internal.a" -headers "$out/headers" \
  -output macos/GhosttyKit.xcframework

# Libraries rustc could not bundle (system dylibs and frameworks) must be
# linked by the app; Ghostty.xcodeproj links the Apple frameworks already.
echo "native libraries required by libghostty:"
grep -h "native-static-libs" target/*-apple-darwin-native-libs.txt | sort -u | sed 's/^note: /  /'
echo "built macos/GhosttyKit.xcframework"
