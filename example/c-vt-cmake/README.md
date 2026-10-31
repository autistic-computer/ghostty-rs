# c-vt-cmake

Demonstrates consuming libghostty-vt from a CMake project using
`FetchContent`. Creates a terminal, writes VT sequences into it, and
formats the screen contents as plain text.

The project declares this checkout as the `ghostty` dependency
(`FetchContent_Declare(ghostty SOURCE_DIR ../..)`); its `CMakeLists.txt`
builds libghostty-vt with cargo (`tools/build_lib_vt.sh`). Upstream fetches
the Zig repository from GitHub instead.

## Building

```shell-session
cd example/c-vt-cmake
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
./build/c_vt_cmake
```

To build against another checkout:

```shell-session
cmake -B build -DFETCHCONTENT_SOURCE_DIR_GHOSTTY=/path/to/ghostty-rs
cmake --build build
```

`example/build_all.sh c-vt-cmake` does the same and runs the program.
