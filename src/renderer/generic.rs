//! Port of `src/renderer/generic.zig`.
//!
//! STATUS: ported
//!
//! Create a renderer type with the provided graphics API wrapper.
//!
//! The graphics API wrapper must provide the interface outlined below.
//! Specific details for the interfaces are documented on the existing
//! implementations (`Metal` and `OpenGL`).
//!
//! Hierarchy of graphics abstractions:
//!
//! ```text
//! [ GraphicsAPI ] - Responsible for configuring the runtime surface
//!    |     |        and providing render `Target`s that draw to it,
//!    |     |        as well as `Frame`s and `Pipeline`s.
//!    |     V
//!    | [ Target ] - Represents an abstract target for rendering, which
//!    |              could be a surface directly but is also used as an
//!    |              abstraction for off-screen frame buffers.
//!    V
//! [ Frame ] - Represents the context for drawing a given frame,
//!    |        provides `RenderPass`es for issuing draw commands
//!    |        to, and reports the frame health when complete.
//!    V
//! [ RenderPass ] - Represents a render pass in a frame, consisting of
//!   :              one or more `Step`s applied to the same target(s),
//! [ Step ] - - - - each describing the input buffers and textures and
//!   :              the vertex/fragment functions and geometry to use.
//!   :_ _ _ _ _ _ _ _ _ _/
//!   v
//! [ Pipeline ] - Describes a vertex and fragment function to be used
//!                for a `Step`; the `GraphicsAPI` is responsible for
//!                these and they should be constructed and cached
//!                ahead of time.
//!
//! [ Buffer ] - An abstraction over a GPU buffer.
//!
//! [ Texture ] - An abstraction over a GPU texture.
//!
//! [ ExportedFrame ] - A finished frame ready to be consumed by the apprt
//!                     in case that the frame needs to be composited with
//!                     UI elements by the graphical toolkit manually.
//! ```
//!
//! Deviations from the Zig code:
//! * `Renderer(GraphicsAPI)` is `Renderer<G: GraphicsApi>`. Zig duck-types
//!   the API (with `@hasDecl` for optional hooks); here [`GraphicsApi`]
//!   is a trait whose optional hooks have no-op defaults. The GPU resource
//!   and shader data types (`Buffer`, `Texture`, `Target`, `RenderPass`,
//!   `shaders.*`, ...) are taken from the build's API module
//!   ([`crate::renderer::graphics_api`]), exactly the types `cell.zig` and
//!   `image.zig` already use through `renderer.Renderer.API`. The few
//!   places where the OpenGL and Metal ports spell these types differently
//!   go through the private `compat` module.
//! * Cell building (`rebuildRow`, `addGlyph`, `addUnderline`, ...) runs on
//!   a private [`RebuildCtx`] borrowing the renderer fields it needs, so
//!   that the font shaper's run iterator (which mutably borrows the
//!   shaper) can be held while glyphs are added. Shaped cells are copied
//!   into a reusable buffer instead of being borrowed from the shaper.
//! * `std.Io.Semaphore` is a small mutex/condvar [`Semaphore`]
//!   (NOTE(port): Zig std library type with no Rust counterpart);
//!   timestamps are `Instant`s.
//! * The allocator field and parameters are dropped; arenas are plain
//!   owned values. `DerivedConfig` owns its data (no arena).
//! * `updateFrame`/`drawFrame` return `Result<(), Error>` with a boxed
//!   error (Zig: inferred error sets).
//! * `frameCompleted` only releases the swap chain semaphore when a swap
//!   chain exists (Zig unwraps it), so frames begun outside `drawFrame`
//!   (the OpenGL frame test) can complete.
//! * The macOS display link is `Option<*mut DisplayLink>` under
//!   `cfg(target_os = "macos")` (Zig: `void` elsewhere).
//! * The OSC 8 / regex link sets, preedit and overlay feature list taken
//!   in the critical section are owned values (Zig: arena allocated).

#![allow(clippy::too_many_arguments)]

use std::ffi::CStr;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex as StdMutex};
use std::time::Instant;

use crate::apprt;
use crate::config::config::{
    AlphaBlending, BackgroundBlur, BackgroundImageFit, BackgroundImagePosition, CustomShaderAnimation,
    FontShapingBreak, FontStyle, TerminalColor, WindowColorspace, WindowPaddingColor,
};
use crate::config::path::{Path as ConfigPath, RepeatablePath};
use crate::config::Config;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::file_type::FileType;
use crate::font::codepoint_resolver::StyleStatus;
use crate::font::glyph::{Constraint, ConstraintSize, RenderOptions};
use crate::font::nerd_font_attributes::get_constraint;
use crate::font::shape::{self, RunOptions, TextRun};
use crate::font::{self, Presentation, SharedGrid, Sprite};
use crate::global::xev;
use crate::input::key::ctrl_or_super;
use crate::input::key_mods::Mods;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::math;
use crate::pkg::oniguruma as oni;
use crate::pkg::wuffs;
use crate::renderer::cell::{self as cellpkg, constraint_width, is_covering, no_min_contrast, Key as CellKey};
use crate::renderer::cursor::{self, Style as CursorStyle, StyleOptions as CursorStyleOptions};
use crate::renderer::graphics_api as api;
use crate::renderer::image::{self as imagepkg, DrawPlacements, State as ImageState};
use crate::renderer::link;
use crate::renderer::message::{SearchMatch, SearchMatches};
use crate::renderer::overlay::{self, Overlay};
use crate::renderer::row::never_extend_bg;
use crate::renderer::shadertoy;
use crate::renderer::size::{CellSize, GridSize, Padding, ScreenSize, Size};
use crate::renderer::state::{Preedit, State};
use crate::renderer::{Health, Options, Thread};
use crate::terminal::color::{Palette, Rgb};
use crate::terminal::modes::Mode;
use crate::terminal::page::{self, Wide};
use crate::terminal::page_list::Scrollbar;
use crate::terminal::point;
use crate::terminal::render::{CellSet, Cells, CursorViewport, Dirty, Highlight, RenderState};
use crate::terminal::sgr::Underline;
use crate::terminal::size::CellCountInt;
use crate::terminal::style::{self, Fg as StyleFg, Style};
use crate::terminal::terminal::ScrollViewport;

#[cfg(target_os = "macos")]
use crate::renderer::display_link::DisplayLink;

const LOG: &str = "generic_renderer";

/// The error type of the renderer's fallible methods (Zig: inferred
/// error sets).
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The interface the generic renderer expects of a graphics API. Zig
/// checks optional hooks with `@hasDecl`; here they have no-op defaults.
/// See the module docs for how the API's types are reached.
pub trait GraphicsApi: Sized {
    /// Frames exported to the apprt (Zig: `void` when the API does not
    /// export frames).
    type ExportedFrame: Send;

    /// Whether the API exports frames (`ExportedFrame != void`).
    const EXPORTS_FRAMES: bool;

    /// The shader language custom shaders are translated to.
    const CUSTOM_SHADER_TARGET: shadertoy::Target;

    /// Whether +Y is down in the coordinate space of exported
    /// frames. Apprts use this to orient frames when presenting.
    const CUSTOM_SHADER_Y_IS_DOWN: bool;

    /// The count of buffers we use for double/triple buffering.
    const SWAP_CHAIN_COUNT: usize;

    /// Initialize the API for a surface (Zig: `GraphicsAPI.init`).
    fn init(device: *const crate::renderer::Device, options: &Options) -> Result<Self, Error>;

    /// Release the API's resources.
    fn deinit(&mut self);

    fn thread_enter(&mut self, _surface: *mut apprt::Surface) -> Result<(), Error> {
        Ok(())
    }

    fn thread_exit(&mut self) {}

    /// Zig: `loopEnter(self)`. The renderer pointer stands in for
    /// `@fieldParentPtr("api", self)`.
    fn loop_enter(&mut self, _renderer: *mut Renderer<Self>) {}

    fn loop_exit(&mut self) {}

    fn display_realized(&mut self) {}

    /// Some graphics APIs need to manually update their viewport
    /// (Zig: `@hasDecl(GraphicsAPI, "setViewport")`).
    fn set_viewport(&self, _width: u32, _height: u32) {}

    /// Zig: `api.blending = ...`.
    fn set_blending(&mut self, blending: AlphaBlending);

    fn surface_size(&self) -> Result<api::SurfaceSize, Error>;

    fn draw_frame_start(&mut self);
    fn draw_frame_end(&mut self);

    fn init_shaders(&self, custom_shaders: &[&CStr]) -> Result<api::shaders::Shaders, Error>;
    fn init_target(&self, width: usize, height: usize) -> Result<api::Target, Error>;

    fn uniform_buffer_options(&self) -> api::buffer::Options;
    fn fg_buffer_options(&self) -> api::buffer::Options;
    fn bg_buffer_options(&self) -> api::buffer::Options;
    fn image_buffer_options(&self) -> api::buffer::Options;
    fn bg_image_buffer_options(&self) -> api::buffer::Options;
    fn texture_options(&self) -> api::texture::Options;
    fn sampler_options(&self) -> api::sampler::Options;
    fn image_texture_options(&self, format: api::ImageTextureFormat, srgb: bool) -> api::texture::Options;
    fn init_atlas_texture(&self, atlas: &font::Atlas) -> Result<api::Texture, Error>;

    fn begin_frame(&self, renderer: *mut Renderer<Self>, target: *mut api::Target) -> Result<api::Frame, Error>;
}

/// The differences between the OpenGL and Metal ports' spellings of the
/// API types.
#[cfg(not(target_os = "macos"))]
mod compat {
    use super::api;
    use super::Error;

    pub fn pipelines(s: &api::shaders::Shaders) -> &api::shaders::PipelineCollection {
        &s.pipelines
    }

    pub fn set_cursor_wide(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.set_cursor_wide(v)
    }
    pub fn set_use_display_p3(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.set_use_display_p3(v)
    }
    pub fn set_use_linear_blending(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.set_use_linear_blending(v)
    }
    pub fn set_use_linear_correction(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.set_use_linear_correction(v)
    }

    pub fn replace_region(t: &api::Texture, w: usize, h: usize, data: &[u8]) -> Result<(), Error> {
        Ok(t.replace_region(0, 0, w, h, data)?)
    }
}

#[cfg(target_os = "macos")]
mod compat {
    use super::api;
    use super::Error;

    pub fn pipelines(s: &api::shaders::Shaders) -> &api::shaders::PipelineCollection {
        s.pipelines.as_ref().expect("shaders are initialized")
    }

    pub fn set_cursor_wide(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.cursor_wide = v
    }
    pub fn set_use_display_p3(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.use_display_p3 = v
    }
    pub fn set_use_linear_blending(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.use_linear_blending = v
    }
    pub fn set_use_linear_correction(u: &mut api::shaders::Uniforms, v: bool) {
        u.bools.use_linear_correction = v
    }

    pub fn replace_region(t: &api::Texture, w: usize, h: usize, data: &[u8]) -> Result<(), Error> {
        t.replace_region(0, 0, w, h, data);
        Ok(())
    }
}

/// A counting semaphore (Zig: `std.Io.Semaphore`).
///
/// NOTE(port): `std.Io.Semaphore` is Zig standard library; Rust's std has
/// no semaphore and `lib::tinyio` does not provide one, so it stays here.
#[derive(Debug)]
pub struct Semaphore {
    permits: StdMutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    pub fn new(permits: usize) -> Semaphore {
        Semaphore {
            permits: StdMutex::new(permits),
            cond: Condvar::new(),
        }
    }

    pub fn wait_uncancelable(&self) {
        let mut p = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        while *p == 0 {
            p = self.cond.wait(p).unwrap_or_else(|e| e.into_inner());
        }
        *p -= 1;
    }

    pub fn post(&self) {
        let mut p = self.permits.lock().unwrap_or_else(|e| e.into_inner());
        *p += 1;
        self.cond.notify_one();
    }
}

/// A health value that can be read and written atomically (Zig:
/// `std.atomic.Value(Health)`).
#[derive(Debug)]
pub struct AtomicHealth(AtomicU8);

impl AtomicHealth {
    pub const fn new(v: Health) -> AtomicHealth {
        AtomicHealth(AtomicU8::new(v as u8))
    }

    pub fn load(&self) -> Health {
        match self.0.load(Ordering::SeqCst) {
            0 => Health::Healthy,
            _ => Health::Unhealthy,
        }
    }

    pub fn store(&self, v: Health) {
        self.0.store(v as u8, Ordering::SeqCst);
    }
}

/// A thread-safe, single-slot "latest wins" queue. The render thread
/// calls `push` with the latest frame; the apprt calls `take` in its
/// snapshot handler to grab the most recent frame. Old frames are
/// dropped and released. For a terminal this is correct — we never
/// want to queue up frames behind a slow compositor.
#[derive(Debug)]
pub struct LatestFrame<F> {
    pub latest: StdMutex<Option<F>>,
}

impl<F> Default for LatestFrame<F> {
    fn default() -> Self {
        LatestFrame {
            latest: StdMutex::new(None),
        }
    }
}

impl<F> LatestFrame<F> {
    pub fn push(&self, value: F) {
        // Old frames are released on drop.
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }

    pub fn take(&self) -> Option<F> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn deinit(&self) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Zig: `HighlightTag`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightTag {
    SearchMatch = 0,
    SearchMatchSelected = 1,
}

impl HighlightTag {
    fn from_u8(v: u8) -> HighlightTag {
        match v {
            0 => HighlightTag::SearchMatch,
            _ => HighlightTag::SearchMatchSelected,
        }
    }
}

/// Swap chain which maintains multiple copies of the state needed to
/// render a frame, so that we can start building the next frame while
/// the previous frame is still being processed on the GPU.
#[derive(Debug)]
pub struct SwapChain {
    /// `buf_count` structs that can hold the
    /// data needed by the GPU to draw a frame.
    pub frames: Vec<FrameState>,
    /// Index of the most recently used frame state struct.
    pub frame_index: usize,
    /// Semaphore that we wait on to make sure we have an available
    /// frame state struct so we can start working on a new frame.
    pub frame_sema: Semaphore,
}

impl SwapChain {
    pub fn init<G: GraphicsApi>(api: &G, custom_shaders: bool) -> Result<SwapChain, Error> {
        let buf_count = G::SWAP_CHAIN_COUNT;
        let mut frames: Vec<FrameState> = Vec::with_capacity(buf_count);

        // Initialize all of our frame state.
        for _ in 0..buf_count {
            match FrameState::init(api, custom_shaders) {
                Ok(f) => frames.push(f),
                Err(e) => {
                    for f in &mut frames {
                        f.deinit();
                    }
                    return Err(e);
                }
            }
        }

        Ok(SwapChain {
            frames,
            frame_index: 0,
            frame_sema: Semaphore::new(buf_count),
        })
    }

    pub fn deinit(&mut self) {
        // Wait for all of our inflight draws to complete
        // so that we can cleanly deinit our GPU state.
        for _ in 0..self.frames.len() {
            self.frame_sema.wait_uncancelable();
        }
        for frame in &mut self.frames {
            frame.deinit();
        }
    }

    /// Get the next frame state to draw to. This will wait on the
    /// semaphore to ensure that the frame is available. This must
    /// always be paired with a call to releaseFrame.
    pub fn next_frame(&mut self) -> &mut FrameState {
        self.frame_sema.wait_uncancelable();
        self.frame_index = (self.frame_index + 1) % self.frames.len();
        &mut self.frames[self.frame_index]
    }

    /// This should be called when the frame has completed drawing.
    pub fn release_frame(&self) {
        self.frame_sema.post();
    }
}

/// State we need duplicated for every frame. Any state that could be
/// in a data race between the GPU and CPU while a frame is being drawn
/// should be in this struct.
///
/// While a draw is in-process, we "lock" the state (via a semaphore)
/// and prevent the CPU from updating the state until our graphics API
/// reports that the frame is complete.
///
/// This is used to implement double/triple buffering.
#[derive(Debug)]
pub struct FrameState {
    pub uniforms: api::Buffer<api::shaders::Uniforms>,
    pub cells: api::Buffer<api::shaders::CellText>,
    pub cells_bg: api::Buffer<api::shaders::CellBg>,

    pub grayscale: api::Texture,
    pub grayscale_modified: usize,
    pub color: api::Texture,
    pub color_modified: usize,

    pub target: api::Target,
    /// See property of same name on Renderer for explanation.
    pub target_config_modified: usize,

    /// Buffer with the vertex data for our background image.
    ///
    /// TODO: Make this an optional and only create it
    ///       if we actually have a background image.
    pub bg_image_buffer: api::Buffer<api::shaders::BgImage>,
    /// See property of same name on Renderer for explanation.
    pub bg_image_buffer_modified: usize,

    /// Custom shader state, this is null if we have no custom shaders.
    pub custom_shader_state: Option<CustomShaderState>,
}

/// A 1x1 atlas description for sizing a texture (Zig: an `Atlas`
/// literal with undefined data; `Atlas.init` cannot make one this small).
fn empty_atlas(format: font::atlas::Format) -> font::Atlas {
    font::Atlas {
        data: vec![0; format.depth() as usize],
        size: 1,
        nodes: Vec::new(),
        format,
        modified: std::sync::atomic::AtomicUsize::new(0),
        resized: std::sync::atomic::AtomicUsize::new(0),
    }
}

impl FrameState {
    pub fn init<G: GraphicsApi>(api: &G, custom_shaders: bool) -> Result<FrameState, Error> {
        // Uniform buffer contains exactly 1 uniform struct. The
        // uniform data will be undefined so this must be set before
        // a frame is drawn.
        let uniforms = api::Buffer::<api::shaders::Uniforms>::init(api.uniform_buffer_options(), 1)?;

        // Create GPU buffers for our cells.
        //
        // We start them off with a size of 1, which will of course be
        // too small, but they will be resized as needed. This is a bit
        // wasteful but since it's a one-time thing it's not really a
        // huge concern.
        let cells = match api::Buffer::<api::shaders::CellText>::init(api.fg_buffer_options(), 1) {
            Ok(v) => v,
            Err(e) => {
                uniforms.deinit();
                return Err(e.into());
            }
        };
        let cells_bg = match api::Buffer::<api::shaders::CellBg>::init(api.bg_buffer_options(), 1) {
            Ok(v) => v,
            Err(e) => {
                cells.deinit();
                uniforms.deinit();
                return Err(e.into());
            }
        };

        // Create a GPU buffer for our background image info.
        let bg_image_buffer = match api::Buffer::<api::shaders::BgImage>::init(api.bg_image_buffer_options(), 1) {
            Ok(v) => v,
            Err(e) => {
                cells_bg.deinit();
                cells.deinit();
                uniforms.deinit();
                return Err(e.into());
            }
        };

        let cleanup_buffers = || {
            bg_image_buffer.deinit();
            cells_bg.deinit();
            cells.deinit();
            uniforms.deinit();
        };

        // Initialize our textures for our font atlas.
        //
        // As with the buffers above, we start these off as small
        // as possible since they'll inevitably be resized anyway.
        let grayscale = match api.init_atlas_texture(&empty_atlas(font::atlas::Format::Grayscale))
        {
            Ok(v) => v,
            Err(e) => {
                cleanup_buffers();
                return Err(e);
            }
        };
        let color = match api.init_atlas_texture(&empty_atlas(font::atlas::Format::Bgra))
        {
            Ok(v) => v,
            Err(e) => {
                grayscale.deinit();
                cleanup_buffers();
                return Err(e);
            }
        };

        let mut custom_shader_state = if custom_shaders {
            match CustomShaderState::init(api) {
                Ok(v) => Some(v),
                Err(e) => {
                    color.deinit();
                    grayscale.deinit();
                    cleanup_buffers();
                    return Err(e);
                }
            }
        } else {
            None
        };

        // Initialize the target. Just as with the other resources,
        // start it off as small as we can since it'll be resized.
        let target = match api.init_target(1, 1) {
            Ok(v) => v,
            Err(e) => {
                if let Some(state) = &mut custom_shader_state {
                    state.deinit();
                }
                color.deinit();
                grayscale.deinit();
                cleanup_buffers();
                return Err(e);
            }
        };

        Ok(FrameState {
            uniforms,
            cells,
            cells_bg,
            bg_image_buffer,
            bg_image_buffer_modified: 0,
            grayscale,
            grayscale_modified: 0,
            color,
            color_modified: 0,
            target,
            target_config_modified: 0,
            custom_shader_state,
        })
    }

    pub fn deinit(&mut self) {
        self.target.deinit();
        self.uniforms.deinit();
        self.cells.deinit();
        self.cells_bg.deinit();
        self.grayscale.deinit();
        self.color.deinit();
        self.bg_image_buffer.deinit();
        if let Some(state) = &mut self.custom_shader_state {
            state.deinit();
        }
    }

    pub fn resize<G: GraphicsApi>(&mut self, api: &G, width: usize, height: usize) -> Result<(), Error> {
        if let Some(state) = &mut self.custom_shader_state {
            state.resize(api, width, height)?;
        }
        let target = api.init_target(width, height)?;
        self.target.deinit();
        self.target = target;
        Ok(())
    }
}

/// State relevant to our custom shaders if we have any.
#[derive(Debug)]
pub struct CustomShaderState {
    /// When we have a custom shader state, we maintain a front
    /// and back texture which we use as a swap chain to render
    /// between when multiple custom shaders are defined.
    pub front_texture: api::Texture,
    pub back_texture: api::Texture,

    /// Shadertoy uses a sampler for accessing the various channel
    /// textures. In Metal, we need to explicitly create these since
    /// the glslang-to-msl compiler doesn't do it for us (as we
    /// normally would in hand-written MSL). To keep it clean and
    /// consistent, we just force all rendering APIs to provide an
    /// explicit sampler.
    ///
    /// Samplers are immutable and describe sampling properties so
    /// we can share the sampler across front/back textures (although
    /// we only need it for the source texture at a time, we don't
    /// need to "swap" it).
    pub sampler: api::Sampler,

    pub uniforms: api::Buffer<shadertoy::Uniforms>,
}

impl CustomShaderState {
    /// Swap the front and back textures.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.front_texture, &mut self.back_texture);
    }

    pub fn init<G: GraphicsApi>(api: &G) -> Result<CustomShaderState, Error> {
        // Create a GPU buffer to hold our uniforms.
        let uniforms = api::Buffer::<shadertoy::Uniforms>::init(api.uniform_buffer_options(), 1)?;

        // Initialize the front and back textures at 1x1 px, this
        // is slightly wasteful but it's only done once so whatever.
        let front_texture = match api::Texture::init(api.texture_options(), 1, 1, None) {
            Ok(v) => v,
            Err(e) => {
                uniforms.deinit();
                return Err(e.into());
            }
        };
        let back_texture = match api::Texture::init(api.texture_options(), 1, 1, None) {
            Ok(v) => v,
            Err(e) => {
                front_texture.deinit();
                uniforms.deinit();
                return Err(e.into());
            }
        };

        let sampler = match api::Sampler::init(api.sampler_options()) {
            Ok(v) => v,
            Err(e) => {
                back_texture.deinit();
                front_texture.deinit();
                uniforms.deinit();
                return Err(e.into());
            }
        };

        Ok(CustomShaderState {
            front_texture,
            back_texture,
            sampler,
            uniforms,
        })
    }

    pub fn deinit(&mut self) {
        self.front_texture.deinit();
        self.back_texture.deinit();
        self.sampler.deinit();
        self.uniforms.deinit();
    }

    pub fn resize<G: GraphicsApi>(&mut self, api: &G, width: usize, height: usize) -> Result<(), Error> {
        let front_texture = api::Texture::init(api.texture_options(), width, height, None)?;
        let back_texture = match api::Texture::init(api.texture_options(), width, height, None) {
            Ok(v) => v,
            Err(e) => {
                front_texture.deinit();
                return Err(e.into());
            }
        };

        self.front_texture.deinit();
        self.back_texture.deinit();

        self.front_texture = front_texture;
        self.back_texture = back_texture;
        Ok(())
    }
}

/// Error from initializing a [`DerivedConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    OutOfMemory,
    /// A link regex failed to compile.
    Oniguruma(oni::errors::Error),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitError {}

/// The configuration for this renderer that is derived from the main
/// configuration. This must be exported so that we don't need to
/// pass around Config pointers which makes memory management a pain.
#[derive(Debug)]
pub struct DerivedConfig {
    pub font_thicken: bool,
    pub font_thicken_strength: u8,
    pub font_features: Vec<String>,
    pub font_styles: StyleStatus,
    pub font_shaping_break: FontShapingBreak,
    pub cursor_color: Option<TerminalColor>,
    pub cursor_opacity: f64,
    pub cursor_text: Option<TerminalColor>,
    pub background: Rgb,
    pub background_opacity: f64,
    pub background_opacity_cells: bool,
    pub foreground: Rgb,
    pub selection_background: Option<TerminalColor>,
    pub selection_foreground: Option<TerminalColor>,
    pub search_background: TerminalColor,
    pub search_foreground: TerminalColor,
    pub search_selected_background: TerminalColor,
    pub search_selected_foreground: TerminalColor,
    pub bold_color: Option<style::BoldColor>,
    pub faint_opacity: u8,
    pub min_contrast: f32,
    pub padding_color: WindowPaddingColor,
    pub custom_shaders: RepeatablePath,
    pub bg_image: Option<ConfigPath>,
    pub bg_image_opacity: f32,
    pub bg_image_position: BackgroundImagePosition,
    pub bg_image_fit: BackgroundImageFit,
    pub bg_image_repeat: bool,
    pub links: link::Set,
    pub vsync: bool,
    pub colorspace: WindowColorspace,
    pub blending: AlphaBlending,
    pub background_blur: BackgroundBlur,
    pub scroll_to_bottom_on_output: bool,
    pub custom_shader_animation: CustomShaderAnimation,
}

impl Default for DerivedConfig {
    /// The derived config of a default `Config` without its links (an
    /// inexpensive placeholder; Zig has no default).
    fn default() -> Self {
        let config = Config::default();
        DerivedConfig::init_with_links(&config, link::Set::default())
    }
}

impl DerivedConfig {
    pub fn init(config: &Config) -> Result<DerivedConfig, InitError> {
        // Our link configs
        let links = link::Set::from_config(&config.link.links).map_err(InitError::Oniguruma)?;
        Ok(Self::init_with_links(config, links))
    }

    fn init_with_links(config: &Config, links: link::Set) -> DerivedConfig {
        // Copy our shaders
        let custom_shaders = config.custom_shader.clone();

        // Copy our background image
        let bg_image = config.background_image.clone();

        // Copy our font features
        let font_features: Vec<String> = config.font_feature.list.iter().map(|s| s.as_str().to_owned()).collect();

        // Get our font styles
        let mut font_styles = StyleStatus::init_fill(true);
        font_styles.set(font::Style::Bold, !matches!(config.font_style_bold, FontStyle::False));
        font_styles.set(font::Style::Italic, !matches!(config.font_style_italic, FontStyle::False));
        font_styles.set(
            font::Style::BoldItalic,
            !matches!(config.font_style_bold_italic, FontStyle::False),
        );

        DerivedConfig {
            background_opacity: config.background_opacity.clamp(0.0, 1.0),
            background_opacity_cells: config.background_opacity_cells,
            font_thicken: config.font_thicken,
            font_thicken_strength: config.font_thicken_strength,
            font_features,
            font_styles,
            font_shaping_break: config.font_shaping_break,

            cursor_color: config.cursor_color,
            cursor_text: config.cursor_text,
            cursor_opacity: config.cursor_opacity.clamp(0.0, 1.0),

            background: config.background.to_terminal_rgb(),
            foreground: config.foreground.to_terminal_rgb(),
            bold_color: config.bold_color.map(|b| b.to_terminal()),
            faint_opacity: (config.faint_opacity * 255.0).ceil() as u8,

            min_contrast: config.minimum_contrast as f32,
            padding_color: config.window_padding_color,

            selection_background: config.selection_background,
            selection_foreground: config.selection_foreground,
            search_background: config.search_background,
            search_foreground: config.search_foreground,
            search_selected_background: config.search_selected_background,
            search_selected_foreground: config.search_selected_foreground,

            custom_shaders,
            bg_image,
            bg_image_opacity: config.background_image_opacity,
            bg_image_position: config.background_image_position,
            bg_image_fit: config.background_image_fit,
            bg_image_repeat: config.background_image_repeat,
            links,
            vsync: config.window_vsync,
            colorspace: config.window_colorspace,
            blending: config.alpha_blending,
            background_blur: config.background_blur,
            scroll_to_bottom_on_output: config.scroll_to_bottom.output,
            custom_shader_animation: config.custom_shader_animation,
        }
    }

    pub fn deinit(&mut self) {
        self.links.deinit();
    }
}

/// The cadence of continuous (draw-only) animation wakes,
/// i.e. 120fps, and the floor for any animation wake delay.
pub const DRAW_INTERVAL_MS: u64 = 8;

/// A point in the future when the renderer needs to be driven
/// again to keep animating, and what kind of drive it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationWake {
    /// Delay in milliseconds until the wake is due.
    pub delay_ms: u64,
    pub kind: AnimationWakeKind,
}

/// Zig: `AnimationWake.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationWakeKind {
    /// A redraw alone suffices, no updateFrame. Much cheaper
    /// than `update`.
    #[default]
    Draw,

    /// Frame data must be updated first: updateFrame, then draw.
    Update,
}

/// Zig: `PreeditRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreeditRange {
    y: CellCountInt,
    x: [CellCountInt; 2],
    cp_offset: usize,
}

/// Zig: the anonymous `selected` enum in `rebuildRow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selected {
    False,
    Selection,
    Search,
    SearchSelected,
}

/// The generic renderer.
pub struct Renderer<G: GraphicsApi> {
    /// This mutex must be held whenever any state used in `drawFrame` is
    /// being modified, and also when it's being accessed in `drawFrame`.
    pub draw_mutex: Mutex,

    /// The configuration we need derived from the main config.
    pub config: DerivedConfig,

    /// The mailbox for communicating with the window.
    pub surface_mailbox: apprt::surface::Mailbox,

    /// The latest exported frame ready to be consumed by an apprt
    /// who needs to manually composite the frame with UI elements.
    /// Previously exported frames are released when a new frame is
    /// exported and pushed onto the queue.
    ///
    /// Unused if the renderer does not need to export frames to
    /// present its rendered frame.
    pub latest_frame: LatestFrame<G::ExportedFrame>,

    /// Current font metrics defining our grid.
    pub grid_metrics: font::Metrics,

    /// The size of everything.
    pub size: Size,

    /// True if the window is focused
    pub focused: bool,

    /// True if the window is visible.
    pub visible: bool,

    /// Flag to indicate that our focus state changed for custom
    /// shaders to update their state.
    pub custom_shader_focused_changed: bool,

    /// The most recent scrollbar state. We use this as a cache to
    /// determine if we need to notify the apprt that there was a
    /// scrollbar change.
    pub scrollbar: Scrollbar,
    pub scrollbar_dirty: bool,

    /// Tracks the last bottom-right pin of the screen to detect new output.
    /// When the final line changes (node or y differs), new content was added.
    /// Used for scroll-to-bottom on output feature.
    pub last_bottom_node: Option<usize>,
    pub last_bottom_y: CellCountInt,

    /// The most recent viewport matches so that we can render search
    /// matches in the visible frame. This is provided asynchronously
    /// from the search thread so we have the dirty flag to also note
    /// if we need to rebuild our cells to include search highlights.
    ///
    /// Note that the selections MAY BE INVALID (point to PageList nodes
    /// that do not exist anymore). These must be validated prior to use.
    pub search_matches: Option<SearchMatches>,
    pub search_selected_match: Option<SearchMatch>,
    pub search_matches_dirty: bool,

    /// The current set of cells to render. This is rebuilt on every frame
    /// but we keep this around so that we don't reallocate. Each set of
    /// cells goes into a separate shader.
    pub cells: cellpkg::Contents,

    /// Set to true after rebuildCells is called. This can be used
    /// to determine if any possible changes have been made to the
    /// cells for the draw call.
    pub cells_rebuilt: bool,

    /// Set to true after the Kitty image placements are rebuilt. Like
    /// `cells_rebuilt`, this means the next frame must be drawn. This
    /// matters when the images change but no cell does, for example
    /// when a program places an image.
    pub images_rebuilt: bool,

    /// The current GPU uniform values.
    pub uniforms: api::shaders::Uniforms,

    /// Custom shader uniform values.
    pub custom_shader_uniforms: shadertoy::Uniforms,

    /// Timestamp we rendered out first frame.
    ///
    /// This is used when updating custom shader uniforms.
    pub first_frame_time: Option<Instant>,

    /// Timestamp when we rendered out more recent frame.
    ///
    /// This is used when updating custom shader uniforms.
    pub last_frame_time: Option<Instant>,

    /// The font structures.
    pub font_grid: *mut SharedGrid,
    pub font_shaper: font::Shaper,
    pub font_shaper_cache: font::ShaperCache,

    /// The images that we may render.
    pub images: ImageState,

    /// Background image, if we have one.
    pub bg_image: Option<imagepkg::Image>,
    /// Set whenever the background image changes, signalling
    /// that the new background image needs to be uploaded to
    /// the GPU.
    ///
    /// This is initialized as true so that we load the image
    /// on renderer initialization, not just on config change.
    pub bg_image_changed: bool,
    /// Background image vertex buffer.
    pub bg_image_buffer: api::shaders::BgImage,
    /// This value is used to force-update the swap chain copy
    /// of the background image buffer whenever we change it.
    pub bg_image_buffer_modified: usize,

    /// Graphics API state.
    pub api: G,

    /// The display link that paces drawing to the display's refresh
    /// rate. It is created lazily on the render thread the first
    /// time `syncDisplayLink` runs, and stays null if vsync is
    /// disabled or creation failed. This only exists on platforms
    /// with a display link (macOS).
    ///
    /// The link is only ever started and stopped on the main thread.
    /// See `renderer/DisplayLink.zig` for why that matters.
    #[cfg(target_os = "macos")]
    pub display_link: Option<*mut DisplayLink>,

    /// Health of the most recently completed frame.
    pub health: AtomicHealth,

    /// Health of how well the apprt can present our frames.
    ///
    /// This is separate from `health` because a renderer
    /// can produce healthy frames that the apprt can't present.
    pub presentation_health: AtomicHealth,

    /// True when we have a graphics context that can create GPU
    /// resources. Creating any GPU resource while this is false is invalid.
    pub display_realized: bool,

    /// Our swap chain (multiple buffering). Null when it has
    /// been released, either because the surface is hidden
    /// (`releaseGpuResources`) or because the display is
    /// unrealized. Rebuilt on the next `drawFrame`.
    pub swap_chain: Option<SwapChain>,

    /// This value is used to force-update swap chain targets in the
    /// event of a config change that requires it (such as blending mode).
    pub target_config_modified: usize,

    /// If something happened that requires us to reinitialize our shaders,
    /// this is set to true so that we can do that whenever possible.
    pub reinitialize_shaders: bool,

    /// Whether or not we have custom shaders.
    pub has_custom_shaders: bool,

    /// Our shader pipelines.
    pub shaders: api::shaders::Shaders,

    /// The render state we update per loop.
    pub terminal_state: RenderState,

    /// The number of frames since the last terminal state reset.
    /// We reset the terminal state after ~100,000 frames (about 10 to
    /// 15 minutes at 120Hz) to prevent wasted memory buildup from
    /// a large screen.
    pub terminal_state_frame_count: usize,

    /// Our overlay state, if any.
    pub overlay: Option<Overlay>,

    /// The base timestamp for the Kitty graphics animation clock.
    /// Animation frame timing is expressed as milliseconds since
    /// this instant. Set on the first frame update that observes
    /// Kitty images.
    pub kitty_animation_clock: Option<Instant>,

    /// When the next Kitty animation frame is due, in
    /// milliseconds on the animation clock, from the most recent
    /// frame update. Null when no running animation needs a
    /// wakeup.
    pub kitty_animation_next_ms: Option<u64>,
}

// SAFETY: like Zig, the renderer is created on the main thread and then
// driven by the renderer thread; shared state is protected by
// `draw_mutex` and the atomics.
unsafe impl<G: GraphicsApi> Send for Renderer<G> {}

impl<G: GraphicsApi> fmt::Debug for Renderer<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renderer")
            .field("size", &self.size)
            .field("focused", &self.focused)
            .field("visible", &self.visible)
            .field("display_realized", &self.display_realized)
            .finish_non_exhaustive()
    }
}

/// The data extracted from the critical area of `updateFrame`.
struct Critical {
    links: CellSet,
    preedit: Option<Preedit>,
    scrollbar: Scrollbar,
    overlay_features: Vec<overlay::Feature>,
}

impl<G: GraphicsApi> Renderer<G> {
    /// The cadence of continuous (draw-only) animation wakes,
    /// i.e. 120fps, and the floor for any animation wake delay.
    pub const DRAW_INTERVAL_MS: u64 = DRAW_INTERVAL_MS;

    /// Whether +Y is down in the coordinate space of exported
    /// frames. Apprts use this to orient frames when presenting.
    pub const CUSTOM_SHADER_Y_IS_DOWN: bool = G::CUSTOM_SHADER_Y_IS_DOWN;

    pub fn init(options: Options) -> Result<Self, Error> {
        // Initialize our graphics API wrapper, this will prepare the
        // surface provided by the apprt and set up any API- and surface-
        // specific GPU resources.
        let api = G::init(options.device, &options)?;
        Self::init_with_api(api, options)
    }

    /// The part of `init` after the graphics API is initialized. On error
    /// the API is deinitialized (Zig: `errdefer api.deinit()`).
    fn init_with_api(api: G, options: Options) -> Result<Self, Error> {
        let mut api = api;
        let has_custom_shaders = !options.config.custom_shaders.value.is_empty();

        // Create the font shaper.
        let font_shaper = {
            let features: Vec<&str> = options.config.font_features.iter().map(|s| s.as_str()).collect();
            match font::Shaper::init(shape::Options { features: &features }) {
                Ok(s) => s,
                Err(e) => {
                    api.deinit();
                    return Err(e.into());
                }
            }
        };

        // Initialize all the data that requires a critical font section.
        let metrics: font::Metrics = {
            // SAFETY: the font grid outlives the renderer.
            let grid: &SharedGrid = unsafe { &*options.font_grid };
            let _guard = grid.lock.read().unwrap_or_else(|e| e.into_inner());
            grid.metrics
        };

        let mut uniforms = api::shaders::Uniforms {
            padding_extend: api::shaders::PaddingExtend::new(false, false, false, false),
            min_contrast: options.config.min_contrast,
            cursor_pos: [u16::MAX, u16::MAX],
            bg_color: [
                options.config.background.r,
                options.config.background.g,
                options.config.background.b,
                // Note that if we're on macOS with glass effects
                // we'll disable background opacity but we handle
                // that in updateFrame.
                (options.config.background_opacity * 255.0).round() as u8,
            ],
            ..Default::default()
        };
        compat::set_cursor_wide(&mut uniforms, false);
        compat::set_use_display_p3(&mut uniforms, options.config.colorspace == WindowColorspace::DisplayP3);
        compat::set_use_linear_blending(&mut uniforms, options.config.blending.is_linear());
        compat::set_use_linear_correction(
            &mut uniforms,
            options.config.blending == AlphaBlending::LinearCorrected,
        );

        let custom_shader_uniforms = shadertoy::Uniforms {
            resolution: [0.0, 0.0, 1.0],
            time: 0.0,
            time_delta: 0.0,
            frame_rate: 60.0, // not currently updated
            frame: 0,
            focus: 1, // assume focused initially
            ..Default::default()
        };

        let mut result = Renderer {
            draw_mutex: Mutex::new(),
            config: options.config,
            surface_mailbox: options.surface_mailbox,
            latest_frame: LatestFrame::default(),
            grid_metrics: metrics,
            size: options.size,
            focused: true,
            visible: true,
            custom_shader_focused_changed: false,
            scrollbar: Scrollbar::ZERO,
            scrollbar_dirty: false,
            last_bottom_node: None,
            last_bottom_y: 0,
            search_matches: None,
            search_selected_match: None,
            search_matches_dirty: false,

            // Render state
            cells: cellpkg::Contents::default(),
            cells_rebuilt: false,
            images_rebuilt: false,
            uniforms,
            custom_shader_uniforms,
            first_frame_time: None,
            last_frame_time: None,

            // Fonts
            font_grid: options.font_grid,
            font_shaper,
            font_shaper_cache: font::ShaperCache::init(),

            images: ImageState::default(),
            bg_image: None,
            bg_image_changed: true,
            bg_image_buffer: api::shaders::BgImage {
                opacity: 0.0,
                info: api::shaders::BgImageInfo::new(
                    api::shaders::BgImagePosition::Mc,
                    api::shaders::BgImageFit::Contain,
                    false,
                ),
            },
            bg_image_buffer_modified: 0,

            // Graphics API stuff
            api,
            #[cfg(target_os = "macos")]
            display_link: None,
            health: AtomicHealth::new(Health::Healthy),
            presentation_health: AtomicHealth::new(Health::Healthy),
            display_realized: true,
            swap_chain: None,
            target_config_modified: 0,
            has_custom_shaders,
            reinitialize_shaders: true,
            // Shaders are initialized lazily on the render thread.
            shaders: api::shaders::Shaders::UNINIT,
            terminal_state: RenderState::empty(),
            terminal_state_frame_count: 0,
            overlay: None,
            kitty_animation_clock: None,
            kitty_animation_next_ms: None,
        };

        // Ensure our undefined values above are correctly initialized.
        result.update_font_grid_uniforms();
        result.update_screen_size_uniforms();
        result.update_bg_image_buffer();
        if let Err(e) = result.prep_background_image() {
            result.font_shaper_cache = font::ShaperCache::init();
            result.api.deinit();
            return Err(e);
        }

        Ok(result)
    }

    pub fn deinit(&mut self) {
        // This only deinitializes and frees CPU-side state
        // and does not free GPU resources like the swap chain and
        // shaders. Those are freed with `releaseGpuResources`.

        if let Some(overlay) = &mut self.overlay {
            overlay.deinit();
        }
        self.overlay = None;
        self.terminal_state = RenderState::empty();
        self.search_selected_match = None;
        self.search_matches = None;

        self.latest_frame.deinit();

        // Stop and release the display link. Surfaces are destroyed
        // on the main thread, where `close` stops the link before
        // returning. After this the display link callback is never
        // invoked again, so `renderer.Thread` can free the `draw_now`
        // handle the callback points at.
        #[cfg(target_os = "macos")]
        if let Some(display_link) = self.display_link.take() {
            // SAFETY: the link was created by `syncDisplayLink` and is
            // closed exactly once.
            unsafe { DisplayLink::close(display_link) };
        }

        self.cells.deinit();

        self.font_shaper_cache = font::ShaperCache::init();

        self.config.deinit();
        self.api.deinit();
    }

    fn init_shaders(&mut self) -> Result<(), Error> {
        // Load our custom shaders
        let custom_shaders = match shadertoy::load_from_files(&self.config.custom_shaders, G::CUSTOM_SHADER_TARGET) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error loading custom shaders err={err:?}");
                Vec::new()
            }
        };

        let has_custom_shaders = !custom_shaders.is_empty();

        let refs: Vec<&CStr> = custom_shaders.iter().map(|s| s.as_c_str()).collect();
        let shaders = self.api.init_shaders(&refs)?;

        self.shaders = shaders;
        self.has_custom_shaders = has_custom_shaders;
        Ok(())
    }

    /// Callback called by renderer.Thread when it begins.
    pub fn thread_enter(&mut self, surface: *mut apprt::Surface) -> Result<(), Error> {
        // If our API has to do things on thread enter, let it.
        self.api.thread_enter(surface)
    }

    /// Callback called by renderer.Thread when it exits. Called on the
    /// render thread. Releases all GPU resources before the API tears down
    /// its context, since after this the GL context will be gone.
    pub fn thread_exit(&mut self) {
        {
            self.draw_mutex.lock_uncancelable(TinyIo::INIT);

            // Release swap chain and shaders. Shaders are only freed
            // while unrealized and not every apprt unrealizes before
            // destroying a surface (macOS never does), so mark it
            // unrealized. This also stops any later draw.
            self.display_realized = false;
            self.release_gpu_resources();

            // We don't release images in `releaseGpuResources`
            // since it can be called whenever the terminal is
            // occluded or unrealized, and we don't want to
            // reupload images every time that happens.
            self.images.deinit();
            self.images = ImageState::default();

            if let Some(img) = self.bg_image.take() {
                img.deinit();
            }

            self.draw_mutex.unlock(TinyIo::INIT);
        }

        // If our API has to do things on thread exit, let it.
        self.api.thread_exit();
    }

    /// Called by renderer.Thread when it starts the main loop.
    pub fn loop_enter(&mut self, thr: *mut Thread) -> Result<(), Error> {
        // If our API has to do things on loop enter, let it.
        let this: *mut Self = self;
        self.api.loop_enter(this);

        // If we don't support a display link we have no work to do.
        #[cfg(target_os = "macos")]
        {
            // SAFETY: the thread outlives its loop.
            let draw_now: *mut xev::Async = unsafe { &mut (*thr).draw_now };
            self.sync_display_link(None, Some(draw_now));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = thr;
        Ok(())
    }

    /// Called by renderer.Thread when it exits the main loop.
    pub fn loop_exit(&mut self) {
        // If our API has to do things on loop exit, let it.
        self.api.loop_exit();

        // If we don't support a display link we have no work to do.
        //
        // Ask the display link to stop. The stop itself happens on
        // the main thread shortly after. `deinit` later stops the
        // link for certain before anything the display link callback
        // references is freed.
        #[cfg(target_os = "macos")]
        if let Some(display_link) = self.display_link {
            // SAFETY: the link stays alive until `deinit` closes it.
            unsafe { (*display_link).set_running(false) };
        }
    }

    /// This is called by the GTK apprt after the surface is
    /// reinitialized (e.g. after the widget is re-realized following
    /// a display change or reparenting).
    pub fn display_realized(&mut self) -> Result<(), Error> {
        // If our API has to do things on realize, let it.
        self.api.display_realized();

        // Lock the draw mutex so that we can safely update state.
        self.draw_mutex.lock_uncancelable(TinyIo::INIT);

        // Mark the display as realized. The render thread will lazily
        // rebuild the swap chain and shaders on the next `drawFrame`,
        // which is the right place for GL resource creation (it
        // guarantees a current context on the render thread).
        self.display_realized = true;
        self.reinitialize_shaders = true;
        self.target_config_modified = 1;

        self.draw_mutex.unlock(TinyIo::INIT);
        Ok(())
    }

    /// This is called when the surface is being unrealized.
    /// This can happen because the surface is being closed but
    /// also when moving the window between displays or splitting.
    ///
    /// This runs on the main thread and only updates CPU-side state
    /// here; resource cleanup happens on the render thread via
    /// `releaseGpuResources`.
    pub fn display_unrealized(&mut self) {
        // Lock the draw mutex so that we can safely update state.
        self.draw_mutex.lock_uncancelable(TinyIo::INIT);

        // Clearing `display_realized` ensures drawFrame doesn't attempt
        // to rebuild the swap chain or make any graphics API calls.
        // The actual GPU resource release is done by the render thread.
        self.display_realized = false;

        self.draw_mutex.unlock(TinyIo::INIT);
    }

    /// Push the latest completed frame, replacing if one previously
    /// existed. Called on the render thread.
    ///
    /// Has no effect for renderers that do not export frames
    /// (i.e. `ExportedFrame == void`).
    pub fn push_frame(&mut self, frame: G::ExportedFrame) {
        if !G::EXPORTS_FRAMES {
            return;
        }
        self.latest_frame.push(frame);
    }

    /// Take the latest completed frame for the apprt to composite.
    /// Returns null if no frame is available. The caller takes
    /// ownership of the returned frame. Called on the main thread.
    ///
    /// Has no effect for renderers that do not export frames
    /// (i.e. `ExportedFrame == void`).
    pub fn take_frame(&mut self) -> Option<G::ExportedFrame> {
        if !G::EXPORTS_FRAMES {
            return None;
        }
        self.latest_frame.take()
    }

    /// The display link output callback. CoreVideo invokes this on
    /// its own thread once per display refresh while the link is
    /// running. All it does is wake the render thread, which then
    /// draws a frame.
    #[cfg(target_os = "macos")]
    fn display_link_callback(_: &crate::pkg::macos::video::DisplayLink, ud: Option<&mut xev::Async>) {
        let Some(draw_now) = ud else { return };
        if let Err(err) = draw_now.notify() {
            log::error!(target: LOG, "error notifying draw_now err={err}");
        }
    }

    /// Mark the full screen as dirty so that we redraw everything.
    #[inline]
    pub fn mark_dirty(&mut self) {
        self.terminal_state.dirty = Dirty::Full;
    }

    /// Called when the window moves to a different display, with
    /// the CGDirectDisplayID of the new one. The display link is
    /// bound to that display so that it follows its refresh rate.
    ///
    /// Must be called on the render thread.
    pub fn set_macos_display_id(&mut self, id: u32, draw_now: *mut xev::Async) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        self.sync_display_link(Some(id), Some(draw_now));
        #[cfg(not(target_os = "macos"))]
        let _ = (id, draw_now);
        Ok(())
    }

    /// The soonest animation wake this renderer needs, if any:
    /// custom shader animation wants continuous draw-only wakes
    /// at draw_interval_ms while active, and a running Kitty
    /// graphics animation wants an update wake when its next
    /// frame is due. The renderer thread drives its animation
    /// timer off this, re-querying after every wake.
    ///
    /// Must be called on the render thread.
    pub fn animation_wake(&self) -> Option<AnimationWake> {
        // Custom shaders animate by redrawing on a fixed cadence,
        // gated by configuration and focus.
        let shader_delay: Option<u64> = if !self.has_custom_shaders {
            None
        } else {
            match self.config.custom_shader_animation {
                CustomShaderAnimation::False => None,
                CustomShaderAnimation::Always => Some(DRAW_INTERVAL_MS),
                CustomShaderAnimation::True => self.focused.then_some(DRAW_INTERVAL_MS),
            }
        };

        // Kitty animations tick during updateFrame; between
        // updates the deadline is absolute on the animation
        // clock, so a stream of draw wakes recomputing this
        // cannot starve it into the future.
        let kitty_delay: Option<u64> = match (self.kitty_animation_next_ms, self.kitty_animation_clock) {
            (Some(next), Some(base)) => {
                let now_ms = base.elapsed().as_millis() as u64;
                // Never wake faster than the draw interval; an
                // overdue frame is picked up on the next wake.
                Some(next.saturating_sub(now_ms).max(DRAW_INTERVAL_MS))
            }
            _ => None,
        };

        // An update wake includes a draw, so it wins ties.
        if let Some(k) = kitty_delay {
            if shader_delay.is_none_or(|s| k <= s) {
                return Some(AnimationWake {
                    delay_ms: k,
                    kind: AnimationWakeKind::Update,
                });
            }
        }

        shader_delay.map(|s| AnimationWake {
            delay_ms: s,
            kind: AnimationWakeKind::Draw,
        })
    }

    /// Returns true if the display link is running, meaning frames
    /// are paced by display refresh. In that case the render thread
    /// must only draw when the display link wakes it through
    /// `draw_now`, and not on its own.
    ///
    /// This reflects the link's actual state. Right after
    /// `syncDisplayLink` asks the link to start, this is still false
    /// until the main thread starts it, and the render thread draws
    /// immediately in the meantime.
    pub fn has_vsync(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            let Some(display_link) = self.display_link else { return false };
            // SAFETY: the link stays alive until `deinit` closes it.
            unsafe { (*display_link).is_running() }
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Callback when the focus changes for the terminal this is rendering.
    ///
    /// Must be called on the render thread.
    pub fn set_focus(&mut self, focus: bool) -> Result<(), Error> {
        debug_assert!(self.focused != focus);

        self.focused = focus;

        // Flag that we need to update our custom shaders
        self.custom_shader_focused_changed = true;

        self.sync_display_link(None, None);
        Ok(())
    }

    /// Called when the apprt reports a change in how well
    /// frames can be presented.
    pub fn set_presentation_health(&mut self, health: Health) {
        self.presentation_health.store(health);
    }

    /// Returns how well frames can be presented.
    pub fn presentation_health(&self) -> Health {
        self.presentation_health.load()
    }

    /// Callback when the window is visible or occluded.
    ///
    /// Must be called on the render thread.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.sync_display_link(None, None);

        // When we're hidden, release our GPU resources.
        if !visible {
            self.draw_mutex.lock_uncancelable(TinyIo::INIT);
            self.release_gpu_resources();
            self.draw_mutex.unlock(TinyIo::INIT);
        }
    }

    /// Release the GPU resources we hold while the surface is not
    /// visible. Today this is the swap chain (render targets, font
    /// atlas texture copies, cell buffers, custom shader textures),
    /// which makes up nearly all of a surface's GPU memory usage.
    /// The swap chain is rebuilt on the next `drawFrame`.
    ///
    /// Note that images are NOT released here since we don't want
    /// to reupload images every time the terminal is brought back
    /// from being occluded or unrealized.
    ///
    /// Caller must lock the draw mutex before calling this function.
    /// Resources that are already released are skipped.
    pub fn release_gpu_resources(&mut self) {
        if let Some(mut sc) = self.swap_chain.take() {
            // Waits for any in-flight frames to complete, then
            // frees all GPU resources.
            sc.deinit();
        }

        // Release the shaders as well if we're unrealized.
        if !self.display_realized {
            self.shaders.deinit();
        }
    }

    /// Create the display link if needed, then ask it to match the
    /// current surface state: bound to `display_id` if one is given,
    /// and running only while the surface is visible and has
    /// something to draw (rebuilt cells or a running animation).
    ///
    /// `draw_now` is required the first time this is called, since
    /// creating the link needs the handle its callback will notify.
    /// Later calls can pass null.
    ///
    /// Must be called on the render thread. This never blocks. The
    /// display link applies the requested state on the main thread
    /// shortly after, because starting and stopping a CVDisplayLink
    /// anywhere else can deadlock against CoreVideo's own display
    /// reconfiguration handling. See `renderer/DisplayLink.zig`
    /// for the full explanation.
    #[allow(unused_variables)]
    fn sync_display_link(&mut self, display_id: Option<u32>, draw_now: Option<*mut xev::Async>) {
        #[cfg(target_os = "macos")]
        {
            let display_link = match self.display_link {
                Some(v) => v,
                None => {
                    if !self.config.vsync {
                        return;
                    }
                    let Some(callback) = draw_now else { return };
                    // SAFETY: `draw_now` is owned by the renderer thread,
                    // which outlives the display link (see `deinit`).
                    let userdata: &'static mut xev::Async = unsafe { &mut *callback };
                    let result = match DisplayLink::create::<xev::Async>(
                        Self::display_link_callback,
                        Some(userdata),
                    ) {
                        Ok(v) => v,
                        Err(err) => {
                            // A locked macOS session can temporarily have no active
                            // displays. Rendering continues without vsync and the
                            // next call to this function tries again.
                            log::warn!(
                                target: LOG,
                                "error creating display link; using fallback rendering err={err:?}"
                            );
                            return;
                        }
                    };

                    self.display_link = Some(result);
                    log::info!(target: LOG, "created display link");
                    result
                }
            };

            // SAFETY: the link stays alive until `deinit` closes it.
            let display_link = unsafe { &mut *display_link };
            if let Some(id) = display_id {
                display_link.set_display(id);
            }

            let should_run =
                // Non-visible windows never vsync
                self.visible
                    // Only vsync if we have cell or image changes or animation
                    && (self.cells_rebuilt || self.images_rebuilt || self.animation_wake().is_some());

            display_link.set_running(should_run);
        }
    }

    /// Set the new font grid.
    ///
    /// Must be called on the render thread.
    pub fn set_font_grid(&mut self, grid: *mut SharedGrid) {
        self.draw_mutex.lock_uncancelable(TinyIo::INIT);

        // Update our grid
        self.font_grid = grid;

        // Update all our textures so that they sync on the next frame.
        // We can modify this without a lock because the GPU does not
        // touch this data. A released swap chain is rebuilt with
        // fresh frames that sync all textures on first use.
        if let Some(sc) = &mut self.swap_chain {
            for frame in &mut sc.frames {
                frame.grayscale_modified = 0;
                frame.color_modified = 0;
            }
        }

        // Get our metrics from the grid. This doesn't require a lock because
        // the metrics are never recalculated.
        // SAFETY: the font grid outlives the renderer.
        let metrics = unsafe { (*grid).metrics };
        self.grid_metrics = metrics;

        // Reset our shaper cache. If our font changed (not just the size) then
        // the data in the shaper cache may be invalid and cannot be used, so we
        // always clear the cache just in case.
        self.font_shaper_cache = font::ShaperCache::init();

        // Update cell size.
        self.size.cell = CellSize {
            width: metrics.cell_width,
            height: metrics.cell_height,
        };

        // Update relevant uniforms
        self.update_font_grid_uniforms();

        // Force a full rebuild, because cached rows may still reference
        // an outdated atlas from the old grid and this can cause garbage
        // to be rendered.
        self.mark_dirty();

        self.draw_mutex.unlock(TinyIo::INIT);
    }

    /// Update uniforms that are based on the font grid.
    ///
    /// Caller must hold the draw mutex.
    fn update_font_grid_uniforms(&mut self) {
        self.uniforms.cell_size = [self.grid_metrics.cell_width as f32, self.grid_metrics.cell_height as f32];
    }

    /// Update the frame data.
    pub fn update_frame(&mut self, state: &State, cursor_blink_visible: bool) -> Result<(), Error> {
        // CoreText shaping accumulates objects for deferred release over
        // the course of a frame. Always flush those objects, including
        // when rebuilding the frame fails due to memory pressure.
        let r = self.update_frame_inner(state, cursor_blink_visible);
        self.font_shaper.end_frame();
        r
    }

    fn update_frame_inner(&mut self, state: &State, cursor_blink_visible: bool) -> Result<(), Error> {
        // We fully deinit and reset the terminal state every so often
        // so that a particularly large terminal state doesn't cause
        // the renderer to hold on to retained memory.
        //
        // Frame count is ~12 minutes at 120Hz.
        const MAX_TERMINAL_STATE_FRAME_COUNT: usize = 100_000;
        if self.terminal_state_frame_count >= MAX_TERMINAL_STATE_FRAME_COUNT {
            self.terminal_state = RenderState::empty();
            self.terminal_state_frame_count = 0;
        }
        self.terminal_state_frame_count += 1;

        // Update all our data as tightly as possible within the mutex.
        let critical: Critical = {
            state.lock_demand(TinyIo::INIT);
            let r = self.update_frame_critical(state);
            state.unlock_demand(TinyIo::INIT);
            match r {
                Some(c) => c,
                // Synchronized output: skip this render.
                None => return Ok(()),
            }
        };
        let mut critical = critical;

        // Outside the critical area, complete the update we began
        // within it. This must be done before anything reads the
        // render state (e.g. rebuildCells).
        self.terminal_state.end_update();

        // Outside the critical area we can update our links to contain
        // our regex results.
        if let Err(err) = self.config.links.render_cell_map(
            &mut critical.links,
            &self.terminal_state,
            state.mouse.point,
            state.mouse.mods,
        ) {
            log::warn!(target: LOG, "error searching for regex links err={err}");
        }

        // Clear our highlight state and update.
        if self.search_matches_dirty || self.terminal_state.dirty != Dirty::False {
            self.search_matches_dirty = false;

            // Clear the prior highlights
            let row_data = &mut self.terminal_state.row_data;
            let mut any_dirty: bool = false;
            for (highlights, dirty) in row_data.highlights.iter_mut().zip(row_data.dirty.iter_mut()) {
                if !highlights.is_empty() {
                    highlights.clear();
                    *dirty = true;
                    any_dirty = true;
                }
            }
            if any_dirty && self.terminal_state.dirty == Dirty::False {
                self.terminal_state.dirty = Dirty::Partial;
            }

            // NOTE: The order below matters. Highlights added earlier
            // will take priority.

            if let Some(m) = &self.search_selected_match {
                self.terminal_state.update_highlights_flattened(
                    HighlightTag::SearchMatchSelected as u8,
                    std::slice::from_ref(&m.r#match),
                );
            }

            if let Some(m) = &self.search_matches {
                self.terminal_state
                    .update_highlights_flattened(HighlightTag::SearchMatch as u8, &m.matches);
            }
        }

        // From this point forward no more errors.

        // Rebuild the overlay image if we have one. We can do this
        // outside of any critical areas.
        if let Err(err) = self.rebuild_overlay(&critical.overlay_features) {
            log::warn!(target: LOG, "error rebuilding overlay surface err={err}");
        }

        // Acquire the draw mutex for all remaining state updates.
        {
            self.draw_mutex.lock_uncancelable(TinyIo::INIT);

            // Build our GPU cells
            let style = cursor::style(
                &self.terminal_state,
                CursorStyleOptions {
                    preedit: critical.preedit.is_some(),
                    focused: self.focused,
                    blink_visible: cursor_blink_visible,
                },
            );
            self.rebuild_cells(critical.preedit.as_ref(), style, &critical.links);

            // The scrollbar is only emitted during draws so we also
            // check the scrollbar cache here and update if needed.
            // This is pretty fast.
            if !self.scrollbar.eql(critical.scrollbar) {
                self.scrollbar = critical.scrollbar;
                self.scrollbar_dirty = true;
            }

            // Update our background color
            self.uniforms.bg_color = [
                self.terminal_state.colors.background.r,
                self.terminal_state.colors.background.g,
                self.terminal_state.colors.background.b,
                (self.config.background_opacity * 255.0).round() as u8,
            ];

            // If we're on macOS and have glass styles, we remove
            // the background opacity because the glass effect handles
            // it.
            if cfg!(target_os = "macos") {
                match self.config.background_blur {
                    BackgroundBlur::MacosGlassRegular | BackgroundBlur::MacosGlassClear => {
                        self.uniforms.bg_color[3] = 0
                    }
                    _ => {}
                }
            }

            // Prepare our overlay image for upload (or unload). This
            // has to use our general allocator since it modifies
            // state that survives frames.
            if let Err(err) = self.images.overlay_update(self.overlay.as_mut()) {
                log::warn!(target: LOG, "error updating overlay images err={err}");
            }

            // Update custom shader uniforms that depend on terminal state.
            self.update_custom_shader_uniforms_from_state();

            self.draw_mutex.unlock(TinyIo::INIT);
        }

        // Reset our dirty state after updating.
        self.terminal_state.dirty = Dirty::False;

        // Start the display link now that the rebuilt frame is ready.
        self.sync_display_link(None, None);
        Ok(())
    }

    /// The critical (terminal-locked) section of `updateFrame`. Returns
    /// `None` when the terminal is in synchronized output.
    fn update_frame_critical(&mut self, state: &State) -> Option<Critical> {
        // SAFETY: the terminal is alive and we hold its mutex.
        let terminal = unsafe { &mut *state.terminal.as_ptr() };

        // If we're in a synchronized output state, we pause all rendering.
        if terminal.modes.get(Mode::SynchronizedOutput) {
            log::debug!(target: LOG, "synchronized output started, skipping render");
            return None;
        }

        // If scroll-to-bottom on output is enabled, check if the final line
        // changed by comparing the bottom-right pin. If the node pointer or
        // y offset changed, new content was added to the screen.
        // Update this BEFORE we update our render state so we can
        // draw the new scrolled data immediately.
        if self.config.scroll_to_bottom_on_output {
            if let Some(br) = terminal.screens.active.pages.get_bottom_right(point::Tag::Screen) {
                let node = br.node.as_ptr() as usize;
                // If the pin hasn't changed, then don't scroll.
                if !(self.last_bottom_node == Some(node) && self.last_bottom_y == br.y) {
                    // Update tracked pin state for next frame
                    self.last_bottom_node = Some(node);
                    self.last_bottom_y = br.y;

                    // Scroll
                    terminal.scroll_viewport(ScrollViewport::Bottom);
                }
            }
        }

        // Advance any running Kitty graphics animations to the
        // frame due now, and remember when the next frame is
        // due (as an absolute deadline, see animationWake) so
        // the renderer thread can schedule a wakeup for it.
        //
        // This runs before the render state update. Advancing
        // a frame gives the image a new generation, so the
        // update then reports placements with the new frame,
        // and the frame is uploaded and drawn right away.
        self.kitty_animation_next_ms = 'next: {
            // Likely case: we have no kitty images, so do nothing.
            // SAFETY: the active screen is live while the terminal is.
            let storage = &mut unsafe { terminal.screens.active.as_mut() }.kitty_images;
            if storage.images.is_empty() {
                break 'next None;
            }

            let now = Instant::now();
            let base = *self.kitty_animation_clock.get_or_insert(now);
            let now_ms: u64 = now.duration_since(base).as_millis() as u64;
            let Some(delay) = storage.animation_tick(now_ms) else {
                break 'next None;
            };
            Some(now_ms + delay)
        };

        // Begin the update of our terminal state. Work that
        // doesn't require terminal access (e.g. style
        // denormalization) is deferred to the endUpdate call
        // outside of this critical section, keeping our lock
        // hold time as short as possible.
        self.terminal_state.begin_update(terminal);

        // If our terminal state is dirty at all we need to redo
        // the viewport search.
        if self.terminal_state.dirty != Dirty::False {
            terminal.flags.search_viewport_dirty = true;
        }

        // Get our scrollbar out of the terminal. We synchronize
        // the scrollbar read with frame data updates because this
        // naturally limits the number of calls to this method (it
        // can be expensive) and also makes it so we don't need another
        // cross-thread mailbox message within the IO path.
        // SAFETY: the active screen is live while the terminal is.
        let scrollbar = unsafe { terminal.screens.active.as_mut() }.pages.scrollbar();

        // Get our preedit state
        let preedit: Option<Preedit> = state.preedit.as_ref().map(|p| p.clone_preedit());

        // Rebuild our Kitty image placements when the placements
        // or the images changed. The render state update above
        // has already positioned every placement, so this is
        // cheap. Most frames that only change text skip it.
        if self.images.kitty_requires_update(terminal, &self.terminal_state) {
            // We need to grab the draw mutex since this updates
            // our image state that drawFrame uses.
            self.draw_mutex.lock_uncancelable(TinyIo::INIT);
            self.images.kitty_update(terminal, &self.terminal_state);
            self.images_rebuilt = true;
            self.draw_mutex.unlock(TinyIo::INIT);
        }

        // Get our OSC8 links we're hovering if we have a mouse.
        // This requires terminal state because of URLs.
        let links: CellSet = 'osc8: {
            // If our mouse isn't hovering, we have no links.
            let Some(vp) = state.mouse.point else { break 'osc8 CellSet::default() };

            // If the right mods aren't pressed, then we can't match.
            if !state.mouse.mods.equal(ctrl_or_super(Mods::default())) {
                break 'osc8 CellSet::default();
            }

            self.terminal_state.link_cells(vp)
        };

        let overlay_features: Vec<overlay::Feature> = match state.inspector {
            None => Vec::new(),
            Some(mut insp) => {
                // SAFETY: the inspector is alive while it is set and we
                // hold the terminal mutex.
                let insp = unsafe { insp.as_mut() };
                insp.renderer_info().overlay_features()
            }
        };

        Some(Critical {
            links,
            preedit,
            scrollbar,
            overlay_features,
        })
    }

    /// Draw the frame to the screen.
    ///
    /// If `sync` is true, this will synchronously block until
    /// the frame is finished drawing and has been presented.
    pub fn draw_frame(&mut self, sync: bool) -> Result<(), Error> {
        // Everything that touches draw state happens under the draw
        // mutex. The display link is synced after the mutex is
        // released. It doesn't need the lock, and this keeps the
        // critical section to just drawing.
        let sync_display_link = {
            self.draw_mutex.lock_uncancelable(TinyIo::INIT);
            let r = self.draw_frame_locked(sync);
            self.draw_mutex.unlock(TinyIo::INIT);
            r?
        };

        if sync_display_link {
            self.sync_display_link(None, None);
        }
        Ok(())
    }

    /// The body of `drawFrame`. Must be called with `draw_mutex` held.
    ///
    /// Returns true if the display link should be resynced once the
    /// draw mutex is released. This is only ever true on the no-redraw
    /// path, which a sync draw never takes. Sync draws come from the
    /// main thread, so this keeps `syncDisplayLink` on the render
    /// thread as its contract requires.
    fn draw_frame_locked(&mut self, sync: bool) -> Result<bool, Error> {
        // Let our graphics API do any bookkeeping, etc.
        // that it needs to do before / after `drawFrame`.
        self.api.draw_frame_start();
        let r = self.draw_frame_api(sync);
        self.api.draw_frame_end();

        // After the graphics API is complete (so we defer) we want to
        // update our scrollbar state.
        if self.scrollbar_dirty {
            // Fail instantly if the surface mailbox if full, we'll just
            // get it on the next frame.
            if self
                .surface_mailbox
                .push(apprt::surface::Message::Scrollbar(self.scrollbar), QueueTimeout::Instant)
                > 0
            {
                self.scrollbar_dirty = false;
            }
        }

        r
    }

    /// The part of `drawFrameLocked` between `drawFrameStart` and
    /// `drawFrameEnd`.
    fn draw_frame_api(&mut self, sync: bool) -> Result<bool, Error> {
        // Retrieve the most up-to-date surface size from the Graphics API
        let surface_size = self.api.surface_size()?;

        // If either of our surface dimensions is zero
        // then drawing is absurd, so we just return.
        if surface_size.width == 0 || surface_size.height == 0 {
            return Ok(false);
        }

        // If we have no graphics context we can't draw. This is
        // only the case while unrealized (GTK) or after the render
        // thread exits; displayRealized rebuilds the swap chain.
        if !self.display_realized {
            return Ok(false);
        }

        // Get our swap chain, rebuilding it if it was released
        // while we were hidden. Rebuilding is deferred to draw
        // time because resource creation must happen somewhere
        // our graphics API allows it (OpenGL requires a current
        // context, which drawFrame guarantees).
        let swap_chain_rebuilt = if self.swap_chain.is_some() {
            false
        } else {
            self.swap_chain = Some(SwapChain::init(&self.api, self.has_custom_shaders)?);
            true
        };

        let size_changed =
            self.size.screen.width != surface_size.width || self.size.screen.height != surface_size.height;

        // Conditions under which we need to draw the frame, otherwise we
        // don't need to since the previous frame should be identical.
        //
        // While any animation is in progress (a pending animation wake)
        // every draw must actually render.
        let needs_redraw = size_changed
            || swap_chain_rebuilt
            || self.cells_rebuilt
            || self.images_rebuilt
            || self.animation_wake().is_some()
            || sync;

        if !needs_redraw {
            // Ask our caller to resync the display link once the draw
            // mutex is released, because we can probably pause the
            // display link at this point.
            return Ok(true);
        }
        self.cells_rebuilt = false;
        self.images_rebuilt = false;

        // Wait for a frame to be available.
        //
        // SAFETY (for the raw pointer): the swap chain is not replaced or
        // dropped while a frame is being drawn (that requires the draw
        // mutex, which we hold), and the frame state is only touched by
        // this draw until `frameCompleted` releases it.
        let frame: *mut FrameState = self.swap_chain.as_mut().unwrap().next_frame();
        let frame = unsafe { &mut *frame };

        match self.draw_frame_prepare(frame, surface_size, sync) {
            Ok(()) => Ok(false),
            Err(e) => {
                if let Some(sc) = &self.swap_chain {
                    sc.release_frame();
                }
                Err(e)
            }
        }
    }

    /// Prepare the frame state and the render passes, then complete the
    /// frame. On error before the frame context exists the caller releases
    /// the frame (Zig: `errdefer swap_chain.releaseFrame()`).
    fn draw_frame_prepare(&mut self, frame: &mut FrameState, surface_size: api::SurfaceSize, sync: bool) -> Result<(), Error> {
        // If we need to reinitialize our shaders, do so.
        if self.reinitialize_shaders {
            self.reinitialize_shaders = false;
            self.shaders.deinit();
            self.init_shaders()?;
        }

        // Our shaders should not be defunct at this point.
        debug_assert!(!self.shaders.defunct);

        // If we have custom shaders, make sure we have the
        // custom shader state in our frame state, otherwise
        // if we have a state but don't need it we remove it.
        if self.has_custom_shaders {
            if frame.custom_shader_state.is_none() {
                let mut state = CustomShaderState::init(&self.api)?;
                let r = state.resize(&self.api, surface_size.width as usize, surface_size.height as usize);
                frame.custom_shader_state = Some(state);
                r?;
            }
        } else if let Some(mut state) = frame.custom_shader_state.take() {
            state.deinit();
        }

        // If our stored size doesn't match the
        // surface size we need to update it.
        if self.size.screen.width != surface_size.width || self.size.screen.height != surface_size.height {
            self.size.screen = ScreenSize {
                width: surface_size.width,
                height: surface_size.height,
            };
            self.update_screen_size_uniforms();
        }

        // If this frame's target isn't the correct size, or the target
        // config has changed (such as when the blending mode changes),
        // remove it and replace it with a new one with the right values.
        if frame.target.width != self.size.screen.width as usize
            || frame.target.height != self.size.screen.height as usize
            || frame.target_config_modified != self.target_config_modified
        {
            frame.resize(
                &self.api,
                self.size.screen.width as usize,
                self.size.screen.height as usize,
            )?;
            frame.target_config_modified = self.target_config_modified;
        }

        // Upload images to the GPU as necessary.
        let _ = self.images.upload(&self.api);

        // Upload the background image to the GPU as necessary.
        self.upload_background_image()?;

        // Update per-frame custom shader uniforms.
        self.update_custom_shader_uniforms_for_frame()?;

        // Setup our frame data
        frame.uniforms.sync(&[self.uniforms])?;
        frame.cells_bg.sync(&self.cells.bg_cells)?;
        let fg_count = frame.cells.sync_from_array_lists(&self.cells.fg_rows)?;

        // If our background image buffer has changed, sync it.
        if frame.bg_image_buffer_modified != self.bg_image_buffer_modified {
            frame.bg_image_buffer.sync(&[self.bg_image_buffer])?;

            frame.bg_image_buffer_modified = self.bg_image_buffer_modified;
        }

        // If our font atlas changed, sync the texture data
        {
            // SAFETY: the font grid outlives the renderer.
            let grid = unsafe { &*self.font_grid };
            let state = grid.lock.read().unwrap_or_else(|e| e.into_inner());
            let modified = state.atlas_grayscale.modified.load(Ordering::Relaxed);
            if modified > frame.grayscale_modified {
                frame.grayscale_modified = modified;
                self.sync_atlas_texture(&state.atlas_grayscale, &mut frame.grayscale)?;
            }
            let modified = state.atlas_color.modified.load(Ordering::Relaxed);
            if modified > frame.color_modified {
                frame.color_modified = modified;
                self.sync_atlas_texture(&state.atlas_color, &mut frame.color)?;
            }
        }

        // Get a frame context from the graphics API.
        let this: *mut Self = self;
        let mut frame_ctx = self.api.begin_frame(this, &mut frame.target)?;

        let r = self.draw_frame_passes(&frame_ctx, frame, fg_count);

        // Zig: `defer frame_ctx.complete(sync)`.
        frame_ctx_complete(&mut frame_ctx, sync);
        r
    }

    /// Encode the render passes of a frame.
    fn draw_frame_passes(&mut self, frame_ctx: &api::Frame, frame: &mut FrameState, fg_count: usize) -> Result<(), Error> {
        let pipelines = compat::pipelines(&self.shaders);
        {
            let attachments = [api::render_pass::Attachment {
                target: match &frame.custom_shader_state {
                    Some(state) => api::render_pass::AttachmentTarget::Texture(state.back_texture),
                    None => api::render_pass::AttachmentTarget::Target(frame.target),
                },
                clear_color: Some([0.0, 0.0, 0.0, 0.0]),
            }];
            #[allow(unused_mut)]
            let mut pass = frame_ctx.render_pass(&attachments);

            // First we draw our background image, if we have one.
            // The bg image shader also draws the main bg color.
            //
            // Otherwise, if we don't have a background image, we
            // draw the background color by itself in its own step.
            //
            // NOTE: We don't use the clear_color for this because that
            //       would require us to do color space conversion on the
            //       CPU-side. In the future when we have utilities for
            //       that we should remove this step and use clear_color.

            if let Some(img) = &self.bg_image {
                if let imagepkg::Image::Ready(texture) = img {
                    pass.step(api::render_pass::Step {
                        pipeline: pipelines.bg_image,
                        uniforms: Some(frame.uniforms.buffer),
                        buffers: &[Some(frame.bg_image_buffer.buffer)],
                        textures: &[Some(*texture)],
                        samplers: &[],
                        draw: api::render_pass::Draw {
                            r#type: api::Primitive::Triangle,
                            vertex_count: 3,
                            instance_count: 1,
                        },
                    });
                }
            } else {
                pass.step(api::render_pass::Step {
                    pipeline: pipelines.bg_color,
                    uniforms: Some(frame.uniforms.buffer),
                    buffers: &[None, Some(frame.cells_bg.buffer)],
                    textures: &[],
                    samplers: &[],
                    draw: api::render_pass::Draw {
                        r#type: api::Primitive::Triangle,
                        vertex_count: 3,
                        instance_count: 1,
                    },
                });
            }

            // Then we draw any kitty images that need
            // to be behind text AND cell backgrounds.
            self.images
                .draw(&self.api, pipelines.image, &mut pass, DrawPlacements::KittyBelowBg);

            // Then we draw any opaque cell backgrounds.
            pass.step(api::render_pass::Step {
                pipeline: pipelines.cell_bg,
                uniforms: Some(frame.uniforms.buffer),
                buffers: &[None, Some(frame.cells_bg.buffer)],
                textures: &[],
                samplers: &[],
                draw: api::render_pass::Draw {
                    r#type: api::Primitive::Triangle,
                    vertex_count: 3,
                    instance_count: 1,
                },
            });

            // Kitty images between cell backgrounds and text.
            self.images
                .draw(&self.api, pipelines.image, &mut pass, DrawPlacements::KittyBelowText);

            // Text.
            pass.step(api::render_pass::Step {
                pipeline: pipelines.cell_text,
                uniforms: Some(frame.uniforms.buffer),
                buffers: &[Some(frame.cells.buffer), Some(frame.cells_bg.buffer)],
                textures: &[Some(frame.grayscale), Some(frame.color)],
                samplers: &[],
                draw: api::render_pass::Draw {
                    r#type: api::Primitive::TriangleStrip,
                    vertex_count: 4,
                    instance_count: fg_count,
                },
            });

            // Kitty images in front of text.
            self.images
                .draw(&self.api, pipelines.image, &mut pass, DrawPlacements::KittyAboveText);

            // Debug overlay. We do this before any custom shader state
            // because our debug overlay is aligned with the grid.
            if self.overlay.is_some() {
                self.images
                    .draw(&self.api, pipelines.image, &mut pass, DrawPlacements::Overlay);
            }

            pass.complete();
        }

        // If we have custom shaders, then we render them.
        if let Some(state) = &mut frame.custom_shader_state {
            // Sync our uniforms.
            state.uniforms.sync(&[self.custom_shader_uniforms])?;

            let post_len = self.shaders.post_pipelines.len();
            for (i, pipeline) in self.shaders.post_pipelines.iter().enumerate() {
                {
                    let attachments = [api::render_pass::Attachment {
                        target: if i < post_len - 1 {
                            api::render_pass::AttachmentTarget::Texture(state.front_texture)
                        } else {
                            api::render_pass::AttachmentTarget::Target(frame.target)
                        },
                        clear_color: Some([0.0, 0.0, 0.0, 0.0]),
                    }];
                    #[allow(unused_mut)]
                    let mut pass = frame_ctx.render_pass(&attachments);

                    pass.step(api::render_pass::Step {
                        pipeline: *pipeline,
                        uniforms: Some(state.uniforms.buffer),
                        buffers: &[],
                        textures: &[Some(state.back_texture)],
                        samplers: &[Some(state.sampler)],
                        draw: api::render_pass::Draw {
                            r#type: api::Primitive::Triangle,
                            vertex_count: 3,
                            instance_count: 1,
                        },
                    });
                    pass.complete();
                }
                state.swap();
            }
        }

        Ok(())
    }

    // Callback from the graphics API when a frame is completed.
    pub fn frame_completed(&mut self, health: Health) {
        // If our health value hasn't changed, then we do nothing. We don't
        // do a cmpxchg here because strict atomicity isn't important.
        if self.health.load() != health {
            self.health.store(health);

            // Our health value changed, so we notify the surface so that it
            // can do something about it.
            let _ = self
                .surface_mailbox
                .push(apprt::surface::Message::RendererHealth(health), QueueTimeout::Forever);
        }

        // Always release our semaphore. The swap chain is
        // guaranteed to exist here: it is only torn down after
        // waiting for all in-flight frames to complete, and this
        // callback is what signals that completion.
        //
        // (Frames begun outside a swap chain, e.g. in tests, have no
        // semaphore to release.)
        if let Some(sc) = &self.swap_chain {
            sc.release_frame();
        }
    }

    /// Call this any time the background image path changes.
    ///
    /// Caller must hold the draw mutex.
    fn prep_background_image(&mut self) -> Result<(), Error> {
        // Then we try to load the background image if we have a path.
        if let Some(p) = &self.config.bg_image {
            let path: &str = match p {
                ConfigPath::Required(s) | ConfigPath::Optional(s) => s.as_str(),
            };

            'load_background: {
                // Open the file and read it
                let contents = match std::fs::read(path) {
                    Ok(v) => v,
                    Err(err) => {
                        log::warn!(target: LOG, "error reading background image file \"{path}\": {err}");
                        break 'load_background;
                    }
                };

                // Figure out what type it probably is.
                let file_type = match FileType::detect(&contents) {
                    FileType::Unknown => {
                        let ext = std::path::Path::new(path)
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy()))
                            .unwrap_or_default();
                        FileType::guess_from_extension(ext.as_bytes())
                    }
                    t => t,
                };

                // Decode it if we know how.
                let image_data = match file_type {
                    FileType::Png => wuffs::png::decode(&contents)?,
                    FileType::Jpeg => wuffs::jpeg::decode(&contents)?,
                    FileType::Unknown => {
                        log::warn!(
                            target: LOG,
                            "Cannot determine file type for background image file \"{path}\"!"
                        );
                        break 'load_background;
                    }
                    f => {
                        log::warn!(
                            target: LOG,
                            "Unsupported file type {f:?} for background image file \"{path}\"!"
                        );
                        break 'load_background;
                    }
                };

                let image = imagepkg::Image::Pending(imagepkg::Pending::from_owned(
                    image_data.width,
                    image_data.height,
                    imagepkg::PixelFormat::Rgba,
                    image_data.data,
                ));

                // If we have an existing background image, replace it.
                // Otherwise, set this as our background image directly.
                if let Some(img) = &mut self.bg_image {
                    img.mark_for_replace(image);
                } else {
                    self.bg_image = Some(image);
                }
            }
        } else {
            // If we don't have a background image path, mark our
            // background image for unload if we currently have one.
            if let Some(img) = &mut self.bg_image {
                img.mark_for_unload();
            }
        }
        Ok(())
    }

    fn upload_background_image(&mut self) -> Result<(), Error> {
        // Make sure our bg image is uploaded if it needs to be.
        if let Some(bg) = &mut self.bg_image {
            if bg.is_unloading() {
                let bg = self.bg_image.take().unwrap();
                bg.deinit();
                return Ok(());
            }
            if bg.is_pending() {
                bg.upload(&self.api)?;
            }
        }
        Ok(())
    }

    /// Update the configuration.
    pub fn change_config(&mut self, config: &mut DerivedConfig) -> Result<(), Error> {
        self.draw_mutex.lock_uncancelable(TinyIo::INIT);
        let r = self.change_config_locked(config);
        self.draw_mutex.unlock(TinyIo::INIT);
        r
    }

    fn change_config_locked(&mut self, config: &mut DerivedConfig) -> Result<(), Error> {
        // We always redo the font shaper in case font features changed. We
        // could check to see if there was an actual config change but this is
        // easier and rare enough to not cause performance issues.
        {
            let features: Vec<&str> = config.font_features.iter().map(|s| s.as_str()).collect();
            let font_shaper = font::Shaper::init(shape::Options { features: &features })?;
            self.font_shaper = font_shaper;
        }

        // We also need to reset the shaper cache so shaper info
        // from the previous font isn't reused for the new font.
        self.font_shaper_cache = font::ShaperCache::init();

        // Set our new minimum contrast
        self.uniforms.min_contrast = config.min_contrast;

        // Set our new color space and blending
        compat::set_use_display_p3(&mut self.uniforms, config.colorspace == WindowColorspace::DisplayP3);
        compat::set_use_linear_blending(&mut self.uniforms, config.blending.is_linear());
        compat::set_use_linear_correction(&mut self.uniforms, config.blending == AlphaBlending::LinearCorrected);

        let bg_image_config_changed = self.config.bg_image_fit != config.bg_image_fit
            || self.config.bg_image_position != config.bg_image_position
            || self.config.bg_image_repeat != config.bg_image_repeat
            || self.config.bg_image_opacity != config.bg_image_opacity;

        let bg_image_changed = match (&self.config.bg_image, &config.bg_image) {
            (Some(old), Some(new)) => !old.equal(new),
            (Some(_), None) => true,
            (None, new) => new.is_some(),
        };

        let old_blending = self.config.blending;
        let custom_shaders_changed = !self.config.custom_shaders.equal(&config.custom_shaders);

        self.config.deinit();
        self.config = std::mem::take(config);

        // If our background image path changed, prepare the new bg image.
        if bg_image_changed {
            self.prep_background_image()?;
        }

        // If our background image config changed, update the vertex buffer.
        if bg_image_config_changed {
            self.update_bg_image_buffer();
        }

        // Reset our viewport to force a rebuild, in case of a font change.
        self.mark_dirty();

        let blending_changed = old_blending != self.config.blending;

        if blending_changed {
            // We update our API's blending mode.
            self.api.set_blending(self.config.blending);
            // And indicate that we need to reinitialize our shaders.
            self.reinitialize_shaders = true;
            // And indicate that our swap chain targets need to
            // be re-created to account for the new blending mode.
            self.target_config_modified = self.target_config_modified.wrapping_add(1);
        }

        if custom_shaders_changed {
            self.reinitialize_shaders = true;
        }
        Ok(())
    }

    /// Resize the screen.
    pub fn set_screen_size(&mut self, size: Size) {
        self.draw_mutex.lock_uncancelable(TinyIo::INIT);

        self.size = size;
        self.update_screen_size_uniforms();

        // Some graphics APIs need to manually update their viewport,
        // like OpenGL. Do so here.
        self.api.set_viewport(self.size.screen.width, self.size.screen.height);

        log::debug!(target: LOG, "screen size size={size:?}");

        self.draw_mutex.unlock(TinyIo::INIT);
    }

    /// Update uniforms that are based on the screen size.
    ///
    /// Caller must hold the draw mutex.
    fn update_screen_size_uniforms(&mut self) {
        let terminal_size = self.size.terminal();

        // Blank space around the grid.
        let blank: Padding = self
            .size
            .screen
            .blank_padding(
                self.size.padding,
                GridSize {
                    columns: self.cells.size.columns,
                    rows: self.cells.size.rows,
                },
                CellSize {
                    width: self.grid_metrics.cell_width,
                    height: self.grid_metrics.cell_height,
                },
            )
            .add(self.size.padding);

        // Setup our uniforms
        self.uniforms.projection_matrix = math::ortho2d(
            -(self.size.padding.left as f32),
            (terminal_size.width + self.size.padding.right) as f32,
            (terminal_size.height + self.size.padding.bottom) as f32,
            -(self.size.padding.top as f32),
        );
        self.uniforms.grid_padding = [blank.top as f32, blank.right as f32, blank.bottom as f32, blank.left as f32];
        self.uniforms.screen_size = [self.size.screen.width as f32, self.size.screen.height as f32];
    }

    /// Update the background image vertex buffer (CPU-side).
    ///
    /// This should be called if and when configs change that
    /// could affect the background image.
    ///
    /// Caller must hold the draw mutex.
    fn update_bg_image_buffer(&mut self) {
        use api::shaders::{BgImageFit as Fit, BgImagePosition as Pos};
        self.bg_image_buffer = api::shaders::BgImage {
            opacity: self.config.bg_image_opacity,
            info: api::shaders::BgImageInfo::new(
                match self.config.bg_image_position {
                    BackgroundImagePosition::TopLeft => Pos::Tl,
                    BackgroundImagePosition::TopCenter => Pos::Tc,
                    BackgroundImagePosition::TopRight => Pos::Tr,
                    BackgroundImagePosition::CenterLeft => Pos::Ml,
                    BackgroundImagePosition::CenterCenter | BackgroundImagePosition::Center => Pos::Mc,
                    BackgroundImagePosition::CenterRight => Pos::Mr,
                    BackgroundImagePosition::BottomLeft => Pos::Bl,
                    BackgroundImagePosition::BottomCenter => Pos::Bc,
                    BackgroundImagePosition::BottomRight => Pos::Br,
                },
                match self.config.bg_image_fit {
                    BackgroundImageFit::Contain => Fit::Contain,
                    BackgroundImageFit::Cover => Fit::Cover,
                    BackgroundImageFit::Stretch => Fit::Stretch,
                    BackgroundImageFit::None => Fit::None,
                },
                self.config.bg_image_repeat,
            ),
        };
        // Signal that the buffer was modified.
        self.bg_image_buffer_modified = self.bg_image_buffer_modified.wrapping_add(1);
    }

    /// Update custom shader uniforms that depend on terminal state.
    ///
    /// This should be called in `updateFrame` when terminal state changes.
    fn update_custom_shader_uniforms_from_state(&mut self) {
        // We only need to do this if we have custom shaders.
        if !self.has_custom_shaders {
            return;
        }

        // Only update when terminal state is dirty.
        if self.terminal_state.dirty == Dirty::False {
            return;
        }

        let uniforms: &mut shadertoy::Uniforms = &mut self.custom_shader_uniforms;
        let colors = &self.terminal_state.colors;

        fn rgba(c: Rgb) -> [f32; 4] {
            [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, 1.0]
        }

        // 256-color palette
        for (i, color) in colors.palette.iter().enumerate() {
            uniforms.palette[i] = rgba(*color);
        }

        // Background color
        uniforms.background_color = rgba(colors.background);

        // Foreground color
        uniforms.foreground_color = rgba(colors.foreground);

        // Cursor color
        if let Some(cursor_color) = colors.cursor {
            uniforms.cursor_color = rgba(cursor_color);
        }

        // NOTE: the following could be optimized to follow a change in
        // config for a slight optimization however this is only 12 bytes
        // each being updated and likely isn't a cause for concern

        // Cursor text color (Zig reads `.color` of the union, which is
        // only meaningful for an explicit color).
        if let Some(TerminalColor::Color(c)) = self.config.cursor_text {
            uniforms.cursor_text = rgba(c.to_terminal_rgb());
        }

        // Selection background color
        if let Some(TerminalColor::Color(c)) = self.config.selection_background {
            uniforms.selection_background_color = rgba(c.to_terminal_rgb());
        }

        // Selection foreground color
        if let Some(TerminalColor::Color(c)) = self.config.selection_foreground {
            uniforms.selection_foreground_color = rgba(c.to_terminal_rgb());
        }

        // Cursor visibility
        uniforms.cursor_visible = self.terminal_state.cursor.visible as i32;

        // Cursor style
        let cursor_style = CursorStyle::from_terminal(self.terminal_state.cursor.visual_style);
        uniforms.previous_cursor_style = uniforms.current_cursor_style;
        uniforms.current_cursor_style = cursor_style as i32;
    }

    /// Update per-frame custom shader uniforms.
    ///
    /// This should be called exactly once per frame, inside `drawFrame`.
    fn update_custom_shader_uniforms_for_frame(&mut self) -> Result<(), Error> {
        // We only need to do this if we have custom shaders.
        if !self.has_custom_shaders {
            return Ok(());
        }

        let uniforms: &mut shadertoy::Uniforms = &mut self.custom_shader_uniforms;

        let now = Instant::now();
        let first_frame_time = *self.first_frame_time.get_or_insert(now);
        let last_frame_time = self.last_frame_time.unwrap_or(now);

        let since_ns = now.duration_since(first_frame_time).as_nanos() as f32;
        uniforms.time = since_ns / 1_000_000_000.0;

        let delta_ns = now.duration_since(last_frame_time).as_nanos() as f32;
        uniforms.time_delta = delta_ns / 1_000_000_000.0;

        uniforms.frame += 1;

        let screen = self.size.screen;
        let padding = self.size.padding;
        let cell = self.size.cell;

        uniforms.resolution = [screen.width as f32, screen.height as f32, 1.0];
        uniforms.channel_resolution[0] = [screen.width as f32, screen.height as f32, 1.0, 0.0];

        if let Some(cursor) = self.cells.get_cursor_glyph() {
            let cursor_width = cursor.glyph_size[0] as f32;
            let cursor_height = cursor.glyph_size[1] as f32;

            // Left edge of the cell the cursor is in.
            let mut pixel_x = (cursor.grid_pos[0] as u32 * cell.width + padding.left) as f32;
            // Top edge, relative to the top of the
            // screen, of the cell the cursor is in.
            let mut pixel_y = (cursor.grid_pos[1] as u32 * cell.height + padding.top) as f32;

            // If +Y is up in our shaders, we need to flip the coordinate
            // so that it's instead the top edge of the cell relative to
            // the *bottom* of the screen.
            if !G::CUSTOM_SHADER_Y_IS_DOWN {
                pixel_y = screen.height as f32 - pixel_y;
            }

            // Add the X bearing to get the -X (left) edge of the cursor.
            pixel_x += cursor.bearings[0] as f32;

            // How we deal with the Y bearing depends on which direction
            // is "up", since we want our final `pixel_y` value to be the
            // +Y edge of the cursor.
            if G::CUSTOM_SHADER_Y_IS_DOWN {
                // As a reminder, the Y bearing is the distance from the
                // bottom of the cell to the top of the glyph, so to get
                // the +Y edge we need to add the cell height, subtract
                // the Y bearing, and add the glyph height to get the +Y
                // (bottom) edge of the cursor.
                pixel_y += cell.height as f32;
                pixel_y -= cursor.bearings[1] as f32;
                pixel_y += cursor.glyph_size[1] as f32;
            } else {
                // If the Y direction is reversed though, we instead want
                // the *top* edge of the cursor, which means we just need
                // to subtract the cell height and add the Y bearing.
                pixel_y -= cell.height as f32;
                pixel_y += cursor.bearings[1] as f32;
            }

            let new_cursor: [f32; 4] = [pixel_x, pixel_y, cursor_width, cursor_height];
            let cursor_color: [f32; 4] = [
                cursor.color[0] as f32 / 255.0,
                cursor.color[1] as f32 / 255.0,
                cursor.color[2] as f32 / 255.0,
                cursor.color[3] as f32 / 255.0,
            ];

            let cursor_changed = new_cursor != uniforms.current_cursor || cursor_color != uniforms.current_cursor_color;

            if cursor_changed {
                uniforms.previous_cursor = uniforms.current_cursor;
                uniforms.previous_cursor_color = uniforms.current_cursor_color;
                uniforms.current_cursor = new_cursor;
                uniforms.current_cursor_color = cursor_color;
                uniforms.cursor_change_time = uniforms.time;
            }
        }

        // Update focus uniforms
        uniforms.focus = self.focused as i32;

        // If we need to update the time our focus state changed
        // then update it to our current frame time. This may not be
        // exactly correct since it is frame time, not exact focus
        // time, but focus time on its own isn't exactly correct anyways
        // since it comes async from a message.
        if self.custom_shader_focused_changed && self.focused {
            uniforms.time_focus = uniforms.time;
            self.custom_shader_focused_changed = false;
        }

        self.last_frame_time = Some(now);
        Ok(())
    }

    /// Build the overlay as configured. Returns null if there is no
    /// overlay currently configured.
    fn rebuild_overlay(&mut self, features: &[overlay::Feature]) -> Result<(), overlay::InitError> {
        // If we have no features enabled, don't build an overlay.
        // If we had a previous overlay, deallocate it.
        if features.is_empty() {
            if let Some(mut old) = self.overlay.take() {
                old.deinit();
            }
            return Ok(());
        }

        // If we had a previous overlay, clear it. Otherwise, init.
        let reuse = match &self.overlay {
            Some(v) => {
                // Verify that our overlay size matches our screen
                // size as we know it now. If not, deinit and reinit.
                // Note: these casts are always safe because z2d
                // stores as i32 but we always init with a u32.
                let width = v.surface.get_width() as u32;
                let height = v.surface.get_height() as u32;
                let term_size = self.size.terminal();
                // We also depend on cell size.
                width == term_size.width
                    && height == term_size.height
                    && v.cell_size.width == self.size.cell.width
                    && v.cell_size.height == self.size.cell.height
            }
            None => false,
        };

        if reuse {
            // Everything matches, so we can just reset the surface
            // and redraw.
            self.overlay.as_mut().unwrap().reset();
        } else {
            // If we reached this point we want to reset our overlay.
            if let Some(mut v) = self.overlay.take() {
                v.deinit();
            }

            debug_assert!(self.overlay.is_none());
            self.overlay = Some(Overlay::init(self.size)?);
        }

        let overlay = self.overlay.as_mut().unwrap();
        overlay.apply_features(&self.terminal_state, features);
        Ok(())
    }

    /// Convert the terminal state to GPU cells stored in CPU memory. These
    /// are then synced to the GPU in the next frame. This only updates CPU
    /// memory and doesn't touch the GPU.
    ///
    /// This requires the draw mutex.
    ///
    /// Dirty state on terminal state won't be reset by this.
    fn rebuild_cells(&mut self, preedit: Option<&Preedit>, cursor_style_: Option<CursorStyle>, links: &CellSet) {
        let state: &mut RenderState = &mut self.terminal_state;

        let grid_size_diff = self.cells.size.rows != state.rows || self.cells.size.columns != state.cols;

        if grid_size_diff {
            let mut new_size = self.cells.size;
            new_size.rows = state.rows;
            new_size.columns = state.cols;
            self.cells.resize(new_size);

            // Update our uniforms accordingly, otherwise
            // our background cells will be out of place.
            self.uniforms.grid_size = [new_size.columns, new_size.rows];
        }

        let rebuild = state.dirty == Dirty::Full || grid_size_diff;
        if rebuild {
            // If we are doing a full rebuild, then we clear the entire cell buffer.
            self.cells.reset();

            // We also reset our padding extension depending on the screen type
            match self.config.padding_color {
                WindowPaddingColor::Background => {}

                // For extension, assume we are extending in all directions.
                // For "extend" this may be disabled due to heuristics below.
                WindowPaddingColor::Extend | WindowPaddingColor::ExtendAlways => {
                    self.uniforms.padding_extend = api::shaders::PaddingExtend::new(true, true, true, true);
                }
            }
        }

        // From this point on we never fail. We produce some kind of
        // working terminal state, even if incorrect.

        // If our cell contents buffer is shorter than the screen viewport,
        // we render the rows that fit, starting from the bottom. If instead
        // the viewport is shorter than the cell contents buffer, we align
        // the top of the viewport with the top of the contents buffer.
        let row_len: usize = (state.rows as usize).min(self.cells.size.rows as usize);

        // Determine our x/y range for preedit. We don't want to render anything
        // here because we will render the preedit separately.
        let preedit_range: Option<PreeditRange> = preedit.and_then(|preedit_v| {
            // We base the preedit on the position of the cursor in the
            // viewport. If the cursor isn't visible in the viewport we
            // don't show it.
            let cursor_vp = state.cursor.viewport?;

            // If our preedit row isn't dirty then we don't need the
            // preedit range. This also avoids an issue later where we
            // unconditionally add preedit cells when this is set.
            if !rebuild && !state.row_data.dirty[cursor_vp.y as usize] {
                return None;
            }

            let range = preedit_v.range(cursor_vp.x, state.cols - 1);
            Some(PreeditRange {
                y: cursor_vp.y,
                x: [range.start, range.end],
                cp_offset: range.cp_offset,
            })
        });

        // SAFETY: the font grid outlives the renderer.
        let grid: &SharedGrid = unsafe { &*self.font_grid };
        let mut ctx = RebuildCtx {
            cells: &mut self.cells,
            uniforms: &mut self.uniforms,
            shaper_cache: &mut self.font_shaper_cache,
            config: &self.config,
            grid,
            grid_metrics: self.grid_metrics,
            focused: self.focused,
            colors_palette: &state.colors.palette,
            colors_foreground: state.colors.foreground,
            colors_background: state.colors.background,
            cursor_viewport: state.cursor.viewport,
            cols: state.cols,
        };

        {
            let row_data = &mut state.row_data;
            for y_usize in 0..row_len {
                let y = y_usize as CellCountInt;

                if !rebuild {
                    // Only rebuild if we are doing a full rebuild or this row is dirty.
                    if !row_data.dirty[y_usize] {
                        continue;
                    }

                    // Clear the cells if the row is dirty
                    ctx.cells.clear(y);
                }

                // Unmark the dirty state in our render state.
                row_data.dirty[y_usize] = false;

                if let Err(err) = ctx.rebuild_row(
                    &mut self.font_shaper,
                    y,
                    row_data.raw[y_usize],
                    &row_data.cells[y_usize],
                    preedit_range,
                    row_data.selection[y_usize],
                    &row_data.highlights[y_usize],
                    links,
                ) {
                    // This should never happen except under exceptional
                    // scenarios. In this case, we don't want to corrupt
                    // our render state so just clear this row and keep
                    // trying to finish it out.
                    log::warn!(target: LOG, "error building row y={y} err={err}");
                    ctx.cells.clear(y);
                }
            }
        }

        // Setup our cursor rendering information.
        'cursor: {
            // Clear our cursor by default.
            ctx.cells.set_cursor(None, None);
            ctx.uniforms.cursor_pos = [u16::MAX, u16::MAX];

            // If the cursor isn't visible on the viewport, don't show
            // a cursor. Otherwise, get our cursor cell, because we may
            // need it for styling.
            let Some(cursor_vp) = state.cursor.viewport else { break 'cursor };
            let cursor_style: Style = {
                let cell = state.row_data.cells[cursor_vp.y as usize].get(cursor_vp.x as usize);
                if cell.raw.has_styling() {
                    cell.style
                } else {
                    Style::default()
                }
            };

            // If we have preedit text, we don't setup a cursor
            if preedit.is_some() {
                break 'cursor;
            }

            // If there isn't a cursor visual style requested then
            // we don't render a cursor.
            let Some(style) = cursor_style_ else { break 'cursor };

            let fg_style = || {
                cursor_style.fg(StyleFg {
                    default: state.colors.foreground,
                    palette: &state.colors.palette,
                    bold: ctx.config.bold_color,
                })
            };
            let bg_style = || {
                cursor_style
                    .bg(&state.cursor.cell, &state.colors.palette)
                    .unwrap_or(state.colors.background)
            };

            // Determine the cursor color.
            let cursor_color: Rgb = 'cursor_color: {
                // If an explicit cursor color was set by OSC 12, use that.
                if let Some(v) = state.colors.cursor {
                    break 'cursor_color v;
                }

                // Use our configured color if specified
                if let Some(v) = ctx.config.cursor_color {
                    break 'cursor_color match v {
                        TerminalColor::Color(color) => color.to_terminal_rgb(),
                        TerminalColor::CellForeground => {
                            if cursor_style.flags.inverse {
                                bg_style()
                            } else {
                                fg_style()
                            }
                        }
                        TerminalColor::CellBackground => {
                            if cursor_style.flags.inverse {
                                fg_style()
                            } else {
                                bg_style()
                            }
                        }
                    };
                }

                state.colors.foreground
            };

            ctx.add_cursor(&state.cursor.cell, cursor_vp, style, cursor_color);

            // If the cursor is visible then we set our uniforms.
            if style == CursorStyle::Block {
                let wide = state.cursor.cell.wide();

                ctx.uniforms.cursor_pos = [
                    // If we are a spacer tail of a wide cell, our cursor needs
                    // to move back one cell. The saturate is to ensure we don't
                    // overflow but this shouldn't happen with well-formed input.
                    match wide {
                        Wide::Narrow | Wide::SpacerHead | Wide::Wide => cursor_vp.x,
                        Wide::SpacerTail => cursor_vp.x.saturating_sub(1),
                    },
                    cursor_vp.y,
                ];

                compat::set_cursor_wide(
                    ctx.uniforms,
                    match wide {
                        Wide::Narrow | Wide::SpacerHead => false,
                        Wide::Wide | Wide::SpacerTail => true,
                    },
                );

                let uniform_color = if let Some(txt) = ctx.config.cursor_text {
                    // If cursor-text is set, then compute the correct color.
                    // Otherwise, use the background color.
                    match txt {
                        // Use the color set by cursor-text, if any.
                        TerminalColor::Color(c) => c.to_terminal_rgb(),
                        // If the cell is reversed, use the opposite cell color instead.
                        TerminalColor::CellForeground => {
                            if cursor_style.flags.inverse {
                                bg_style()
                            } else {
                                fg_style()
                            }
                        }
                        TerminalColor::CellBackground => {
                            if cursor_style.flags.inverse {
                                fg_style()
                            } else {
                                bg_style()
                            }
                        }
                    }
                } else {
                    state.colors.background
                };

                ctx.uniforms.cursor_color = [uniform_color.r, uniform_color.g, uniform_color.b, 255];
            }
        }

        // Setup our preedit text.
        if let (Some(preedit_v), Some(range)) = (preedit, preedit_range) {
            let mut x = range.x[0];
            for cp in &preedit_v.codepoints[range.cp_offset..] {
                if let Err(err) = ctx.add_preedit_cell(
                    *cp,
                    point::Coordinate {
                        x,
                        y: range.y as u32,
                    },
                    state.colors.foreground,
                ) {
                    log::warn!(
                        target: LOG,
                        "error building preedit cell, will be invalid x={x} y={}, err={err}",
                        range.y
                    );
                }

                x += if cp.wide { 2 } else { 1 };
            }
        }

        // Update that our cells rebuilt
        self.cells_rebuilt = true;

        // Log some things
        // log.debug("rebuildCells complete cached_runs={}", .{
        //     self.font_shaper_cache.count(),
        // });
    }

    /// Sync the atlas data to the given texture. This copies the bytes
    /// associated with the atlas to the given texture. If the atlas no
    /// longer fits into the texture, the texture will be resized.
    fn sync_atlas_texture(&self, atlas: &font::Atlas, texture: &mut api::Texture) -> Result<(), Error> {
        if atlas.size as usize > texture.width {
            // Free our old texture
            texture.deinit();

            // Reallocate
            *texture = self.api.init_atlas_texture(atlas)?;
        }

        compat::replace_region(texture, atlas.size as usize, atlas.size as usize, &atlas.data)
    }
}

/// Zig: `defer frame_ctx.complete(sync)`. Metal's `complete` takes
/// `&mut self`, OpenGL's `&self`.
#[allow(clippy::needless_pass_by_ref_mut)]
fn frame_ctx_complete(frame_ctx: &mut api::Frame, sync: bool) {
    frame_ctx.complete(sync);
}

/// The renderer fields `rebuildRow` and the `add*` helpers use, borrowed
/// separately from the font shaper (see the module docs).
struct RebuildCtx<'a> {
    cells: &'a mut cellpkg::Contents,
    uniforms: &'a mut api::shaders::Uniforms,
    shaper_cache: &'a mut font::ShaperCache,
    config: &'a DerivedConfig,
    grid: &'a SharedGrid,
    grid_metrics: font::Metrics,
    focused: bool,
    colors_palette: &'a Palette,
    colors_foreground: Rgb,
    colors_background: Rgb,
    cursor_viewport: Option<CursorViewport>,
    cols: CellCountInt,
}


/// Shape `run` into `buf`, preferring the shaper cache (Zig: the
/// `shaper_cells orelse cache.get(run) orelse shape(run)` blocks).
fn shape_run(
    cache: &mut font::ShaperCache,
    shaper: &mut font::Shaper,
    run: TextRun<'_>,
    buf: &mut Vec<shape::Cell>,
) -> Result<(), Error> {
    buf.clear();

    // Try to read the cells from the shaping cache if we can.
    if let Some(cached) = cache.get(&run) {
        buf.extend_from_slice(cached);
        return Ok(());
    }

    // Otherwise we have to shape them.
    let new_cells = shaper.shape(run)?;

    // Try to cache them. If caching fails for any reason we
    // continue because it is just a performance optimization,
    // not a correctness issue.
    cache.put(&run, new_cells);

    // The cells we get from direct shaping are always owned
    // by the shaper and valid until the next shaping call so
    // we can safely use them.
    buf.extend_from_slice(new_cells);
    Ok(())
}

impl RebuildCtx<'_> {
    fn rebuild_row(
        &mut self,
        font_shaper: &mut font::Shaper,
        y: CellCountInt,
        row: page::Row,
        cells: &Cells,
        preedit_range: Option<PreeditRange>,
        selection: Option<[CellCountInt; 2]>,
        highlights: &[Highlight],
        links: &CellSet,
    ) -> Result<(), Error> {
        // If our viewport is wider than our cell contents buffer,
        // we still only process cells up to the width of the buffer.
        let cells_slice = cells.slice();
        let cells_len = cells_slice.len().min(self.cells.size.columns as usize);
        let cells_raw = cells_slice.raw;
        let cells_style = cells_slice.style;

        // On primary screen, we still apply vertical padding
        // extension under certain conditions we feel are safe.
        //
        // This helps make some scenarios look better while
        // avoiding scenarios we know do NOT look good.
        match self.config.padding_color {
            // These already have the correct values set above.
            WindowPaddingColor::Background | WindowPaddingColor::ExtendAlways => {}

            // Apply heuristics for padding extension.
            WindowPaddingColor::Extend => {
                let pe = self.uniforms.padding_extend;
                if y == 0 {
                    let up = !never_extend_bg(row, cells_raw, cells_style, self.colors_palette, self.colors_background);
                    self.uniforms.padding_extend = api::shaders::PaddingExtend::new(pe.left(), pe.right(), up, pe.down());
                } else if y == self.cells.size.rows - 1 {
                    let down = !never_extend_bg(row, cells_raw, cells_style, self.colors_palette, self.colors_background);
                    self.uniforms.padding_extend = api::shaders::PaddingExtend::new(pe.left(), pe.right(), pe.up(), down);
                }
            }
        }

        // Iterator of runs for shaping.
        let mut run_iter_opts = RunOptions {
            grid: self.grid,
            cells: cells_slice,
            selection,

            // We want to do font shaping as long as the cursor is
            // visible on this viewport.
            cursor_x: match self.cursor_viewport {
                Some(vp) if vp.y == y => Some(vp.x as usize),
                _ => None,
            },
        };
        run_iter_opts.apply_break_config(self.config.font_shaping_break);
        let mut run_iter = font_shaper.run_iterator(run_iter_opts);
        let mut shaper_run: Option<TextRun<'_>> = run_iter.next()?;
        // Zig: `shaper_cells: ?[]const font.shape.Cell` (borrowed from the
        // shaper or its cache). Here the shaped cells are copied into
        // `shaper_cells_buf` and `shaper_cells` says whether it is valid.
        let mut shaper_cells_buf: Vec<shape::Cell> = Vec::new();
        let mut shaper_cells: bool = false;
        let mut shaper_cells_i: usize = 0;

        for x in 0..cells_len {
            let cell = &cells_raw[x];
            let managed_style = &cells_style[x];

            // If this cell falls within our preedit range then we
            // skip this because preedits are setup separately.
            if let Some(range) = preedit_range {
                // (Zig: a labeled block; `in_range` is its `continue`.)
                let mut in_range = false;
                'preedit: {
                    // We're not on the preedit line, no actions necessary.
                    if range.y != y {
                        break 'preedit;
                    }
                    // We're before the preedit range, no actions necessary.
                    if x < range.x[0] as usize {
                        break 'preedit;
                    }
                    // We're in the preedit range, skip this cell.
                    if x <= range.x[1] as usize {
                        in_range = true;
                        break 'preedit;
                    }
                    // After exiting the preedit range we need to catch
                    // the run position up because of the missed cells.
                    // In all other cases, no action is necessary.
                    if x != range.x[1] as usize + 1 {
                        break 'preedit;
                    }

                    // Step the run iterator until we find a run that ends
                    // after the current cell, which will be the soonest run
                    // that might contain glyphs for our cell.
                    while let Some(run) = shaper_run {
                        if run.offset as usize + run.cells as usize > x {
                            break;
                        }
                        shaper_run = run_iter.next()?;
                        shaper_cells = false;
                        shaper_cells_i = 0;
                    }

                    let Some(run) = shaper_run else { break 'preedit };

                    // If we haven't shaped this run, do so now.
                    if !shaper_cells {
                        shape_run(self.shaper_cache, run_iter.hooks.shaper, run, &mut shaper_cells_buf)?;
                        shaper_cells = true;
                    }

                    // Advance our index until we reach or pass
                    // our current x position in the shaper cells.
                    while shaper_cells_i < shaper_cells_buf.len()
                        && (run.offset as usize + shaper_cells_buf[shaper_cells_i].x as usize) < x
                    {
                        shaper_cells_i += 1;
                    }
                }
                if in_range {
                    continue;
                }
            }

            let wide = cell.wide();
            let style: Style = if cell.has_styling() { *managed_style } else { Style::default() };

            // True if this cell is selected
            let selected: Selected = 'selected: {
                // Order below matters for precedence.

                // Selection should take the highest precedence.
                let x_compare = if wide == Wide::SpacerTail { x.saturating_sub(1) } else { x };
                if let Some(sel) = selection {
                    if x_compare >= sel[0] as usize && x_compare <= sel[1] as usize {
                        break 'selected Selected::Selection;
                    }
                }

                // If we're highlighted, then we're selected. In the
                // future we want to use a different style for this
                // but this to get started.
                for hl in highlights {
                    if x_compare >= hl.range[0] as usize && x_compare <= hl.range[1] as usize {
                        break 'selected match HighlightTag::from_u8(hl.tag) {
                            HighlightTag::SearchMatch => Selected::Search,
                            HighlightTag::SearchMatchSelected => Selected::SearchSelected,
                        };
                    }
                }

                Selected::False
            };

            // The `_style` suffixed values are the colors based on
            // the cell style (SGR), before applying any additional
            // configuration, inversions, selections, etc.
            let bg_style: Option<Rgb> = style.bg(cell, self.colors_palette);
            let fg_style: Rgb = style.fg(StyleFg {
                default: self.colors_foreground,
                palette: self.colors_palette,
                bold: self.config.bold_color,
            });

            // Resolve a configured background color against the cell's colors.
            let resolve_bg = |v: TerminalColor| -> Option<Rgb> {
                match v {
                    TerminalColor::Color(color) => Some(color.to_terminal_rgb()),
                    TerminalColor::CellForeground => {
                        if style.flags.inverse {
                            bg_style
                        } else {
                            Some(fg_style)
                        }
                    }
                    TerminalColor::CellBackground => {
                        if style.flags.inverse {
                            Some(fg_style)
                        } else {
                            bg_style
                        }
                    }
                }
            };

            // The final background color for the cell.
            let bg: Option<Rgb> = match selected {
                // If we have an explicit selection background color
                // specified in the config, use that.
                //
                // If no configuration, then our selection background
                // is our foreground color.
                Selected::Selection => match self.config.selection_background {
                    Some(v) => resolve_bg(v),
                    None => Some(self.colors_foreground),
                },

                Selected::Search => resolve_bg(self.config.search_background),

                Selected::SearchSelected => resolve_bg(self.config.search_selected_background),

                // Not selected
                Selected::False => {
                    if style.flags.inverse != is_covering(cell.codepoint()) {
                        // Two cases cause us to invert (use the fg color as the bg)
                        // - The "inverse" style flag.
                        // - A "covering" glyph; we use fg for bg in that
                        //   case to help make sure that padding extension
                        //   works correctly.
                        //
                        // If one of these is true (but not the other)
                        // then we use the fg style color for the bg.
                        Some(fg_style)
                    } else {
                        // Otherwise they cancel out.
                        bg_style
                    }
                }
            };

            let fg: Rgb = {
                // Our happy-path non-selection background color
                // is our style or our configured defaults.
                let final_bg = bg_style.unwrap_or(self.colors_background);

                let resolve_fg = |v: TerminalColor| -> Rgb {
                    match v {
                        TerminalColor::Color(color) => color.to_terminal_rgb(),
                        TerminalColor::CellForeground => {
                            if style.flags.inverse {
                                final_bg
                            } else {
                                fg_style
                            }
                        }
                        TerminalColor::CellBackground => {
                            if style.flags.inverse {
                                fg_style
                            } else {
                                final_bg
                            }
                        }
                    }
                };

                // Whether we need to use the bg color as our fg color:
                // - Cell is selected, inverted, and set to cell-foreground
                // - Cell is selected, not inverted, and set to cell-background
                // - Cell is inverted and not selected
                match selected {
                    Selected::Selection => match self.config.selection_foreground {
                        Some(v) => resolve_fg(v),
                        None => self.colors_background,
                    },

                    Selected::Search => resolve_fg(self.config.search_foreground),

                    Selected::SearchSelected => resolve_fg(self.config.search_selected_foreground),

                    Selected::False => {
                        if style.flags.inverse {
                            final_bg
                        } else {
                            fg_style
                        }
                    }
                }
            };

            // Foreground alpha for this cell.
            let alpha: u8 = if style.flags.faint { self.config.faint_opacity } else { 255 };

            // Set the cell's background color.
            {
                let rgb = bg.unwrap_or(self.colors_background);

                // Determine our background alpha. If we have transparency configured
                // then this is dynamic depending on some situations. This is all
                // in an attempt to make transparency look the best for various
                // situations. See inline comments.
                let bg_alpha: u8 = 'bg_alpha: {
                    let default: u8 = 255;

                    // Cells that are selected should be fully opaque.
                    if selected != Selected::False {
                        break 'bg_alpha default;
                    }

                    // Cells that are reversed should be fully opaque.
                    if style.flags.inverse {
                        break 'bg_alpha default;
                    }

                    // If the user requested to have opacity on all cells, apply it.
                    if self.config.background_opacity_cells && bg_style.is_some() {
                        let mut opacity: f64 = default as f64;
                        opacity *= self.config.background_opacity;
                        break 'bg_alpha opacity as u8;
                    }

                    // Cells that have an explicit bg color should be fully opaque.
                    if bg_style.is_some() {
                        break 'bg_alpha default;
                    }

                    // Otherwise, we won't draw the bg for this cell,
                    // we'll let the already-drawn background color
                    // show through.
                    0
                };

                *self.cells.bg_cell(y as usize, x) = [rgb.r, rgb.g, rgb.b, bg_alpha];
            }

            // If the invisible flag is set on this cell then we
            // don't need to render any foreground elements, so
            // we just skip all glyphs with this x coordinate.
            //
            // NOTE: This behavior matches xterm. Some other terminal
            // emulators, e.g. Alacritty, still render text decorations
            // and only make the text itself invisible. The decision
            // has been made here to match xterm's behavior for this.
            if style.flags.invisible {
                continue;
            }

            // Give links a single underline, unless they already have
            // an underline, in which case use a double underline to
            // distinguish them.
            let underline: Underline = if links.contains(&point::Coordinate {
                x: x as CellCountInt,
                y: y as u32,
            }) {
                if style.flags.underline == Underline::Single {
                    Underline::Double
                } else {
                    Underline::Single
                }
            } else {
                style.flags.underline
            };

            // We draw underlines first so that they layer underneath text.
            // This improves readability when a colored underline is used
            // which intersects parts of the text (descenders).
            if underline != Underline::None {
                if let Err(err) = self.add_underline(
                    x as CellCountInt,
                    y,
                    underline,
                    style.underline_color(self.colors_palette).unwrap_or(fg),
                    alpha,
                ) {
                    log::warn!(
                        target: LOG,
                        "error adding underline to cell, will be invalid x={x} y={y}, err={err}"
                    );
                }
            }

            if style.flags.overline {
                if let Err(err) = self.add_overline(x as CellCountInt, y, fg, alpha) {
                    log::warn!(
                        target: LOG,
                        "error adding overline to cell, will be invalid x={x} y={y}, err={err}"
                    );
                }
            }

            // If we're at or past the end of our shaper run then
            // we need to get the next run from the run iterator.
            if shaper_cells && shaper_cells_i >= shaper_cells_buf.len() {
                shaper_run = run_iter.next()?;
                shaper_cells = false;
                shaper_cells_i = 0;
            }

            if let Some(run) = shaper_run {
                'glyphs: {
                    // If we haven't shaped this run yet, do so.
                    if !shaper_cells {
                        shape_run(self.shaper_cache, run_iter.hooks.shaper, run, &mut shaper_cells_buf)?;
                        shaper_cells = true;
                    }

                    // If there are no shaper cells for this run, ignore it.
                    // This can occur for runs of empty cells, and is fine.
                    if shaper_cells_buf.is_empty() {
                        break 'glyphs;
                    }

                    // If we encounter a shaper cell to the left of the current
                    // cell then we have some problems. This logic relies on x
                    // position monotonically increasing.
                    debug_assert!(
                        shaper_cells_i >= shaper_cells_buf.len()
                            || run.offset as usize + shaper_cells_buf[shaper_cells_i].x as usize >= x
                    );

                    // NOTE: An assumption is made here that a single cell will never
                    // be present in more than one shaper run. If that assumption is
                    // violated, this logic breaks.

                    while shaper_cells_i < shaper_cells_buf.len()
                        && run.offset as usize + shaper_cells_buf[shaper_cells_i].x as usize == x
                    {
                        if let Err(err) = self.add_glyph(
                            x as CellCountInt,
                            y,
                            self.cols as usize,
                            cells_raw,
                            shaper_cells_buf[shaper_cells_i],
                            run,
                            fg,
                            alpha,
                        ) {
                            log::warn!(
                                target: LOG,
                                "error adding glyph to cell, will be invalid x={x} y={y}, err={err}"
                            );
                        }
                        shaper_cells_i += 1;
                    }
                }
            }

            // Finally, draw a strikethrough if necessary.
            if style.flags.strikethrough {
                if let Err(err) = self.add_strikethrough(x as CellCountInt, y, fg, alpha) {
                    log::warn!(
                        target: LOG,
                        "error adding strikethrough to cell, will be invalid x={x} y={y}, err={err}"
                    );
                }
            }
        }

        Ok(())
    }

    /// Render a sprite glyph one cell wide (the decoration helpers).
    fn render_sprite(&self, sprite: Sprite, cell_width: u8) -> Result<font::shared_grid::Render, Error> {
        let mut opts = RenderOptions::new(self.grid_metrics);
        opts.cell_width = Some(cell_width);
        Ok(self.grid.render_glyph(font::SPRITE_INDEX, sprite as u32, opts)?)
    }

    /// Add a decoration cell for a rendered sprite.
    fn add_sprite_cell(
        &mut self,
        key: CellKey,
        render: font::shared_grid::Render,
        x: CellCountInt,
        y: CellCountInt,
        color: Rgb,
        alpha: u8,
    ) {
        let mut cell = api::shaders::CellText::new([x, y], [color.r, color.g, color.b, alpha], api::shaders::CellTextAtlas::Grayscale);
        cell.glyph_pos = [render.glyph.atlas_x, render.glyph.atlas_y];
        cell.glyph_size = [render.glyph.width, render.glyph.height];
        cell.bearings = [render.glyph.offset_x as i16, render.glyph.offset_y as i16];
        self.cells.add(key, cell);
    }

    /// Add an underline decoration to the specified cell
    fn add_underline(
        &mut self,
        x: CellCountInt,
        y: CellCountInt,
        style: Underline,
        color: Rgb,
        alpha: u8,
    ) -> Result<(), Error> {
        let sprite: Sprite = match style {
            Underline::None => unreachable!(),
            Underline::Single => Sprite::Underline,
            Underline::Double => Sprite::UnderlineDouble,
            Underline::Dotted => Sprite::UnderlineDotted,
            Underline::Dashed => Sprite::UnderlineDashed,
            Underline::Curly => Sprite::UnderlineCurly,
        };

        let render = self.render_sprite(sprite, 1)?;
        self.add_sprite_cell(CellKey::Underline, render, x, y, color, alpha);
        Ok(())
    }

    /// Add a overline decoration to the specified cell
    fn add_overline(&mut self, x: CellCountInt, y: CellCountInt, color: Rgb, alpha: u8) -> Result<(), Error> {
        let render = self.render_sprite(Sprite::Overline, 1)?;
        self.add_sprite_cell(CellKey::Overline, render, x, y, color, alpha);
        Ok(())
    }

    /// Add a strikethrough decoration to the specified cell
    fn add_strikethrough(&mut self, x: CellCountInt, y: CellCountInt, color: Rgb, alpha: u8) -> Result<(), Error> {
        let render = self.render_sprite(Sprite::Strikethrough, 1)?;
        self.add_sprite_cell(CellKey::Strikethrough, render, x, y, color, alpha);
        Ok(())
    }

    // Add a glyph to the specified cell.
    fn add_glyph(
        &mut self,
        x: CellCountInt,
        y: CellCountInt,
        cols: usize,
        cell_raws: &[page::Cell],
        shaper_cell: shape::Cell,
        shaper_run: TextRun<'_>,
        color: Rgb,
        alpha: u8,
    ) -> Result<(), Error> {
        let cell = cell_raws[x as usize];
        let cp = cell.codepoint();

        // Render
        let mut opts = RenderOptions::new(self.grid_metrics);
        opts.thicken = self.config.font_thicken;
        opts.thicken_strength = self.config.font_thicken_strength;
        opts.cell_width = Some(cell.grid_width());
        // If there's no Nerd Font constraint for this codepoint
        // then, if it's a symbol, we constrain it to fit inside
        // its cell(s), we don't modify the alignment at all.
        opts.constraint = get_constraint(cp).unwrap_or(if cellpkg::is_symbol(cp) {
            Constraint {
                size: ConstraintSize::Fit,
                ..Constraint::NONE
            }
        } else {
            Constraint::NONE
        });
        opts.constraint_width = constraint_width(cell_raws, x as usize, cols);
        let render = self.grid.render_glyph(shaper_run.font_index, shaper_cell.glyph_index, opts)?;

        // If the glyph is 0 width or height, it will be invisible
        // when drawn, so don't bother adding it to the buffer.
        if render.glyph.width == 0 || render.glyph.height == 0 {
            return Ok(());
        }

        let mut c = api::shaders::CellText::new(
            [x, y],
            [color.r, color.g, color.b, alpha],
            match render.presentation {
                Presentation::Emoji => api::shaders::CellTextAtlas::Color,
                Presentation::Text => api::shaders::CellTextAtlas::Grayscale,
            },
        );
        c.bools = api::shaders::CellTextBools::new(no_min_contrast(cp), false);
        c.glyph_pos = [render.glyph.atlas_x, render.glyph.atlas_y];
        c.glyph_size = [render.glyph.width, render.glyph.height];
        c.bearings = [
            (render.glyph.offset_x + shaper_cell.x_offset as i32) as i16,
            (render.glyph.offset_y + shaper_cell.y_offset as i32) as i16,
        ];
        self.cells.add(CellKey::Text, c);
        Ok(())
    }

    fn add_cursor(
        &mut self,
        cursor_cell: &page::Cell,
        cursor_vp: CursorViewport,
        cursor_style: CursorStyle,
        cursor_color: Rgb,
    ) {
        // Add the cursor. We render the cursor over the wide character if
        // we're on the wide character tail.
        let (wide, x) = if !cursor_vp.wide_tail {
            // The cursor goes over the screen cursor position.
            (cursor_cell.wide() == Wide::Wide, cursor_vp.x)
        } else {
            // If we're part of a wide character, we move the cursor back
            // to the actual character.
            (true, cursor_vp.x - 1)
        };

        let alpha: u8 = if !self.focused {
            255
        } else {
            let alpha = 255.0 * self.config.cursor_opacity;
            alpha.ceil() as u8
        };

        let render = match cursor_style {
            CursorStyle::Block | CursorStyle::BlockHollow | CursorStyle::Bar | CursorStyle::Underline => {
                let sprite: Sprite = match cursor_style {
                    CursorStyle::Block => Sprite::CursorRect,
                    CursorStyle::BlockHollow => Sprite::CursorHollowRect,
                    CursorStyle::Bar => Sprite::CursorBar,
                    CursorStyle::Underline => Sprite::CursorUnderline,
                    CursorStyle::Lock => unreachable!(),
                };

                match self.render_sprite(sprite, if wide { 2 } else { 1 }) {
                    Ok(r) => r,
                    Err(err) => {
                        log::warn!(target: LOG, "error rendering cursor glyph err={err}");
                        return;
                    }
                }
            }

            CursorStyle::Lock => {
                let mut opts = RenderOptions::new(self.grid_metrics);
                opts.cell_width = Some(if wide { 2 } else { 1 });
                match self.grid.render_codepoint(
                    0xF023, // lock symbol
                    font::Style::Regular,
                    Some(Presentation::Text),
                    opts,
                ) {
                    Err(err) => {
                        log::warn!(target: LOG, "error rendering cursor glyph err={err}");
                        return;
                    }
                    Ok(None) => {
                        // This should never happen because we embed nerd
                        // fonts so we just log and return instead of fallback.
                        log::warn!(target: LOG, "failed to find lock symbol for cursor codepoint=0xF023");
                        return;
                    }
                    Ok(Some(r)) => r,
                }
            }
        };

        let mut cell = api::shaders::CellText::new(
            [x, cursor_vp.y],
            [cursor_color.r, cursor_color.g, cursor_color.b, alpha],
            api::shaders::CellTextAtlas::Grayscale,
        );
        cell.bools = api::shaders::CellTextBools::new(false, true);
        cell.glyph_pos = [render.glyph.atlas_x, render.glyph.atlas_y];
        cell.glyph_size = [render.glyph.width, render.glyph.height];
        cell.bearings = [render.glyph.offset_x as i16, render.glyph.offset_y as i16];
        self.cells.set_cursor(Some(cell), Some(cursor_style));
    }

    fn add_preedit_cell(
        &mut self,
        cp: crate::renderer::state::Codepoint,
        coord: point::Coordinate,
        screen_fg: Rgb,
    ) -> Result<(), Error> {
        // Render the glyph for our preedit text
        let mut opts = RenderOptions::new(self.grid_metrics);
        opts.thicken = self.config.font_thicken;
        opts.thicken_strength = self.config.font_thicken_strength;
        let render_ = match self
            .grid
            .render_codepoint(cp.codepoint, font::Style::Regular, Some(Presentation::Text), opts)
        {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error rendering preedit glyph err={err}");
                return Ok(());
            }
        };
        let Some(render) = render_ else {
            log::warn!(target: LOG, "failed to find font for preedit codepoint={:X}", cp.codepoint);
            return Ok(());
        };

        // Add our text
        let x = coord.x;
        let y = coord.y as CellCountInt;
        let mut cell = api::shaders::CellText::new(
            [x, y],
            [screen_fg.r, screen_fg.g, screen_fg.b, 255],
            api::shaders::CellTextAtlas::Grayscale,
        );
        cell.glyph_pos = [render.glyph.atlas_x, render.glyph.atlas_y];
        cell.glyph_size = [render.glyph.width, render.glyph.height];
        cell.bearings = [render.glyph.offset_x as i16, render.glyph.offset_y as i16];
        self.cells.add(CellKey::Text, cell);

        // Add underline
        self.add_underline(x, y, Underline::Single, screen_fg, 255)?;
        if cp.wide && x < self.cells.size.columns - 1 {
            self.add_underline(x + 1, y, Underline::Single, screen_fg, 255)?;
        }
        Ok(())
    }
}

/// Test support: a font grid for renderer tests.
#[cfg(test)]
pub(crate) mod test_util {
    use crate::font::codepoint_resolver::CodepointResolver;
    use crate::font::collection::{AddOptions, Collection, LoadOptions, SizeAdjustment};
    use crate::font::embedded;
    use crate::font::face::{DesiredSize, Face};
    use crate::font::library::Library;
    use crate::font::shared_grid::SharedGrid;
    use crate::font::Style;

    /// A font grid with the embedded Inconsolata (and the embedded emoji
    /// fonts as fallbacks) at 12pt. The grid (and its font library) is
    /// leaked: renderers keep a raw pointer to it, like the Zig app's
    /// grid set.
    pub(crate) fn test_grid() -> *mut SharedGrid {
        let add_opts = || AddOptions {
            style: Style::Regular,
            fallback: false,
            size_adjustment: SizeAdjustment::None,
        };

        let lib = Library::init().unwrap();

        let mut c = Collection::init();
        c.load_options = Some(LoadOptions::new(lib.clone()));
        c.add(
            Face::init(lib.clone(), embedded::INCONSOLATA, DesiredSize::new(12.0).into()).unwrap(),
            add_opts(),
        )
        .unwrap();
        if !crate::font::OPTIONS.backend.has_coretext() {
            c.add(
                Face::init(lib.clone(), embedded::EMOJI, DesiredSize::new(12.0).into()).unwrap(),
                add_opts(),
            )
            .unwrap();
        }
        c.add(
            Face::init(lib.clone(), embedded::EMOJI_TEXT, DesiredSize::new(12.0).into()).unwrap(),
            add_opts(),
        )
        .unwrap();

        Box::into_raw(Box::new(SharedGrid::init(CodepointResolver::new(c)).unwrap()))
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    // Not a Zig test: generic.zig has none. This renders a small terminal
    // end to end with the real OpenGL backend into an offscreen target on
    // a surfaceless EGL context and checks the pixels read back.
    use std::ptr::NonNull;

    use super::*;
    use crate::renderer::opengl::device::{self, test_context::egl_lock, Device};
    use crate::app::MailboxQueue as AppQueue;
    use crate::renderer::opengl::test_util::test_options_with_mailbox;
    use crate::renderer::opengl::{ExportedFrame, Memory, OpenGL};
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    fn save_png(path: &str, width: u32, height: u32, rgba: &[u8]) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(rgba).unwrap();
    }

    /// The RGBA pixel at (x, y) with y = 0 at the top of the screen.
    /// OpenGL reads rows bottom-up.
    fn pixel(m: &Memory, x: u32, y: u32) -> [u8; 4] {
        let row = m.height - 1 - y;
        let i = ((row * m.width + x) * 4) as usize;
        [m.pixels[i], m.pixels[i + 1], m.pixels[i + 2], m.pixels[i + 3]]
    }

    /// Count the pixels in a rectangle of cells of one row matching `pred`.
    fn count_in_cells(m: &Memory, size: Size, cols: std::ops::Range<u32>, row: u32, pred: impl Fn([u8; 4]) -> bool) -> usize {
        let mut n = 0;
        let y0 = size.padding.top + row * size.cell.height;
        let x0 = size.padding.left + cols.start * size.cell.width;
        let x1 = size.padding.left + cols.end * size.cell.width;
        for y in y0..y0 + size.cell.height {
            for x in x0..x1 {
                if pred(pixel(m, x, y)) {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn render_terminal_end_to_end_opengl() {
        let _guard = egl_lock();
        let mut device = Device::init(device::Options::default()).unwrap();
        let app = apprt::App::default();
        let queue = AppQueue::create();
        let mut opts = test_options_with_mailbox(&mut *device, &app, &*queue);

        // SAFETY: the test grid is leaked and lives forever.
        let metrics = unsafe { (*opts.font_grid).metrics };
        let cols: u16 = 24;
        let rows: u16 = 4;
        let cell = CellSize {
            width: metrics.cell_width,
            height: metrics.cell_height,
        };
        let padding = Padding {
            top: 4,
            bottom: 4,
            left: 4,
            right: 4,
        };
        let size = Size {
            screen: ScreenSize {
                width: cols as u32 * cell.width + padding.left + padding.right,
                height: rows as u32 * cell.height + padding.top + padding.bottom,
            },
            cell,
            padding,
        };
        opts.size = size;
        opts.config.background = Rgb { r: 0x10, g: 0x10, b: 0x18 };
        opts.config.foreground = Rgb { r: 0xFF, g: 0xFF, b: 0xFF };

        let mut renderer = Renderer::<OpenGL>::init(opts).unwrap();
        renderer.thread_enter(std::ptr::null_mut()).unwrap();
        // Read frames back through CPU memory rather than a DMABUF.
        renderer.set_presentation_health(Health::Unhealthy);
        renderer.set_screen_size(size);

        // A terminal with plain text, a red word, a black-on-green word
        // and the cursor after a prompt.
        let mut t = Terminal::init(TerminalOptions::new(cols, rows)).unwrap();
        {
            let mut s = t.vt_stream();
            s.next_slice(b"Hello, world!\r\n\x1b[31mred\x1b[0m \x1b[30;42mgreen\x1b[0m\r\n$ ");
            s.deinit();
        }
        let mut mutex = Mutex::new();
        let state = State::new(NonNull::from(&mut mutex), NonNull::from(&mut t));

        renderer.update_frame(&state, true).unwrap();

        // The CPU cells: one glyph per visible character, a cursor, and
        // backgrounds for the green cells.
        assert_eq!(cols, renderer.cells.size.columns);
        assert_eq!(rows, renderer.cells.size.rows);
        let row0 = &renderer.cells.fg_rows[1];
        assert_eq!("Hello,world!".len(), row0.len());
        assert!(row0.iter().all(|c| c.color == [0xFF, 0xFF, 0xFF, 0xFF]));
        let row1 = &renderer.cells.fg_rows[2];
        assert_eq!("redgreen".len(), row1.len());
        let cursor = renderer.cells.get_cursor_glyph().expect("cursor glyph");
        assert_eq!([2, 2], cursor.grid_pos);
        assert_eq!([2, 2], renderer.uniforms.cursor_pos);
        // "green" has an explicit (palette 2) background, the rest does not.
        assert_eq!(0xFF, renderer.cells.bg_cell(1, 4)[3]);
        assert_eq!(0, renderer.cells.bg_cell(1, 0)[3]);
        assert!(renderer.cells_rebuilt);

        renderer.draw_frame(true).unwrap();
        assert_eq!(Health::Healthy, renderer.health.load());
        assert!(!renderer.cells_rebuilt);

        let frame = renderer.take_frame().expect("exported frame");
        let ExportedFrame::Memory(m) = &frame else {
            panic!("expected a CPU memory frame");
        };
        assert_eq!(size.screen.width, m.width);
        assert_eq!(size.screen.height, m.height);

        // Save the frame so it can be looked at.
        if let Ok(path) = std::env::var("GHOSTTY_RENDER_SMOKE_PNG") {
            let mut top_down = Vec::with_capacity(m.pixels.len());
            for y in 0..m.height {
                for x in 0..m.width {
                    top_down.extend_from_slice(&pixel(m, x, y));
                }
            }
            save_png(&path, m.width, m.height, &top_down);
        }

        let is_bg = |p: [u8; 4]| p[0] <= 0x20 && p[1] <= 0x20 && p[2] <= 0x28;
        let is_bright = |p: [u8; 4]| p[0] > 0x80 && p[1] > 0x80 && p[2] > 0x80;

        // The padding and the empty cells are background colored and opaque.
        let p = pixel(m, 1, 1);
        assert!(is_bg(p), "padding pixel {p:?}");
        assert_eq!(0xFF, p[3]);
        assert_eq!(0, count_in_cells(m, size, 20..24, 0, |p| !is_bg(p)));

        // "Hello, world!" drew bright glyph pixels in every non-space cell.
        for x in (0..13).filter(|&x| x != 6) {
            let n = count_in_cells(m, size, x..x + 1, 0, is_bright);
            assert!(n > 0, "no glyph pixels in cell ({x}, 0)");
        }
        // The space between them has none.
        assert_eq!(0, count_in_cells(m, size, 6..7, 0, |p| !is_bg(p)));

        // "red" is drawn in palette red.
        let palette = renderer.terminal_state.colors.palette;
        let red = count_in_cells(m, size, 0..3, 1, |p| {
            p[0] as i32 > p[1] as i32 + 0x30 && p[0] as i32 > p[2] as i32 + 0x30
        });
        assert!(red > 0, "no red glyph pixels");

        // "green" has a palette green background with dark glyphs on it.
        let green = palette[2];
        let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 8;
        let green_bg = count_in_cells(m, size, 4..9, 1, |p| {
            near(p[0], green.r) && near(p[1], green.g) && near(p[2], green.b)
        });
        let cell_area = (5 * size.cell.width * size.cell.height) as usize;
        assert!(green_bg > cell_area / 2, "green background {green_bg}/{cell_area}");
        let dark_on_green = count_in_cells(m, size, 4..9, 1, |p| p[1] < 0x40);
        assert!(dark_on_green > 0, "no dark glyph pixels on green");

        // The block cursor at (2, 2) fills most of its cell with the
        // foreground color.
        let cursor_px = count_in_cells(m, size, 2..3, 2, is_bright);
        let one_cell = (size.cell.width * size.cell.height) as usize;
        assert!(cursor_px > one_cell / 2, "cursor pixels {cursor_px}/{one_cell}");

        // A second draw without changes is skipped.
        renderer.draw_frame(false).unwrap();
        assert!(renderer.take_frame().is_none());

        drop(frame);
        renderer.thread_exit();
        renderer.deinit();
        device.deinit();
    }
}
