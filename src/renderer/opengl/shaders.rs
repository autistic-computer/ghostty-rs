//! Port of `src/renderer/opengl/shaders.zig`.
//!
//! STATUS: ported

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::mem::{offset_of, size_of};
use std::sync::OnceLock;

use super::pipeline::{
    self, AttributeType, Pipeline, StepFunction, VertexAttribute, VertexAttributes, VertexLayout,
};

/// The GLSL sources under `../shaders/glsl/`, embedded at compile time
/// (Zig: `@embedFile`). Keyed by their path relative to that directory.
const SHADER_FILES: &[(&str, &str)] = &[
    (
        "bg_color.f.glsl",
        include_str!("../shaders/glsl/bg_color.f.glsl"),
    ),
    (
        "bg_image.f.glsl",
        include_str!("../shaders/glsl/bg_image.f.glsl"),
    ),
    (
        "bg_image.v.glsl",
        include_str!("../shaders/glsl/bg_image.v.glsl"),
    ),
    (
        "cell_bg.f.glsl",
        include_str!("../shaders/glsl/cell_bg.f.glsl"),
    ),
    (
        "cell_text.f.glsl",
        include_str!("../shaders/glsl/cell_text.f.glsl"),
    ),
    (
        "cell_text.v.glsl",
        include_str!("../shaders/glsl/cell_text.v.glsl"),
    ),
    ("common.glsl", include_str!("../shaders/glsl/common.glsl")),
    (
        "full_screen.v.glsl",
        include_str!("../shaders/glsl/full_screen.v.glsl"),
    ),
    ("image.f.glsl", include_str!("../shaders/glsl/image.f.glsl")),
    ("image.v.glsl", include_str!("../shaders/glsl/image.v.glsl")),
];

/// The names of the pipelines in [`PipelineCollection`], in declaration
/// order (Zig: the first element of each `pipeline_descs` entry).
pub const PIPELINE_NAMES: [&str; 5] = ["bg_color", "cell_bg", "cell_text", "image", "bg_image"];

/// Zig: `pipeline_descs`.
fn pipeline_descs() -> [(&'static str, PipelineDescription); 5] {
    [
        (
            "bg_color",
            PipelineDescription {
                vertex_fn: load_shader_code("../shaders/glsl/full_screen.v.glsl"),
                fragment_fn: load_shader_code("../shaders/glsl/bg_color.f.glsl"),
                blending_enabled: false,
                ..PipelineDescription::DEFAULT
            },
        ),
        (
            "cell_bg",
            PipelineDescription {
                vertex_fn: load_shader_code("../shaders/glsl/full_screen.v.glsl"),
                fragment_fn: load_shader_code("../shaders/glsl/cell_bg.f.glsl"),
                blending_enabled: true,
                ..PipelineDescription::DEFAULT
            },
        ),
        (
            "cell_text",
            PipelineDescription {
                vertex_attributes: Some(VertexLayout::of::<CellText>()),
                vertex_fn: load_shader_code("../shaders/glsl/cell_text.v.glsl"),
                fragment_fn: load_shader_code("../shaders/glsl/cell_text.f.glsl"),
                step_fn: StepFunction::PerInstance,
                blending_enabled: true,
            },
        ),
        (
            "image",
            PipelineDescription {
                vertex_attributes: Some(VertexLayout::of::<Image>()),
                vertex_fn: load_shader_code("../shaders/glsl/image.v.glsl"),
                fragment_fn: load_shader_code("../shaders/glsl/image.f.glsl"),
                step_fn: StepFunction::PerInstance,
                blending_enabled: true,
            },
        ),
        (
            "bg_image",
            PipelineDescription {
                vertex_attributes: Some(VertexLayout::of::<BgImage>()),
                vertex_fn: load_shader_code("../shaders/glsl/bg_image.v.glsl"),
                fragment_fn: load_shader_code("../shaders/glsl/bg_image.f.glsl"),
                step_fn: StepFunction::PerInstance,
                blending_enabled: true,
            },
        ),
    ]
}

/// All the comptime-known info about a pipeline, so that
/// we can define them ahead-of-time in an ergonomic way.
#[derive(Debug, Clone, Copy)]
struct PipelineDescription {
    vertex_attributes: Option<VertexLayout>,
    vertex_fn: &'static CStr,
    fragment_fn: &'static CStr,
    step_fn: StepFunction,
    blending_enabled: bool,
}

impl PipelineDescription {
    /// The Zig field defaults (the shader sources have none; they are
    /// always overridden).
    const DEFAULT: PipelineDescription = PipelineDescription {
        vertex_attributes: None,
        vertex_fn: c"",
        fragment_fn: c"",
        step_fn: StepFunction::PerVertex,
        blending_enabled: true,
    };

    fn init_pipeline(self) -> Result<Pipeline, pipeline::Error> {
        Pipeline::init(
            self.vertex_attributes,
            pipeline::Options {
                vertex_fn: self.vertex_fn,
                fragment_fn: self.fragment_fn,
                step_fn: self.step_fn,
                blending_enabled: self.blending_enabled,
            },
        )
    }
}

/// We create a type for the pipeline collection based on our desc array.
/// (Zig builds this struct type at comptime from `pipeline_descs`; the
/// field names here are [`PIPELINE_NAMES`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineCollection {
    pub bg_color: Pipeline,
    pub cell_bg: Pipeline,
    pub cell_text: Pipeline,
    pub image: Pipeline,
    pub bg_image: Pipeline,
}

impl PipelineCollection {
    /// Zig: `@field(pipelines, name)`.
    pub fn field(&self, name: &str) -> Option<&Pipeline> {
        match name {
            "bg_color" => Some(&self.bg_color),
            "cell_bg" => Some(&self.cell_bg),
            "cell_text" => Some(&self.cell_text),
            "image" => Some(&self.image),
            "bg_image" => Some(&self.bg_image),
            _ => None,
        }
    }

    fn field_mut(&mut self, name: &str) -> &mut Pipeline {
        match name {
            "bg_color" => &mut self.bg_color,
            "cell_bg" => &mut self.cell_bg,
            "cell_text" => &mut self.cell_text,
            "image" => &mut self.image,
            "bg_image" => &mut self.bg_image,
            _ => unreachable!(),
        }
    }

    /// Stand-in for Zig's `undefined` collection: all GL object names 0.
    const UNDEFINED: PipelineCollection = {
        const P: Pipeline = Pipeline {
            program: crate::pkg::opengl::Program { id: 0 },
            fbo: crate::pkg::opengl::Framebuffer { id: 0 },
            vao: crate::pkg::opengl::VertexArray { id: 0 },
            stride: 0,
            blending_enabled: false,
        };
        PipelineCollection {
            bg_color: P,
            cell_bg: P,
            cell_text: P,
            image: P,
            bg_image: P,
        }
    };
}

/// This contains the state for the shaders used by the Metal renderer.
#[derive(Debug)]
pub struct Shaders {
    /// Collection of available render pipelines.
    pub pipelines: PipelineCollection,

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
        pipelines: PipelineCollection::UNDEFINED,
        post_pipelines: Vec::new(),
        defunct: true,
    };

    /// Initialize our shader set.
    ///
    /// "post_shaders" is an optional list of postprocess shaders to run
    /// against the final drawable texture. This is an array of shader source
    /// code, not file paths.
    pub fn init(post_shaders: &[&CStr]) -> Result<Shaders, pipeline::Error> {
        let mut pipelines = PipelineCollection::UNDEFINED;

        let descs = pipeline_descs();
        let mut initialized_pipelines: usize = 0;

        for (name, desc) in descs.iter() {
            match desc.init_pipeline() {
                Ok(p) => *pipelines.field_mut(name) = p,
                Err(err) => {
                    // errdefer: deinit the pipelines initialized so far.
                    for (i, (name, _)) in descs.iter().enumerate() {
                        if i < initialized_pipelines {
                            pipelines.field_mut(name).deinit();
                        }
                    }
                    return Err(err);
                }
            }
            initialized_pipelines += 1;
        }

        let post_pipelines = match init_post_pipelines(post_shaders) {
            Ok(p) => p,
            Err(err) => {
                // If an error happens while building postprocess shaders we
                // want to just not use any postprocess shaders since we don't
                // want to block Ghostty from working.
                log::warn!(target: "opengl", "error initializing postprocess shaders err={err}");
                Vec::new()
            }
        };

        Ok(Shaders {
            pipelines,
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
        for name in PIPELINE_NAMES {
            self.pipelines.field_mut(name).deinit();
        }

        // Release our postprocess shaders
        if !self.post_pipelines.is_empty() {
            for pipeline in &self.post_pipelines {
                pipeline.deinit();
            }
            self.post_pipelines = Vec::new();
        }
    }
}

/// The uniforms that are passed to our shaders.
///
/// Zig: an `extern struct` with explicit field alignments; the padding
/// those alignments introduce is spelled out as `_padding_*` fields here
/// (the layout is checked in the tests and matches the std140 `Globals`
/// block in `common.glsl`).
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

    /// The minimum contrast ratio for text. The contrast ratio is calculated
    /// according to the WCAG 2.0 spec.
    pub min_contrast: f32,

    /// The cursor position and color.
    pub cursor_pos: [u16; 2],
    pub cursor_color: [u8; 4],

    /// The background color for the whole surface.
    pub bg_color: [u8; 4],

    /// Various booleans, in a packed struct for space efficiency.
    pub bools: UniformsBools,
}

/// Zig: `Uniforms.Bools`, a `packed struct(u32)` (LSB first).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct UniformsBools(pub u32);

impl UniformsBools {
    const CURSOR_WIDE: u32 = 1 << 0;
    const USE_DISPLAY_P3: u32 = 1 << 1;
    const USE_LINEAR_BLENDING: u32 = 1 << 2;
    const USE_LINEAR_CORRECTION: u32 = 1 << 3;

    pub const fn new(
        cursor_wide: bool,
        use_display_p3: bool,
        use_linear_blending: bool,
        use_linear_correction: bool,
    ) -> Self {
        UniformsBools(
            (cursor_wide as u32) * Self::CURSOR_WIDE
                | (use_display_p3 as u32) * Self::USE_DISPLAY_P3
                | (use_linear_blending as u32) * Self::USE_LINEAR_BLENDING
                | (use_linear_correction as u32) * Self::USE_LINEAR_CORRECTION,
        )
    }

    fn set(&mut self, bit: u32, v: bool) {
        if v {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// Whether the cursor is 2 cells wide.
    pub const fn cursor_wide(self) -> bool {
        self.0 & Self::CURSOR_WIDE != 0
    }
    pub fn set_cursor_wide(&mut self, v: bool) {
        self.set(Self::CURSOR_WIDE, v)
    }

    /// Indicates that colors provided to the shader are already in
    /// the P3 color space, so they don't need to be converted from
    /// sRGB.
    pub const fn use_display_p3(self) -> bool {
        self.0 & Self::USE_DISPLAY_P3 != 0
    }
    pub fn set_use_display_p3(&mut self, v: bool) {
        self.set(Self::USE_DISPLAY_P3, v)
    }

    /// Indicates that the color attachments for the shaders have
    /// an `*_srgb` pixel format, which means the shaders need to
    /// output linear RGB colors rather than gamma encoded colors,
    /// since blending will be performed in linear space and then
    /// Metal itself will re-encode the colors for storage.
    pub const fn use_linear_blending(self) -> bool {
        self.0 & Self::USE_LINEAR_BLENDING != 0
    }
    pub fn set_use_linear_blending(&mut self, v: bool) {
        self.set(Self::USE_LINEAR_BLENDING, v)
    }

    /// Enables a weight correction step that makes text rendered
    /// with linear alpha blending have a similar apparent weight
    /// (thickness) to gamma-incorrect blending.
    pub const fn use_linear_correction(self) -> bool {
        self.0 & Self::USE_LINEAR_CORRECTION != 0
    }
    pub fn set_use_linear_correction(&mut self, v: bool) {
        self.set(Self::USE_LINEAR_CORRECTION, v)
    }
}

/// Zig: `Uniforms.PaddingExtend`, a `packed struct(u32)` (LSB first).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PaddingExtend(pub u32);

impl PaddingExtend {
    const LEFT: u32 = 1 << 0;
    const RIGHT: u32 = 1 << 1;
    const UP: u32 = 1 << 2;
    const DOWN: u32 = 1 << 3;

    pub const fn new(left: bool, right: bool, up: bool, down: bool) -> Self {
        PaddingExtend(
            (left as u32) * Self::LEFT
                | (right as u32) * Self::RIGHT
                | (up as u32) * Self::UP
                | (down as u32) * Self::DOWN,
        )
    }

    fn set(&mut self, bit: u32, v: bool) {
        if v {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    pub const fn left(self) -> bool {
        self.0 & Self::LEFT != 0
    }
    pub fn set_left(&mut self, v: bool) {
        self.set(Self::LEFT, v)
    }
    pub const fn right(self) -> bool {
        self.0 & Self::RIGHT != 0
    }
    pub fn set_right(&mut self, v: bool) {
        self.set(Self::RIGHT, v)
    }
    pub const fn up(self) -> bool {
        self.0 & Self::UP != 0
    }
    pub fn set_up(&mut self, v: bool) {
        self.set(Self::UP, v)
    }
    pub const fn down(self) -> bool {
        self.0 & Self::DOWN != 0
    }
    pub fn set_down(&mut self, v: bool) {
        self.set(Self::DOWN, v)
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
            size: 2,
            typ: AttributeType::U32,
        },
        VertexAttribute {
            offset: offset_of!(CellText, glyph_size),
            size: 2,
            typ: AttributeType::U32,
        },
        VertexAttribute {
            offset: offset_of!(CellText, bearings),
            size: 2,
            typ: AttributeType::I16,
        },
        VertexAttribute {
            offset: offset_of!(CellText, grid_pos),
            size: 2,
            typ: AttributeType::U16,
        },
        VertexAttribute {
            offset: offset_of!(CellText, color),
            size: 4,
            typ: AttributeType::U8,
        },
        // enum(u8): the tag type.
        VertexAttribute {
            offset: offset_of!(CellText, atlas),
            size: 1,
            typ: AttributeType::U8,
        },
        // packed struct(u8): the backing integer.
        VertexAttribute {
            offset: offset_of!(CellText, bools),
            size: 1,
            typ: AttributeType::U8,
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

/// Zig: the `packed struct(u8)` type of `CellText.bools` (LSB first).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellTextBools(pub u8);

impl CellTextBools {
    const NO_MIN_CONTRAST: u8 = 1 << 0;
    const IS_CURSOR_GLYPH: u8 = 1 << 1;

    pub const fn new(no_min_contrast: bool, is_cursor_glyph: bool) -> Self {
        CellTextBools(
            (no_min_contrast as u8) * Self::NO_MIN_CONTRAST
                | (is_cursor_glyph as u8) * Self::IS_CURSOR_GLYPH,
        )
    }

    pub const fn no_min_contrast(self) -> bool {
        self.0 & Self::NO_MIN_CONTRAST != 0
    }
    pub fn set_no_min_contrast(&mut self, v: bool) {
        if v {
            self.0 |= Self::NO_MIN_CONTRAST;
        } else {
            self.0 &= !Self::NO_MIN_CONTRAST;
        }
    }
    pub const fn is_cursor_glyph(self) -> bool {
        self.0 & Self::IS_CURSOR_GLYPH != 0
    }
    pub fn set_is_cursor_glyph(&mut self, v: bool) {
        if v {
            self.0 |= Self::IS_CURSOR_GLYPH;
        } else {
            self.0 &= !Self::IS_CURSOR_GLYPH;
        }
    }
}

/// This is a single parameter for the cell bg shader.
pub type CellBg = [u8; 4];

/// Single parameter for the image shader. See shader for field details.
#[repr(C, align(16))]
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
            size: 2,
            typ: AttributeType::F32,
        },
        VertexAttribute {
            offset: offset_of!(Image, cell_offset),
            size: 2,
            typ: AttributeType::F32,
        },
        VertexAttribute {
            offset: offset_of!(Image, source_rect),
            size: 4,
            typ: AttributeType::F32,
        },
        VertexAttribute {
            offset: offset_of!(Image, dest_size),
            size: 2,
            typ: AttributeType::F32,
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
            size: 1,
            typ: AttributeType::F32,
        },
        // packed struct(u8): the backing integer.
        VertexAttribute {
            offset: offset_of!(BgImage, info),
            size: 1,
            typ: AttributeType::U8,
        },
    ];
}

/// Zig: `BgImage.Info`, a `packed struct(u8)`: `position: u4` (bits
/// 0-3), `fit: u2` (bits 4-5), `repeat: bool` (bit 6), 1 bit padding.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BgImageInfo(pub u8);

impl BgImageInfo {
    pub const fn new(position: BgImagePosition, fit: BgImageFit, repeat: bool) -> Self {
        BgImageInfo((position as u8) | (fit as u8) << 4 | (repeat as u8) << 6)
    }

    pub fn position(self) -> BgImagePosition {
        BgImagePosition::from_u4(self.0 & 0xF).expect("valid BgImage position")
    }

    pub fn fit(self) -> BgImageFit {
        match (self.0 >> 4) & 0x3 {
            0 => BgImageFit::Contain,
            1 => BgImageFit::Cover,
            2 => BgImageFit::Stretch,
            _ => BgImageFit::None,
        }
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

impl BgImagePosition {
    fn from_u4(v: u8) -> Option<Self> {
        Some(match v {
            0 => BgImagePosition::Tl,
            1 => BgImagePosition::Tc,
            2 => BgImagePosition::Tr,
            3 => BgImagePosition::Ml,
            4 => BgImagePosition::Mc,
            5 => BgImagePosition::Mr,
            6 => BgImagePosition::Bl,
            7 => BgImagePosition::Bc,
            8 => BgImagePosition::Br,
            _ => return None,
        })
    }
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

/// Initialize our custom shader pipelines. The shaders argument is a
/// set of shader source code, not file paths.
fn init_post_pipelines(shaders: &[&CStr]) -> Result<Vec<Pipeline>, pipeline::Error> {
    // If we have no shaders, do nothing.
    if shaders.is_empty() {
        return Ok(Vec::new());
    }

    // Initialize our result set. If any error happens, we undo everything.
    let mut pipelines: Vec<Pipeline> = Vec::with_capacity(shaders.len());

    // Build each shader. The vector's length tracks how many shaders we
    // successfully built so we can clean up on error.
    for &source in shaders {
        match init_post_pipeline(source) {
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
fn init_post_pipeline(data: &CStr) -> Result<Pipeline, pipeline::Error> {
    Pipeline::init(
        None,
        pipeline::Options::new(load_shader_code("../shaders/glsl/full_screen.v.glsl"), data),
    )
}

/// Load shader code from the target path, processing `#include` directives.
///
/// The Zig version runs at comptime; here the sources are embedded at
/// compile time ([`SHADER_FILES`]) and processed once, on first use, into
/// leaked (`'static`) strings. Like the Zig code, this is really sloppy
/// and makes a bunch of assumptions about things being well formed and
/// file names not containing quote marks. If we ever want to process
/// `#include`s for custom shaders then we need to write something better
/// than this for it.
fn load_shader_code(path: &str) -> &'static CStr {
    static CACHE: OnceLock<HashMap<&'static str, &'static CStr>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        SHADER_FILES
            .iter()
            .map(|&(name, contents)| {
                let processed = process_includes(contents, &|inc| embedded_file(inc));
                let c = CString::new(processed).expect("shader source has no NUL bytes");
                (name, &*Box::leak(c.into_boxed_c_str()))
            })
            .collect()
    });

    let name = path
        .strip_prefix("../shaders/glsl/")
        .expect("shader path is under ../shaders/glsl/");
    cache
        .get(name)
        .copied()
        .unwrap_or_else(|| panic!("unknown shader {path}"))
}

/// Zig: `@embedFile(basedir ++ "/" ++ name)`.
fn embedded_file(name: &str) -> &'static str {
    SHADER_FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or_else(|| panic!("unknown shader include {name}"))
}

/// Used by load_shader_code
fn process_includes(contents: &str, embed: &dyn Fn(&str) -> &'static str) -> String {
    let bytes = contents.as_bytes();
    let mut i: usize = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"#include") {
            debug_assert!(bytes[i..].starts_with(b"#include \""));
            let start = i + "#include \"".len();
            let end = start + memchr::memchr(b'"', &bytes[start..]).expect("unterminated #include");
            return format!(
                "{}{}{}",
                &contents[..i],
                embed(&contents[start..end]),
                process_includes(&contents[end + 1..], embed),
            );
        }
        if let Some(j) = memchr::memmem::find(&bytes[i..], b"\n#") {
            i += j + 1;
        } else {
            break;
        }
    }
    contents.to_owned()
}

/// Compile-time layout checks (Zig: the `extern struct` layouts; the
/// GLSL side reads these byte-for-byte).
const _: () = {
    assert!(size_of::<Uniforms>() == 144);
    assert!(offset_of!(Uniforms, projection_matrix) == 0);
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

    assert!(size_of::<CellText>() == 32);
    assert!(std::mem::align_of::<CellText>() == 8);
    assert!(offset_of!(CellText, glyph_size) == 8);
    assert!(offset_of!(CellText, bearings) == 16);
    assert!(offset_of!(CellText, grid_pos) == 20);
    assert!(offset_of!(CellText, color) == 24);
    assert!(offset_of!(CellText, atlas) == 28);
    assert!(offset_of!(CellText, bools) == 29);

    assert!(size_of::<Image>() == 48);
    assert!(offset_of!(Image, cell_offset) == 8);
    assert!(offset_of!(Image, source_rect) == 16);
    assert!(offset_of!(Image, dest_size) == 32);

    assert!(size_of::<BgImage>() == 8);
    assert!(offset_of!(BgImage, info) == 4);

    assert!(size_of::<CellBg>() == 4);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    // Zig has this commented out in `CellText`; the size is also checked
    // at compile time above.
    #[test]
    fn cell_text_size() {
        // Minimizing the size of this struct is important,
        // so we test it in order to be aware of any changes.
        assert_eq!(32, size_of::<CellText>());
    }

    // The tests below are not Zig tests (shaders.zig has none).

    #[test]
    fn layouts() {
        assert_eq!(144, size_of::<Uniforms>());
        assert_eq!(16, std::mem::align_of::<Uniforms>());
        assert_eq!(48, size_of::<Image>());
        assert_eq!(8, size_of::<BgImage>());
        assert_eq!(7, CellText::ATTRIBUTES.len());
        assert_eq!(4, Image::ATTRIBUTES.len());
        assert_eq!(2, BgImage::ATTRIBUTES.len());
    }

    #[test]
    fn packed_bits() {
        let b = UniformsBools::new(true, false, true, false);
        assert_eq!(0b0101, b.0);
        assert!(b.cursor_wide() && b.use_linear_blending());
        assert!(!b.use_display_p3() && !b.use_linear_correction());
        let mut p = PaddingExtend::default();
        p.set_down(true);
        p.set_left(true);
        assert_eq!(0b1001, p.0);
        assert_eq!(0b10, CellTextBools::new(false, true).0);
        let info = BgImageInfo::new(BgImagePosition::Br, BgImageFit::Stretch, true);
        assert_eq!(8 | 2 << 4 | 1 << 6, info.0);
        assert_eq!(BgImagePosition::Br, info.position());
        assert_eq!(BgImageFit::Stretch, info.fit());
        assert!(info.repeat());
    }

    #[test]
    fn process_includes_inlines_common() {
        let embed = |name: &str| -> &'static str {
            assert_eq!("inc.glsl", name);
            "INCLUDED\n"
        };
        assert_eq!(
            "#version 1\nINCLUDED\n\nvoid main() {}\n",
            process_includes(
                "#version 1\n#include \"inc.glsl\"\nvoid main() {}\n",
                &embed
            )
        );
        assert_eq!(
            "INCLUDED\n\nx",
            process_includes("#include \"inc.glsl\"\nx", &embed)
        );
        assert_eq!("#define X\nfoo", process_includes("#define X\nfoo", &embed));

        let src = load_shader_code("../shaders/glsl/cell_bg.f.glsl")
            .to_str()
            .unwrap();
        // The directive was replaced (common.glsl itself mentions
        // `#include "common.glsl"` in a comment, which stays).
        assert!(!src.lines().any(|l| l.starts_with("#include")));
        assert!(src.contains("uniform Globals"));
        assert!(src.starts_with("#version 430 core"));
    }

    #[test]
    fn init_shaders() {
        let _ctx = TestContext::new();
        let mut shaders = Shaders::init(&[c"#version 430 core
            layout(binding = 0) uniform sampler2D iChannel0;
            out vec4 frag;
            void main() { frag = texelFetch(iChannel0, ivec2(gl_FragCoord.xy), 0); }"])
        .unwrap();
        assert!(!shaders.defunct);
        assert_eq!(1, shaders.post_pipelines.len());
        assert_eq!(size_of::<CellText>(), shaders.pipelines.cell_text.stride);
        assert_eq!(0, shaders.pipelines.bg_color.stride);
        assert!(!shaders.pipelines.bg_color.blending_enabled);
        for name in PIPELINE_NAMES {
            assert_ne!(0, shaders.pipelines.field(name).unwrap().program.id);
        }
        shaders.deinit();
        assert!(shaders.defunct);
        // Double deinit is ignored.
        shaders.deinit();

        // Invalid postprocess shaders are skipped.
        let mut shaders = Shaders::init(&[c"not glsl"]).unwrap();
        assert!(shaders.post_pipelines.is_empty());
        shaders.deinit();

        let mut uninit = Shaders::UNINIT;
        uninit.deinit();
    }
}
