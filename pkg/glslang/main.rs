//! Port of `src/glslang/main.zig`.
//!
//! STATUS: ported
//!
//! Deviation: Ghostty's Zig build links glslang (C++) to compile GLSL to
//! SPIR-V. The Rust port implements the same wrapper API (`init`, `Shader`,
//! `Program`, the C API's types in [`c`]) in pure Rust on top of `naga`'s
//! GLSL frontend, validator and SPIR-V backend. naga's GLSL frontend is
//! stricter than glslang in a few places, so [`Shader::preprocess`] applies a
//! small compatibility rewrite first (see `shader.rs`).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
#[path = "init.rs"]
pub mod init;
#[path = "program.rs"]
pub mod program;
#[path = "shader.rs"]
pub mod shader;
#[path = "test.rs"]
pub mod test;
// @generated-mods-end

use std::fmt;

pub use init::{finalize, init};
pub use program::Program;
pub use shader::Shader;
/// `pub const testing = @import("test.zig");`
pub use test as testing;

/// Errors returned by the glslang wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    GlslangInitFailed,
    GlslangFailed,
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::GlslangInitFailed => "GlslangInitFailed",
            Error::GlslangFailed => "GlslangFailed",
            Error::OutOfMemory => "OutOfMemory",
        })
    }
}

impl std::error::Error for Error {}
