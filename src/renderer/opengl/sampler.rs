//! Port of `src/renderer/opengl/Sampler.zig`.
//!
//! STATUS: ported
//!
//! Wrapper for handling samplers.

use crate::pkg::opengl as gl;

/// Options for initializing a sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub min_filter: gl::texture::MinFilter,
    pub mag_filter: gl::texture::MagFilter,
    pub wrap_s: gl::texture::Wrap,
    pub wrap_t: gl::texture::Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    pub sampler: gl::Sampler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An OpenGL API call failed.
    OpenGLFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl Sampler {
    /// Initialize a sampler
    pub fn init(opts: Options) -> Result<Sampler, Error> {
        let sampler = gl::Sampler::create().map_err(|_| Error::OpenGLFailed)?;
        let result = (|| {
            use gl::texture::Parameter;
            sampler.parameter(Parameter::WrapS, opts.wrap_s)?;
            sampler.parameter(Parameter::WrapT, opts.wrap_t)?;
            sampler.parameter(Parameter::MinFilter, opts.min_filter)?;
            sampler.parameter(Parameter::MagFilter, opts.mag_filter)?;
            Ok::<(), gl::errors::Error>(())
        })();
        if result.is_err() {
            // errdefer sampler.destroy();
            sampler.destroy();
            return Err(Error::OpenGLFailed);
        }

        Ok(Sampler { sampler })
    }

    pub fn deinit(self) {
        self.sampler.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    // Not a Zig test: Sampler.zig has none.
    #[test]
    fn init_deinit() {
        let _ctx = TestContext::new();
        let sampler = Sampler::init(Options {
            min_filter: gl::texture::MinFilter::Linear,
            mag_filter: gl::texture::MagFilter::Linear,
            wrap_s: gl::texture::Wrap::ClampToEdge,
            wrap_t: gl::texture::Wrap::ClampToEdge,
        })
        .unwrap();
        assert_ne!(0, sampler.sampler.id);
        sampler.deinit();
    }
}
