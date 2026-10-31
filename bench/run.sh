#!/usr/bin/env bash
# Build bench/capi_bench.c against zz's Zig engine and this crate, then compare.
#
#   bench/run.sh ZIG_PREFIX [capi_compare.py options]
#
# ZIG_PREFIX comes from tools/zig_reference.sh.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
zig=${1:?usage: bench/run.sh ZIG_PREFIX [options]}
shift
out=${OUT:-$root/target/capi-bench}
mkdir -p "$out"
cargo rustc -q --release --manifest-path "$root/Cargo.toml" --no-default-features --features lib-vt --lib --crate-type staticlib
rust_lib="${CARGO_TARGET_DIR:-$root/target}/release/libghostty.a"
case "$(uname -s)" in
Darwin) cxx=-lc++; sys= ;;
*) cxx=-lstdc++; sys="-lm -lpthread -ldl" ;;
esac
cc -O2 -I "$zig/include" "$here/capi_bench.c" "$zig/lib/libghostty-vt.a" $cxx $sys -o "$out/zig-bench"
cc -O2 -I "$root/include" "$here/capi_bench.c" "$rust_lib" $sys -o "$out/rust-bench"
python3 -I "$here/capi_compare.py" "$out/zig-bench" "$out/rust-bench" "$@"
