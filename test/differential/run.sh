#!/usr/bin/env bash
# Differential check against zz's Zig libghostty-vt.
#
#   test/differential/run.sh ZIG_PREFIX
#
# ZIG_PREFIX is a `zig build -Demit-lib-vt=true --prefix` output from zz's
# third_party/ghostty (it holds include/ and lib/libghostty-vt.a). The same
# compare.c is built against that library and against this crate's static
# library, both run the cases from generate.py at several write-chunk sizes,
# and every snapshot line must match. BUILD_ONLY=1 stops after building the
# two runners in OUT (fuzz.py uses this). RUST_PROFILE=dev builds the crate
# with debug assertions, which turn on the page integrity checks.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
zig=${1:?usage: run.sh ZIG_PREFIX}
out=${OUT:-$root/target/differential}
mkdir -p "$out"

profile=${RUST_PROFILE:-release}
cargo rustc -q --profile "$profile" --manifest-path "$root/Cargo.toml" --no-default-features --features lib-vt --lib --crate-type staticlib
[ "$profile" = dev ] && profile=debug
rust_lib="${CARGO_TARGET_DIR:-$root/target}/$profile/libghostty.a"

case "$(uname -s)" in
Darwin) cxx=-lc++; sys= ;;
*) cxx=-lstdc++; sys="-lm -lpthread -ldl" ;;
esac
cc -O2 -I "$zig/include" "$here/compare.c" "$zig/lib/libghostty-vt.a" $cxx $sys -o "$out/zig-runner"
cc -O2 -I "$root/include" "$here/compare.c" "$rust_lib" $sys -o "$out/rust-runner"
[ -n "${BUILD_ONLY:-}" ] && exit 0
python3 -I "$here/generate.py" "$out/cases.bin" >/dev/null

fail=0
for chunk in ${CHUNKS:-0 1 3 4 7 15 16 17 31 32 33 64 4096}; do
  "$out/zig-runner" "$out/cases.bin" "$chunk" >"$out/zig-$chunk.txt"
  "$out/rust-runner" "$out/cases.bin" "$chunk" >"$out/rust-$chunk.txt"
  if ! cmp -s "$out/zig-$chunk.txt" "$out/rust-$chunk.txt"; then
    echo "chunk $chunk: outputs differ"
    diff "$out/zig-$chunk.txt" "$out/rust-$chunk.txt" | head -4
    fail=1
  fi
done
[ $fail = 0 ] && echo "differential: $(wc -l <"$out/zig-0.txt") snapshots match at every chunk size"
exit $fail
