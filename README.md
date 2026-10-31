# ghostty-rs

A file-for-file port of the [Ghostty](https://github.com/ghostty-org/ghostty)
terminal emulator from Zig to Rust (reference revision `93967b7`).

Source imported from [ghrossty](https://github.com/vestergaardtechnology/ghrossty)
at revision `22c6649679632701e7f6386c8825a1bc21a0a578`.

![The Rust port running as a GTK terminal](docs/screenshot-gtk.png)

Every `ghostty/src/**/*.zig` (and `ghostty/pkg/**/*.zig`) file has a Rust
counterpart at the mirrored snake_case path, e.g. `src/terminal/PageList.zig`
→ `src/terminal/page_list.rs`, with its tests ported alongside. See
[PORTING.md](PORTING.md) for the conventions and `PORTING_STATUS.tsv` for the
full file mapping.

## Building and running (Linux)

System libraries: GTK 4, libadwaita, FreeType, HarfBuzz, fontconfig,
Oniguruma, EGL/GL. On Debian/Ubuntu:

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libfreetype-dev libharfbuzz-dev \
  libfontconfig-dev libonig-dev libegl-dev
cargo run --release              # the GTK terminal
cargo run --release -- +version  # CLI actions work as in Ghostty
cargo test --lib                 # the ported test suite
```

`cargo build --no-default-features` builds without the GTK runtime.
`--features inspector` enables the Dear ImGui terminal inspector.
`ghostty-bench` and `ghostty-gen` are the benchmark and synthetic-data tools;
`ghostty-build-data` generates the terminfo, completion and syntax files.

`tools/install_resources.sh [prefix]` installs the runtime resources (terminfo,
shell integration, themes, completions, vim/bat syntax, translations) into
`<prefix>/share` (default `target/dist/share`), like the Zig build's
`zig-out/share`.

## libghostty-vt

libghostty-vt is Ghostty's standalone terminal library: the `ghostty_*` C API
of `src/terminal/c/` declared in `include/ghostty/vt.h`.
`tools/build_lib_vt.sh [prefix]` builds it like `zig build -Demit-lib-vt`
(only a C compiler, binutils and cargo are needed; no GTK, FreeType or
fontconfig) and installs, Zig-style:

```
<prefix>/lib/libghostty-vt.so.0.1.0   (soname libghostty-vt.so.0, + .so.0/.so symlinks)
<prefix>/lib/libghostty-vt.a
<prefix>/include/ghostty/vt.h, include/ghostty/vt/*.h
<prefix>/share/pkgconfig/libghostty-vt.pc, libghostty-vt-static.pc
```

The default prefix is `target/lib-vt/out`; `--debug` builds the dev profile
and `GHOSTTY_LIB_VT_VERSION` overrides the version (Zig's
`-Dlib-version-string`). Both libraries export only the libghostty-vt C API
(205 functions, the same set as the Zig library): the crate is built as a
`staticlib` into `target/lib-vt` with the lib-vt terminal options (no tmux
control mode, version 0.1.0-dev), partially linked with `--gc-sections`
from those functions, and every other symbol is made local. The normal
`cargo build` is unaffected.

```sh
tools/build_lib_vt.sh /usr/local
export PKG_CONFIG_PATH=/usr/local/share/pkgconfig
cc app.c $(pkg-config --cflags --libs libghostty-vt)                            # shared
cc -DGHOSTTY_STATIC app.c $(pkg-config --cflags --static --libs libghostty-vt-static)  # static
```

CMake projects can use `CMakeLists.txt` (the upstream wrapper, driving cargo):
`FetchContent` / `add_subdirectory` gives the `ghostty-vt` and
`ghostty-vt-static` targets, `cmake --install` plus `find_package(ghostty-vt)`
gives `ghostty-vt::ghostty-vt`.

`example/` holds upstream's C/C++ examples; `example/build_all.sh` builds the
library, compiles every example against it with pkg-config (and the CMake
examples with CMake) and runs them (`--prefix <dir>` runs them against
another installed libghostty-vt, e.g. Zig's, for comparison). The Zig API
examples are Rust examples of the crate's API:
`cargo run --example vt`, `cargo run --example vt_stream`,
`printf '\e[31mred\n' | cargo run --example formatter`.

## The macOS app

`macos/` is Ghostty's Swift/AppKit app, unchanged except that it bundles
resources from `target/dist/share`. It links `GhosttyKit.xcframework`, the
libghostty C API (`include/ghostty.h`), which the Rust port provides through
`src/apprt/embedded.rs` and `src/main_c.rs`. On macOS with Xcode:

```sh
brew install pkg-config freetype harfbuzz fontconfig oniguruma
tools/install_resources.sh               # target/dist/share
tools/build_xcframework.sh native        # or: universal (arm64 + x86_64)
macos/build.nu --configuration Debug     # -> macos/build/Debug/Ghostty.app
```

`tools/build_xcframework.sh` replaces `zig build -Demit-macos-app=false`.

## Testing

- `cargo test --no-default-features --lib terminal::`: terminal unit tests.
- `cargo test --no-default-features --lib simd::vt::`: decoder unit tests.
- `test/esctest/run.sh`: esctest conformance tests for libghostty-vt.
- `test/fuzz-libghostty`: fuzz harnesses and committed regression corpora.
- `example/build_all.sh`: build and run the C/C++ API examples.

## Progress

```sh
python3 tools/status.py      # summary
python3 tools/status.py -v   # per file
```

Each Rust file's header says `STATUS: ported`, `partial — ...`, or
`not yet ported`; every Zig file is now ported. macOS code (Metal, CoreText,
the embedded apprt) is ported and type-checks with
`cargo check --target x86_64-apple-darwin`, but has not been run, and the
Swift app has not been built against the Rust library yet.

## Working with the reference source

```sh
git clone https://github.com/ghostty-org/ghostty.git ghostty
git -C ghostty checkout 93967b7f9478e94dcbb42125c8a90f34feb40d75
```
