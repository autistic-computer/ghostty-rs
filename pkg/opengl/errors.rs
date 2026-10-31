//! Port of `pkg/opengl/errors.zig`.
//!
//! STATUS: ported

use super::c;
use super::glad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidEnum,
    InvalidValue,
    InvalidOperation,
    InvalidFramebufferOperation,
    OutOfMemory,

    Unknown,

    // Not part of Zig's `errors.Error`: these come from the inferred error
    // sets of `Shader.setSourceAndCompile`, `Program.link` and
    // `Program.setUniform`, which Rust needs a named type for.
    CompileFailed,
    UniformNameInvalid,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// getError returns the error (if any) from the last OpenGL operation.
pub fn get_error() -> Result<(), Error> {
    match glad::call!(GetError()) {
        c::GL_NO_ERROR => Ok(()),
        c::GL_INVALID_ENUM => Err(Error::InvalidEnum),
        c::GL_INVALID_VALUE => Err(Error::InvalidValue),
        c::GL_INVALID_OPERATION => Err(Error::InvalidOperation),
        c::GL_INVALID_FRAMEBUFFER_OPERATION => Err(Error::InvalidFramebufferOperation),
        c::GL_OUT_OF_MEMORY => Err(Error::OutOfMemory),
        _ => Err(Error::Unknown),
    }
}

/// mustError just calls getError but always results in an error being returned.
/// If getError has no error, then Unknown is returned.
pub fn must_error() -> Result<(), Error> {
    get_error()?;
    Err(Error::Unknown)
}

/// Defines a non-exhaustive GL enum (a Zig `enum(...) { ..., _ }`) as a
/// transparent newtype with one associated constant per named tag, so any
/// raw value can be represented and named tags read like enum variants
/// (`Target::Array`).
macro_rules! gl_enum {
    (
        $(#[$m:meta])*
        $vis:vis struct $name:ident($ty:ty) {
            $($(#[$vm:meta])* $variant:ident = $val:expr,)*
        }
    ) => {
        $(#[$m])*
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis struct $name(pub $ty);

        #[allow(non_upper_case_globals)]
        impl $name {
            $($(#[$vm])* pub const $variant: $name = $name($val as $ty);)*
        }
    };
}
pub(crate) use gl_enum;
