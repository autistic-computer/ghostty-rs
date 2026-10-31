//! Crate root for the Rust port of Ghostty.
//!
//! The module tree mirrors ghostty's `src/` directory. See PORTING.md.

#![allow(dead_code, unused_imports, unused_variables, clippy::all)]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[cfg(feature = "app")]
#[path = "app.rs"]
pub mod app;
#[cfg(feature = "app")]
#[path = "apprt.rs"]
pub mod apprt;
#[cfg(feature = "app")]
#[path = "benchmark/main.rs"]
pub mod benchmark;
#[cfg(feature = "app")]
#[path = "build_config.rs"]
pub mod build_config;
#[cfg(feature = "app")]
#[path = "cli.rs"]
pub mod cli;
#[cfg(feature = "app")]
#[path = "command.rs"]
pub mod command;
#[cfg(feature = "app")]
#[path = "config.rs"]
pub mod config;
#[cfg(feature = "app")]
#[path = "crash/main.rs"]
pub mod crash;
#[path = "crc32c.rs"]
pub mod crc32c;
#[path = "datastruct/main.rs"]
pub mod datastruct;
#[cfg(feature = "app")]
#[path = "extra/mod.rs"]
pub mod extra;
#[path = "fastmem.rs"]
pub mod fastmem;
#[path = "fastprint.rs"]
pub mod fastprint;
#[cfg(feature = "app")]
#[path = "file_type.rs"]
pub mod file_type;
#[cfg(feature = "app")]
#[path = "font/main.rs"]
pub mod font;
#[cfg(feature = "app")]
#[path = "global.rs"]
pub mod global;
#[cfg(feature = "app")]
#[path = "helpgen.rs"]
pub mod helpgen;
#[cfg(feature = "app")]
#[path = "input.rs"]
pub mod input;
#[cfg(feature = "app")]
#[path = "inspector/main.rs"]
pub mod inspector;
#[path = "lib/main.rs"]
pub mod lib;
#[path = "lib_vt.rs"]
pub mod lib_vt;
#[cfg(feature = "app")]
#[path = "main_bench.rs"]
pub mod main_bench;
#[cfg(feature = "app")]
#[path = "main_build_data.rs"]
pub mod main_build_data;
#[cfg(feature = "app")]
#[path = "main_c.rs"]
pub mod main_c;
#[cfg(feature = "app")]
#[path = "main_gen.rs"]
pub mod main_gen;
#[cfg(feature = "app")]
#[path = "main_ghostty.rs"]
pub mod main_ghostty;
#[cfg(feature = "app")]
#[path = "main_wasm.rs"]
pub mod main_wasm;
#[cfg(feature = "app")]
#[path = "math.rs"]
pub mod math;
#[cfg(feature = "app")]
#[path = "os/main.rs"]
pub mod os;
#[cfg(feature = "app")]
#[path = "pty.rs"]
pub mod pty;
#[path = "quirks.rs"]
pub mod quirks;
#[path = "quirks_memset.rs"]
pub mod quirks_memset;
#[cfg(feature = "app")]
#[path = "renderer.rs"]
pub mod renderer;
#[path = "simd/main.rs"]
pub mod simd;
#[cfg(feature = "app")]
#[path = "stb/main.rs"]
pub mod stb;
#[cfg(feature = "app")]
#[path = "surface.rs"]
pub mod surface;
#[cfg(feature = "app")]
#[path = "surface_mouse.rs"]
pub mod surface_mouse;
#[cfg(feature = "app")]
#[path = "synthetic/main.rs"]
pub mod synthetic;
#[path = "terminal/main.rs"]
pub mod terminal;
#[path = "terminfo/main.rs"]
pub mod terminfo;
#[cfg(feature = "app")]
#[path = "termio.rs"]
pub mod termio;
#[path = "tripwire.rs"]
pub mod tripwire;
#[path = "unicode/main.rs"]
pub mod unicode;
// @generated-mods-end

/// Ports of ghostty's `pkg/` Zig bindings (C library wrappers).
#[cfg(feature = "app")]
#[path = "../pkg/mod.rs"]
pub mod pkg;

#[cfg(not(any(feature = "app", feature = "lib-vt")))]
compile_error!("enable the `app` feature (the Ghostty application) or `lib-vt` (libghostty-vt only)");

// libghostty-vt without the app (`--no-default-features --features lib-vt`):
// only the files of the app modules above that the terminal and its C API
// reach, at their usual paths.

#[cfg(not(feature = "app"))]
pub mod font {
    #[path = "glyph.rs"]
    pub mod glyph;
    #[path = "metrics.rs"]
    pub mod metrics;
    // Test fonts and nerd font constraints of the tests.
    #[cfg(test)]
    #[path = "embedded.rs"]
    pub mod embedded;
    #[cfg(test)]
    #[path = "nerd_font_attributes.rs"]
    pub mod nerd_font_attributes;
    #[cfg(test)]
    #[path = "nerd_font_tables.rs"]
    pub mod nerd_font_tables;
    pub mod opentype {
        #[path = "glyf.rs"]
        pub mod glyf;
        #[path = "head.rs"]
        pub mod head;
        #[path = "sfnt.rs"]
        pub mod sfnt;
    }
}

#[cfg(not(feature = "app"))]
pub mod input {
    #[path = "config.rs"]
    pub mod config;
    #[path = "function_keys.rs"]
    pub mod function_keys;
    #[path = "key.rs"]
    pub mod key;
    #[path = "key_encode.rs"]
    pub mod key_encode;
    #[path = "key_mods.rs"]
    pub mod key_mods;
    #[path = "kitty.rs"]
    pub mod kitty;
    #[path = "mouse.rs"]
    pub mod mouse;
    #[path = "mouse_encode.rs"]
    pub mod mouse_encode;
    #[path = "paste.rs"]
    pub mod paste;

    pub use self::key::ctrl_or_super;
}

#[cfg(not(feature = "app"))]
pub mod os {
    #[path = "mach.rs"]
    pub mod mach;
    #[path = "stderr.rs"]
    pub mod stderr;
    #[path = "string_encoding.rs"]
    pub mod string_encoding;
    #[path = "uri.rs"]
    pub mod uri;
    #[path = "wasm.rs"]
    pub mod wasm;
    #[path = "windows.rs"]
    pub mod windows;
}

#[cfg(not(feature = "app"))]
#[path = "../pkg"]
pub mod pkg {
    #[path = "wuffs/src"]
    pub mod wuffs {
        #[path = "error.rs"]
        pub mod error;
        #[path = "swizzle.rs"]
        pub mod swizzle;
        #[path = "zlib.rs"]
        pub mod zlib;

        pub use self::error::Error;
    }
}

#[cfg(not(feature = "app"))]
pub mod renderer {
    #[path = "size.rs"]
    pub mod size;
}
