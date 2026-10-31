#!/usr/bin/env bash
# Build zz's Zig libghostty-vt as the reference for bench/ and test/differential/.
#
#   tools/zig_reference.sh ZZ_CHECKOUT [PREFIX]
#
# Uses ZZ_CHECKOUT/third_party/ghostty with the Zig version zz pins, at
# ReleaseFast and baseline CPU like zz's release builds. PREFIX defaults to
# target/zig-reference.
set -euo pipefail
zz=${1:?usage: zig_reference.sh ZZ_CHECKOUT [PREFIX]}
root=$(cd "$(dirname "$0")/.." && pwd)
prefix=$(mkdir -p "${2:-$root/target/zig-reference}" && cd "${2:-$root/target/zig-reference}" && pwd)
cd "$zz/third_party/ghostty"
zig build -Demit-lib-vt=true -Doptimize=ReleaseFast -Dcpu=baseline --prefix "$prefix"
echo "$prefix"
