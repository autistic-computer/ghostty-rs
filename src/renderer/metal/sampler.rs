//! Port of `src/renderer/metal/Sampler.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Wrapper for handling samplers.

use std::ffi::{c_ulong, c_void};
use std::fmt;

use super::api as mtl;
use super::objc;

/// Options for initializing a sampler.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// MTLDevice
    pub device: objc::Object,
    pub min_filter: mtl::MTLSamplerMinMagFilter,
    pub mag_filter: mtl::MTLSamplerMinMagFilter,
    pub s_address_mode: mtl::MTLSamplerAddressMode,
    pub t_address_mode: mtl::MTLSamplerAddressMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    /// The underlying MTLSamplerState Object.
    pub sampler: objc::Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A Metal API call failed.
    MetalFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MetalFailed")
    }
}

impl std::error::Error for Error {}

impl Sampler {
    /// Initialize a sampler
    pub fn init(opts: Options) -> Result<Sampler, Error> {
        // SAFETY: standard Metal object creation.
        unsafe {
            // Create our descriptor
            let class = objc::get_class(c"MTLSamplerDescriptor").ok_or(Error::MetalFailed)?;
            let id_alloc: objc::Object = class.msg_send(objc::sel(c"alloc"), ());
            let desc: objc::Object = id_alloc.msg_send(objc::sel(c"init"), ());

            // Properties
            desc.set_property(c"minFilter", opts.min_filter as c_ulong);
            desc.set_property(c"magFilter", opts.mag_filter as c_ulong);
            desc.set_property(c"sAddressMode", opts.s_address_mode as c_ulong);
            desc.set_property(c"tAddressMode", opts.t_address_mode as c_ulong);

            // Create the sampler state
            let id: *mut c_void =
                opts.device
                    .msg_send(objc::sel(c"newSamplerStateWithDescriptor:"), (desc.value,));
            desc.release();
            if id.is_null() {
                return Err(Error::MetalFailed);
            }

            Ok(Sampler {
                sampler: objc::Object::from_id(id),
            })
        }
    }

    pub fn deinit(&self) {
        self.sampler.release();
    }
}
