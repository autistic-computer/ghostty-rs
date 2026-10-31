//! Port of `src/apprt/gtk.zig`.
//!
//! STATUS: ported
//!
//! The GTK apprt is only built with the `gtk` Cargo feature. This module
//! itself always exists (`apprt::rt` names it on Linux); its ported children
//! each start with `#![cfg(feature = "gtk")]`.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "gtk/adw_version.rs"]
pub mod adw_version;
#[path = "gtk/app.rs"]
pub mod app;
#[path = "gtk/build/mod.rs"]
pub mod build;
#[path = "gtk/cgroup.rs"]
pub mod cgroup;
#[path = "gtk/class.rs"]
pub mod class;
#[path = "gtk/desktop_notifications.rs"]
pub mod desktop_notifications;
#[path = "gtk/ext.rs"]
pub mod ext;
#[path = "gtk/flatpak.rs"]
pub mod flatpak;
#[path = "gtk/gsettings.rs"]
pub mod gsettings;
#[path = "gtk/gtk_version.rs"]
pub mod gtk_version;
#[path = "gtk/ipc/mod.rs"]
pub mod ipc;
#[path = "gtk/key.rs"]
pub mod key;
#[path = "gtk/media.rs"]
pub mod media;
#[path = "gtk/portal.rs"]
pub mod portal;
#[path = "gtk/post_fork.rs"]
pub mod post_fork;
#[path = "gtk/pre_exec.rs"]
pub mod pre_exec;
#[path = "gtk/scale.rs"]
pub mod scale;
#[path = "gtk/surface.rs"]
pub mod surface;
#[path = "gtk/weak_ref.rs"]
pub mod weak_ref;
#[path = "gtk/winproto.rs"]
pub mod winproto;
// @generated-mods-end

// The required comptime API for any apprt.
#[cfg(feature = "gtk")]
pub use self::app::App;
#[cfg(feature = "gtk")]
pub use self::flatpak::resources_dir;
#[cfg(feature = "gtk")]
pub use self::surface::Surface;

// The exported API, custom for the apprt.
#[cfg(feature = "gtk")]
pub use self::weak_ref::WeakRef;

/// Wraps a GTK runtime app as [`crate::apprt::App`] (the identity, except in
/// test builds where `apprt::App` is the recording stand-in).
#[cfg(all(feature = "gtk", not(test)))]
pub fn wrap_app(app: App) -> crate::apprt::App {
    app
}

/// See the non-test variant.
#[cfg(all(feature = "gtk", test))]
pub fn wrap_app(app: App) -> crate::apprt::App {
    crate::apprt::standin::App {
        rt: Some(app),
        ..Default::default()
    }
}

/// Wraps a GTK runtime surface as [`crate::apprt::Surface`] (see
/// [`wrap_app`]).
#[cfg(all(feature = "gtk", not(test)))]
pub fn wrap_surface(surface: Surface) -> crate::apprt::Surface {
    surface
}

/// See the non-test variant.
#[cfg(all(feature = "gtk", test))]
pub fn wrap_surface(surface: Surface) -> crate::apprt::Surface {
    crate::apprt::standin::Surface {
        rt: Some(surface),
        ..Default::default()
    }
}

/// The GTK runtime app of a [`crate::apprt::App`].
#[cfg(all(feature = "gtk", not(test)))]
pub fn rt_app(app: &crate::apprt::App) -> &App {
    app
}

/// See the non-test variant.
#[cfg(all(feature = "gtk", test))]
pub fn rt_app(app: &crate::apprt::App) -> &App {
    app.rt.as_ref().expect("app has a GTK runtime app")
}

/// The GTK runtime surface of a [`crate::apprt::Surface`] (Zig:
/// `core.rt_surface`).
#[cfg(all(feature = "gtk", not(test)))]
pub fn rt_surface(surface: &crate::apprt::Surface) -> &Surface {
    surface
}

/// See the non-test variant.
#[cfg(all(feature = "gtk", test))]
pub fn rt_surface(surface: &crate::apprt::Surface) -> &Surface {
    surface.rt.as_ref().expect("surface has a GTK runtime surface")
}

/// The GObject surface of a core surface (Zig:
/// `core.rt_surface.surface`/`core.rt_surface.gobj()`).
///
/// # Safety
/// `core` must be a live core surface created by the GTK apprt.
#[cfg(feature = "gtk")]
pub unsafe fn gobj_of(core: *const crate::surface::Surface) -> class::surface::Surface {
    rt_surface(&*(*core).rt_surface).gobj()
}

#[cfg(all(test, feature = "gtk"))]
mod tests {
    #[test]
    fn ref_all_decls() {
        // Zig: `refAllDecls` plus imports of the tested sibling files. The
        // Rust module tree compiles (and tests) every child module.
        let _ = super::build::info::APPLICATION_ID;
    }
}
