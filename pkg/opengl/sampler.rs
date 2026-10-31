//! Port of `pkg/opengl/Sampler.zig`.
//!
//! STATUS: ported

use std::os::raw::c_uint;

use super::c;
use super::errors::{self, Error};
use super::glad;
use super::texture::{Parameter, ParameterValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    pub id: c::GLuint,
}

impl Sampler {
    /// Create a single sampler.
    pub fn create() -> Result<Sampler, Error> {
        let mut id: c::GLuint = 0;
        glad::call!(GenSamplers(1, &mut id));
        errors::get_error()?;
        Ok(Sampler { id })
    }

    /// glBindSampler
    pub fn bind(self, index: c_uint) -> Result<(), Error> {
        glad::call!(BindSampler(index, self.id));
        errors::get_error()
    }

    pub fn parameter<V: ParameterValue>(self, name: Parameter, value: V) -> Result<(), Error> {
        debug_assert_eq!(V::TYPE, name.type_(), "wrong value type for {name:?}");
        glad::call!(SamplerParameteri(
            self.id,
            name as c::GLenum,
            value.to_gl_int()
        ));
        errors::get_error()
    }

    pub fn destroy(self) {
        glad::call!(DeleteSamplers(1, &self.id));
    }
}
