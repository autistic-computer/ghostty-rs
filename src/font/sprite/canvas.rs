//! Port of `src/font/sprite/canvas.zig`.
//!
//! STATUS: ported
//!
//! This exposes primitives to draw 2D graphics and export the graphic to
//! a font atlas.
//!
//! The Zig code draws with the `z2d` vector graphics library. This port
//! replaces it with a small z2d-shaped layer defined in this file
//! ([`Surface`], [`Path`], [`Context`], [`FillOptions`], [`StrokeOptions`],
//! [`Transformation`]) whose path building, curve flattening, arc
//! approximation, path offsetting and integer `src_over` compositing follow
//! z2d, and whose scanline rasterization (anti-aliased coverage) and
//! stroking are done by `tiny-skia`. Anti-aliased edges can therefore
//! differ slightly from z2d's 4x multisampling.

use crate::font::atlas::{self, Atlas};

pub use self::z2d::{
    AntiAliasMode, CapMode, Context, FillOptions, FillRule, JoinMode, Operator, Path, PathNode,
    StrokeOptions, Surface, Transformation,
};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Line<T> {
    pub p0: Point<T>,
    pub p1: Point<T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Box<T> {
    pub p0: Point<T>,
    pub p1: Point<T>,
}

impl<T> Box<T>
where
    T: Copy + PartialOrd + std::ops::Sub<Output = T>,
{
    pub fn rect(self) -> Rect<T> {
        let min = |a: T, b: T| if b < a { b } else { a };
        let max = |a: T, b: T| if b > a { b } else { a };
        let tl_x = min(self.p0.x, self.p1.x);
        let tl_y = min(self.p0.y, self.p1.y);
        let br_x = max(self.p0.x, self.p1.x);
        let br_y = max(self.p0.y, self.p1.y);

        Rect {
            x: tl_x,
            y: tl_y,
            width: br_x - tl_x,
            height: br_y - tl_y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect<T> {
    pub x: T,
    pub y: T,
    pub width: T,
    pub height: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Triangle<T> {
    pub p0: Point<T>,
    pub p1: Point<T>,
    pub p2: Point<T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quad<T> {
    pub p0: Point<T>,
    pub p1: Point<T>,
    pub p2: Point<T>,
    pub p3: Point<T>,
}

/// We only use alpha-channel so a pixel can only be "on" or "off".
///
/// (Zig non-exhaustive `enum(u8) { on = 255, off = 0, _ }`: any other
/// alpha value is allowed too.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u8);

impl Color {
    pub const ON: Color = Color(255);
    pub const OFF: Color = Color(0);
}

/// Errors from drawing operations (the z2d path/painter errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawError {
    /// Out of memory.
    OutOfMemory,
    /// The supplied path (and any sub-paths) have not been explicitly
    /// closed, which is required by the fill operation.
    PathNotClosed,
    /// An operation that requires a current point was called without one.
    NoCurrentPoint,
    /// Something went wrong while doing math.
    MathError,
}

impl std::fmt::Display for DrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for DrawError {}

impl From<atlas::AllocError> for DrawError {
    fn from(_: atlas::AllocError) -> DrawError {
        DrawError::OutOfMemory
    }
}

/// This is a managed struct (in Zig it keeps a reference to the allocator
/// that is used to initialize it).
#[derive(Debug, Clone)]
pub struct Canvas {
    /// The underlying (alpha8) surface.
    pub sfc: Surface,

    pub padding_x: u32,
    pub padding_y: u32,

    pub clip_top: u32,
    pub clip_left: u32,
    pub clip_right: u32,
    pub clip_bottom: u32,
}

impl Canvas {
    pub fn init(width: u32, height: u32, padding_x: u32, padding_y: u32) -> Canvas {
        // Create the surface we'll be using.
        // We add padding to both sides (hence `2 *`)
        let sfc = Surface::init(
            (width + 2 * padding_x) as i32,
            (height + 2 * padding_y) as i32,
        );

        Canvas {
            sfc,
            padding_x,
            padding_y,
            clip_top: 0,
            clip_left: 0,
            clip_right: 0,
            clip_bottom: 0,
        }
    }

    /// Write the data in this drawing to the atlas.
    pub fn write_atlas(&mut self, atlas: &mut Atlas) -> Result<atlas::Region, atlas::Error> {
        debug_assert!(atlas.format == atlas::Format::Grayscale);

        self.trim();

        let sfc_width = self.sfc.get_width() as u32;
        let sfc_height = self.sfc.get_height() as u32;

        // Subtract our clip margins from the
        // width and height to get region size.
        let region_width = sfc_width
            .saturating_sub(self.clip_left)
            .saturating_sub(self.clip_right);
        let region_height = sfc_height
            .saturating_sub(self.clip_top)
            .saturating_sub(self.clip_bottom);

        // Allocate our texture atlas region
        let region = atlas.reserve(region_width, region_height)?;

        if region.width > 0 && region.height > 0 {
            // Write the glyph information into the atlas
            debug_assert!(region.width == region_width);
            debug_assert!(region.height == region_height);
            atlas.set_from_larger(
                region,
                &self.sfc.buf,
                sfc_width,
                self.clip_left,
                self.clip_top,
            );
        }

        Ok(region)
    }

    // Adjust clip boundaries to trim off any fully transparent rows or columns.
    // This circumvents abstractions from z2d so that it can be performant.
    fn trim(&mut self) {
        let width = self.sfc.get_width() as u32;
        let height = self.sfc.get_height() as u32;

        let buf = &self.sfc.buf;

        'top: while self.clip_top < height - self.clip_bottom {
            let y = self.clip_top;
            let x0 = self.clip_left;
            let x1 = width - self.clip_right;
            for &v in &buf[(y * width) as usize..][x0 as usize..x1 as usize] {
                if v != 0 {
                    break 'top;
                }
            }
            self.clip_top += 1;
        }

        'bottom: while self.clip_bottom < height - self.clip_top {
            let y = (height - self.clip_bottom).saturating_sub(1);
            let x0 = self.clip_left;
            let x1 = width - self.clip_right;
            for &v in &buf[(y * width) as usize..][x0 as usize..x1 as usize] {
                if v != 0 {
                    break 'bottom;
                }
            }
            self.clip_bottom += 1;
        }

        'left: while self.clip_left < width - self.clip_right {
            let x = self.clip_left;
            let y0 = self.clip_top;
            let y1 = height - self.clip_bottom;
            for y in y0..y1 {
                if buf[(y * width + x) as usize] != 0 {
                    break 'left;
                }
            }
            self.clip_left += 1;
        }

        'right: while self.clip_right < width - self.clip_left {
            let x = (width - self.clip_right).saturating_sub(1);
            let y0 = self.clip_top;
            let y1 = height - self.clip_bottom;
            for y in y0..y1 {
                if buf[(y * width + x) as usize] != 0 {
                    break 'right;
                }
            }
            self.clip_right += 1;
        }
    }

    /// Only really useful for test purposes, since the clipping region is
    /// automatically excluded when writing to an atlas with `writeAtlas`.
    pub fn clear_clipping_regions(&mut self) {
        let width = self.sfc.get_width() as usize;
        let height = self.sfc.get_height() as usize;
        let clip_left = self.clip_left as usize;
        let clip_right = self.clip_right as usize;
        let clip_top = self.clip_top as usize;
        let clip_bottom = self.clip_bottom as usize;
        let buf = &mut self.sfc.buf;

        for y in 0..height {
            for x in 0..clip_left {
                buf[y * width + x] = 0;
            }
        }

        for y in 0..height {
            for x in width - clip_right..width {
                buf[y * width + x] = 0;
            }
        }

        for y in 0..clip_top {
            for x in 0..width {
                buf[y * width + x] = 0;
            }
        }

        for y in height - clip_bottom..height {
            for x in 0..width {
                buf[y * width + x] = 0;
            }
        }
    }

    /// Return a transformation representing the translation for our padding.
    pub fn transformation(&self) -> Transformation {
        Transformation {
            ax: 1.0,
            by: 0.0,
            cx: 0.0,
            dy: 1.0,
            tx: self.padding_x as f64,
            ty: self.padding_y as f64,
        }
    }

    /// Acquires a drawing context (the Zig version requires a matching
    /// `deinit`; here the context is simply dropped).
    pub fn get_context(&mut self) -> Context<'_> {
        let transformation = self.transformation();
        let mut ctx = Context::init(&mut self.sfc);
        // Offset by our padding to keep
        // coordinates relative to the cell.
        ctx.set_transformation(transformation);
        ctx
    }

    /// Draw and fill a single pixel
    pub fn pixel(&mut self, x: i32, y: i32, color: Color) {
        self.sfc.put_pixel(
            x + self.padding_x as i32,
            y + self.padding_y as i32,
            color.0,
        );
    }

    /// Draw and fill a rectangle. This is the main primitive for drawing
    /// lines as well (which are just generally skinny rectangles...)
    pub fn rect(&mut self, v: Rect<i32>, color: Color) {
        let mut y = v.y;
        while y < v.y + v.height {
            let mut x = v.x;
            while x < v.x + v.width {
                self.pixel(x, y, color);
                x += 1;
            }
            y += 1;
        }
    }

    /// Convenience wrapper for `Canvas.rect`
    pub fn r#box(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        self.rect(
            Box {
                p0: Point { x: x0, y: y0 },
                p1: Point { x: x1, y: y1 },
            }
            .rect(),
            color,
        );
    }

    /// Draw and fill a quad.
    pub fn quad(&mut self, q: Quad<f64>, color: Color) -> Result<(), DrawError> {
        let mut path = self.static_path(6); // nodes.len = 0
        path.move_to(q.p0.x, q.p0.y); // +1, nodes.len = 1
        path.line_to(q.p1.x, q.p1.y); // +1, nodes.len = 2
        path.line_to(q.p2.x, q.p2.y); // +1, nodes.len = 3
        path.line_to(q.p3.x, q.p3.y); // +1, nodes.len = 4
        path.close(); // +2, nodes.len = 6
        self.fill_path(&path, FillOptions::default(), color)
    }

    /// Draw and fill a triangle.
    pub fn triangle(&mut self, t: Triangle<f64>, color: Color) -> Result<(), DrawError> {
        let mut path = self.static_path(5); // nodes.len = 0
        path.move_to(t.p0.x, t.p0.y); // +1, nodes.len = 1
        path.line_to(t.p1.x, t.p1.y); // +1, nodes.len = 2
        path.line_to(t.p2.x, t.p2.y); // +1, nodes.len = 3
        path.close(); // +2, nodes.len = 5
        self.fill_path(&path, FillOptions::default(), color)
    }

    /// Stroke a line.
    pub fn line(&mut self, l: Line<f64>, thickness: f64, color: Color) -> Result<(), DrawError> {
        let mut path = self.static_path(2); // nodes.len = 0
        path.move_to(l.p0.x, l.p0.y); // +1, nodes.len = 1
        path.line_to(l.p1.x, l.p1.y); // +1, nodes.len = 2
        self.stroke_path(
            &path,
            StrokeOptions {
                line_cap_mode: CapMode::Butt,
                line_width: thickness,
                ..StrokeOptions::default()
            },
            color,
        )
    }

    /// Create a path with room for `len` nodes and initialize it.
    /// Use this function instead of making the path manually since
    /// it ensures that the transform is applied.
    ///
    /// (Zig returns a fixed-capacity `z2d.StaticPath(len)`; here the
    /// capacity is only a hint.)
    #[inline]
    pub fn static_path(&self, len: usize) -> Path {
        let mut path = Path::with_capacity(len);
        path.transformation = self.transformation();
        path
    }

    /// Stroke a path.
    pub fn stroke_path(
        &mut self,
        path: &Path,
        opts: StrokeOptions,
        color: Color,
    ) -> Result<(), DrawError> {
        z2d::painter::stroke(&mut self.sfc, color.0, &path.nodes, &opts)
    }

    /// Do an inner stroke using a 1/2 stroke width inset of the supplied
    /// `path`.
    pub fn inner_stroke_path(
        &mut self,
        path: &Path,
        opts: StrokeOptions,
        color: Color,
    ) -> Result<(), DrawError> {
        let inset_path = path.offset(-opts.line_width / 2.0)?;
        self.stroke_path(&inset_path, opts, color)
    }

    /// Fill a path.
    pub fn fill_path(
        &mut self,
        path: &Path,
        opts: FillOptions,
        color: Color,
    ) -> Result<(), DrawError> {
        z2d::painter::fill(&mut self.sfc, color.0, &path.nodes, &opts)
    }

    /// Invert all pixels on the canvas.
    pub fn invert(&mut self) {
        for v in self.sfc.buf.iter_mut() {
            *v = 255 - *v;
        }
    }

    /// Mirror the canvas horizontally.
    pub fn flip_horizontal(&mut self) -> Result<(), DrawError> {
        let clone = self.sfc.buf.clone();
        let width = self.sfc.get_width() as usize;
        let height = self.sfc.get_height() as usize;
        let buf = &mut self.sfc.buf;
        for y in 0..height {
            for x in 0..width {
                buf[y * width + x] = clone[y * width + width - x - 1];
            }
        }
        std::mem::swap(&mut self.clip_left, &mut self.clip_right);
        Ok(())
    }

    /// Mirror the canvas vertically.
    pub fn flip_vertical(&mut self) -> Result<(), DrawError> {
        let clone = self.sfc.buf.clone();
        let width = self.sfc.get_width() as usize;
        let height = self.sfc.get_height() as usize;
        let buf = &mut self.sfc.buf;
        for y in 0..height {
            for x in 0..width {
                buf[y * width + x] = clone[(height - y - 1) * width + x];
            }
        }
        std::mem::swap(&mut self.clip_top, &mut self.clip_bottom);
        Ok(())
    }
}

/// Replacement for the parts of the `z2d` library used by the sprite
/// renderer.
// NOTE(port): z2d is a third-party Zig package with no Rust port, so this
// keeps its API shape and delegates rasterization to tiny-skia.
pub mod z2d {
    use super::DrawError;

    /// A 2D affine transformation (z2d `Transformation`).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Transformation {
        pub ax: f64,
        pub by: f64,
        pub cx: f64,
        pub dy: f64,
        pub tx: f64,
        pub ty: f64,
    }

    impl Transformation {
        pub const IDENTITY: Transformation = Transformation {
            ax: 1.0,
            by: 0.0,
            cx: 0.0,
            dy: 1.0,
            tx: 0.0,
            ty: 0.0,
        };

        /// Apply the transformation to a point.
        #[inline]
        pub fn apply(&self, x: f64, y: f64) -> Point {
            Point {
                x: self.ax * x + self.cx * y + self.tx,
                y: self.by * x + self.dy * y + self.ty,
            }
        }
    }

    impl Default for Transformation {
        fn default() -> Self {
            Transformation::IDENTITY
        }
    }

    /// A point in device space.
    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct Point {
        pub x: f64,
        pub y: f64,
    }

    /// A path node (z2d `PathNode`); points are in device space.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum PathNode {
        MoveTo(Point),
        LineTo(Point),
        CurveTo(Point, Point, Point),
        ClosePath,
    }

    /// The default tolerance for approximating curves and arcs.
    pub const DEFAULT_TOLERANCE: f64 = 0.1;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum AntiAliasMode {
        /// Do not use anti-aliasing.
        None,
        /// Anti-aliased rasterization.
        #[default]
        Default,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum FillRule {
        #[default]
        NonZero,
        EvenOdd,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum CapMode {
        #[default]
        Butt,
        Round,
        Square,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum JoinMode {
        #[default]
        Miter,
        Round,
        Bevel,
    }

    /// Compositing operators (the subset used here).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Operator {
        Clear,
        Src,
        #[default]
        SrcOver,
    }

    /// Options for [`painter::fill`] (z2d `painter.FillOptions`).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct FillOptions {
        /// The anti-aliasing mode to use with the fill operation.
        pub anti_aliasing_mode: AntiAliasMode,
        /// The fill rule to use during the fill operation.
        pub fill_rule: FillRule,
        /// The operator to use for compositing.
        pub operator: Operator,
        /// The maximum error tolerance used for approximating curves and arcs.
        pub tolerance: f64,
    }

    impl Default for FillOptions {
        fn default() -> Self {
            FillOptions {
                anti_aliasing_mode: AntiAliasMode::Default,
                fill_rule: FillRule::NonZero,
                operator: Operator::SrcOver,
                tolerance: DEFAULT_TOLERANCE,
            }
        }
    }

    /// Options for [`painter::stroke`] (z2d `painter.StrokeOptions`).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct StrokeOptions {
        pub anti_aliasing_mode: AntiAliasMode,
        pub line_cap_mode: CapMode,
        pub line_join_mode: JoinMode,
        pub line_width: f64,
        pub miter_limit: f64,
        pub operator: Operator,
        pub tolerance: f64,
    }

    impl Default for StrokeOptions {
        fn default() -> Self {
            StrokeOptions {
                anti_aliasing_mode: AntiAliasMode::Default,
                line_cap_mode: CapMode::Butt,
                line_join_mode: JoinMode::Miter,
                line_width: 2.0,
                miter_limit: 10.0,
                operator: Operator::SrcOver,
                tolerance: DEFAULT_TOLERANCE,
            }
        }
    }

    /// An 8-bit alpha-only image surface (z2d `image_surface_alpha8`).
    #[derive(Debug, Clone, PartialEq)]
    pub struct Surface {
        width: i32,
        height: i32,
        /// Row-major alpha values.
        pub buf: Vec<u8>,
    }

    impl Surface {
        /// Create a transparent surface.
        pub fn init(width: i32, height: i32) -> Surface {
            Surface {
                width,
                height,
                buf: vec![0; (width.max(0) as usize) * (height.max(0) as usize)],
            }
        }

        pub fn get_width(&self) -> i32 {
            self.width
        }

        pub fn get_height(&self) -> i32 {
            self.height
        }

        /// Set a pixel; out of bounds coordinates are ignored.
        pub fn put_pixel(&mut self, x: i32, y: i32, a: u8) {
            if x < 0 || y < 0 || x >= self.width || y >= self.height {
                return;
            }
            self.buf[(self.width * y + x) as usize] = a;
        }

        /// Get a pixel, or `None` when out of bounds.
        pub fn get_pixel(&self, x: i32, y: i32) -> Option<u8> {
            if x < 0 || y < 0 || x >= self.width || y >= self.height {
                return None;
            }
            Some(self.buf[(self.width * y + x) as usize])
        }

        /// Composite `src` onto this surface with its top-left at
        /// `(dst_x, dst_y)`.
        pub fn composite(&mut self, src: &Surface, operator: Operator, dst_x: i32, dst_y: i32) {
            for sy in 0..src.height {
                let y = dst_y + sy;
                if y < 0 || y >= self.height {
                    continue;
                }
                for sx in 0..src.width {
                    let x = dst_x + sx;
                    if x < 0 || x >= self.width {
                        continue;
                    }
                    let s = src.buf[(sy * src.width + sx) as usize];
                    let d = &mut self.buf[(y * self.width + x) as usize];
                    *d = composite_px(operator, *d, s);
                }
            }
        }
    }

    /// Integer-precision alpha compositing (z2d `IntegerOps`).
    #[inline]
    pub fn composite_px(op: Operator, dst: u8, src: u8) -> u8 {
        let d = dst as u32;
        let s = src as u32;
        match op {
            Operator::Clear => 0,
            Operator::Src => src,
            Operator::SrcOver => (s + d - (s * d) / 255) as u8,
        }
    }

    /// A path (z2d `Path`). Points are transformed by `transformation`
    /// when they are added.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Path {
        pub nodes: Vec<PathNode>,
        pub transformation: Transformation,
        pub tolerance: f64,
        /// Current point, in device space.
        current_point: Option<Point>,
        /// Start of the current sub-path, in device space.
        initial_point: Option<Point>,
    }

    impl Default for Path {
        fn default() -> Self {
            Path::with_capacity(0)
        }
    }

    impl Path {
        pub fn with_capacity(len: usize) -> Path {
            Path {
                nodes: Vec::with_capacity(len),
                transformation: Transformation::IDENTITY,
                tolerance: DEFAULT_TOLERANCE,
                current_point: None,
                initial_point: None,
            }
        }

        /// Starts a new sub-path at `(x, y)`.
        pub fn move_to(&mut self, x: f64, y: f64) {
            let point = self.transformation.apply(x, y);
            self.nodes.push(PathNode::MoveTo(point));
            self.current_point = Some(point);
            self.initial_point = Some(point);
        }

        /// Draws a line to `(x, y)`. Without a current point this is a
        /// `move_to`.
        pub fn line_to(&mut self, x: f64, y: f64) {
            if self.current_point.is_none() {
                return self.move_to(x, y);
            }
            let point = self.transformation.apply(x, y);
            self.nodes.push(PathNode::LineTo(point));
            self.current_point = Some(point);
        }

        /// Draws a cubic Bézier curve. Without a current point the curve
        /// starts at `(x1, y1)`.
        pub fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
            if self.current_point.is_none() {
                self.move_to(x1, y1);
            }
            let p1 = self.transformation.apply(x1, y1);
            let p2 = self.transformation.apply(x2, y2);
            let p3 = self.transformation.apply(x3, y3);
            self.nodes.push(PathNode::CurveTo(p1, p2, p3));
            self.current_point = Some(p3);
        }

        /// Closes the current sub-path. This adds a `close_path` node and
        /// an implicit `move_to` back to the start of the sub-path.
        pub fn close(&mut self) {
            let Some(initial) = self.initial_point else {
                return;
            };
            if self.current_point.is_none() {
                return;
            }
            self.nodes.push(PathNode::ClosePath);
            self.nodes.push(PathNode::MoveTo(initial));
            self.current_point = Some(initial);
        }

        /// Adds a circular arc of the given radius to the path, centered at
        /// `(xc, yc)`, from `angle1` increasing to `angle2` (radians). If
        /// there is a current point, a line is drawn to the arc start.
        pub fn arc(&mut self, xc: f64, yc: f64, radius: f64, angle1: f64, angle2: f64) {
            let mut effective_angle2 = angle2;
            while effective_angle2 < angle1 {
                effective_angle2 += std::f64::consts::PI * 2.0;
            }
            arc::arc_in_direction(
                self,
                xc,
                yc,
                radius,
                angle1,
                effective_angle2,
                self.tolerance.max(0.001),
            );
        }

        /// Returns true if every sub-path that draws anything is closed.
        pub fn is_closed(&self) -> bool {
            let mut open = false;
            for node in &self.nodes {
                match node {
                    PathNode::MoveTo(_) => {
                        if open {
                            return false;
                        }
                    }
                    PathNode::LineTo(_) | PathNode::CurveTo(..) => open = true,
                    PathNode::ClosePath => open = false,
                }
            }
            !open
        }

        /// Offsets (insets for negative values, outsets for positive) the
        /// path by `value`. Curves are flattened first.
        pub fn offset(&self, value: f64) -> Result<Path, DrawError> {
            let nodes = offset::run(&self.nodes, self.tolerance, value)?;
            let mut result = Path {
                nodes,
                transformation: self.transformation,
                tolerance: self.tolerance,
                current_point: None,
                initial_point: None,
            };
            // fixState
            for node in &result.nodes {
                match *node {
                    PathNode::MoveTo(p) => {
                        result.current_point = Some(p);
                        result.initial_point = Some(p);
                    }
                    PathNode::LineTo(p) | PathNode::CurveTo(_, _, p) => {
                        result.current_point = Some(p)
                    }
                    PathNode::ClosePath => result.current_point = result.initial_point,
                }
            }
            Ok(result)
        }
    }

    /// Port of z2d's `internal/arc.zig` (forward arcs only).
    mod arc {
        use super::Path;
        use std::f64::consts::PI;

        const MAX_FULL_CIRCLES: f64 = 65536.0;

        fn arc_error_normalized(angle: f64) -> f64 {
            2.0 / 27.0 * (angle / 4.0).sin().powi(6) / (angle / 4.0).cos().powi(2)
        }

        fn arc_max_angle_for_tolerance_normalized(tolerance: f64) -> f64 {
            const TABLE: [(f64, f64); 11] = [
                (PI / 1.0, 0.0185185185185185036127),
                (PI / 2.0, 0.000272567143730179811158),
                (PI / 3.0, 2.38647043651461047433e-05),
                (PI / 4.0, 4.2455377443222443279e-06),
                (PI / 5.0, 1.11281001494389081528e-06),
                (PI / 6.0, 3.72662000942734705475e-07),
                (PI / 7.0, 1.47783685574284411325e-07),
                (PI / 8.0, 6.63240432022601149057e-08),
                (PI / 9.0, 3.2715520137536980553e-08),
                (PI / 10.0, 1.73863223499021216974e-08),
                (PI / 11.0, 9.81410988043554039085e-09),
            ];

            const MAX_SEGMENTS: usize = 1000;

            for (angle, err) in TABLE {
                if err < tolerance {
                    return angle;
                }
            }

            let mut angle = 0.0;
            for i in TABLE.len()..MAX_SEGMENTS {
                angle = PI / i as f64;
                let err = arc_error_normalized(angle);
                if err <= tolerance {
                    break;
                }
            }

            angle
        }

        fn arc_segments_needed(angle: f64, radius: f64, tolerance: f64) -> i32 {
            // Our transformations are always translations (unity scale), so
            // the major axis of the transformed circle is the radius.
            let major_axis = radius;
            let max_angle = arc_max_angle_for_tolerance_normalized(tolerance / major_axis);

            (angle.abs() / max_angle).ceil() as i32
        }

        fn arc_segment(path: &mut Path, xc: f64, yc: f64, radius: f64, angle_a: f64, angle_b: f64) {
            let r_sin_a = radius * angle_a.sin();
            let r_cos_a = radius * angle_a.cos();
            let r_sin_b = radius * angle_b.sin();
            let r_cos_b = radius * angle_b.cos();

            let h = 4.0 / 3.0 * ((angle_b - angle_a) / 4.0).tan();

            path.curve_to(
                xc + r_cos_a - h * r_sin_a,
                yc + r_sin_a + h * r_cos_a,
                xc + r_cos_b + h * r_sin_b,
                yc + r_sin_b - h * r_cos_b,
                xc + r_cos_b,
                yc + r_sin_b,
            );
        }

        pub(super) fn arc_in_direction(
            path: &mut Path,
            xc: f64,
            yc: f64,
            radius: f64,
            angle_min: f64,
            angle_max: f64,
            tolerance: f64,
        ) {
            let mut amin = angle_min;
            let mut amax = angle_max;

            if !amax.is_finite() || !amin.is_finite() {
                return;
            }

            debug_assert!(amax >= amin);

            if amax - amin > 2.0 * PI * MAX_FULL_CIRCLES {
                amax = (amax - amin).rem_euclid(2.0 * PI);
                amin = amin.rem_euclid(2.0 * PI);
                amax += amin + 2.0 * PI * MAX_FULL_CIRCLES;
            }

            if amax - amin > PI {
                let amid = amin + (amax - amin) / 2.0;
                arc_in_direction(path, xc, yc, radius, amin, amid, tolerance);
                arc_in_direction(path, xc, yc, radius, amid, amax, tolerance);
            } else if amax != amin {
                let mut segments = arc_segments_needed(amax - amin, radius, tolerance);
                let step = (amax - amin) / segments as f64;
                segments -= 1;

                path.line_to(xc + radius * amin.cos(), yc + radius * amin.sin());

                for _ in 0..segments.max(0) {
                    arc_segment(path, xc, yc, radius, amin, amin + step);
                    amin += step;
                }

                arc_segment(path, xc, yc, radius, amin, amax);
            } else {
                path.line_to(xc + radius * amin.cos(), yc + radius * amin.sin());
            }
        }
    }

    /// Port of z2d's cubic spline decomposition (`internal/tess/Spline.zig`).
    pub mod spline {
        use super::Point;

        #[derive(Clone, Copy)]
        struct Knots {
            a: Point,
            b: Point,
            c: Point,
            d: Point,
        }

        fn dot_sq(x: f64, y: f64) -> f64 {
            x * x + y * y
        }

        fn lerp_half(a: Point, b: Point) -> Point {
            Point {
                x: a.x + ((b.x - a.x) / 2.0),
                y: a.y + ((b.y - a.y) / 2.0),
            }
        }

        impl Knots {
            fn error_sq(&self) -> f64 {
                let mut b_x_delta = self.b.x - self.a.x;
                let mut b_y_delta = self.b.y - self.a.y;
                let mut c_x_delta = self.c.x - self.a.x;
                let mut c_y_delta = self.c.y - self.a.y;

                if self.a.x != self.d.x || self.a.y != self.d.y {
                    let d_x_delta = self.d.x - self.a.x;
                    let d_y_delta = self.d.y - self.a.y;
                    let d_dot_sq = dot_sq(d_x_delta, d_y_delta);

                    let b_d_dot = b_x_delta * d_x_delta + b_y_delta * d_y_delta;
                    if b_d_dot >= d_dot_sq {
                        b_x_delta -= d_x_delta;
                        b_y_delta -= d_y_delta;
                    } else {
                        b_x_delta -= b_d_dot / d_dot_sq * d_x_delta;
                        b_y_delta -= b_d_dot / d_dot_sq * d_y_delta;
                    }

                    let c_d_dot = c_x_delta * d_x_delta + c_y_delta * d_y_delta;
                    if c_d_dot >= d_dot_sq {
                        c_x_delta -= d_x_delta;
                        c_y_delta -= d_y_delta;
                    } else {
                        c_x_delta -= c_d_dot / d_dot_sq * d_x_delta;
                        c_y_delta -= c_d_dot / d_dot_sq * d_y_delta;
                    }
                }

                let b_err = dot_sq(b_x_delta, b_y_delta);
                let c_err = dot_sq(c_x_delta, c_y_delta);

                if b_err > c_err {
                    b_err
                } else {
                    c_err
                }
            }

            fn de_casteljau(&mut self) -> Knots {
                let ab = lerp_half(self.a, self.b);
                let bc = lerp_half(self.b, self.c);
                let cd = lerp_half(self.c, self.d);
                let abbc = lerp_half(ab, bc);
                let bccd = lerp_half(bc, cd);
                let fin = lerp_half(abbc, bccd);

                let result = Knots {
                    a: fin,
                    b: bccd,
                    c: cd,
                    d: self.d,
                };

                self.b = ab;
                self.c = abbc;
                self.d = fin;

                result
            }
        }

        /// Decompose the curve `a`-`b`-`c`-`d` into line segments within
        /// `tolerance`, calling `line_to` for each point after `a`.
        pub fn decompose(
            a: Point,
            b: Point,
            c: Point,
            d: Point,
            tolerance: f64,
            line_to: &mut dyn FnMut(Point),
        ) {
            if a == b && c == d {
                line_to(d);
                return;
            }

            let mut s1 = Knots { a, b, c, d };

            decompose_into(&mut s1, a, tolerance * tolerance, line_to);

            line_to(d);
        }

        fn decompose_into(
            s1: &mut Knots,
            start: Point,
            tolerance: f64,
            line_to: &mut dyn FnMut(Point),
        ) {
            if s1.error_sq() < tolerance {
                if s1.a != start {
                    line_to(s1.a);
                }
                return;
            }

            let mut s2 = s1.de_casteljau();

            decompose_into(s1, start, tolerance, line_to);
            decompose_into(&mut s2, start, tolerance, line_to);
        }
    }

    /// Flatten all curves in `nodes` into line segments.
    pub fn flatten(nodes: &[PathNode], tolerance: f64) -> Vec<PathNode> {
        let mut out = Vec::with_capacity(nodes.len());
        let mut last: Option<Point> = None;
        for node in nodes {
            match *node {
                PathNode::MoveTo(p) | PathNode::LineTo(p) => {
                    out.push(*node);
                    last = Some(p);
                }
                PathNode::CurveTo(p1, p2, p3) => {
                    let a = last.unwrap_or(p1);
                    spline::decompose(a, p1, p2, p3, tolerance, &mut |p| {
                        out.push(PathNode::LineTo(p))
                    });
                    last = Some(p3);
                }
                PathNode::ClosePath => out.push(PathNode::ClosePath),
            }
        }
        out
    }

    /// Port of z2d's path offsetting (`internal/path_fx/offset.zig`, with
    /// the parts of `InputSet`/`OutputSet` it needs).
    ///
    /// This is a very simple path offset based off of adjusting a path inward
    /// (inset) or outward (outset) based on the direction of the polygon or path.
    ///
    /// Closed paths are expected to represent simple polygons (no
    /// self-intersections). Open paths are allowed but are expected to not change
    /// direction.
    pub mod offset {
        use super::{spline, DrawError, PathNode, Point};

        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Orientation {
            Invalid,
            Cw,
            Ccw,
        }

        /// Slope for the segment, always stored in normalized form.
        #[derive(Debug, Clone, Copy, PartialEq)]
        struct Slope {
            dx: f64,
            dy: f64,
        }

        impl Slope {
            /// Asserts that a != b.
            fn init(a: Point, b: Point) -> Slope {
                debug_assert!(a != b);
                let dx_real = b.x - a.x;
                let dy_real = b.y - a.y;

                if dx_real == 0.0 {
                    return if dy_real > 0.0 {
                        Slope { dx: 0.0, dy: 1.0 }
                    } else {
                        Slope { dx: 0.0, dy: -1.0 }
                    };
                } else if dy_real == 0.0 {
                    return if dx_real > 0.0 {
                        Slope { dx: 1.0, dy: 0.0 }
                    } else {
                        Slope { dx: -1.0, dy: 0.0 }
                    };
                }

                let mag = dx_real.hypot(dy_real);
                Slope {
                    dx: dx_real / mag,
                    dy: dy_real / mag,
                }
            }

            fn compare(a: Slope, b: Slope) -> i8 {
                let a_dx = a.dx;
                let a_dy = a.dy;
                let b_dx = snap_eps_to(a.dx, b.dx);
                let b_dy = snap_eps_to(a.dy, b.dy);
                let v = a_dy * b_dx - b_dy * a_dx;
                if v > 0.0 {
                    1
                } else if v < 0.0 {
                    -1
                } else {
                    0
                }
            }
        }

        fn snap_eps_to(a: f64, b: f64) -> f64 {
            if (b - a).abs() > f64::EPSILON {
                b
            } else {
                a
            }
        }

        #[derive(Debug, Clone, Copy)]
        struct Segment {
            p0: Point,
            p1: Point,
            orientation: Orientation,
            slope: Slope,
        }

        impl Segment {
            fn init(p0: Point, p1: Point) -> Segment {
                Segment {
                    p0,
                    p1,
                    orientation: Orientation::Invalid,
                    slope: Slope::init(p0, p1),
                }
            }
        }

        #[derive(Debug, Clone, Default)]
        struct Contour {
            segments: Vec<Segment>,
            closed: bool,
        }

        impl Contour {
            /// Will combine co-linear consecutive segments into single
            /// segments.
            fn plot(&mut self, point: Point) {
                debug_assert!(!self.segments.is_empty());
                debug_assert!(!self.closed);
                let prev = *self.segments.last().unwrap();
                let mut current = Segment::init(prev.p1, point);
                current.orientation = match Slope::compare(prev.slope, current.slope) {
                    -1 => Orientation::Cw,
                    1 => Orientation::Ccw,
                    _ => {
                        self.segments.last_mut().unwrap().p1 = point;
                        return;
                    }
                };

                if self.segments.len() == 1 {
                    // Setting the first segment's orientation on the second
                    // plot ensures our special case for orientation on the
                    // first segment for open segments gets handled correctly.
                    self.segments[0].orientation = current.orientation;
                }

                self.segments.push(current);
            }

            fn close(&mut self) {
                debug_assert!(self.segments.len() >= 2);
                debug_assert!(!self.closed);

                'plot_last: {
                    let prev = *self.segments.last().unwrap();
                    if prev.p1 == self.segments[0].p0 {
                        // Skip adding a segment as both the ending point and
                        // initial point are the same
                        break 'plot_last;
                    }

                    let mut current = Segment::init(prev.p1, self.segments[0].p0);
                    current.orientation = match Slope::compare(prev.slope, current.slope) {
                        -1 => Orientation::Cw,
                        1 => Orientation::Ccw,
                        _ => {
                            let p0 = self.segments[0].p0;
                            self.segments.last_mut().unwrap().p1 = p0;
                            break 'plot_last;
                        }
                    };

                    self.segments.push(current);
                }

                self.closed = true;
                let prev = *self.segments.last().unwrap();
                let current = &mut self.segments[0];
                current.orientation = if Slope::compare(prev.slope, current.slope) < 0 {
                    Orientation::Cw
                } else {
                    Orientation::Ccw
                };
            }

            /// Aligns the segments in the contour so that the first left-most
            /// vertex encountered is the first point.
            fn align_segments(&mut self) {
                debug_assert!(self.closed);
                let mut start_idx: usize = 0;
                for (new_idx, seg) in self.segments.iter().enumerate() {
                    if seg.p0.x < self.segments[start_idx].p0.x {
                        start_idx = new_idx;
                    }
                }
                self.segments.rotate_left(start_idx);
            }
        }

        /// Build contours from path nodes (z2d `InputSet.fromNodes`).
        fn contours_from_nodes(nodes: &[PathNode], tolerance: f64) -> Result<Vec<Contour>, DrawError> {
            let mut result: Vec<Contour> = Vec::new();
            let mut current: Contour = Contour::default();
            let mut points: Vec<Point> = Vec::new();

            fn add_point(current: &mut Contour, points: &mut Vec<Point>, point: Point) {
                let last_point = *points.last().unwrap();
                if last_point != point {
                    if current.segments.is_empty() {
                        current.segments.push(Segment::init(last_point, point));
                    } else {
                        current.plot(point);
                    }
                    points.push(point);
                }
            }

            for (i, node) in nodes.iter().enumerate() {
                match *node {
                    PathNode::MoveTo(p) => {
                        if !current.segments.is_empty() {
                            result.push(std::mem::take(&mut current));
                            points.clear();
                        }

                        if i == nodes.len() - 1 {
                            break;
                        }
                        points.clear();
                        points.push(p);
                    }
                    PathNode::LineTo(p) => {
                        if points.is_empty() {
                            return Err(DrawError::NoCurrentPoint);
                        }
                        add_point(&mut current, &mut points, p);
                    }
                    PathNode::CurveTo(p1, p2, p3) => {
                        if points.is_empty() {
                            return Err(DrawError::NoCurrentPoint);
                        }
                        let a = *points.last().unwrap();
                        spline::decompose(a, p1, p2, p3, tolerance, &mut |p| {
                            add_point(&mut current, &mut points, p)
                        });
                    }
                    PathNode::ClosePath => {
                        if points.len() >= 2 && current.segments.len() >= 2 {
                            current.close();
                            result.push(std::mem::take(&mut current));
                            points.clear();
                        }
                    }
                }
            }

            if !current.segments.is_empty() {
                result.push(current);
            }

            Ok(result)
        }

        /// Run the offset; returns the new nodes (all lines).
        pub fn run(nodes: &[PathNode], tolerance: f64, offset: f64) -> Result<Vec<PathNode>, DrawError> {
            let mut contours = contours_from_nodes(nodes, tolerance)?;
            let mut out: Vec<(Vec<Point>, bool)> = Vec::new();
            if offset == 0.0 {
                // no-op all nodes if zero offset was supplied
                for contour in &contours {
                    noop_contour(&mut out, contour);
                }
            } else {
                for contour in contours.iter_mut() {
                    if contour.closed {
                        contour.align_segments();
                        run_closed(&mut out, contour, offset);
                    } else {
                        run_open(&mut out, contour, offset);
                    }
                }
            }

            // OutputSet.toNodes
            let mut result = Vec::new();
            for (points, closed) in out {
                if points.len() <= 1 {
                    continue;
                }
                result.push(PathNode::MoveTo(points[0]));
                for p in &points[1..] {
                    result.push(PathNode::LineTo(*p));
                }
                if closed {
                    result.push(PathNode::ClosePath);
                    result.push(PathNode::MoveTo(points[0]));
                }
            }
            Ok(result)
        }

        fn noop_contour(out: &mut Vec<(Vec<Point>, bool)>, contour: &Contour) {
            if contour.segments.is_empty() {
                return;
            }
            if contour.closed {
                out.push((contour.segments.iter().map(|s| s.p0).collect(), true));
            } else {
                let mut points = vec![contour.segments[0].p0, contour.segments[0].p1];
                for seg in &contour.segments[1..] {
                    points.push(seg.p1);
                }
                out.push((points, false));
            }
        }

        fn run_closed(out: &mut Vec<(Vec<Point>, bool)>, contour: &Contour, offset: f64) {
            debug_assert!(contour.closed);
            debug_assert!(contour.segments.len() >= 3);

            let segs = &contour.segments;
            let mut result = Vec::with_capacity(segs.len());
            let clockwise = segs[0].orientation == Orientation::Cw;

            // Initial point
            result.push(intersect(segs[segs.len() - 1], segs[0], offset, clockwise));

            // Everything else
            for in_idx in 0..segs.len() - 1 {
                result.push(intersect(segs[in_idx], segs[in_idx + 1], offset, clockwise));
            }

            out.push((result, true));
        }

        fn run_open(out: &mut Vec<(Vec<Point>, bool)>, contour: &Contour, offset: f64) {
            let segs = &contour.segments;
            let mut result = Vec::with_capacity(segs.len() + 1);
            let clockwise = match segs[0].orientation {
                Orientation::Cw => true,
                Orientation::Ccw => false,
                Orientation::Invalid => {
                    // Invalid means that there was only one element, always
                    // assume clockwise and return.
                    debug_assert!(segs.len() == 1);
                    result.push(offset_single(segs[0].p0, segs[0].slope, offset, true));
                    result.push(offset_single(segs[0].p1, segs[0].slope, offset, true));
                    out.push((result, false));
                    return;
                }
            };

            result.push(offset_single(segs[0].p0, segs[0].slope, offset, clockwise));
            for in_idx in 0..segs.len() - 1 {
                result.push(intersect(segs[in_idx], segs[in_idx + 1], offset, clockwise));
            }
            let last = segs[segs.len() - 1];
            result.push(offset_single(last.p1, last.slope, offset, clockwise));
            out.push((result, false));
        }

        fn intersect(in_seg: Segment, out_seg: Segment, offset: f64, clockwise: bool) -> Point {
            let dx1 = in_seg.slope.dx;
            let dy1 = in_seg.slope.dy;
            let dx2 = out_seg.slope.dx;
            let dy2 = out_seg.slope.dy;

            let xoff1 = -dy1 * offset;
            let yoff1 = dx1 * offset;
            let xoff2 = -dy2 * offset;
            let yoff2 = dx2 * offset;

            let (x1, y1, x2, y2) = if clockwise {
                (
                    in_seg.p1.x - xoff1,
                    in_seg.p1.y - yoff1,
                    out_seg.p0.x - xoff2,
                    out_seg.p0.y - yoff2,
                )
            } else {
                (
                    in_seg.p1.x + xoff1,
                    in_seg.p1.y + yoff1,
                    out_seg.p0.x + xoff2,
                    out_seg.p0.y + yoff2,
                )
            };

            let div = dx1 * dy2 - dy1 * dx2;
            debug_assert!(div != 0.0);

            let t = ((x2 - x1) * dy2 - (y2 - y1) * dx2) / div;

            Point {
                x: x1 + t * dx1,
                y: y1 + t * dy1,
            }
        }

        fn offset_single(point: Point, slope: Slope, offset: f64, clockwise: bool) -> Point {
            let xoff1 = -slope.dy * offset;
            let yoff1 = slope.dx * offset;
            if clockwise {
                Point {
                    x: point.x - xoff1,
                    y: point.y - yoff1,
                }
            } else {
                Point {
                    x: point.x + xoff1,
                    y: point.y + yoff1,
                }
            }
        }
    }

    /// Fill and stroke operations (z2d `painter`), rasterized by tiny-skia.
    pub mod painter {
        use super::*;

        /// Build a tiny-skia path out of (flattened) nodes.
        fn build_path(nodes: &[PathNode], tolerance: f64) -> Option<tiny_skia::Path> {
            let flat = flatten(nodes, tolerance);
            let mut pb = tiny_skia::PathBuilder::with_capacity(flat.len(), flat.len());
            let mut pending_move: Option<Point> = None;
            for node in &flat {
                match *node {
                    PathNode::MoveTo(p) => pending_move = Some(p),
                    PathNode::LineTo(p) => {
                        if let Some(m) = pending_move.take() {
                            pb.move_to(m.x as f32, m.y as f32);
                        }
                        pb.line_to(p.x as f32, p.y as f32);
                    }
                    PathNode::CurveTo(..) => unreachable!(),
                    PathNode::ClosePath => pb.close(),
                }
            }
            pb.finish()
        }

        /// Composite a coverage mask onto the surface with a solid alpha.
        fn composite_mask(
            surface: &mut Surface,
            alpha: u8,
            mask: &tiny_skia::Mask,
            operator: Operator,
            anti_alias: bool,
        ) {
            let a = alpha as u32;
            for (d, &cov) in surface.buf.iter_mut().zip(mask.data().iter()) {
                if cov == 0 {
                    continue;
                }
                let cov = if anti_alias { cov as u32 } else { 255 };
                // dstIn(src, mask): src.a * coverage
                let s = ((a * cov) / 255) as u8;
                *d = composite_px(operator, *d, s);
            }
        }

        fn new_mask(surface: &Surface) -> Option<tiny_skia::Mask> {
            tiny_skia::Mask::new(surface.get_width() as u32, surface.get_height() as u32)
        }

        /// Runs a fill operation on the path set represented by `nodes`.
        pub fn fill(
            surface: &mut Surface,
            alpha: u8,
            nodes: &[PathNode],
            opts: &FillOptions,
        ) -> Result<(), DrawError> {
            let mut closed = true;
            {
                let mut open = false;
                for node in nodes {
                    match node {
                        PathNode::MoveTo(_) => {
                            if open {
                                closed = false;
                            }
                        }
                        PathNode::LineTo(_) | PathNode::CurveTo(..) => open = true,
                        PathNode::ClosePath => open = false,
                    }
                }
                if open {
                    closed = false;
                }
            }
            if !closed {
                return Err(DrawError::PathNotClosed);
            }

            let Some(path) = build_path(nodes, opts.tolerance) else {
                return Ok(());
            };
            let Some(mut mask) = new_mask(surface) else {
                return Ok(());
            };
            let anti_alias = opts.anti_aliasing_mode != AntiAliasMode::None;
            let rule = match opts.fill_rule {
                FillRule::NonZero => tiny_skia::FillRule::Winding,
                FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
            };
            mask.fill_path(&path, rule, anti_alias, tiny_skia::Transform::identity());
            composite_mask(surface, alpha, &mask, opts.operator, anti_alias);
            Ok(())
        }

        /// Runs a stroke operation on the path set represented by `nodes`.
        pub fn stroke(
            surface: &mut Surface,
            alpha: u8,
            nodes: &[PathNode],
            opts: &StrokeOptions,
        ) -> Result<(), DrawError> {
            let Some(path) = build_path(nodes, opts.tolerance) else {
                return Ok(());
            };
            let stroke = tiny_skia::Stroke {
                width: opts.line_width as f32,
                miter_limit: opts.miter_limit as f32,
                line_cap: match opts.line_cap_mode {
                    CapMode::Butt => tiny_skia::LineCap::Butt,
                    CapMode::Round => tiny_skia::LineCap::Round,
                    CapMode::Square => tiny_skia::LineCap::Square,
                },
                line_join: match opts.line_join_mode {
                    JoinMode::Miter => tiny_skia::LineJoin::Miter,
                    JoinMode::Round => tiny_skia::LineJoin::Round,
                    JoinMode::Bevel => tiny_skia::LineJoin::Bevel,
                },
                dash: None,
            };
            let Some(outline) = path.stroke(&stroke, 1.0) else {
                return Ok(());
            };
            let Some(mut mask) = new_mask(surface) else {
                return Ok(());
            };
            let anti_alias = opts.anti_aliasing_mode != AntiAliasMode::None;
            mask.fill_path(
                &outline,
                tiny_skia::FillRule::Winding,
                anti_alias,
                tiny_skia::Transform::identity(),
            );
            composite_mask(surface, alpha, &mask, opts.operator, anti_alias);
            Ok(())
        }
    }

    /// A drawing context (z2d `Context`), the subset used by the sprites.
    pub struct Context<'a> {
        pub surface: &'a mut Surface,
        pub path: Path,
        /// The source alpha (z2d pattern; default opaque).
        pub source: u8,
        pub anti_aliasing_mode: AntiAliasMode,
        pub fill_rule: FillRule,
        pub line_cap_mode: CapMode,
        pub line_join_mode: JoinMode,
        pub line_width: f64,
        pub miter_limit: f64,
        pub operator: Operator,
        pub tolerance: f64,
        pub transformation: Transformation,
    }

    impl<'a> Context<'a> {
        pub fn init(surface: &'a mut Surface) -> Context<'a> {
            Context {
                surface,
                path: Path::default(),
                // Default pattern is opaque black, i.e. alpha 0xFF.
                source: 0xFF,
                anti_aliasing_mode: AntiAliasMode::Default,
                fill_rule: FillRule::NonZero,
                line_cap_mode: CapMode::Butt,
                line_join_mode: JoinMode::Miter,
                line_width: 2.0,
                miter_limit: 10.0,
                operator: Operator::SrcOver,
                tolerance: DEFAULT_TOLERANCE,
                transformation: Transformation::IDENTITY,
            }
        }

        /// Set the source to a solid alpha value.
        pub fn set_source(&mut self, color: super::Color) {
            self.source = color.0;
        }

        pub fn set_line_width(&mut self, width: f64) {
            self.line_width = width;
        }

        pub fn set_transformation(&mut self, t: Transformation) {
            self.transformation = t;
            self.path.transformation = t;
        }

        pub fn move_to(&mut self, x: f64, y: f64) -> Result<(), DrawError> {
            self.path.move_to(x, y);
            Ok(())
        }

        pub fn line_to(&mut self, x: f64, y: f64) -> Result<(), DrawError> {
            self.path.line_to(x, y);
            Ok(())
        }

        pub fn curve_to(
            &mut self,
            x1: f64,
            y1: f64,
            x2: f64,
            y2: f64,
            x3: f64,
            y3: f64,
        ) -> Result<(), DrawError> {
            if self.path.current_point.is_none() {
                return Err(DrawError::NoCurrentPoint);
            }
            self.path.curve_to(x1, y1, x2, y2, x3, y3);
            Ok(())
        }

        pub fn arc(
            &mut self,
            xc: f64,
            yc: f64,
            radius: f64,
            angle1: f64,
            angle2: f64,
        ) -> Result<(), DrawError> {
            self.path.arc(xc, yc, radius, angle1, angle2);
            Ok(())
        }

        pub fn close_path(&mut self) -> Result<(), DrawError> {
            self.path.close();
            Ok(())
        }

        /// Fill the current path. The path is retained.
        pub fn fill(&mut self) -> Result<(), DrawError> {
            painter::fill(
                self.surface,
                self.source,
                &self.path.nodes,
                &FillOptions {
                    anti_aliasing_mode: self.anti_aliasing_mode,
                    fill_rule: self.fill_rule,
                    operator: self.operator,
                    tolerance: self.tolerance,
                },
            )
        }

        /// Stroke the current path. The path is retained.
        pub fn stroke(&mut self) -> Result<(), DrawError> {
            painter::stroke(
                self.surface,
                self.source,
                &self.path.nodes,
                &StrokeOptions {
                    anti_aliasing_mode: self.anti_aliasing_mode,
                    line_cap_mode: self.line_cap_mode,
                    line_join_mode: self.line_join_mode,
                    line_width: self.line_width,
                    miter_limit: self.miter_limit,
                    operator: self.operator,
                    tolerance: self.tolerance,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::z2d::{offset, PathNode, Point as P};
    use super::*;

    #[test]
    fn box_rect() {
        let b = Box {
            p0: Point { x: 5, y: 1 },
            p1: Point { x: 2, y: 7 },
        };
        assert_eq!(
            Rect {
                x: 2,
                y: 1,
                width: 3,
                height: 6
            },
            b.rect()
        );
    }

    #[test]
    fn rect_and_pixel_with_padding() {
        let mut c = Canvas::init(4, 4, 1, 1);
        c.rect(
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            Color::ON,
        );
        // Out of bounds pixels are ignored.
        c.pixel(-5, -5, Color::ON);
        assert_eq!(255, c.sfc.buf[1 * 6 + 1]);
        assert_eq!(255, c.sfc.buf[1 * 6 + 2]);
        assert_eq!(2 * 255, c.sfc.buf.iter().map(|&v| v as u32).sum::<u32>());
    }

    #[test]
    fn write_atlas_trims() {
        let mut atlas = Atlas::init(16, atlas::Format::Grayscale).unwrap();
        let mut c = Canvas::init(4, 4, 1, 1);
        c.r#box(1, 1, 3, 2, Color::ON);
        let region = c.write_atlas(&mut atlas).unwrap();
        assert_eq!((2, 1), (region.width, region.height));
        assert_eq!((2, 2, 2, 3), (c.clip_left, c.clip_top, c.clip_right, c.clip_bottom));
        let idx = (region.y * 16 + region.x) as usize;
        assert_eq!(&[255, 255], &atlas.data[idx..idx + 2]);
    }

    #[test]
    fn fill_triangle_coverage() {
        let mut c = Canvas::init(10, 10, 0, 0);
        c.triangle(
            Triangle {
                p0: Point { x: 0.0, y: 0.0 },
                p1: Point { x: 10.0, y: 0.0 },
                p2: Point { x: 0.0, y: 10.0 },
            },
            Color::ON,
        )
        .unwrap();
        // Fully inside, fully outside, and on the diagonal (half covered).
        assert_eq!(255, c.sfc.buf[1 * 10 + 1]);
        assert_eq!(0, c.sfc.buf[9 * 10 + 9]);
        let diag = c.sfc.buf[4 * 10 + 5];
        assert!(diag > 64 && diag < 192, "diag = {diag}");
        // Total coverage is close to the area (50 px).
        let total: f64 = c.sfc.buf.iter().map(|&v| v as f64 / 255.0).sum();
        assert!((total - 50.0).abs() < 2.0, "total = {total}");
    }

    #[test]
    fn fill_requires_closed_path() {
        let mut c = Canvas::init(4, 4, 0, 0);
        let mut path = c.static_path(3);
        path.move_to(0.0, 0.0);
        path.line_to(4.0, 0.0);
        path.line_to(0.0, 4.0);
        assert_eq!(
            Err(DrawError::PathNotClosed),
            c.fill_path(&path, FillOptions::default(), Color::ON)
        );
    }

    #[test]
    fn invert_and_flip() {
        let mut c = Canvas::init(3, 2, 0, 0);
        c.pixel(0, 0, Color::ON);
        c.clip_left = 1;
        c.flip_horizontal().unwrap();
        assert_eq!(255, c.sfc.buf[2]);
        assert_eq!((0, 1), (c.clip_left, c.clip_right));
        c.flip_vertical().unwrap();
        assert_eq!(255, c.sfc.buf[3 + 2]);
        c.invert();
        assert_eq!(0, c.sfc.buf[3 + 2]);
        assert_eq!(255, c.sfc.buf[0]);
    }

    #[test]
    fn clear_clipping_regions() {
        let mut c = Canvas::init(4, 4, 0, 0);
        c.r#box(0, 0, 4, 4, Color::ON);
        c.clip_left = 1;
        c.clip_top = 1;
        c.clip_right = 1;
        c.clip_bottom = 1;
        c.clear_clipping_regions();
        assert_eq!(4 * 255, c.sfc.buf.iter().map(|&v| v as u32).sum::<u32>());
        assert_eq!(255, c.sfc.buf[5]);
    }

    fn p(x: f64, y: f64) -> P {
        P { x, y }
    }

    // Port of the triangle contour of z2d's "offset, e2e" test.
    #[test]
    fn offset_triangle() {
        let input = [
            PathNode::MoveTo(p(110.0, 110.0)),
            PathNode::LineTo(p(60.0, 10.0)),
            PathNode::LineTo(p(10.0, 110.0)),
            PathNode::ClosePath,
            PathNode::MoveTo(p(110.0, 110.0)),
        ];
        let out = offset::run(&input, 0.1, -2.0).unwrap();
        assert_eq!(
            vec![
                PathNode::MoveTo(p(13.236067977499792, 108.0)),
                PathNode::LineTo(p(106.76393202250021, 108.0)),
                PathNode::LineTo(p(60.0, 14.472135954999583)),
                PathNode::ClosePath,
                PathNode::MoveTo(p(13.236067977499792, 108.0)),
            ],
            out
        );

        let out = offset::run(&input, 0.1, 2.0).unwrap();
        assert_eq!(
            vec![
                PathNode::MoveTo(p(6.763932022500208, 112.0)),
                PathNode::LineTo(p(113.23606797749979, 112.0)),
                PathNode::LineTo(p(60.0, 5.527864045000417)),
                PathNode::ClosePath,
                PathNode::MoveTo(p(6.763932022500208, 112.0)),
            ],
            out
        );
    }

    // Port of the open contours of z2d's "offset, e2e" test.
    #[test]
    fn offset_open() {
        let input = [
            PathNode::MoveTo(p(10.0, 300.0)),
            PathNode::LineTo(p(110.0, 400.0)),
        ];
        let out = offset::run(&input, 0.1, -2.0).unwrap();
        assert_eq!(
            vec![
                PathNode::MoveTo(p(8.585786437626904, 301.4142135623731)),
                PathNode::LineTo(p(108.58578643762691, 401.4142135623731)),
            ],
            out
        );

        let input = [
            PathNode::MoveTo(p(130.0, 20.0)),
            PathNode::LineTo(p(149.0, 30.0)),
            PathNode::LineTo(p(160.0, 50.0)),
            PathNode::LineTo(p(160.0, 70.0)),
            PathNode::LineTo(p(150.0, 90.0)),
            PathNode::LineTo(p(130.0, 100.0)),
        ];
        let out = offset::run(&input, 0.1, -2.0).unwrap();
        let expected = [
            p(129.06850713433477, 21.769836444763964),
            p(147.53701445992533, 31.490103458232674),
            p(158.0, 50.51371353109574),
            p(158.0, 69.5278640450004),
            p(148.50928801500012, 88.50928801500015),
            p(129.10557280900008, 98.21114561800017),
        ];
        assert_eq!(expected.len(), out.len());
        for (e, o) in expected.iter().zip(out.iter()) {
            let (PathNode::MoveTo(o) | PathNode::LineTo(o)) = o else {
                panic!("unexpected node {o:?}");
            };
            assert!((e.x - o.x).abs() < 1e-9 && (e.y - o.y).abs() < 1e-9, "{e:?} != {o:?}");
        }

        // Zero offset is a no-op.
        let out = offset::run(&input, 0.1, 0.0).unwrap();
        assert_eq!(&input[..], &out[..]);
    }
}
