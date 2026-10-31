//! Port of `src/renderer/metal/shaders.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Deviations:
//! * Zig embeds a `ghostty_metallib` precompiled by the build (`xcrun
//!   metal`). Cargo has no Metal toolchain step (and cannot run one when
//!   cross-checking), so the library is compiled at runtime from
//!   `shaders/shaders.metal` with `newLibraryWithSource:options:error:`.
//! * The comptime-generated `PipelineCollection` struct is spelled out.
//! * Vertex types implement [`VertexAttributes`] (see `pipeline.rs`).

use std::ffi::{c_void, CStr};
use std::mem::{offset_of, size_of};
use std::time::Instant;

use crate::pkg::macos;

use super::api as mtl;
use super::objc;
use super::pipeline::{
    self, format_for, FieldType, Pipeline, VertexAttribute, VertexAttributes, VertexLayout,
};
use super::{check_error, Error};

const LOG: &str = "metal";

/// The Metal shader library source (Zig: compiled into
/// `ghostty_metallib` at build time).
const SHADERS_METAL: &str = include_str!("../shaders/shaders.metal");

/// All the comptime-known info about a pipeline, so that
/// we can define them ahead-of-time in an ergonomic way.
#[derive(Debug, Clone, Copy)]
struct PipelineDescription {
    vertex_attributes: Option<VertexLayout>,
    vertex_fn: &'static str,
    fragment_fn: &'static str,
    step_fn: mtl::MTLVertexStepFunction,
    blending_enabled: bool,
}

impl PipelineDescription {
    fn init_pipeline(
        &self,
        device: objc::Object,
        library: objc::Object,
        pixel_format: mtl::MTLPixelFormat,
    ) -> Result<Pipeline, Error> {
        Pipeline::init(
            self.vertex_attributes,
            pipeline::Options {
                device,
                vertex_fn: self.vertex_fn,
                fragment_fn: self.fragment_fn,
                vertex_library: library,
                fragment_library: library,
                step_fn: self.step_fn,
                attachments: &[pipeline::Attachment {
                    pixel_format,
                    blending_enabled: self.blending_enabled,
                }],
            },
        )
    }
}

const PIPELINE_DESCS: [(&str, PipelineDescription); 5] = [
    (
        "bg_color",
        PipelineDescription {
            vertex_attributes: None,
            vertex_fn: "full_screen_vertex",
            fragment_fn: "bg_color_fragment",
            step_fn: mtl::MTLVertexStepFunction::PerVertex,
            blending_enabled: false,
        },
    ),
    (
        "cell_bg",
        PipelineDescription {
            vertex_attributes: None,
            vertex_fn: "full_screen_vertex",
            fragment_fn: "cell_bg_fragment",
            step_fn: mtl::MTLVertexStepFunction::PerVertex,
            blending_enabled: true,
        },
    ),
    (
        "cell_text",
        PipelineDescription {
            vertex_attributes: Some(VertexLayout::of::<CellText>()),
            vertex_fn: "cell_text_vertex",
            fragment_fn: "cell_text_fragment",
            step_fn: mtl::MTLVertexStepFunction::PerInstance,
            blending_enabled: true,
        },
    ),
    (
        "image",
        PipelineDescription {
            vertex_attributes: Some(VertexLayout::of::<Image>()),
            vertex_fn: "image_vertex",
            fragment_fn: "image_fragment",
            step_fn: mtl::MTLVertexStepFunction::PerInstance,
            blending_enabled: true,
        },
    ),
    (
        "bg_image",
        PipelineDescription {
            vertex_attributes: Some(VertexLayout::of::<BgImage>()),
            vertex_fn: "bg_image_vertex",
            fragment_fn: "bg_image_fragment",
            step_fn: mtl::MTLVertexStepFunction::PerInstance,
            blending_enabled: true,
        },
    ),
];

/// The pipeline collection type (Zig: a struct generated from
/// `pipeline_descs` with one `Pipeline` field per entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineCollection {
    pub bg_color: Pipeline,
    pub cell_bg: Pipeline,
    pub cell_text: Pipeline,
    pub image: Pipeline,
    pub bg_image: Pipeline,
}

impl PipelineCollection {
    fn each(&self) -> [Pipeline; 5] {
        [self.bg_color, self.cell_bg, self.cell_text, self.image, self.bg_image]
    }
}

/// This contains the state for the shaders used by the Metal renderer.
#[derive(Debug)]
pub struct Shaders {
    pub library: objc::Object,

    /// Collection of available render pipelines (`None` only for
    /// [`Shaders::UNINIT`]).
    pub pipelines: Option<PipelineCollection>,

    /// Custom shaders to run against the final drawable texture. This
    /// can be used to apply a lot of effects. Each shader is run in sequence
    /// against the output of the previous shader.
    pub post_pipelines: Vec<Pipeline>,

    /// Set to true when deinited, if you try to deinit a defunct set
    /// of shaders it will just be ignored, to prevent double-free.
    pub defunct: bool,
}

impl Shaders {
    pub const UNINIT: Shaders = Shaders {
        library: objc::Object {
            value: std::ptr::null_mut(),
        },
        pipelines: None,
        post_pipelines: Vec::new(),
        defunct: true,
    };

    /// Initialize our shader set.
    ///
    /// "post_shaders" is an optional list of postprocess shaders to run
    /// against the final drawable texture. This is an array of shader source
    /// code, not file paths.
    pub fn init(
        device: objc::Object,
        post_shaders: &[&CStr],
        pixel_format: mtl::MTLPixelFormat,
    ) -> Result<Shaders, Error> {
        let library = init_library(device)?;
        let release_library = || {
            // SAFETY: we own the library.
            unsafe { library.msg_send::<(), _>(objc::sel(c"release"), ()) };
        };

        let mut initialized: Vec<Pipeline> = Vec::with_capacity(PIPELINE_DESCS.len());
        for (_, desc) in PIPELINE_DESCS.iter() {
            match desc.init_pipeline(device, library, pixel_format) {
                Ok(p) => initialized.push(p),
                Err(err) => {
                    for p in &initialized {
                        p.deinit();
                    }
                    release_library();
                    return Err(err);
                }
            }
        }
        let pipelines = PipelineCollection {
            bg_color: initialized[0],
            cell_bg: initialized[1],
            cell_text: initialized[2],
            image: initialized[3],
            bg_image: initialized[4],
        };

        let post_pipelines = match init_post_pipelines(device, library, post_shaders, pixel_format) {
            Ok(p) => p,
            Err(err) => {
                // If an error happens while building postprocess shaders we
                // want to just not use any postprocess shaders since we don't
                // want to block Ghostty from working.
                log::warn!(target: LOG, "error initializing postprocess shaders err={err}");
                Vec::new()
            }
        };

        Ok(Shaders {
            library,
            pipelines: Some(pipelines),
            post_pipelines,
            defunct: false,
        })
    }

    pub fn deinit(&mut self) {
        if self.defunct {
            return;
        }
        self.defunct = true;

        // Release our primary shaders
        if let Some(pipelines) = self.pipelines.take() {
            for p in pipelines.each() {
                p.deinit();
            }
        }
        // SAFETY: we own the library.
        unsafe { self.library.msg_send::<(), _>(objc::sel(c"release"), ()) };

        // Release our postprocess shaders
        for pipeline in self.post_pipelines.drain(..) {
            pipeline.deinit();
        }
    }
}

/// The uniforms that are passed to our shaders.
///
/// Zig: an `extern struct` with explicit field alignments (copied from the
/// MSL developer reference); the padding they introduce is spelled out as
/// `_padding_*` fields.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Uniforms {
    /// The projection matrix for turning world coordinates to normalized.
    /// This is calculated based on the size of the screen.
    pub projection_matrix: [[f32; 4]; 4],

    /// Size of the screen (render target) in pixels.
    pub screen_size: [f32; 2],

    /// Size of a single cell in pixels, unscaled.
    pub cell_size: [f32; 2],

    /// Size of the grid in columns and rows.
    pub grid_size: [u16; 2],

    /// Padding from `grid_padding: [4]f32 align(16)`.
    pub _padding_grid_padding: [u8; 12],

    /// The padding around the terminal grid in pixels. In order:
    /// top, right, bottom, left.
    pub grid_padding: [f32; 4],

    /// Bit mask defining which directions to
    /// extend cell colors in to the padding.
    /// Order, LSB first: left, right, up, down
    pub padding_extend: PaddingExtend,

    /// Padding from `min_contrast: f32 align(4)`.
    pub _padding_min_contrast: [u8; 3],

    /// The minimum contrast ratio for text. The contrast ratio is calculated
    /// according to the WCAG 2.0 spec.
    pub min_contrast: f32,

    /// The cursor position and color.
    pub cursor_pos: [u16; 2],
    pub cursor_color: [u8; 4],

    /// The background color for the whole surface.
    pub bg_color: [u8; 4],

    /// Various booleans.
    ///
    /// TODO: Maybe put these in a packed struct, like for OpenGL.
    pub bools: UniformsBools,
}

/// Zig: the anonymous `extern struct` of `Uniforms.bools`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UniformsBools {
    /// Whether the cursor is 2 cells wide.
    pub cursor_wide: bool,

    /// Indicates that colors provided to the shader are already in
    /// the P3 color space, so they don't need to be converted from
    /// sRGB.
    pub use_display_p3: bool,

    /// Indicates that the color attachments for the shaders have
    /// an `*_srgb` pixel format, which means the shaders need to
    /// output linear RGB colors rather than gamma encoded colors,
    /// since blending will be performed in linear space and then
    /// Metal itself will re-encode the colors for storage.
    pub use_linear_blending: bool,

    /// Enables a weight correction step that makes text rendered
    /// with linear alpha blending have a similar apparent weight
    /// (thickness) to gamma-incorrect blending.
    pub use_linear_correction: bool,
}

/// Zig: `Uniforms.PaddingExtend`, a `packed struct(u8)` (LSB first).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PaddingExtend(pub u8);

impl PaddingExtend {
    const LEFT: u8 = 1 << 0;
    const RIGHT: u8 = 1 << 1;
    const UP: u8 = 1 << 2;
    const DOWN: u8 = 1 << 3;

    pub const fn new(left: bool, right: bool, up: bool, down: bool) -> Self {
        PaddingExtend(
            (left as u8) * Self::LEFT
                | (right as u8) * Self::RIGHT
                | (up as u8) * Self::UP
                | (down as u8) * Self::DOWN,
        )
    }

    pub const fn left(self) -> bool {
        self.0 & Self::LEFT != 0
    }
    pub const fn right(self) -> bool {
        self.0 & Self::RIGHT != 0
    }
    pub const fn up(self) -> bool {
        self.0 & Self::UP != 0
    }
    pub const fn down(self) -> bool {
        self.0 & Self::DOWN != 0
    }
}

/// This is a single parameter for the terminal cell shader.
///
/// Zig: an `extern struct` whose `align(8)` on `glyph_pos` makes the
/// struct 8-aligned (32 bytes).
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellText {
    pub glyph_pos: [u32; 2],
    pub glyph_size: [u32; 2],
    pub bearings: [i16; 2],
    pub grid_pos: [u16; 2],
    pub color: [u8; 4],
    pub atlas: CellTextAtlas,
    pub bools: CellTextBools,
}

impl CellText {
    /// A cell with the Zig field defaults for the fields that have them.
    pub const fn new(grid_pos: [u16; 2], color: [u8; 4], atlas: CellTextAtlas) -> Self {
        CellText {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearings: [0, 0],
            grid_pos,
            color,
            atlas,
            bools: CellTextBools(0),
        }
    }
}

impl VertexAttributes for CellText {
    const ATTRIBUTES: &'static [VertexAttribute] = &[
        VertexAttribute {
            offset: offset_of!(CellText, glyph_pos),
            format: format_for(FieldType::U32x2),
        },
        VertexAttribute {
            offset: offset_of!(CellText, glyph_size),
            format: format_for(FieldType::U32x2),
        },
        VertexAttribute {
            offset: offset_of!(CellText, bearings),
            format: format_for(FieldType::I16x2),
        },
        VertexAttribute {
            offset: offset_of!(CellText, grid_pos),
            format: format_for(FieldType::U16x2),
        },
        VertexAttribute {
            offset: offset_of!(CellText, color),
            format: format_for(FieldType::U8x4),
        },
        // enum(u8): the tag type.
        VertexAttribute {
            offset: offset_of!(CellText, atlas),
            format: format_for(FieldType::U8),
        },
        // packed struct(u8): the backing integer.
        VertexAttribute {
            offset: offset_of!(CellText, bools),
            format: format_for(FieldType::U8),
        },
    ];
}

/// Zig: `CellText.Atlas`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellTextAtlas {
    Grayscale = 0,
    Color = 1,
}

/// Zig: the `packed struct(u8)` of `CellText.bools`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellTextBools(pub u8);

impl CellTextBools {
    const NO_MIN_CONTRAST: u8 = 1 << 0;
    const IS_CURSOR_GLYPH: u8 = 1 << 1;

    pub const fn new(no_min_contrast: bool, is_cursor_glyph: bool) -> Self {
        CellTextBools((no_min_contrast as u8) * Self::NO_MIN_CONTRAST | (is_cursor_glyph as u8) * Self::IS_CURSOR_GLYPH)
    }

    pub const fn no_min_contrast(self) -> bool {
        self.0 & Self::NO_MIN_CONTRAST != 0
    }

    pub const fn is_cursor_glyph(self) -> bool {
        self.0 & Self::IS_CURSOR_GLYPH != 0
    }
}

/// This is a single parameter for the cell bg shader.
pub type CellBg = [u8; 4];

/// Single parameter for the image shader. See shader for field details.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Image {
    pub grid_pos: [f32; 2],
    pub cell_offset: [f32; 2],
    pub source_rect: [f32; 4],
    pub dest_size: [f32; 2],
}

impl VertexAttributes for Image {
    const ATTRIBUTES: &'static [VertexAttribute] = &[
        VertexAttribute {
            offset: offset_of!(Image, grid_pos),
            format: format_for(FieldType::F32x2),
        },
        VertexAttribute {
            offset: offset_of!(Image, cell_offset),
            format: format_for(FieldType::F32x2),
        },
        VertexAttribute {
            offset: offset_of!(Image, source_rect),
            format: format_for(FieldType::F32x4),
        },
        VertexAttribute {
            offset: offset_of!(Image, dest_size),
            format: format_for(FieldType::F32x2),
        },
    ];
}

/// Single parameter for the bg image shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgImage {
    pub opacity: f32,
    pub info: BgImageInfo,
}

impl VertexAttributes for BgImage {
    const ATTRIBUTES: &'static [VertexAttribute] = &[
        VertexAttribute {
            offset: offset_of!(BgImage, opacity),
            format: format_for(FieldType::F32),
        },
        // packed struct(u8): the backing integer.
        VertexAttribute {
            offset: offset_of!(BgImage, info),
            format: format_for(FieldType::U8),
        },
    ];
}

/// Zig: `BgImage.Info`, a `packed struct(u8)`: position (u4), fit (u2),
/// repeat (bool), 1 bit padding (LSB first).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BgImageInfo(pub u8);

impl BgImageInfo {
    pub const fn new(position: BgImagePosition, fit: BgImageFit, repeat: bool) -> Self {
        BgImageInfo((position as u8) | ((fit as u8) << 4) | ((repeat as u8) << 6))
    }

    pub const fn repeat(self) -> bool {
        self.0 & (1 << 6) != 0
    }
}

/// Zig: `BgImage.Info.Position` (`enum(u4)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BgImagePosition {
    Tl = 0,
    Tc = 1,
    Tr = 2,
    Ml = 3,
    Mc = 4,
    Mr = 5,
    Bl = 6,
    Bc = 7,
    Br = 8,
}

/// Zig: `BgImage.Info.Fit` (`enum(u2)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BgImageFit {
    Contain = 0,
    Cover = 1,
    Stretch = 2,
    None = 3,
}

// Layout checks (these match the Zig `extern struct`s).
const _: () = {
    assert!(offset_of!(Uniforms, screen_size) == 64);
    assert!(offset_of!(Uniforms, cell_size) == 72);
    assert!(offset_of!(Uniforms, grid_size) == 80);
    assert!(offset_of!(Uniforms, grid_padding) == 96);
    assert!(offset_of!(Uniforms, padding_extend) == 112);
    assert!(offset_of!(Uniforms, min_contrast) == 116);
    assert!(offset_of!(Uniforms, cursor_pos) == 120);
    assert!(offset_of!(Uniforms, cursor_color) == 124);
    assert!(offset_of!(Uniforms, bg_color) == 128);
    assert!(offset_of!(Uniforms, bools) == 132);
    assert!(size_of::<Uniforms>() == 144);
    assert!(size_of::<Image>() == 40);
    assert!(size_of::<BgImage>() == 8);
};

/// Create an NSString (toll-free bridged CFString) from UTF-8 bytes.
fn ns_string(bytes: &[u8]) -> Result<&'static macos::foundation::String, Error> {
    Ok(macos::foundation::String::create_with_bytes(
        bytes,
        macos::foundation::StringEncoding::Utf8,
        false,
    )?)
}

/// Initialize the MTLLibrary. A MTLLibrary is a collection of shaders.
fn init_library(device: objc::Object) -> Result<objc::Object, Error> {
    let start = Instant::now();

    let source = ns_string(SHADERS_METAL.as_bytes())?;
    let mut err: objc::Id = std::ptr::null_mut();
    // SAFETY: valid MTLDevice and NSString.
    let library: objc::Object = unsafe {
        device.msg_send(
            objc::sel(c"newLibraryWithSource:options:error:"),
            (source.as_ptr(), std::ptr::null_mut::<c_void>(), &mut err as *mut objc::Id),
        )
    };
    source.release();
    check_error(err)?;

    log::debug!(
        target: LOG,
        "shader library loaded time={}us",
        start.elapsed().as_micros()
    );

    Ok(library)
}

/// Initialize our custom shader pipelines.
///
/// The shaders argument is a set of shader source code, not file paths.
fn init_post_pipelines(
    device: objc::Object,
    library: objc::Object,
    shaders: &[&CStr],
    pixel_format: mtl::MTLPixelFormat,
) -> Result<Vec<Pipeline>, Error> {
    // If we have no shaders, do nothing.
    if shaders.is_empty() {
        return Ok(Vec::new());
    }

    // Initialize our result set. If any error happens, we undo everything.
    let mut pipelines: Vec<Pipeline> = Vec::with_capacity(shaders.len());

    // Build each shader.
    for source in shaders {
        match init_post_pipeline(device, library, source, pixel_format) {
            Ok(p) => pipelines.push(p),
            Err(err) => {
                for pipeline in &pipelines {
                    pipeline.deinit();
                }
                return Err(err);
            }
        }
    }

    Ok(pipelines)
}

/// Initialize a single custom shader pipeline from shader source.
fn init_post_pipeline(
    device: objc::Object,
    library: objc::Object,
    data: &CStr,
    pixel_format: mtl::MTLPixelFormat,
) -> Result<Pipeline, Error> {
    // Create our library which has the shader source
    let post_library = {
        let source = ns_string(data.to_bytes())?;

        let mut err: objc::Id = std::ptr::null_mut();
        // SAFETY: valid MTLDevice and NSString.
        let post_library: objc::Object = unsafe {
            device.msg_send(
                objc::sel(c"newLibraryWithSource:options:error:"),
                (source.as_ptr(), std::ptr::null_mut::<c_void>(), &mut err as *mut objc::Id),
            )
        };
        source.release();
        check_error(err)?;

        post_library
    };

    let result = Pipeline::init(
        None,
        pipeline::Options {
            device,
            vertex_fn: "full_screen_vertex",
            fragment_fn: "main0",
            vertex_library: library,
            fragment_library: post_library,
            step_fn: mtl::MTLVertexStepFunction::PerVertex,
            attachments: &[pipeline::Attachment {
                pixel_format,
                blending_enabled: false,
            }],
        },
    );
    // SAFETY: we own the post library.
    unsafe { post_library.msg_send::<(), _>(objc::sel(c"release"), ()) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_text_size() {
        // Minimizing the size of this struct is important,
        // so we test it in order to be aware of any changes.
        assert_eq!(32, size_of::<CellText>());
    }
}
