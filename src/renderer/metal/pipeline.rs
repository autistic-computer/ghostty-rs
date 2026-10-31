//! Port of `src/renderer/metal/Pipeline.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Wrapper for handling render pipelines.
//!
//! Deviation: Zig derives the vertex descriptor from the fields of a
//! comptime vertex type (`autoAttribute`). Rust has no field reflection,
//! so vertex types implement [`VertexAttributes`], listing each field's
//! offset and Metal vertex format (using the same type -> format mapping
//! as `autoAttribute`, see [`format_for`]), and [`Pipeline::init`] takes
//! an optional [`VertexLayout`].

use std::ffi::{c_ulong, c_void};

use crate::pkg::macos;

use super::api as mtl;
use super::objc;
use super::{check_error, Error};

/// Describes a color attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attachment {
    pub pixel_format: mtl::MTLPixelFormat,
    pub blending_enabled: bool,
}

impl Attachment {
    pub const fn new(pixel_format: mtl::MTLPixelFormat) -> Attachment {
        Attachment {
            pixel_format,
            blending_enabled: true,
        }
    }
}

/// Options for initializing a render pipeline.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// MTLDevice
    pub device: objc::Object,

    /// Name of the vertex function
    pub vertex_fn: &'a str,
    /// Name of the fragment function
    pub fragment_fn: &'a str,

    /// MTLLibrary to get the vertex function from
    pub vertex_library: objc::Object,
    /// MTLLibrary to get the fragment function from
    pub fragment_library: objc::Object,

    /// Vertex step function
    pub step_fn: mtl::MTLVertexStepFunction,

    /// Info about the color attachments used by this render pipeline.
    pub attachments: &'a [Attachment],
}

/// One field of a vertex type: its byte offset and Metal vertex format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexAttribute {
    pub offset: usize,
    pub format: mtl::MTLVertexFormat,
}

/// A vertex type usable as pipeline vertex input (Zig: the comptime
/// `VertexAttributes` type passed to `init`).
pub trait VertexAttributes: Sized {
    /// The fields of the type, in declaration order.
    const ATTRIBUTES: &'static [VertexAttribute];
}

/// The layout of a vertex type: its stride and attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexLayout {
    pub size: usize,
    pub attributes: &'static [VertexAttribute],
}

impl VertexLayout {
    pub const fn of<T: VertexAttributes>() -> VertexLayout {
        VertexLayout {
            size: std::mem::size_of::<T>(),
            attributes: T::ATTRIBUTES,
        }
    }
}

/// The scalar/array field types `autoAttribute` knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    U8x4,
    U16x2,
    I16x2,
    F32,
    F32x2,
    F32x4,
    I32,
    I32x2,
    I32x4,
    U32,
    U32x2,
    U32x4,
    U8,
    I8,
}

/// Zig `autoAttribute`'s type -> vertex format table ("Very incomplete
/// list, expand as necessary."). Packed structs and enums use their
/// backing/tag integer type.
pub const fn format_for(t: FieldType) -> mtl::MTLVertexFormat {
    use mtl::MTLVertexFormat as F;
    match t {
        FieldType::U8x4 => F::Uchar4,
        FieldType::U16x2 => F::Ushort2,
        FieldType::I16x2 => F::Short2,
        FieldType::F32 => F::Float,
        FieldType::F32x2 => F::Float2,
        FieldType::F32x4 => F::Float4,
        FieldType::I32 => F::Int,
        FieldType::I32x2 => F::Int2,
        // Zig maps `[4]i32` to `int2` too.
        FieldType::I32x4 => F::Int2,
        FieldType::U32 => F::Uint,
        FieldType::U32x2 => F::Uint2,
        FieldType::U32x4 => F::Uint4,
        FieldType::U8 => F::Uchar,
        FieldType::I8 => F::Char,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipeline {
    /// MTLRenderPipelineState
    pub state: objc::Object,
}

/// Look up a function in a library by name, returning a +1 MTLFunction.
///
/// # Safety
///
/// `library` must be a valid MTLLibrary.
unsafe fn new_function(library: objc::Object, name: &str) -> Result<objc::Object, Error> {
    let str = macos::foundation::String::create_with_bytes(
        name.as_bytes(),
        macos::foundation::StringEncoding::Utf8,
        false,
    )?;
    // SAFETY: caller guarantee; NSString is toll-free bridged.
    let ptr: *mut c_void = unsafe { library.msg_send(objc::sel(c"newFunctionWithName:"), (str.as_ptr(),)) };
    str.release();
    if ptr.is_null() {
        return Err(Error::MetalFailed);
    }
    Ok(objc::Object::from_id(ptr))
}

impl Pipeline {
    pub fn init(vertex_attributes: Option<VertexLayout>, opts: Options<'_>) -> Result<Pipeline, Error> {
        // SAFETY: standard Metal object creation on valid objects.
        unsafe {
            // Create our descriptor
            let class = objc::get_class(c"MTLRenderPipelineDescriptor").ok_or(Error::MetalFailed)?;
            let id_alloc: objc::Object = class.msg_send(objc::sel(c"alloc"), ());
            let desc: objc::Object = id_alloc.msg_send(objc::sel(c"init"), ());
            struct Release(objc::Object);
            impl Drop for Release {
                fn drop(&mut self) {
                    // SAFETY: we own the descriptor.
                    unsafe { self.0.msg_send::<(), _>(objc::sel(c"release"), ()) };
                }
            }
            let _desc_guard = Release(desc);

            // Get our vertex and fragment functions and add them to the descriptor.
            {
                let func_vert = new_function(opts.vertex_library, opts.vertex_fn)?;
                desc.set_property(c"vertexFunction", func_vert.value);
                func_vert.msg_send::<(), _>(objc::sel(c"release"), ());
            }
            {
                let func_frag = new_function(opts.fragment_library, opts.fragment_fn)?;
                desc.set_property(c"fragmentFunction", func_frag.value);
                func_frag.msg_send::<(), _>(objc::sel(c"release"), ());
            }

            // If we have vertex attributes, create and add a vertex descriptor.
            if let Some(v) = vertex_attributes {
                let class = objc::get_class(c"MTLVertexDescriptor").ok_or(Error::MetalFailed)?;
                let id_alloc: objc::Object = class.msg_send(objc::sel(c"alloc"), ());
                let vertex_desc: objc::Object = id_alloc.msg_send(objc::sel(c"init"), ());
                let _vertex_desc_guard = Release(vertex_desc);

                // Our attributes are the fields of the input
                let attrs = objc::Object::from_id(vertex_desc.get_property(c"attributes"));
                auto_attribute(v, attrs);

                // The layout describes how and when we fetch the next vertex input.
                let layouts = objc::Object::from_id(vertex_desc.get_property(c"layouts"));
                {
                    let layout: objc::Object =
                        layouts.msg_send(objc::sel(c"objectAtIndexedSubscript:"), (0 as c_ulong,));

                    layout.set_property(c"stepFunction", opts.step_fn as c_ulong);
                    layout.set_property(c"stride", v.size as c_ulong);
                }

                desc.set_property(c"vertexDescriptor", vertex_desc.value);
            }

            // Set our color attachment
            let attachments = objc::Object::from_id(desc.get_property(c"colorAttachments"));
            for (i, at) in opts.attachments.iter().enumerate() {
                let attachment: objc::Object =
                    attachments.msg_send(objc::sel(c"objectAtIndexedSubscript:"), (i as c_ulong,));

                attachment.set_property(c"pixelFormat", at.pixel_format as c_ulong);

                attachment.set_property(c"blendingEnabled", at.blending_enabled as objc::BOOL);
                // We always use premultiplied alpha blending for now.
                if at.blending_enabled {
                    attachment.set_property(c"rgbBlendOperation", mtl::MTLBlendOperation::Add as c_ulong);
                    attachment.set_property(c"alphaBlendOperation", mtl::MTLBlendOperation::Add as c_ulong);
                    attachment.set_property(c"sourceRGBBlendFactor", mtl::MTLBlendFactor::One as c_ulong);
                    attachment.set_property(c"sourceAlphaBlendFactor", mtl::MTLBlendFactor::One as c_ulong);
                    attachment.set_property(
                        c"destinationRGBBlendFactor",
                        mtl::MTLBlendFactor::OneMinusSourceAlpha as c_ulong,
                    );
                    attachment.set_property(
                        c"destinationAlphaBlendFactor",
                        mtl::MTLBlendFactor::OneMinusSourceAlpha as c_ulong,
                    );
                }
            }

            // Make our state
            let mut err: objc::Id = std::ptr::null_mut();
            let pipeline_state: objc::Object = opts.device.msg_send(
                objc::sel(c"newRenderPipelineStateWithDescriptor:error:"),
                (desc.value, &mut err as *mut objc::Id),
            );
            check_error(err)?;

            Ok(Pipeline {
                state: pipeline_state,
            })
        }
    }

    pub fn deinit(&self) {
        self.state.release();
    }
}

/// # Safety
///
/// `attrs` must be a valid `MTLVertexAttributeDescriptorArray`.
unsafe fn auto_attribute(v: VertexLayout, attrs: objc::Object) {
    for (i, a) in v.attributes.iter().enumerate() {
        // SAFETY: caller guarantee.
        unsafe {
            let attr: objc::Object = attrs.msg_send(objc::sel(c"objectAtIndexedSubscript:"), (i as c_ulong,));

            attr.set_property(c"format", a.format as c_ulong);
            attr.set_property(c"offset", a.offset as c_ulong);
            attr.set_property(c"bufferIndex", 0 as c_ulong);
        }
    }
}
