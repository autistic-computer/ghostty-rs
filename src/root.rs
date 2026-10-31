//! Crate root for the Rust port of Ghostty.
//!
//! The module tree mirrors ghostty's `src/` directory. See PORTING.md.

#![allow(dead_code, unused_imports, unused_variables, clippy::all)]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "app.rs"]
pub mod app;
#[path = "apprt.rs"]
pub mod apprt;
#[path = "benchmark/main.rs"]
pub mod benchmark;
#[path = "build_config.rs"]
pub mod build_config;
#[path = "cli.rs"]
pub mod cli;
#[path = "command.rs"]
pub mod command;
#[path = "config.rs"]
pub mod config;
#[path = "crash/main.rs"]
pub mod crash;
#[path = "crc32c.rs"]
pub mod crc32c;
#[path = "datastruct/main.rs"]
pub mod datastruct;
#[path = "extra/mod.rs"]
pub mod extra;
#[path = "fastmem.rs"]
pub mod fastmem;
#[path = "fastprint.rs"]
pub mod fastprint;
#[path = "file_type.rs"]
pub mod file_type;
#[path = "font/main.rs"]
pub mod font;
#[path = "global.rs"]
pub mod global;
#[path = "helpgen.rs"]
pub mod helpgen;
#[path = "input.rs"]
pub mod input;
#[path = "inspector/main.rs"]
pub mod inspector;
#[path = "lib/main.rs"]
pub mod lib;
#[path = "lib_vt.rs"]
pub mod lib_vt;
#[path = "main_bench.rs"]
pub mod main_bench;
#[path = "main_build_data.rs"]
pub mod main_build_data;
#[path = "main_c.rs"]
pub mod main_c;
#[path = "main_gen.rs"]
pub mod main_gen;
#[path = "main_ghostty.rs"]
pub mod main_ghostty;
#[path = "main_wasm.rs"]
pub mod main_wasm;
#[path = "math.rs"]
pub mod math;
#[path = "os/main.rs"]
pub mod os;
#[path = "pty.rs"]
pub mod pty;
#[path = "quirks.rs"]
pub mod quirks;
#[path = "quirks_memset.rs"]
pub mod quirks_memset;
#[path = "renderer.rs"]
pub mod renderer;
#[path = "simd/main.rs"]
pub mod simd;
#[path = "stb/main.rs"]
pub mod stb;
#[path = "surface.rs"]
pub mod surface;
#[path = "surface_mouse.rs"]
pub mod surface_mouse;
#[path = "synthetic/main.rs"]
pub mod synthetic;
#[path = "terminal/main.rs"]
pub mod terminal;
#[path = "terminfo/main.rs"]
pub mod terminfo;
#[path = "termio.rs"]
pub mod termio;
#[path = "tripwire.rs"]
pub mod tripwire;
#[path = "unicode/main.rs"]
pub mod unicode;
// @generated-mods-end

/// Ports of ghostty's `pkg/` Zig bindings (C library wrappers).
#[path = "../pkg/mod.rs"]
pub mod pkg;
