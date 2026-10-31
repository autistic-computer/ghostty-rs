#!/usr/bin/env bash
# Merge a finished port branch into main, verify build + tests, push.
# usage: tools/merge_branch.sh <branch> [message]
set -euo pipefail
cd "$(dirname "$0")/.."
br="$1"; msg="${2:-merge $1}"
git checkout -q main
if ! git merge --no-ff --no-edit -m "$msg" "$br"; then
  others=$(git diff --name-only --diff-filter=U | grep -v -E '^Cargo\.(toml|lock)$' || true)
  if [ -n "$others" ]; then echo "MERGE CONFLICT"; echo "$others"; exit 2; fi
  python3 tools/resolve_cargo_conflict.py Cargo.toml
  git checkout --ours Cargo.lock 2>/dev/null || true
  cargo metadata -q --format-version 1 >/dev/null
  git add Cargo.toml Cargo.lock && git commit -q --no-edit
fi
python3 tools/scaffold.py >/dev/null
if ! git diff --quiet; then git commit -qam "scaffold: refresh generated mod blocks"; fi
timeout 3000 cargo build -q 2>&1 | grep -E '^error' -A12 | head -40 || true
timeout 3000 cargo test -q --lib 2>&1 | grep -E '^test result|FAILED|panicked' | tail -15
git push -q origin main 2>&1 | tail -2; python3 tools/status.py | head -3
