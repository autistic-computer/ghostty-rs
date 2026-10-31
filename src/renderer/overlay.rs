//! Port of `src/renderer/Overlay.zig`.
//!
//! STATUS: ported
//!
//! The debug overlay that can be drawn on top of the terminal
//! during the rendering process.
//!
//! This is implemented by doing all the drawing on the CPU via z2d,
//! since the debug overlay isn't that common, z2d is pretty fast, and
//! it simplifies our implementation quite a bit by not relying on us
//! having a bunch of shaders that we have to write per-platform.
//!
//! Initialize the overlay, apply features with `applyFeatures`, then
//! get the resulting image with `pendingImage` to upload to the GPU.
//! This works in concert with `renderer.image.State` to simplify. Draw
//! it on the GPU as an image composited on top of the terminal output.
//!
//! Deviations:
//! * The Rust port has no z2d. The RGBA surface ([`Surface`]) and pixel
//!   types ([`Rgb`], [`Rgba`]) are defined here following z2d's
//!   `image_surface_rgba` / `pixel` (premultiplied RGBA, integer
//!   `src_over`). Paths are built and rasterized with the z2d-shaped
//!   layer in [`crate::font::sprite::canvas::z2d`], which produces an
//!   8-bit coverage mask that is then composited with the solid source
//!   pixel. z2d's 1px "hairline" stroke is a 1px-wide stroke here.
//! * `pending_image` takes `&mut self` since the returned image points
//!   at mutable pixel data.

use std::fmt;

use crate::font::sprite::canvas::z2d;
use crate::font::sprite::canvas::{Color as AlphaColor, DrawError};
use crate::renderer::size::{CellSize, Size};
use crate::terminal::page::{SemanticContent, SemanticPrompt};

use crate::terminal::render::RenderState;

use crate::renderer::image::{Pending, PixelFormat};

const LOG: &str = "renderer_overlay";

/// An RGB pixel (z2d `pixel.RGB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// z2d `RGB.asPixel`; RGB pixels are opaque.
    pub fn as_pixel(self) -> Rgba {
        Rgba {
            r: self.r,
            g: self.g,
            b: self.b,
            a: 255,
        }
    }
}

/// An RGBA pixel (z2d `pixel.RGBA`, also standing in for the `z2d.Pixel`
/// union since the overlay only uses RGBA). The surface stores
/// premultiplied values.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// z2d `RGBA.multiply`: premultiply the color channels by alpha.
    pub fn multiply(self) -> Rgba {
        let a = self.a as u16;
        Rgba {
            r: (self.r as u16 * a / 255) as u8,
            g: (self.g as u16 * a / 255) as u8,
            b: (self.b as u16 * a / 255) as u8,
            a: self.a,
        }
    }

    /// Integer `src_over` of a premultiplied source onto a premultiplied
    /// destination, with the source scaled by `coverage` (z2d compositor
    /// with a coverage mask).
    fn src_over(dst: Rgba, src: Rgba, coverage: u8) -> Rgba {
        let cov = coverage as u32;
        let s = |v: u8| v as u32 * cov / 255;
        let (sr, sg, sb, sa) = (s(src.r), s(src.g), s(src.b), s(src.a));
        let d = |dv: u8, sv: u32| (sv + dv as u32 * (255 - sa) / 255) as u8;
        Rgba {
            r: d(dst.r, sr),
            g: d(dst.g, sg),
            b: d(dst.b, sb),
            a: d(dst.a, sa),
        }
    }
}

/// An RGBA image surface (z2d `image_surface_rgba`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    width: i32,
    height: i32,
    /// Row-major pixels.
    pub buf: Vec<Rgba>,
}

impl Surface {
    /// z2d `Surface.initPixel`: a surface filled with `px`.
    pub fn init_pixel(px: Rgba, width: i32, height: i32) -> Result<Surface, InitError> {
        if width < 1 || height < 1 {
            return Err(InitError::InvalidDimensions);
        }
        let len = (width as usize)
            .checked_mul(height as usize)
            .ok_or(InitError::InvalidDimensions)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(len)
            .map_err(|_| InitError::OutOfMemory)?;
        buf.resize(len, px);
        Ok(Surface { width, height, buf })
    }

    pub fn get_width(&self) -> i32 {
        self.width
    }

    pub fn get_height(&self) -> i32 {
        self.height
    }

    /// z2d `Surface.paintPixel`: replace every pixel with `px`.
    pub fn paint_pixel(&mut self, px: Rgba) {
        self.buf.fill(px);
    }

    /// Composite a solid premultiplied pixel through an 8-bit coverage
    /// mask whose top-left is at `(dst_x, dst_y)`.
    fn composite_mask(&mut self, px: Rgba, mask: &z2d::Surface, dst_x: i32, dst_y: i32) {
        for my in 0..mask.get_height() {
            let y = dst_y + my;
            if y < 0 || y >= self.height {
                continue;
            }
            for mx in 0..mask.get_width() {
                let x = dst_x + mx;
                if x < 0 || x >= self.width {
                    continue;
                }
                let cov = mask.buf[(my * mask.get_width() + mx) as usize];
                if cov == 0 {
                    continue;
                }
                let d = &mut self.buf[(y * self.width + x) as usize];
                *d = Rgba::src_over(*d, px, cov);
            }
        }
    }
}

/// The colors we use for overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// light blue
    Hyperlink,
    /// orange/gold
    SemanticPrompt,
    /// cyan
    SemanticInput,
}

impl Color {
    pub fn rgb(self) -> Rgb {
        match self {
            Color::Hyperlink => Rgb {
                r: 180,
                g: 180,
                b: 255,
            },
            Color::SemanticPrompt => Rgb {
                r: 255,
                g: 200,
                b: 64,
            },
            Color::SemanticInput => Rgb {
                r: 64,
                g: 200,
                b: 255,
            },
        }
    }

    /// The fill color for rectangles.
    pub fn rect_fill(self) -> Rgba {
        self.alpha_pixel(96)
    }

    /// The border color for rectangles.
    pub fn rect_border(self) -> Rgba {
        self.alpha_pixel(200)
    }

    /// The raw RGB as a pixel.
    pub fn pixel(self) -> Rgba {
        self.rgb().as_pixel()
    }

    fn alpha_pixel(self, alpha: u8) -> Rgba {
        let mut rgba = self.pixel();
        rgba.a = alpha;
        rgba.multiply()
    }
}

/// The set of available features and their configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    HighlightHyperlinks,
    SemanticPrompts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    OutOfMemory,
    /// The terminal dimensions are invalid to support an overlay.
    /// Either too small or too big.
    InvalidDimensions,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitError {}

/// Errors from drawing a highlight rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawRectError {
    Overflow,
    Draw(DrawError),
}

impl fmt::Display for DrawRectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DrawRectError::Overflow => f.write_str("Overflow"),
            DrawRectError::Draw(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for DrawRectError {}

impl From<DrawError> for DrawRectError {
    fn from(e: DrawError) -> Self {
        DrawRectError::Draw(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// The surface we're drawing our overlay to.
    pub surface: Surface,

    /// Cell size information so we can map grid coordinates to pixels.
    pub cell_size: CellSize,
}

/// `std.math.cast(i32, a * b)` with the multiplication checked too.
fn mul_i32(a: usize, b: usize) -> Result<i32, DrawRectError> {
    let v = a.checked_mul(b).ok_or(DrawRectError::Overflow)?;
    i32::try_from(v).map_err(|_| DrawRectError::Overflow)
}

impl Overlay {
    /// Initialize a new, blank overlay.
    pub fn init(sz: Size) -> Result<Overlay, InitError> {
        // Our surface does NOT need to take into account padding because
        // we render the overlay using the image subsystem and shaders which
        // already take that into account.
        let term_size = sz.terminal();
        let sfc = Surface::init_pixel(
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            i32::try_from(term_size.width).map_err(|_| InitError::InvalidDimensions)?,
            i32::try_from(term_size.height).map_err(|_| InitError::InvalidDimensions)?,
        )?;

        Ok(Overlay {
            surface: sfc,
            cell_size: sz.cell,
        })
    }

    pub fn deinit(&mut self) {}

    /// Returns a pending image that can be used to copy, convert, upload, etc.
    pub fn pending_image(&mut self) -> Pending {
        Pending {
            width: self.surface.get_width() as u32,
            height: self.surface.get_height() as u32,
            pixel_format: PixelFormat::Rgba,
            data: self.surface.buf.as_mut_ptr().cast::<u8>(),
        }
    }

    /// Clear the overlay.
    pub fn reset(&mut self) {
        self.surface.paint_pixel(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        });
    }

    /// Apply the given features to this overlay. This will draw on top of
    /// any pre-existing content in the overlay.
    pub fn apply_features(&mut self, state: &RenderState, features: &[Feature]) {
        for f in features {
            match f {
                Feature::HighlightHyperlinks => self.highlight_hyperlinks(state),
                Feature::SemanticPrompts => self.highlight_semantic_prompts(state),
            }
        }
    }

    /// Add rectangles around contiguous hyperlinks in the render state.
    ///
    /// Note: this currently doesn't take into account unique hyperlink IDs
    /// because the render state doesn't contain this. This will be added
    /// later.
    fn highlight_hyperlinks(&mut self, state: &RenderState) {
        let border_color = Color::Hyperlink.rect_border();
        let fill_color = Color::Hyperlink.rect_fill();

        let row_raw = &state.row_data.raw;
        let row_cells = &state.row_data.cells;
        for (y, (row, cells)) in row_raw.iter().zip(row_cells.iter()).enumerate() {
            if !row.hyperlink() {
                continue;
            }

            let raw_cells = &cells.raw;

            let mut x: usize = 0;
            while x < raw_cells.len() {
                // Skip cells without hyperlinks
                if !raw_cells[x].hyperlink() {
                    x += 1;
                    continue;
                }

                // Found start of a hyperlink run
                let start_x = x;

                // Find end of contiguous hyperlink cells
                while x < raw_cells.len() && raw_cells[x].hyperlink() {
                    x += 1;
                }
                let end_x = x;

                if let Err(err) =
                    self.highlight_grid_rect(start_x, y, end_x - start_x, 1, border_color, fill_color)
                {
                    log::warn!("Error drawing hyperlink border: {err}");
                }
            }
        }
    }

    fn highlight_semantic_prompts(&mut self, state: &RenderState) {
        let row_raw = &state.row_data.raw;
        let row_cells = &state.row_data.cells;

        // Highlight the row-level semantic prompt bars. The prompts are easy
        // because they're part of the row metadata.
        {
            let prompt_border = Color::SemanticPrompt.rect_border();
            let prompt_fill = Color::SemanticPrompt.rect_fill();

            let mut y: usize = 0;
            while y < row_raw.len() {
                // If its not a semantic prompt row, skip it.
                if row_raw[y].semantic_prompt() == SemanticPrompt::None {
                    y += 1;
                    continue;
                }

                // Find the full length of the semantic prompt row by connecting
                // all continuations.
                let start_y = y;
                y += 1;
                while y < row_raw.len() && row_raw[y].semantic_prompt() == SemanticPrompt::PromptContinuation
                {
                    y += 1;
                }
                let end_y = y; // Exclusive

                let bar_width = 5usize.min(self.cell_size.width as usize);
                if let Err(err) = self.highlight_pixel_rect(
                    0,
                    start_y,
                    bar_width,
                    end_y - start_y,
                    prompt_border,
                    prompt_fill,
                ) {
                    log::warn!(target: LOG, "Error drawing semantic prompt bar: {err}");
                }
            }
        }

        // Highlight contiguous semantic cells within rows.
        for (y, cells) in row_cells.iter().enumerate() {
            let raw_cells = &cells.raw;

            let mut x: usize = 0;
            while x < raw_cells.len() {
                let cell = raw_cells[x];
                let content = cell.semantic_content();
                let start_x = x;

                // We skip output because its just the rest of the non-prompt
                // parts and it makes the overlay too noisy.
                if content == SemanticContent::Output {
                    x += 1;
                    continue;
                }

                // Find the end of this content.
                x += 1;
                while x < raw_cells.len() {
                    let next = raw_cells[x];
                    if next.semantic_content() != content {
                        break;
                    }
                    x += 1;
                }

                let color = match content {
                    SemanticContent::Prompt => Color::SemanticPrompt,
                    SemanticContent::Input => Color::SemanticInput,
                    SemanticContent::Output => unreachable!(),
                };

                if let Err(err) = self.highlight_grid_rect(
                    start_x,
                    y,
                    x - start_x,
                    1,
                    color.rect_border(),
                    color.rect_fill(),
                ) {
                    log::warn!(target: LOG, "Error drawing semantic content highlight: {err}");
                }
            }
        }
    }

    /// Creates a rectangle for highlighting a grid region. x/y/width/height
    /// are all in grid cells.
    fn highlight_grid_rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        border_color: Rgba,
        fill_color: Rgba,
    ) -> Result<(), DrawRectError> {
        // All math below uses checked arithmetic to avoid overflows. The
        // inputs aren't trusted and the path this is in isn't hot enough
        // to wrarrant unsafe optimizations.

        // Calculate our width/height in pixels.
        let px_width = mul_i32(width, self.cell_size.width as usize)?;
        let px_height = mul_i32(height, self.cell_size.height as usize)?;

        // Calculate pixel coordinates
        let start_x = mul_i32(x, self.cell_size.width as usize)? as f64;
        let start_y = mul_i32(y, self.cell_size.height as usize)? as f64;

        self.draw_rect(start_x, start_y, px_width, px_height, border_color, fill_color)
    }

    /// Creates a rectangle for highlighting a region. x/y are grid cells and
    /// width/height are pixels.
    fn highlight_pixel_rect(
        &mut self,
        x: usize,
        y: usize,
        width_px: usize,
        height: usize,
        border_color: Rgba,
        fill_color: Rgba,
    ) -> Result<(), DrawRectError> {
        let px_width = i32::try_from(width_px).map_err(|_| DrawRectError::Overflow)?;
        let px_height = mul_i32(height, self.cell_size.height as usize)?;

        let start_x = mul_i32(x, self.cell_size.width as usize)? as f64;
        let start_y = mul_i32(y, self.cell_size.height as usize)? as f64;

        self.draw_rect(start_x, start_y, px_width, px_height, border_color, fill_color)
    }

    /// The shared drawing part of `highlightGridRect`/`highlightPixelRect`:
    /// fill and then stroke (1px) the rectangle path.
    fn draw_rect(
        &mut self,
        start_x: f64,
        start_y: f64,
        px_width: i32,
        px_height: i32,
        border_color: Rgba,
        fill_color: Rgba,
    ) -> Result<(), DrawRectError> {
        let end_x: f64 = start_x + px_width as f64;
        let end_y: f64 = start_y + px_height as f64;

        // The coverage mask covers the rectangle plus a 1px margin for the
        // border stroke; its origin is at (start - 1) on the surface.
        let mask_w = px_width.checked_add(3).ok_or(DrawRectError::Overflow)?;
        let mask_h = px_height.checked_add(3).ok_or(DrawRectError::Overflow)?;
        let origin_x = start_x as i32 - 1;
        let origin_y = start_y as i32 - 1;
        let mut mask = z2d::Surface::init(mask_w, mask_h);

        let draw = |mask: &mut z2d::Surface, stroke: bool| -> Result<(), DrawError> {
            // Grab our context to draw
            let mut ctx = z2d::Context::init(mask);

            // Don't need AA because we use sharp edges
            ctx.anti_aliasing_mode = z2d::AntiAliasMode::None;
            ctx.set_transformation(z2d::Transformation {
                tx: -(origin_x as f64),
                ty: -(origin_y as f64),
                ..z2d::Transformation::IDENTITY
            });
            ctx.set_source(AlphaColor(0xFF));

            // Draw rectangle path
            ctx.move_to(start_x, start_y)?;
            ctx.line_to(end_x, start_y)?;
            ctx.line_to(end_x, end_y)?;
            ctx.line_to(start_x, end_y)?;
            ctx.close_path()?;

            if stroke {
                // Border. Zig uses a hairline (1px) stroke.
                ctx.set_line_width(1.0);
                ctx.stroke()
            } else {
                // Fill
                ctx.fill()
            }
        };

        // Fill
        draw(&mut mask, false)?;
        self.surface
            .composite_mask(fill_color, &mask, origin_x, origin_y);

        // Border
        mask.buf.fill(0);
        draw(&mut mask, true)?;
        self.surface
            .composite_mask(border_color, &mask, origin_x, origin_y);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::size::{GridSize, Padding, ScreenSize};
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    /// A render state of a 10x2 terminal (the grid of [`size`]) after
    /// writing `input`.
    fn render_state(input: &[u8]) -> RenderState {
        let mut t = Terminal::init(TerminalOptions::new(10, 2)).unwrap();
        {
            let mut s = t.vt_stream();
            s.next_slice(input);
        }
        let mut state = RenderState::empty();
        state.update(&mut t);
        state
    }

    // Not Zig tests: Overlay.zig has none. These check the drawing basics.

    fn size() -> Size {
        Size {
            screen: ScreenSize {
                width: 100,
                height: 40,
            },
            cell: CellSize {
                width: 10,
                height: 20,
            },
            padding: Padding::default(),
        }
    }

    #[test]
    fn init_reset_pending() {
        let mut o = Overlay::init(size()).unwrap();
        assert_eq!(100, o.surface.get_width());
        assert_eq!(40, o.surface.get_height());
        let p = o.pending_image();
        assert_eq!(PixelFormat::Rgba, p.pixel_format);
        assert_eq!(100 * 40 * 4, p.len());
        o.reset();
        assert!(o.surface.buf.iter().all(|px| *px == Rgba::default()));
        let _ = GridSize::default();
    }

    #[test]
    fn init_invalid_dimensions() {
        let mut sz = size();
        sz.screen.width = 0;
        assert_eq!(Err(InitError::InvalidDimensions), Overlay::init(sz));
    }

    #[test]
    fn color_premultiplied() {
        let fill = Color::Hyperlink.rect_fill();
        assert_eq!(96, fill.a);
        assert_eq!((180u16 * 96 / 255) as u8, fill.r);
        assert_eq!((255u16 * 96 / 255) as u8, fill.b);
    }

    #[test]
    fn highlight_hyperlinks_draws_in_rect() {
        let mut o = Overlay::init(size()).unwrap();
        // A hyperlink over cells 2..4 of row 0.
        let state = render_state(b"\x1b[1;3H\x1b]8;;http://example.com\x1b\\AB\x1b]8;;\x1b\\");
        assert!(state.row_data.raw[0].hyperlink());
        o.apply_features(&state, &[Feature::HighlightHyperlinks]);

        let px = |x: i32, y: i32| o.surface.buf[(y * 100 + x) as usize];
        // Inside the rect (cells 2..4 => px 20..40, rows 0..20).
        assert_ne!(0, px(30, 10).a);
        // Outside the rect.
        assert_eq!(0, px(10, 10).a);
        assert_eq!(0, px(50, 10).a);
        assert_eq!(0, px(30, 30).a);
    }

    #[test]
    fn semantic_prompt_bar() {
        let mut o = Overlay::init(size()).unwrap();
        let mut state = render_state(b"");
        state.row_data.raw[0].set_semantic_prompt(SemanticPrompt::Prompt);
        o.apply_features(&state, &[Feature::SemanticPrompts]);
        let px = |x: i32, y: i32| o.surface.buf[(y * 100 + x) as usize];
        assert_ne!(0, px(2, 10).a);
        assert_eq!(0, px(8, 10).a);
        assert_eq!(0, px(2, 30).a);
    }
}
