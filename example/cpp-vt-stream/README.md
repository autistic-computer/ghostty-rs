# Example: VT Stream Processing in C++

This contains a simple example of how to use `ghostty_terminal_vt_write`
to parse and process VT sequences in C++. This is a simplified C++ port
of the `c-vt-stream` example that verifies libghostty compiles in C++
mode.

> [!IMPORTANT]
>
> **`libghostty` is a C library.** This example is only here so our CI
> verifies that the library can be built in used from C++ files.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh cpp-vt-stream
target/examples/bin/cpp_vt_stream
```
