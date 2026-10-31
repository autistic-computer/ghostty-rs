# CMake Support for libghostty-vt

The top-level `CMakeLists.txt` wraps the cargo build so that CMake projects
can consume libghostty-vt without invoking cargo manually. Running
`cmake --build` runs `tools/build_lib_vt.sh` (the Rust port's replacement
for upstream's `zig build -Demit-lib-vt`) automatically.

This means downstream projects do require `cargo`, `bash`, binutils
(`ld`, `objcopy`, `ar`) and a C compiler on `PATH` to build, but don't need
to know any Rust-specific details. CMake's `Release`, `MinSizeRel` and
`RelWithDebInfo` build types use cargo's release profile, anything else the
dev profile (like `zig build`'s Debug default).

## Using FetchContent (recommended)

Add the following to your project's `CMakeLists.txt`:

```cmake
include(FetchContent)
FetchContent_Declare(ghostty
    GIT_REPOSITORY <url of this repository>
    GIT_TAG main
)
FetchContent_MakeAvailable(ghostty)

add_executable(myapp main.c)
target_link_libraries(myapp PRIVATE ghostty-vt)        # shared
# target_link_libraries(myapp PRIVATE ghostty-vt-static) # static
```

### Using a local checkout

```shell-session
cmake -B build -DFETCHCONTENT_SOURCE_DIR_GHOSTTY=/path/to/ghostty-rs
cmake --build build
```

(or `FetchContent_Declare(ghostty SOURCE_DIR /path/to/ghostty-rs)`, as
`example/c-vt-cmake/` does).

## Using find_package (install-based)

Build and install libghostty-vt first:

```shell-session
cd /path/to/ghostty-rs
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
cmake --install build --prefix /usr/local
```

Then in your project:

```cmake
find_package(ghostty-vt REQUIRED)

add_executable(myapp main.c)
target_link_libraries(myapp PRIVATE ghostty-vt::ghostty-vt)
```

The installed static target `ghostty-vt::ghostty-vt-static` does not carry
its system libraries; link `gcc_s`, `m` (and on older glibc `pthread`,
`dl`) yourself, or use `tools/build_lib_vt.sh`'s pkg-config files.

## Building for another target

`ghostty_vt_add_target()` builds libghostty-vt for a Rust target triple
(Linux/ELF targets; the cross linker tools come from `CC`, `LD`, `OBJCOPY`
and `AR`):

```cmake
FetchContent_MakeAvailable(ghostty)
ghostty_vt_add_target(NAME linux-arm64 RUST_TARGET aarch64-unknown-linux-gnu)

add_executable(myapp main.c)
target_link_libraries(myapp PRIVATE ghostty-vt-static-linux-arm64)
```

Upstream's `GhosttyZigCompiler.cmake` (`zig cc` as the cross C compiler)
has no counterpart; use your cross toolchain's CMake toolchain file. See
`example/c-vt-cmake-cross/` for a complete working example.
