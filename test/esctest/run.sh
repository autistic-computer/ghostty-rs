#!/usr/bin/env bash
# Build the runner, fetch esctest at the pinned commit (Zig: the `esctest2`
# dependency in build.zig.zon) next to it, and run it. Arguments go to the
# runner, e.g. `test/esctest/run.sh --include=DECSTR > esctest.log`.
set -euo pipefail
cd "$(dirname "$0")"

# The esctest2 commit of 2026-09-13.
commit=2798f12149a19c3295e9b4853ab2da4b2eff1b2b

target="${CARGO_TARGET_DIR:-target}"
cargo build -q --release
share="$target/share"
if [ "$(git -C "$share/esctest2" rev-parse HEAD 2>/dev/null)" != "$commit" ]; then
  rm -rf "$share/esctest2"
  mkdir -p "$share"
  git clone -q https://github.com/ThomasDickey/esctest2.git "$share/esctest2"
  git -C "$share/esctest2" checkout -q "$commit"
fi
# The runner finds esctest in share/esctest beside its bin directory.
ln -sfn esctest2/esctest "$share/esctest"
exec "$target/release/esctest-libghostty" "$@"
