//! Port of `pkg/opengl/Texture.zig`.
//!
//! STATUS: ported

use std::os::raw::{c_uint, c_void};

use super::c;
use super::errors::{self, gl_enum, Error};
use super::glad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Texture {
    pub id: c::GLuint,
}

impl Texture {
    pub fn active(index: c_uint) -> Result<(), Error> {
        glad::call!(ActiveTexture(index + c::GL_TEXTURE0));
        errors::get_error()
    }

    /// Create a single texture.
    pub fn create() -> Result<Texture, Error> {
        let mut id: c::GLuint = 0;
        glad::call!(GenTextures(1, &mut id));
        errors::get_error()?;
        Ok(Texture { id })
    }

    /// glBindTexture
    pub fn bind(self, target: Target) -> Result<Binding, Error> {
        glad::call!(BindTexture(target as c::GLenum, self.id));
        errors::get_error()?;
        Ok(Binding { target })
    }

    pub fn destroy(self) {
        glad::call!(DeleteTextures(1, &self.id));
    }
}

/// Enum for possible texture binding targets.
///
/// The Zig tags `1d`, `2d`, ... are not Rust identifiers; they are spelled
/// `D1`, `D2`, ... here.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    D1 = c::GL_TEXTURE_1D,
    D2 = c::GL_TEXTURE_2D,
    D3 = c::GL_TEXTURE_3D,
    D1Array = c::GL_TEXTURE_1D_ARRAY,
    D2Array = c::GL_TEXTURE_2D_ARRAY,
    Rectangle = c::GL_TEXTURE_RECTANGLE,
    CubeMap = c::GL_TEXTURE_CUBE_MAP,
    Buffer = c::GL_TEXTURE_BUFFER,
    D2Multisample = c::GL_TEXTURE_2D_MULTISAMPLE,
    D2MultisampleArray = c::GL_TEXTURE_2D_MULTISAMPLE_ARRAY,
}

/// Enum for possible texture parameters.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parameter {
    BaseLevel = c::GL_TEXTURE_BASE_LEVEL,
    CompareFunc = c::GL_TEXTURE_COMPARE_FUNC,
    CompareMode = c::GL_TEXTURE_COMPARE_MODE,
    LodBias = c::GL_TEXTURE_LOD_BIAS,
    MinFilter = c::GL_TEXTURE_MIN_FILTER,
    MagFilter = c::GL_TEXTURE_MAG_FILTER,
    MinLod = c::GL_TEXTURE_MIN_LOD,
    MaxLod = c::GL_TEXTURE_MAX_LOD,
    MaxLevel = c::GL_TEXTURE_MAX_LEVEL,
    SwizzleR = c::GL_TEXTURE_SWIZZLE_R,
    SwizzleG = c::GL_TEXTURE_SWIZZLE_G,
    SwizzleB = c::GL_TEXTURE_SWIZZLE_B,
    SwizzleA = c::GL_TEXTURE_SWIZZLE_A,
    WrapS = c::GL_TEXTURE_WRAP_S,
    WrapT = c::GL_TEXTURE_WRAP_T,
    WrapR = c::GL_TEXTURE_WRAP_R,
}

/// The kind of value a [`Parameter`] takes (Zig: `Parameter.Type()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterType {
    MinFilter,
    MagFilter,
    Wrap,
    Int,
}

impl Parameter {
    pub fn type_(self) -> ParameterType {
        match self {
            Parameter::MinFilter => ParameterType::MinFilter,
            Parameter::MagFilter => ParameterType::MagFilter,
            Parameter::WrapS | Parameter::WrapT | Parameter::WrapR => ParameterType::Wrap,
            _ => ParameterType::Int,
        }
    }
}

/// A value for [`Binding::parameter`]/`Sampler::parameter`. Zig checks at
/// compile time that the value type matches `name.Type()`; here it is a
/// debug assertion.
pub trait ParameterValue: Copy {
    const TYPE: ParameterType;
    fn to_gl_int(self) -> c::GLint;
}

impl ParameterValue for c::GLint {
    const TYPE: ParameterType = ParameterType::Int;
    fn to_gl_int(self) -> c::GLint {
        self
    }
}

impl ParameterValue for MinFilter {
    const TYPE: ParameterType = ParameterType::MinFilter;
    fn to_gl_int(self) -> c::GLint {
        self as c::GLint
    }
}

impl ParameterValue for MagFilter {
    const TYPE: ParameterType = ParameterType::MagFilter;
    fn to_gl_int(self) -> c::GLint {
        self as c::GLint
    }
}

impl ParameterValue for Wrap {
    const TYPE: ParameterType = ParameterType::Wrap;
    fn to_gl_int(self) -> c::GLint {
        self as c::GLint
    }
}

gl_enum! {
    /// Internal format enum for texture images.
    pub struct InternalFormat(i32) {
        Red = c::GL_RED,
        Rgb = c::GL_RGB8,
        Rgba = c::GL_RGBA8,

        Srgb = c::GL_SRGB8,
        Srgba = c::GL_SRGB8_ALPHA8,

        RgbaCompressed = c::GL_COMPRESSED_RGBA_BPTC_UNORM,
        SrgbaCompressed = c::GL_COMPRESSED_SRGB_ALPHA_BPTC_UNORM,
        // There are so many more that I haven't filled in.
    }
}

gl_enum! {
    /// Format for texture images
    pub struct Format(u32) {
        Red = c::GL_RED,
        Rgb = c::GL_RGB,
        Rgba = c::GL_RGBA,
        Bgra = c::GL_BGRA,
        // There are so many more that I haven't filled in.
    }
}

/// Minification filter for textures.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinFilter {
    Nearest = c::GL_NEAREST as i32,
    Linear = c::GL_LINEAR as i32,
    NearestMipmapNearest = c::GL_NEAREST_MIPMAP_NEAREST as i32,
    LinearMipmapNearest = c::GL_LINEAR_MIPMAP_NEAREST as i32,
    NearestMipmapLinear = c::GL_NEAREST_MIPMAP_LINEAR as i32,
    LinearMipmapLinear = c::GL_LINEAR_MIPMAP_LINEAR as i32,
}

/// Magnification filter for textures.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagFilter {
    Nearest = c::GL_NEAREST as i32,
    Linear = c::GL_LINEAR as i32,
}

/// Texture coordinate wrapping mode.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    ClampToEdge = c::GL_CLAMP_TO_EDGE as i32,
    ClampToBorder = c::GL_CLAMP_TO_BORDER as i32,
    MirroredRepeat = c::GL_MIRRORED_REPEAT as i32,
    Repeat = c::GL_REPEAT as i32,
}

gl_enum! {
    /// Data type for texture images.
    pub struct DataType(u32) {
        UnsignedByte = c::GL_UNSIGNED_BYTE,
        // There are so many more that I haven't filled in.
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub target: Target,
}

impl Binding {
    pub fn unbind(&self) {
        glad::call!(BindTexture(self.target as c::GLenum, 0));
    }

    pub fn generate_mipmap(self) {
        glad::call!(GenerateMipmap(self.target as c::GLenum));
    }

    pub fn parameter<V: ParameterValue>(self, name: Parameter, value: V) -> Result<(), Error> {
        debug_assert_eq!(V::TYPE, name.type_(), "wrong value type for {name:?}");
        glad::call!(TexParameteri(
            self.target as c::GLenum,
            name as c::GLenum,
            value.to_gl_int(),
        ));
        errors::get_error()
    }

    /// # Safety
    /// `data` must be null or point to image data matching the given size,
    /// format and type (or be an offset into a bound pixel unpack buffer).
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn image_2d(
        self,
        level: c::GLint,
        internal_format: InternalFormat,
        width: c::GLsizei,
        height: c::GLsizei,
        format: Format,
        typ: DataType,
        data: *const c_void,
    ) -> Result<(), Error> {
        glad::call!(TexImage2D(
            self.target as c::GLenum,
            level,
            internal_format.0,
            width,
            height,
            0,
            format.0,
            typ.0,
            data,
        ));
        errors::get_error()
    }

    /// # Safety
    /// `data` must be null or point to image data matching the given size,
    /// format and type (or be an offset into a bound pixel unpack buffer).
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn sub_image_2d(
        self,
        level: c::GLint,
        xoffset: c::GLint,
        yoffset: c::GLint,
        width: c::GLsizei,
        height: c::GLsizei,
        format: Format,
        typ: DataType,
        data: *const c_void,
    ) -> Result<(), Error> {
        glad::call!(TexSubImage2D(
            self.target as c::GLenum,
            level,
            xoffset,
            yoffset,
            width,
            height,
            format.0,
            typ.0,
            data,
        ));
        errors::get_error()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_sub_image_2d(
        self,
        level: c::GLint,
        xoffset: c::GLint,
        yoffset: c::GLint,
        x: c::GLint,
        y: c::GLint,
        width: c::GLsizei,
        height: c::GLsizei,
    ) -> Result<(), Error> {
        glad::call!(CopyTexSubImage2D(
            self.target as c::GLenum,
            level,
            xoffset,
            yoffset,
            x,
            y,
            width,
            height,
        ));
        errors::get_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_types() {
        assert_eq!(ParameterType::MinFilter, Parameter::MinFilter.type_());
        assert_eq!(ParameterType::Wrap, Parameter::WrapR.type_());
        assert_eq!(ParameterType::Int, Parameter::MaxLevel.type_());
        assert_eq!(c::GL_LINEAR as c::GLint, MagFilter::Linear.to_gl_int());
    }
}
