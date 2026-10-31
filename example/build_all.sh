#!/usr/bin/env bash
# Build and run every C/C++ example against libghostty-vt.
#
# Upstream Ghostty builds each example with its own `build.zig`; here they
# are plain C/C++ programs compiled with the system compiler against an
# installed libghostty-vt, found with pkg-config:
#
#   * the examples marked static upstream (`c-vt-static`, `c-vt-snapshot`)
#     link `libghostty-vt-static` (with `-DGHOSTTY_STATIC`),
#   * every other one links the shared `libghostty-vt`,
#   * the CMake examples (`c-vt-cmake*`) are configured against this
#     checkout's CMakeLists.txt (FETCHCONTENT_SOURCE_DIR_GHOSTTY).
#
# Every program is run; its stdout and stderr are saved in the output
# directory as `<example>.out` and `<example>.err`, so two runs (e.g.
# against the Rust and the Zig library) can be compared with `diff -r`.
#
# Usage: example/build_all.sh [options] [example...]
#
#   --prefix <dir>   use the libghostty-vt installed in <dir> instead of
#                    building it with tools/build_lib_vt.sh (e.g. the
#                    prefix of `zig build -Demit-lib-vt`); skips the CMake
#                    examples, which always build this checkout
#   --out <dir>      output directory (default: target/examples)
#   --no-run         only build
#   --no-cmake       skip the CMake examples
#
# Environment: CC (default cc), CXX (default c++), CFLAGS, CXXFLAGS.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

prefix=""
out="$ROOT/target/examples"
run=1
cmake_examples=1
only=()
while [ $# -gt 0 ]; do
    case "$1" in
    --prefix)
        prefix="$2"
        shift
        ;;
    --out)
        out="$2"
        shift
        ;;
    --no-run) run=0 ;;
    --no-cmake) cmake_examples=0 ;;
    -h | --help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    -*)
        echo "build_all.sh: unknown option $1" >&2
        exit 2
        ;;
    *) only+=("${1%/}") ;;
    esac
    shift
done

if [ -z "$prefix" ]; then
    prefix="$ROOT/target/lib-vt/out"
    "$ROOT/tools/build_lib_vt.sh" "$prefix"
else
    cmake_examples=0
fi
prefix="$(cd "$prefix" && pwd)"
mkdir -p "$out/bin"
out="$(cd "$out" && pwd)"

export PKG_CONFIG_PATH="$prefix/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
CC="${CC:-cc}"
CXX="${CXX:-c++}"
# The examples leave callback parameters unused, and switch over enums
# without handling the headers' `*_MAX_VALUE` sentinels (upstream builds
# them with `zig cc`, without -Wall).
warn=(-Wall -Wextra -Wno-unused-parameter -Wno-switch -Werror)
cflags=(-std=c11 -O2 -g "${warn[@]}")
cxxflags=(-std=c++17 -O2 -g "${warn[@]}")
# shellcheck disable=SC2206
[ -n "${CFLAGS:-}" ] && cflags+=($CFLAGS)
# shellcheck disable=SC2206
[ -n "${CXXFLAGS:-}" ] && cxxflags+=($CXXFLAGS)

# shellcheck disable=SC2207
shared_cflags=($(pkg-config --cflags libghostty-vt))
# shellcheck disable=SC2207
shared_libs=($(pkg-config --libs libghostty-vt) "-Wl,-rpath,$(pkg-config --variable=libdir libghostty-vt)")
# shellcheck disable=SC2207
static_cflags=(-DGHOSTTY_STATIC $(pkg-config --cflags libghostty-vt-static))
# shellcheck disable=SC2207
static_libs=($(pkg-config --static --libs libghostty-vt-static))

# Examples linked statically upstream (`ghostty-vt-static` in build.zig).
is_static() {
    case "$1" in
    c-vt-static | c-vt-snapshot) return 0 ;;
    *) return 1 ;;
    esac
}

selected() {
    [ ${#only[@]} -eq 0 ] && return 0
    local e
    for e in "${only[@]}"; do [ "$e" = "$1" ] && return 0; done
    return 1
}

failed=()
ran=0

run_example() { # name exe
    [ "$run" = 1 ] || return 0
    local status=0
    "$2" >"$out/$1.out" 2>"$out/$1.err" </dev/null || status=$?
    if [ "$status" -ne 0 ]; then
        echo "FAIL  $1 (exit $status)"
        failed+=("$1")
    else
        echo "ok    $1 ($(wc -l <"$out/$1.out") lines)"
    fi
    ran=$((ran + 1))
}

for dir in "$HERE"/c-* "$HERE"/cpp-*; do
    name="$(basename "$dir")"
    selected "$name" || continue
    [ -f "$dir/CMakeLists.txt" ] && continue
    exe="$out/bin/${name//-/_}"
    if [ -f "$dir/src/main.cpp" ]; then
        "$CXX" "${cxxflags[@]}" "${shared_cflags[@]}" "$dir/src/main.cpp" -o "$exe" "${shared_libs[@]}"
    elif is_static "$name"; then
        "$CC" "${cflags[@]}" "${static_cflags[@]}" "$dir/src/main.c" -o "$exe" "${static_libs[@]}"
    else
        "$CC" "${cflags[@]}" "${shared_cflags[@]}" "$dir/src/main.c" -o "$exe" "${shared_libs[@]}"
    fi
    run_example "$name" "$exe"
done

if [ "$cmake_examples" = 1 ]; then
    for dir in "$HERE"/c-vt-cmake*; do
        name="$(basename "$dir")"
        selected "$name" || continue
        build="$out/cmake/$name"
        mkdir -p "$out/cmake"
        cmake -S "$dir" -B "$build" -DCMAKE_BUILD_TYPE=Release \
            -DFETCHCONTENT_SOURCE_DIR_GHOSTTY="$ROOT" >"$build.configure.log" 2>&1 ||
            {
                cat "$build.configure.log" >&2
                exit 1
            }
        cmake --build "$build" >"$build.build.log" 2>&1 || {
            cat "$build.build.log" >&2
            exit 1
        }
        run_example "$name" "$build/${name//-/_}"
    done
fi

if [ "$run" = 1 ]; then
    if [ ${#failed[@]} -gt 0 ]; then
        echo "${#failed[@]} of $ran examples failed: ${failed[*]}" >&2
        exit 1
    fi
    echo "all $ran examples ran successfully; output in $out"
fi
