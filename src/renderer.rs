//! Port of `src/renderer.zig`.
//!
//! STATUS: ported
//!
//! Renderer implementation and utilities. The renderer is responsible for
//! taking the internal screen state and turning into some output format,
//! usually for a screen.
//!
//! The renderer is closely tied to the windowing system which usually
//! has to prepare the window for the given renderer using system-specific
//! APIs. The renderers in this package assume that the renderer is already
//! setup (OpenGL has a context, Vulkan has a surface, etc.)

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "renderer/backend.rs"]
pub mod backend;
#[path = "renderer/cell.rs"]
pub mod cell;
#[path = "renderer/cursor.rs"]
pub mod cursor;
#[path = "renderer/display_link.rs"]
pub mod display_link;
#[path = "renderer/dmabuf.rs"]
pub mod dmabuf;
#[path = "renderer/generic.rs"]
pub mod generic;
#[path = "renderer/image.rs"]
pub mod image;
#[path = "renderer/link.rs"]
pub mod link;
#[path = "renderer/message.rs"]
pub mod message;
#[path = "renderer/metal.rs"]
pub mod metal;
#[path = "renderer/opengl.rs"]
pub mod opengl;
#[path = "renderer/options.rs"]
pub mod options;
#[path = "renderer/overlay.rs"]
pub mod overlay;
#[path = "renderer/row.rs"]
pub mod row;
#[path = "renderer/shadertoy.rs"]
pub mod shadertoy;
#[path = "renderer/size.rs"]
pub mod size;
#[path = "renderer/state.rs"]
pub mod state;
#[path = "renderer/thread.rs"]
pub mod thread;
// @generated-mods-end

pub use self::backend::Backend;
pub use self::cursor::style as cursor_style;
pub use self::cursor::Style as CursorStyle;
pub use self::message::Message;
#[cfg(target_os = "macos")]
pub use self::metal::Metal;
#[cfg(not(target_os = "macos"))]
pub use self::opengl::OpenGL;
pub use self::options::Options;
pub use self::overlay::Overlay;
pub use self::size::{CellSize, Coordinate, GridSize, Padding, ScreenSize, Size};
pub use self::state::State;
pub use self::thread::Thread;
pub use crate::lib;

/// The generic renderer, parameterized by graphics API.
pub use self::generic::Renderer as GenericRenderer;

/// The graphics API of this build (Zig: chosen by `build_config.renderer`).
#[cfg(target_os = "macos")]
pub type GraphicsApiImpl = Metal;
/// The graphics API of this build (Zig: chosen by `build_config.renderer`).
#[cfg(not(target_os = "macos"))]
pub type GraphicsApiImpl = OpenGL;

/// The module of the graphics API of this build (Zig:
/// `Renderer.API`, used as a namespace by `cell.zig` and `image.zig`).
#[cfg(target_os = "macos")]
pub use self::metal as graphics_api;
/// The module of the graphics API of this build (Zig:
/// `Renderer.API`, used as a namespace by `cell.zig` and `image.zig`).
#[cfg(not(target_os = "macos"))]
pub use self::opengl as graphics_api;

/// The implementation to use for the renderer. This is comptime chosen
/// so that every build has exactly one renderer implementation.
pub type Renderer = GenericRenderer<GraphicsApiImpl>;

/// The app-scoped render device from which surface renderers are created.
#[cfg(target_os = "macos")]
pub type Device = metal::device::Device;
/// The app-scoped render device from which surface renderers are created.
#[cfg(not(target_os = "macos"))]
pub type Device = opengl::device::Device;

/// The options of [`Device::init`] (Zig: `Device.Options`).
#[cfg(target_os = "macos")]
pub type DeviceOptions = metal::device::Options;
/// The options of [`Device::init`] (Zig: `Device.Options`).
#[cfg(not(target_os = "macos"))]
pub type DeviceOptions = opengl::device::Options;

crate::lib_enum! {
    /// The health status of a renderer. These must be shared across all
    /// renderers even if some states aren't reachable so that our API users
    /// can use the same enum for all renderers.
    pub enum Health: c {
        Healthy,
        Unhealthy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_h_health() {
        crate::lib::check_ghostty_h_enum::<Health>(
            "GHOSTTY_RENDERER_HEALTH_",
            include_str!("../include/ghostty.h"),
        )
        .unwrap();
    }
}
