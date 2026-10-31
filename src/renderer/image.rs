//! Port of `src/renderer/image.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * Zig refers to the build's graphics API through
//!   `renderer.Renderer.API`; here the texture/buffer/render pass types
//!   come from [`crate::renderer::graphics_api`] and the API value is any
//!   [`GraphicsApi`] (the build's API is the only implementation).
//! * `Image.Pending` / `Image.Pending.PixelFormat` / `Image.Replace` are
//!   the module-level [`Pending`] / [`PixelFormat`] / [`Replace`].
//! * Pending data is a raw pointer like in Zig (it may point at memory the
//!   image does not own, e.g. the overlay surface, until `convertCopy`
//!   makes an owned copy). Owned pending data is a leaked boxed slice of
//!   exactly `Pending::len()` bytes, freed by [`free_data`].
//! * The allocator parameters are dropped.

use std::collections::HashMap;

use crate::pkg::wuffs;
use crate::renderer::generic::GraphicsApi;
use crate::renderer::graphics_api::{self as api, shaders, Texture};
use crate::renderer::overlay::Overlay;
use crate::terminal::kitty::graphics as kitty_graphics;
use crate::terminal::kitty::graphics_command::Format as KittyFormat;
use crate::terminal::render::{ImageLayer, ImagePlacementProtocol, RenderState};
use crate::terminal::terminal::Terminal;

const LOG: &str = "renderer_image";

/// Allocate owned pending data (Zig: data allocated with the renderer's
/// allocator).
fn alloc_data(data: Vec<u8>) -> *mut u8 {
    Box::into_raw(data.into_boxed_slice()).cast::<u8>()
}

/// Free owned pending data (Zig: `alloc.free(p.dataSlice())`).
///
/// # Safety
///
/// `p.data` must have been returned by [`alloc_data`] for exactly
/// `p.len()` bytes and not freed yet.
unsafe fn free_data(p: Pending) {
    if p.data.is_null() {
        return;
    }
    // SAFETY: see the function contract.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(p.data, p.len())) });
}

/// Generic image rendering state for the renderer. This stores all
/// images and their placements and exposes only a limited public API
/// for adding images and placements and drawing them.
#[derive(Debug, Default)]
pub struct State {
    /// The full image state for the renderer that specifies what images
    /// need to be uploaded, pruned, etc.
    pub images: ImageMap,

    /// The placements for the Kitty image protocol, sorted by layer and
    /// then by z-index.
    pub kitty_placements: Vec<Placement>,

    /// Where the layers of `kitty_placements` end. Placements before
    /// `kitty_bg_end` are drawn below the cell backgrounds. Placements
    /// from `kitty_bg_end` up to `kitty_text_end` are drawn below the
    /// text, and the rest are drawn above it.
    pub kitty_bg_end: u32,
    pub kitty_text_end: u32,

    /// The render state's `imagePlacementsGeneration` when
    /// `kitty_placements` was last built. Every Kitty placement comes
    /// from the render state, so a different value means
    /// `kitty_placements` is out of date.
    pub kitty_placements_generation: u64,

    /// The active screen's `ImageStorage.generation` when
    /// `kitty_placements` was last built. Some changes to the images
    /// don't change any placement but still need an update. For example,
    /// deleting an image that has no placements leaves the placements as
    /// they were, but its texture must be unloaded.
    pub kitty_storage_generation: u64,

    /// Overlays
    pub overlay_placements: Vec<Placement>,
}

/// Zig: `State.DrawPlacements`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawPlacements {
    KittyBelowBg,
    KittyBelowText,
    KittyAboveText,
    Overlay,
}

/// Zig: `State.PrepImageError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepImageError {
    OutOfMemory,
    ImageConversionError,
}

impl std::fmt::Display for PrepImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for PrepImageError {}

impl State {
    pub const EMPTY: fn() -> State = State::default;

    pub fn deinit(&mut self) {
        for (_, v) in self.images.drain() {
            v.image.deinit();
        }
        self.kitty_placements = Vec::new();
        self.overlay_placements = Vec::new();
    }

    /// Upload any images to the GPU that need to be uploaded,
    /// and remove any images that are no longer needed on the GPU.
    ///
    /// If any uploads fail, they are ignored. The return value
    /// can be used to detect if upload was a total success (true)
    /// or not (false).
    pub fn upload<G: GraphicsApi>(&mut self, api: &G) -> bool {
        let mut success: bool = true;
        self.images.retain(|_, v| {
            let img = &mut v.image;
            if img.is_unloading() {
                img.deinit();
                return false;
            }

            if img.is_pending() {
                if let Err(err) = img.upload(api) {
                    log::warn!(target: LOG, "error uploading image to GPU err={err}");
                    success = false;
                }
            }

            true
        });

        success
    }

    /// Draw the given named set of placements.
    ///
    /// Any placements that have non-uploaded images are ignored. Any
    /// graphics API errors during drawing are also ignored.
    pub fn draw<G: GraphicsApi>(
        &self,
        api: &G,
        pipeline: api::Pipeline,
        pass: &mut api::RenderPass,
        placement_type: DrawPlacements,
    ) {
        let placements: &[Placement] = match placement_type {
            DrawPlacements::KittyBelowBg => &self.kitty_placements[..self.kitty_bg_end as usize],
            DrawPlacements::KittyBelowText => {
                &self.kitty_placements[self.kitty_bg_end as usize..self.kitty_text_end as usize]
            }
            DrawPlacements::KittyAboveText => &self.kitty_placements[self.kitty_text_end as usize..],
            DrawPlacements::Overlay => &self.overlay_placements,
        };

        for p in placements {
            // Look up the image
            let Some(image) = self.images.get(&p.image_id) else {
                log::warn!(target: LOG, "image not found for placement image_id={:?}", p.image_id);
                continue;
            };

            // Get the texture
            let texture = match image.image {
                Image::Ready(t) | Image::UnloadReady(t) => t,
                _ => {
                    log::warn!(target: LOG, "image not ready for placement image_id={:?}", p.image_id);
                    continue;
                }
            };

            // Create our vertex buffer, which is always exactly one item.
            // future(mitchellh): we can group rendering multiple instances of a single image
            let buf = match api::Buffer::<shaders::Image>::init_fill(
                api.image_buffer_options(),
                &[shaders::Image {
                    grid_pos: [p.x as f32, p.y as f32],

                    cell_offset: [p.cell_offset_x as f32, p.cell_offset_y as f32],

                    source_rect: [
                        p.source_x as f32,
                        p.source_y as f32,
                        p.source_width as f32,
                        p.source_height as f32,
                    ],

                    dest_size: [p.width as f32, p.height as f32],
                }],
            ) {
                Ok(b) => b,
                Err(err) => {
                    log::warn!(target: LOG, "error creating image vertex buffer err={err}");
                    continue;
                }
            };

            pass.step(api::render_pass::Step {
                pipeline,
                uniforms: None,
                buffers: &[Some(buf.buffer)],
                textures: &[Some(texture)],
                samplers: &[],
                draw: api::render_pass::Draw {
                    r#type: api::Primitive::TriangleStrip,
                    vertex_count: 4,
                    instance_count: 1,
                },
            });

            buf.deinit();
        }
    }

    /// Update our overlay state. Null value deletes any existing overlay.
    pub fn overlay_update(&mut self, overlay_: Option<&mut Overlay>) -> Result<(), PrepImageError> {
        let Some(overlay) = overlay_ else {
            // If we don't have an overlay, remove any existing one.
            if let Some(data) = self.images.get_mut(&Id::Overlay) {
                data.image.mark_for_unload();
            }
            return Ok(());
        };

        // Overlays are always considered new content, so we take a
        // fresh generation stamp to force replacing any existing one.
        let generation = kitty_graphics::next_generation();

        // Ensure we have space for our overlay placement. Do this before
        // we upload our image so we don't have to deal with cleaning
        // that up.
        self.overlay_placements.clear();
        self.overlay_placements.reserve(1);

        // Setup our image.
        let pending = overlay.pending_image();
        self.prep_image(Id::Overlay, generation, pending)?;

        // Setup our placement
        self.overlay_placements.push(Placement {
            image_id: Id::Overlay,
            layer: ImageLayer::AboveText,
            x: 0,
            y: 0,
            z: 0,
            width: pending.width,
            height: pending.height,
            cell_offset_x: 0,
            cell_offset_y: 0,
            source_x: 0,
            source_y: 0,
            source_width: pending.width,
            source_height: pending.height,
        });
        Ok(())
    }

    /// Returns true if `kittyUpdate` must run, because the placements or
    /// the images changed since it last ran. `render_state` must have
    /// just been updated from `t`.
    ///
    /// Most frames return false. A frame that only changes text returns
    /// false unless that text had or now has Kitty placeholder cells.
    ///
    /// This does not read or write state used by drawing.
    pub fn kitty_requires_update(&self, t: &Terminal, render_state: &RenderState) -> bool {
        // The placements changed.
        if render_state.image_placements_generation() != self.kitty_placements_generation {
            return true;
        }

        // The images changed. This catches changes that don't move any
        // placement, such as deleting an image with no placements.
        if t.screens.active.kitty_images.generation != self.kitty_storage_generation {
            return true;
        }

        false
    }

    /// Rebuild the Kitty placements from the render state, and prepare
    /// or unload the textures they need. `render_state` must have just
    /// been updated from `t`. The terminal is only read to look up
    /// images.
    ///
    /// This reads and writes state used by drawing.
    pub fn kitty_update(&mut self, t: &Terminal, render_state: &RenderState) {
        let storage = &t.screens.active.kitty_images;

        // Remember what we are building from, so that kittyRequiresUpdate
        // can tell when it changes.
        self.kitty_placements_generation = render_state.image_placements_generation();
        self.kitty_storage_generation = storage.generation;

        // We always clear our previous placements no matter what because
        // we rebuild them from scratch.
        self.kitty_placements.clear();

        // Go through our known images and if there are any that are no longer
        // in use then mark them to be freed.
        //
        // This never conflicts with the below because a placement can't
        // reference an image that doesn't exist.
        for (key, value) in self.images.iter_mut() {
            match *key {
                // We're only looking at Kitty images
                Id::Kitty(id) => {
                    if let Some(image) = storage.image_by_id(id) {
                        // An image whose displayed pixels are pending must
                        // not keep an older texture for the same ID drawable
                        // while its placements wait. For an animation, the
                        // displayed pixels are the current frame's.
                        if image.render_data().is_pending() {
                            value.image.mark_for_unload();
                        }
                    } else {
                        value.image.mark_for_unload();
                    }
                }

                Id::Overlay => {}
            }
        }

        // The render state has already positioned every placement. It
        // was updated under the same lock and only returns placements it
        // can draw, so their images exist and are fully loaded.
        for p in render_state.image_placement_iterator() {
            let image = match p.protocol {
                ImagePlacementProtocol::Kitty(_) => match storage.image_by_id(p.image_id) {
                    Some(i) => i,
                    None => continue,
                },
            };

            // For errors we log and continue. We try to place other
            // placements even if one fails.
            if let Err(err) = self.prep_kitty_image(image) {
                log::warn!(target: LOG, "error preparing kitty placement err={err}");
                continue;
            }
            self.kitty_placements.push(Placement {
                image_id: Id::Kitty(image.id),
                layer: p.layer,
                x: p.x,
                y: p.y,
                z: p.z,
                width: p.width,
                height: p.height,
                cell_offset_x: p.offset_x,
                cell_offset_y: p.offset_y,
                source_x: p.source_x,
                source_y: p.source_y,
                source_width: p.source_width,
                source_height: p.source_height,
            });
        }

        // Sort the placements by layer, then by z-index. Placements with
        // the same z-index are ordered by image, so that the order is
        // stable from frame to frame.
        fn less_than(lhs: &Placement, rhs: &Placement) -> bool {
            let lhs_layer = lhs.layer as u8;
            let rhs_layer = rhs.layer as u8;
            if lhs_layer != rhs_layer {
                return lhs_layer < rhs_layer;
            }
            lhs.z < rhs.z || (lhs.z == rhs.z && lhs.image_id.z_less_than(rhs.image_id))
        }
        self.kitty_placements.sort_unstable_by(|a, b| {
            if less_than(a, b) {
                std::cmp::Ordering::Less
            } else if less_than(b, a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });

        self.kitty_bg_end = Self::layer_end(&self.kitty_placements, ImageLayer::BelowBackground);
        self.kitty_text_end = Self::layer_end(&self.kitty_placements, ImageLayer::BelowText);
    }

    /// Returns the index of the first placement drawn above `layer`, or
    /// the number of placements if there is none. `placements` must be
    /// sorted by layer.
    fn layer_end(placements: &[Placement], layer: ImageLayer) -> u32 {
        for (i, p) in placements.iter().enumerate() {
            if (p.layer as u8) > (layer as u8) {
                return i as u32;
            }
        }
        placements.len() as u32
    }

    /// Prepare an image for upload to the GPU.
    fn prep_image(&mut self, id: Id, generation: u64, pending: Pending) -> Result<(), PrepImageError> {
        // If this image exists and its generation is the same it is the
        // identical image so we don't need to send it to the GPU.
        let found_existing = match self.images.get(&id) {
            Some(v) if v.generation == generation => return Ok(()),
            Some(_) => true,
            None => false,
        };

        // Store it in the map
        let new_image: Image = match pending.convert_copy() {
            Ok(v) => v,
            Err(_) => {
                if found_existing {
                    // If this was an existing entry, it is invalid and
                    // we must unload it.
                    if let Some(v) = self.images.get_mut(&id) {
                        v.image.mark_for_unload();
                    }
                }
                // (A new entry was never inserted, so there is nothing
                // to remove.)

                return Err(PrepImageError::OutOfMemory);
            }
        };

        let entry = self.images.entry(id).or_insert(ImageEntry {
            image: Image::Pending(Pending::EMPTY),
            generation: 0,
        });
        if !found_existing {
            entry.image = new_image;
            entry.generation = 0;
        } else {
            entry.image.mark_for_replace(new_image);
        }

        // If any error happens, we unload the image and it is invalid.
        let p = entry.image.get_pending_pointer().expect("pending after replace");
        if let Err(err) = p.prep_for_upload() {
            log::warn!(target: LOG, "error preparing image for upload err={err}");
            entry.image.mark_for_unload();
            return Err(PrepImageError::ImageConversionError);
        }
        entry.generation = generation;
        Ok(())
    }

    /// Prepare the provided Kitty image for upload to the GPU by copying its
    /// data with our allocator and setting it to the pending state.
    fn prep_kitty_image(&mut self, image: &kitty_graphics::Image) -> Result<(), PrepImageError> {
        // For animated images this is the current animation frame;
        // the image generation changes whenever the current frame
        // does, so the upload cache stays coherent.
        let data = image.render_data().bytes().expect("kitty image data is loaded");
        self.prep_image(
            Id::Kitty(image.id),
            image.generation,
            Pending {
                width: image.width,
                height: image.height,
                pixel_format: match image.format {
                    KittyFormat::Gray => PixelFormat::Gray,
                    KittyFormat::GrayAlpha => PixelFormat::GrayAlpha,
                    KittyFormat::Rgb => PixelFormat::Rgb,
                    KittyFormat::Rgba => PixelFormat::Rgba,
                    KittyFormat::Png => unreachable!(), // should be decoded by now
                },

                // Casting away const is always gross but this one is safe
                // because the data is only read from here and copied into
                // its own buffer.
                data: data.as_ptr().cast_mut(),
            },
        )
    }
}

/// Represents a single image placement on the grid.
/// A placement is a request to render an instance of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The image being rendered. This MUST be in the image map.
    pub image_id: Id,

    /// The layer the placement is drawn in. Kitty placements are sorted
    /// by layer first and then by z-index. Overlays are always drawn
    /// above the text.
    pub layer: ImageLayer,

    /// The grid x/y where this placement is located.
    pub x: i32,
    pub y: i32,
    pub z: i32,

    /// The width/height of the placed image.
    pub width: u32,
    pub height: u32,

    /// The offset in pixels from the top left of the cell.
    /// This is clamped to the size of a cell.
    pub cell_offset_x: u32,
    pub cell_offset_y: u32,

    /// The source rectangle of the placement.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

/// Image identifier used to store and lookup images.
///
/// This is tagged by different image types to make it easier to
/// store different kinds of images in the same map without having
/// to worry about ID collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Id {
    /// Image sent to the terminal state via the kitty graphics protocol.
    /// The value is the ID assigned by the terminal.
    Kitty(u32),

    /// Debug overlay. This is always composited down to a single
    /// image for now. In the future we can support layers here if we want.
    Overlay,
}

impl Id {
    /// Z-ordering tie-breaker for images with the same z value.
    pub fn z_less_than(self, rhs: Id) -> bool {
        match (self, rhs) {
            (Id::Kitty(lhs_id), Id::Kitty(rhs_id)) => lhs_id < rhs_id,
            // If our tags aren't the same, we sort by tag.
            // Kitty images always sort before (lower z) non-kitty images.
            (Id::Kitty(_), _) => true,
            // No sensical ordering
            (Id::Overlay, _) => false,
        }
    }
}

/// The value type of [`ImageMap`] (Zig: an anonymous struct).
#[derive(Debug, Clone, Copy)]
pub struct ImageEntry {
    pub image: Image,

    /// The generation of the terminal image this was created from
    /// (see terminal.kitty.graphics.Image.generation). Used to detect
    /// staleness: a differing generation for the same ID means the
    /// contents changed and the texture must be replaced. Zero is
    /// never a valid stored generation so it marks "not yet uploaded".
    pub generation: u64,
}

/// The map used for storing images.
pub type ImageMap = HashMap<Id, ImageEntry>;

/// The state for a single image that is to be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// The image data is pending upload to the GPU.
    ///
    /// This data is owned by this union so it must be freed once uploaded.
    Pending(Pending),

    /// This is the same as the pending states but there is
    /// a texture already allocated that we want to replace.
    Replace(Replace),

    /// The image is uploaded and ready to be used.
    Ready(Texture),

    /// The image isn't uploaded yet but is scheduled to be unloaded.
    UnloadPending(Pending),
    /// The image is uploaded and is scheduled to be unloaded.
    UnloadReady(Texture),
    /// The image is uploaded and scheduled to be replaced
    /// with new data, but it's also scheduled to be unloaded.
    UnloadReplace(Replace),
}

/// Zig: `Image.Replace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replace {
    pub texture: Texture,
    pub pending: Pending,
}

/// Pending image data that needs to be uploaded to the GPU.
/// (Zig: `Image.Pending`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub height: u32,
    pub width: u32,
    pub pixel_format: PixelFormat,

    /// Data is always expected to be (width * height * bpp).
    pub data: *mut u8,
}

// SAFETY: pending data is either owned by the image (and moved with it)
// or borrowed for the duration of a `prep_image` call, like in Zig.
unsafe impl Send for Pending {}

/// Zig: `Image.Pending.PixelFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 1 byte per pixel grayscale.
    Gray,
    /// 2 bytes per pixel grayscale + alpha.
    GrayAlpha,
    /// 3 bytes per pixel RGB.
    Rgb,
    /// 3 bytes per pixel BGR.
    Bgr,
    /// 4 byte per pixel RGBA.
    Rgba,
    /// 4 byte per pixel BGRA.
    Bgra,
}

impl PixelFormat {
    /// Get bytes per pixel for this format.
    #[inline]
    pub fn bpp(self) -> usize {
        match self {
            PixelFormat::Gray => 1,
            PixelFormat::GrayAlpha => 2,
            PixelFormat::Rgb => 3,
            PixelFormat::Bgr => 3,
            PixelFormat::Rgba => 4,
            PixelFormat::Bgra => 4,
        }
    }
}

impl Pending {
    /// An empty pending image (no data).
    pub const EMPTY: Pending = Pending {
        height: 0,
        width: 0,
        pixel_format: PixelFormat::Rgba,
        data: std::ptr::null_mut(),
    };

    /// Take ownership of `data` as pending image data.
    pub fn from_owned(width: u32, height: u32, pixel_format: PixelFormat, data: Vec<u8>) -> Pending {
        let p = Pending {
            width,
            height,
            pixel_format,
            data: std::ptr::null_mut(),
        };
        debug_assert_eq!(p.len(), data.len());
        Pending {
            data: alloc_data(data),
            ..p
        }
    }

    pub fn data_slice(&self) -> &[u8] {
        if self.data.is_null() {
            return &[];
        }
        // SAFETY: data always points at `len()` bytes.
        unsafe { std::slice::from_raw_parts(self.data, self.len()) }
    }

    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize * self.pixel_format.bpp()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Converts the image data and replaces it with a format that can be uploaded to the GPU.
    /// If the data is already in a format that can be uploaded, this is a
    /// no-op.
    /// Use `convertCopy()` to convert and copy in a single-pass, such as for owning the data to upload.
    fn convert_replace(&mut self) -> Result<(), wuffs::Error> {
        // As things stand, we currently convert all images to RGBA before
        // uploading to the GPU. This just makes things easier. In the future
        // we may want to support other formats.
        if self.pixel_format == PixelFormat::Rgba {
            return Ok(());
        }
        // If the pending data isn't RGBA we'll need to swizzle it.
        let data = self.data_slice();
        let rgba = match self.pixel_format {
            PixelFormat::Gray => wuffs::swizzle::g_to_rgba(data),
            PixelFormat::GrayAlpha => wuffs::swizzle::ga_to_rgba(data),
            PixelFormat::Rgb => wuffs::swizzle::rgb_to_rgba(data),
            PixelFormat::Bgr => wuffs::swizzle::bgr_to_rgba(data),
            PixelFormat::Rgba => unreachable!(),
            PixelFormat::Bgra => wuffs::swizzle::bgra_to_rgba(data),
        }?;
        // SAFETY: the data being replaced is owned (see `prepForUpload`).
        unsafe { free_data(*self) };
        self.data = alloc_data(rgba);
        self.pixel_format = PixelFormat::Rgba;
        Ok(())
    }

    /// Converts the image data to a copy with a format that can be uploaded to the GPU.
    /// If the data is already owned, use `convertReplace()` to be a no-op for data already
    /// in a format that can be uploaded.
    fn convert_copy(&self) -> Result<Image, wuffs::Error> {
        // As things stand, we currently convert all images to RGBA before
        // uploading to the GPU. This just makes things easier. In the future
        // we may want to support other formats.
        let data = self.data_slice();
        let rgba = match self.pixel_format {
            PixelFormat::Gray => wuffs::swizzle::g_to_rgba(data),
            PixelFormat::GrayAlpha => wuffs::swizzle::ga_to_rgba(data),
            PixelFormat::Rgb => wuffs::swizzle::rgb_to_rgba(data),
            PixelFormat::Bgr => wuffs::swizzle::bgr_to_rgba(data),
            PixelFormat::Rgba => Ok(data.to_vec()),
            PixelFormat::Bgra => wuffs::swizzle::bgra_to_rgba(data),
        }?;
        Ok(Image::Pending(Pending::from_owned(
            self.width,
            self.height,
            PixelFormat::Rgba,
            rgba,
        )))
    }

    /// Prepare the pending image data for upload to the GPU.
    /// This doesn't need GPU access so is safe to call any time.
    fn prep_for_upload(&mut self) -> Result<(), wuffs::Error> {
        self.convert_replace()
    }
}

/// Errors from [`Image::upload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadError {
    Wuffs(wuffs::Error),
    /// Texture creation failed, usually a GPU memory issue.
    UploadFailed,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Wuffs(e) => write!(f, "{e}"),
            UploadError::UploadFailed => f.write_str("UploadFailed"),
        }
    }
}

impl std::error::Error for UploadError {}

impl From<wuffs::Error> for UploadError {
    fn from(e: wuffs::Error) -> Self {
        UploadError::Wuffs(e)
    }
}

impl Image {
    /// Free the image's resources (Zig: `deinit`). The image must not be
    /// used afterwards.
    pub fn deinit(self) {
        match self {
            // SAFETY: pending data stored in an image is owned by it.
            Image::Pending(p) | Image::UnloadPending(p) => unsafe { free_data(p) },

            Image::Replace(r) | Image::UnloadReplace(r) => {
                // SAFETY: as above.
                unsafe { free_data(r.pending) };
                r.texture.deinit();
            }

            Image::Ready(t) | Image::UnloadReady(t) => t.deinit(),
        }
    }

    /// Mark this image for unload whatever state it is in.
    pub fn mark_for_unload(&mut self) {
        *self = match *self {
            Image::UnloadPending(_) | Image::UnloadReplace(_) | Image::UnloadReady(_) => return,

            Image::Ready(t) => Image::UnloadReady(t),
            Image::Pending(p) => Image::UnloadPending(p),
            Image::Replace(r) => Image::UnloadReplace(r),
        };
    }

    /// Mark the current image to be replaced with a pending one. This will
    /// attempt to update the existing texture if we have one, otherwise it
    /// will act like a new upload.
    pub fn mark_for_replace(&mut self, img: Image) {
        debug_assert!(img.is_pending());

        // If we have pending data right now, free it.
        if let Some(p) = self.get_pending() {
            // SAFETY: pending data stored in an image is owned by it.
            unsafe { free_data(p) };
        }
        // If we have an existing texture, use it in the replace.
        if let Some(t) = self.get_texture() {
            *self = Image::Replace(Replace {
                texture: t,
                pending: img.get_pending().unwrap(),
            });
            return;
        }
        // Otherwise we just become a pending image.
        *self = Image::Pending(img.get_pending().unwrap());
    }

    /// Returns true if this image is pending upload.
    pub fn is_pending(&self) -> bool {
        self.get_pending().is_some()
    }

    /// Returns true if this image has an associated texture.
    pub fn has_texture(&self) -> bool {
        self.get_texture().is_some()
    }

    /// Returns true if this image is marked for unload.
    pub fn is_unloading(&self) -> bool {
        match self {
            Image::UnloadPending(_) | Image::UnloadReplace(_) | Image::UnloadReady(_) => true,
            Image::Pending(_) | Image::Replace(_) | Image::Ready(_) => false,
        }
    }

    /// Upload the pending image to the GPU and change the state of this
    /// image to ready.
    pub fn upload<G: GraphicsApi>(&mut self, api: &G) -> Result<(), UploadError> {
        debug_assert!(self.is_pending());

        // Get our pending info
        let p = self.get_pending_pointer().unwrap();

        // No error recover is required after this call because it just
        // converts in place and is idempotent.
        p.prep_for_upload()?;

        // Create our texture
        let texture = Texture::init(
            api.image_texture_options(api::ImageTextureFormat::Rgba, true),
            p.width as usize,
            p.height as usize,
            Some(p.data_slice()),
        )
        .map_err(|_| UploadError::UploadFailed)?;

        // Uploaded. We can now clear our data and change our state.
        //
        // NOTE: For the `replace` state, this will free the old texture.
        //       We don't currently actually replace the existing texture
        //       in-place but that is an optimization we can do later.
        self.deinit();
        *self = Image::Ready(texture);
        Ok(())
    }

    /// Returns any pending image data for this image that requires upload.
    ///
    /// If there is no pending data to upload, returns null.
    fn get_pending(&self) -> Option<Pending> {
        match self {
            Image::Pending(p) | Image::UnloadPending(p) => Some(*p),
            Image::Replace(r) | Image::UnloadReplace(r) => Some(r.pending),
            _ => None,
        }
    }

    /// Returns the texture for this image.
    ///
    /// If there is no texture for it yet, returns null.
    fn get_texture(&self) -> Option<Texture> {
        match self {
            Image::Ready(t) | Image::UnloadReady(t) => Some(*t),
            Image::Replace(r) | Image::UnloadReplace(r) => Some(r.texture),
            _ => None,
        }
    }

    // Same as getPending but returns a pointer instead of a copy.
    fn get_pending_pointer(&mut self) -> Option<&mut Pending> {
        match self {
            Image::Pending(p) | Image::UnloadPending(p) => Some(p),
            Image::Replace(r) | Image::UnloadReplace(r) => Some(&mut r.pending),
            _ => None,
        }
    }

    /// The pending data (Zig: `image.pending` field access in tests).
    pub fn pending(&self) -> Option<&Pending> {
        match self {
            Image::Pending(p) => Some(p),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::kitty::graphics_animation::{Animation, Frame as AnimationFrame};
    use crate::terminal::kitty::graphics_command::{DeleteAction, DeleteId};
    use crate::terminal::kitty::graphics_image::{Data as ImageData, Image as KittyImage};
    use crate::terminal::kitty::graphics_storage::{
        Location, Placement as StoragePlacement, PlacementId, PlacementKey, PlacementTag, Relative, TerminalInfo,
    };
    use crate::terminal::modes::Mode;
    use crate::terminal::page_list::PinPtr;
    use crate::terminal::point;
    use crate::terminal::render::Dirty;
    use crate::terminal::screen_set::ScreenPtr;
    use crate::terminal::sgr;
    use crate::terminal::terminal::Options as TerminalOptions;

    fn term(cols: u16, rows: u16, px: u32) -> Terminal {
        let mut t = Terminal::init(TerminalOptions::new(cols, rows)).unwrap();
        t.width_px = px;
        t.height_px = px;
        t
    }

    fn add_image(t: &mut Terminal, img: KittyImage) {
        let mut s: ScreenPtr = t.screens.active;
        let s = &mut *s;
        s.kitty_images.add_image(&mut s.pages, img).unwrap();
    }

    fn add_pending_image(t: &mut Terminal, id: u32) -> crate::terminal::kitty::graphics_storage::PendingImage {
        let mut s: ScreenPtr = t.screens.active;
        let s = &mut *s;
        s.kitty_images
            .add_pending_image(
                &mut s.pages,
                KittyImage {
                    id,
                    width: 1,
                    height: 1,
                    format: KittyFormat::Rgba,
                    data: ImageData::Pending(4),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    fn add_placement(t: &mut Terminal, image_id: u32, placement_id: u32, p: StoragePlacement) {
        let mut s: ScreenPtr = t.screens.active;
        let s = &mut *s;
        s.kitty_images.add_placement(&mut s.pages, image_id, placement_id, p).unwrap();
    }

    /// Track a pin at an active-area cell (or at the cursor if `None`).
    fn track_pin(t: &mut Terminal, at: Option<(u16, u32)>) -> PinPtr {
        let mut s: ScreenPtr = t.screens.active;
        let s = &mut *s;
        let pin = match at {
            Some((x, y)) => s.pages.pin(point::Point::Active(point::Coordinate { x, y })).unwrap(),
            // SAFETY: the cursor pin is tracked by the live page list.
            None => unsafe { *s.cursor.page_pin.as_ref() },
        };
        s.pages.track_pin(pin).unwrap()
    }

    /// A placement at `location` with every other field zero (the Zig
    /// struct defaults).
    fn placement(location: Location) -> StoragePlacement {
        StoragePlacement {
            location,
            x_offset: 0,
            y_offset: 0,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            columns: 0,
            rows: 0,
            z: 0,
        }
    }

    fn pin_placement(pin: PinPtr, columns: u32, rows: u32) -> StoragePlacement {
        StoragePlacement {
            columns,
            rows,
            ..placement(Location::Pin(pin))
        }
    }

    fn relative_placement(h: i32, v: i32, columns: u32, rows: u32, z: i32) -> StoragePlacement {
        let location = Location::Relative(Relative {
                parent: PlacementKey {
                    image_id: 1,
                    placement_id: PlacementId {
                        tag: PlacementTag::External,
                        id: 1,
                    },
                },
            horizontal_offset: h,
            vertical_offset: v,
        });
        StoragePlacement {
            columns,
            rows,
            z,
            ..placement(location)
        }
    }

    fn virtual_placement() -> StoragePlacement {
        placement(Location::Virtual)
    }

    fn rgb_image(id: u32, w: u32, h: u32) -> KittyImage {
        KittyImage {
            id,
            width: w,
            height: h,
            format: KittyFormat::Rgb,
            data: ImageData::Complete(vec![0u8; (w * h * 3) as usize]),
            ..Default::default()
        }
    }

    fn delete(t: &mut Terminal, cmd: DeleteAction) {
        let info = TerminalInfo::from_terminal(t);
        let mut s: ScreenPtr = t.screens.active;
        let s = &mut *s;
        s.kitty_images.delete(&mut s.pages, &info, cmd);
    }

    fn placeholder_colors(t: &mut Terminal) {
        t.set_attribute(sgr::Attribute::Fg256(1)).unwrap();
        t.set_attribute(sgr::Attribute::UnderlineColor256(1)).unwrap();
    }

    fn placeholder_cells(t: &mut Terminal, x: u16, y: u16) {
        // SAFETY: the active screen is live while `t` is.
        unsafe { t.screens.active.as_mut() }.cursor_absolute(x, y);
        t.print_string("\u{10EEEE}\u{0305}\u{0305}").unwrap();
    }

    fn mark_row_dirty(t: &Terminal, y: u32) {
        t.screens
            .active
            .pages
            .pin(point::Point::Active(point::Coordinate { x: 0, y }))
            .unwrap()
            .mark_dirty();
    }

    fn entry(state: &State, id: u32) -> Option<ImageEntry> {
        state.images.get(&Id::Kitty(id)).copied()
    }

    #[test]
    fn kitty_renderer_ignores_pending_payloads_and_removes_replaced_placements() {
        let mut t = term(3, 3, 30);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        let tracked = t.screens.active.pages.count_tracked_pins();
        let pending = add_pending_image(&mut t, 1);
        let pin = track_pin(&mut t, None);
        add_placement(&mut t, 1, 1, pin_placement(pin, 1, 1));

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(1usize, t.screens.active.kitty_images.placements.len());
        assert_eq!(0usize, state.kitty_placements.len());
        assert!(entry(&state, 1).is_none());

        let pixels = b"rgba".to_vec();
        // SAFETY: the active screen is live while `t` is.
        let storage = &mut unsafe { t.screens.active.as_mut() }.kitty_images;
        assert!(pending.complete(storage, pixels).is_ok());
        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(1usize, state.kitty_placements.len());
        assert_eq!(
            t.screens.active.kitty_images.image_by_id(1).unwrap().generation,
            entry(&state, 1).unwrap().generation
        );

        // A newer pending replacement deletes the native placement and marks the
        // copied texture data for unload.
        add_pending_image(&mut t, 1);
        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(0usize, t.screens.active.kitty_images.placements.len());
        assert_eq!(tracked, t.screens.active.pages.count_tracked_pins());
        assert_eq!(0usize, state.kitty_placements.len());
        assert!(entry(&state, 1).unwrap().image.is_unloading());
        state.deinit();
    }

    #[test]
    fn kitty_renderer_uses_the_intersected_source_rectangle() {
        let mut t = term(3, 3, 30);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 4, 3));
        let pin = track_pin(&mut t, None);
        add_placement(
            &mut t,
            1,
            1,
            StoragePlacement {
                source_x: 3,
                source_y: 1,
                ..placement(Location::Pin(pin))
            },
        );

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(1usize, state.kitty_placements.len());

        let placement = state.kitty_placements[0];
        assert_eq!(1u32, placement.width);
        assert_eq!(2u32, placement.height);
        assert_eq!(3u32, placement.source_x);
        assert_eq!(1u32, placement.source_y);
        assert_eq!(1u32, placement.source_width);
        assert_eq!(2u32, placement.source_height);
        state.deinit();
    }

    #[test]
    fn kitty_renderer_positions_relative_placements_from_the_parent_pin() {
        let mut t = term(10, 10, 100);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 1, 1));

        // Parent at (2, 1).
        let pin = track_pin(&mut t, Some((2, 1)));
        add_placement(&mut t, 1, 1, pin_placement(pin, 1, 1));

        // Child offset (3, 2) cells from the parent.
        add_placement(&mut t, 1, 2, relative_placement(3, 2, 1, 1, 1));

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(2usize, state.kitty_placements.len());

        // Sorted by z: parent (z=0) first, child (z=1) second.
        let parent = state.kitty_placements[0];
        assert_eq!(2i32, parent.x);
        assert_eq!(1i32, parent.y);
        let child = state.kitty_placements[1];
        assert_eq!(5i32, child.x);
        assert_eq!(3i32, child.y);

        // The update stored the generations it was built from, so nothing
        // needs another update.
        assert_eq!(render_state.image_placements_generation(), state.kitty_placements_generation);
        assert_eq!(t.screens.active.kitty_images.generation, state.kitty_storage_generation);
        assert!(!state.kitty_requires_update(&t, &render_state));
        state.deinit();
    }

    #[test]
    fn kitty_renderer_relative_placement_with_negative_offsets() {
        let mut t = term(10, 10, 100);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 1, 1));

        let pin = track_pin(&mut t, Some((2, 2)));
        add_placement(&mut t, 1, 1, pin_placement(pin, 1, 1));

        // Hangs up and to the left of the viewport but still has visible
        // cells at (0, 0), so it must be kept with a negative position.
        add_placement(&mut t, 1, 2, relative_placement(-3, -3, 2, 2, 1));

        // Entirely off the left edge of the screen: culled.
        add_placement(&mut t, 1, 3, relative_placement(-9, 0, 2, 2, 2));

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(2usize, state.kitty_placements.len());
        let child = state.kitty_placements[1];
        assert_eq!(-1i32, child.x);
        assert_eq!(-1i32, child.y);
        state.deinit();
    }

    #[test]
    fn kitty_renderer_positions_relative_placements_from_virtual_parent_placeholders() {
        let mut t = term(5, 5, 50);
        t.modes.set(Mode::GraphemeCluster, true);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 10, 10));
        add_placement(&mut t, 1, 1, virtual_placement());

        // Placeholder cells for the virtual placement at (1, 1) and
        // (3, 2): the minimum cell (1, 1) is the anchor for relative
        // placements rooted at the virtual placement.
        placeholder_colors(&mut t);
        placeholder_cells(&mut t, 1, 1);
        placeholder_cells(&mut t, 3, 2);

        // Child of the virtual placement, offset (1, 2) cells.
        add_placement(&mut t, 1, 2, relative_placement(1, 2, 1, 1, 5));

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(render_state.image_placements_generation(), state.kitty_placements_generation);
        assert_eq!(t.screens.active.kitty_images.generation, state.kitty_storage_generation);

        // Two placeholder runs (z=-1) plus the child (z=5), sorted by z.
        assert_eq!(3usize, state.kitty_placements.len());
        let child = state.kitty_placements[2];
        assert_eq!(5i32, child.z);
        assert_eq!(2i32, child.x);
        assert_eq!(3i32, child.y);
        state.deinit();
    }

    #[test]
    fn kitty_renderer_skips_updates_for_rows_without_placeholder_cells() {
        let mut t = term(5, 5, 50);
        t.modes.set(Mode::GraphemeCluster, true);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 10, 10));
        add_placement(&mut t, 1, 1, virtual_placement());
        placeholder_colors(&mut t);
        placeholder_cells(&mut t, 1, 1);
        t.set_attribute(sgr::Attribute::Unset).unwrap();

        render_state.update(&mut t);
        assert!(state.kitty_requires_update(&t, &render_state));
        state.kitty_update(&t, &render_state);
        assert_eq!(render_state.image_placements_generation(), state.kitty_placements_generation);
        assert_eq!(t.screens.active.kitty_images.generation, state.kitty_storage_generation);
        assert_eq!(1usize, state.kitty_placements.len());

        // A row without placeholder cells changes. The render state is
        // dirty, but its placements are not.
        mark_row_dirty(&t, 3);
        render_state.update(&mut t);
        assert!(render_state.dirty != Dirty::False);
        assert!(!state.kitty_requires_update(&t, &render_state));

        // Overwriting the placeholder cells removes the image.
        // SAFETY: the active screen is live while `t` is.
        unsafe { t.screens.active.as_mut() }.cursor_absolute(1, 1);
        t.print_string("x").unwrap();
        render_state.update(&mut t);
        assert!(state.kitty_requires_update(&t, &render_state));
        state.kitty_update(&t, &render_state);
        assert_eq!(0usize, state.kitty_placements.len());
        state.deinit();
    }

    #[test]
    fn kitty_renderer_skips_updates_for_rows_without_placeholder_cells_with_fixed_placements() {
        let mut t = term(5, 5, 50);
        t.modes.set(Mode::GraphemeCluster, true);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 10, 10));

        // A virtual placement shown with placeholder cells on row 1, and a
        // fixed placement at (3, 0).
        add_placement(&mut t, 1, 1, virtual_placement());
        let pin = track_pin(&mut t, Some((3, 0)));
        add_placement(&mut t, 1, 2, pin_placement(pin, 1, 1));
        placeholder_colors(&mut t);
        placeholder_cells(&mut t, 1, 1);
        t.set_attribute(sgr::Attribute::Unset).unwrap();

        render_state.update(&mut t);
        assert!(state.kitty_requires_update(&t, &render_state));
        state.kitty_update(&t, &render_state);
        assert_eq!(2usize, state.kitty_placements.len());

        // A row without placeholder cells changes. The render state is
        // dirty, but neither placement is affected.
        mark_row_dirty(&t, 3);
        render_state.update(&mut t);
        assert!(render_state.dirty != Dirty::False);
        assert!(!state.kitty_requires_update(&t, &render_state));
        state.deinit();
    }

    #[test]
    fn kitty_renderer_unloads_the_texture_of_a_deleted_image_without_placements() {
        let mut t = term(3, 3, 30);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(&mut t, rgb_image(1, 1, 1));
        let pin = track_pin(&mut t, None);
        add_placement(&mut t, 1, 1, pin_placement(pin, 1, 1));
        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        assert_eq!(1usize, state.kitty_placements.len());

        // Delete the placement but keep the image. The texture stays.
        delete(
            &mut t,
            DeleteAction::Id(DeleteId {
                image_id: 1,
                placement_id: 1,
                delete: false,
            }),
        );
        assert!(t.screens.active.kitty_images.image_by_id(1).is_some());
        render_state.update(&mut t);
        assert!(state.kitty_requires_update(&t, &render_state));
        state.kitty_update(&t, &render_state);
        assert_eq!(0usize, state.kitty_placements.len());
        assert!(!entry(&state, 1).unwrap().image.is_unloading());

        // Delete the image. The render state's placements don't change, but
        // the texture must still be unloaded.
        let placements_generation = render_state.image_placements_generation();
        delete(
            &mut t,
            DeleteAction::Id(DeleteId {
                image_id: 1,
                placement_id: 0,
                delete: true,
            }),
        );
        assert!(t.screens.active.kitty_images.image_by_id(1).is_none());
        render_state.update(&mut t);
        assert_eq!(placements_generation, render_state.image_placements_generation());
        assert!(state.kitty_requires_update(&t, &render_state));
        state.kitty_update(&t, &render_state);
        assert!(entry(&state, 1).unwrap().image.is_unloading());
        assert!(!state.kitty_requires_update(&t, &render_state));
        state.deinit();
    }

    #[test]
    fn kitty_renderer_uploads_the_current_animation_frame() {
        let mut t = term(3, 3, 30);

        let mut state = State::default();
        let mut render_state = RenderState::empty();

        add_image(
            &mut t,
            KittyImage {
                id: 1,
                width: 1,
                height: 1,
                format: KittyFormat::Rgba,
                data: ImageData::Complete(vec![255, 0, 0, 255]),
                ..Default::default()
            },
        );
        let pin = track_pin(&mut t, None);
        add_placement(&mut t, 1, 1, pin_placement(pin, 1, 1));

        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        let gen1 = entry(&state, 1).unwrap().generation;
        assert_eq!(
            &[255u8, 0, 0, 255][..],
            entry(&state, 1).unwrap().image.pending().unwrap().data_slice()
        );

        // Attach an animation and make its extra frame current, the way
        // an animation tick would.
        {
            // SAFETY: the active screen is live while `t` is.
            let storage = &mut unsafe { t.screens.active.as_mut() }.kitty_images;
            let img = storage.images.get_mut(&1).unwrap();
            let mut anim = Box::new(Animation::default());
            anim.frames.push(AnimationFrame {
                data: vec![0, 0, 255, 255],
                gap_ms: 40,
                ..Default::default()
            });
            anim.current_index = 1;
            img.animation = Some(anim);
            storage.mark_image_content_changed(1);
        }

        // The renderer must pick up the frame's pixels under a fresh
        // generation.
        render_state.update(&mut t);
        state.kitty_update(&t, &render_state);
        let e = entry(&state, 1).unwrap();
        assert!(e.generation > gen1);
        assert_eq!(&[0u8, 0, 255, 255][..], e.image.pending().unwrap().data_slice());
        state.deinit();
    }
}
