# Example: `ghostty-vt` Paste

This contains a simple example of how to paste into a `ghostty-vt`
terminal with `ghostty_terminal_paste`: plain and bracketed (mode 2004)
text pastes, the unsafe-paste confirmation flow, and Kitty clipboard
protocol paste events (mode 5522) including the program's follow-up
clipboard read. The clipboard's data is produced on demand through a
read callback, so only what is actually pasted is ever read, and the
result streams to the pty in chunks. It also shows the terminal-free
building blocks for checking paste safety and encoding paste data.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-paste
target/examples/bin/c_vt_paste
```
