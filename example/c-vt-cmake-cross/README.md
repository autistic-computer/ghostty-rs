# c-vt-cmake-cross

Demonstrates using `ghostty_vt_add_target()` to build libghostty-vt for an
explicit Rust target triple, with static linking.

Upstream cross-compiles to a different OS than the host with `zig cc`. The
Rust port's library build (`tools/build_lib_vt.sh --target <triple>`)
supports Linux/ELF targets, so by default this example builds for the host
triple (taken from `rustc -vV`) through the same `ghostty_vt_add_target()`
path. To really cross-compile, install the Rust target and a cross C
toolchain and pass both:

```shell-session
rustup target add aarch64-unknown-linux-gnu
CC=aarch64-linux-gnu-gcc LD=aarch64-linux-gnu-ld OBJCOPY=aarch64-linux-gnu-objcopy \
AR=aarch64-linux-gnu-ar \
cmake -B build -DRUST_TARGET=aarch64-unknown-linux-gnu \
      -DCMAKE_C_COMPILER=aarch64-linux-gnu-gcc -DCMAKE_SYSTEM_NAME=Linux
cmake --build build
file build/c_vt_cmake_cross
```

## Building

```shell-session
cd example/c-vt-cmake-cross
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
./build/c_vt_cmake_cross
```

`example/build_all.sh c-vt-cmake-cross` does the same and runs the program.
