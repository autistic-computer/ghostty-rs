//! Port of `src/renderer/Options.zig`.
//!
//! STATUS: ported
//!
//! The options that are used to configure a renderer.
//!
//! Pointers to objects owned elsewhere (device, font grid, apprt surface,
//! renderer thread) stay raw pointers like in Zig.

use crate::apprt;
use crate::font::SharedGrid;
use crate::renderer::generic::DerivedConfig as RendererDerivedConfig;
use crate::renderer::{Device, Size, Thread};

#[derive(Debug)]
pub struct Options {
    /// The device state that corresponds to the physical GPU
    /// we'd like to render to.
    pub device: *mut Device,

    /// The derived configuration for this renderer implementation.
    pub config: RendererDerivedConfig,

    /// The font grid that should be used along with the key for deref-ing.
    pub font_grid: *mut SharedGrid,

    /// The size of everything.
    pub size: Size,

    /// The mailbox for sending the surface messages. This is only valid
    /// once the thread has started and should not be used outside of the thread.
    pub surface_mailbox: apprt::surface::Mailbox,

    /// The apprt surface.
    pub rt_surface: *mut apprt::Surface,

    /// The renderer thread.
    pub thread: *mut Thread,
}
