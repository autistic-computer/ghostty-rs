//! Port of `src/renderer/opengl/Device.zig`.
//!
//! STATUS: ported
//!
//! The OpenGL device context.
//!
//! This holds the EGL display and config used to create EGL
//! contexts, each corresponding to a surface, its renderer
//! object and its render thread.
//!
//! TODO: If there's a way to prefer certain devices like in
//! Vulkan or Metal we should do it here.

use crate::pkg::opengl::egl;

/// Options used when creating the OpenGL device.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Render with the provided EGL display.
    /// Caller is responsible for managing the lifetime of the display.
    /// The default EGL display will be used when null.
    pub display: Option<egl::Display>,
}

/// Errors from [`Device::init`] (Zig: the inferred error set of `egl.load`
/// and the EGL calls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    EglInitFailed,
    Egl(egl::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::EglInitFailed => f.write_str("EglInitFailed"),
            Error::Egl(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<egl::LoadError> for Error {
    fn from(_: egl::LoadError) -> Self {
        Error::EglInitFailed
    }
}

impl From<egl::Error> for Error {
    fn from(e: egl::Error) -> Self {
        Error::Egl(e)
    }
}

#[derive(Debug)]
pub struct Device {
    /// The EGL display.
    ///
    /// The lifecycle of this display is always managed externally, both in the
    /// cases where we were given a display as a config option, or we use the
    /// systemwide default display that is cleaned up by the OS.
    pub display: egl::Display,

    /// The EGL config used to create surface renderer contexts. Chosen
    /// once here so that all contexts share compatible capabilities.
    pub config: egl::Config,
}

impl Device {
    pub fn init(options: Options) -> Result<Box<Device>, Error> {
        egl::load()?;

        let display = match options.display {
            Some(d) => d,
            None => egl::Display::init_platform(
                egl::c::EGL_PLATFORM_SURFACELESS_MESA,
                egl::c::EGL_DEFAULT_DISPLAY,
                None,
            )
            .inspect_err(|&err| {
                if err == egl::Error::BadParameter {
                    log_unsupported_platform();
                }
            })?,
        };

        log::info!(
            target: "opengl",
            "EGL vendor={}",
            display
                .query_string(egl::StringQuery::Vendor)
                .map_or("(unknown)".into(), |s| s.to_string_lossy())
        );
        log::info!(
            target: "opengl",
            "EGL extensions={}",
            display
                .query_string(egl::StringQuery::Extensions)
                .map_or("(unknown)".into(), |s| s.to_string_lossy())
        );

        // Choose a config. We need a config that is renderable with
        // OpenGL and a RGBA8 color buffer.
        let config = egl::Config::choose(
            display,
            &[
                egl::c::EGL_SURFACE_TYPE as egl::c::EGLint,
                0,
                egl::c::EGL_RENDERABLE_TYPE as egl::c::EGLint,
                egl::c::EGL_OPENGL_BIT as egl::c::EGLint,
                egl::c::EGL_RED_SIZE as egl::c::EGLint,
                8,
                egl::c::EGL_GREEN_SIZE as egl::c::EGLint,
                8,
                egl::c::EGL_BLUE_SIZE as egl::c::EGLint,
                8,
                egl::c::EGL_ALPHA_SIZE as egl::c::EGLint,
                8,
                egl::c::EGL_NONE as egl::c::EGLint,
            ],
        )
        .inspect_err(|err| {
            log::warn!(target: "opengl", "failed to choose config err={err}");
        })?;

        Ok(Box::new(Device { display, config }))
    }

    pub fn deinit(self: Box<Self>) {
        // Do not destroy the EGL display as it is externally managed.
        drop(self);
    }
}

/// EGL_BAD_PARAMETER from eglGetPlatformDisplay means that no loaded
/// EGL driver supports the surfaceless platform. Most often that is
/// because no driver could be loaded at all, for example because the
/// system's driver needs a newer libc than Ghostty was built against,
/// so say which it is rather than leaving a bare error name.
fn log_unsupported_platform() {
    // With no driver loaded, libglvnd reports no client extensions
    // at all, since every platform comes from a driver.
    let exts = egl::query_client_extensions().map_or(&[][..], |s| s.to_bytes());
    if exts.is_empty() {
        log::error!(
            target: "opengl",
            "no EGL driver could be loaded; check that a GPU driver \
             is installed and was built against a compatible libc \
             (LD_DEBUG=libs shows why a driver failed to load)"
        );
        return;
    }

    if !egl::has_extension(exts, b"EGL_MESA_platform_surfaceless") {
        log::error!(
            target: "opengl",
            "the EGL driver does not support EGL_MESA_platform_surfaceless \
             client extensions={}",
            String::from_utf8_lossy(exts)
        );
        return;
    }

    log::error!(
        target: "opengl",
        "the EGL driver rejected the surfaceless platform; \
         client extensions={}",
        String::from_utf8_lossy(exts)
    );
}

/// Test support: a GL 4.3 core context made current on the calling
/// thread via a surfaceless EGL [`Device`] (the same setup the OpenGL
/// renderer uses), with the GLAD context loaded.
#[cfg(test)]
pub(crate) mod test_context {
    use super::*;
    use crate::pkg::opengl as gl;

    pub(crate) struct TestContext {
        pub device: Box<Device>,
        pub context: egl::Context,
        /// Serializes with the `pkg::opengl::egl` tests, which terminate
        /// the (process-wide) surfaceless display. Dropped last.
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    /// Lock out the EGL tests that terminate the shared display.
    pub(crate) fn egl_lock() -> std::sync::MutexGuard<'static, ()> {
        egl::tests::EGL_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    impl TestContext {
        pub(crate) fn new() -> TestContext {
            let _guard = egl_lock();
            egl::load().unwrap();
            egl::bind_api(egl::c::EGL_OPENGL_API).unwrap();
            let device = Device::init(Options::default()).unwrap();
            let context = egl::Context::create(
                device.display,
                device.config,
                None,
                &[
                    egl::c::EGL_CONTEXT_MAJOR_VERSION as egl::c::EGLint,
                    4,
                    egl::c::EGL_CONTEXT_MINOR_VERSION as egl::c::EGLint,
                    3,
                    egl::c::EGL_CONTEXT_OPENGL_PROFILE_MASK as egl::c::EGLint,
                    egl::c::EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT as egl::c::EGLint,
                    egl::c::EGL_NONE as egl::c::EGLint,
                ],
            )
            .unwrap();
            device
                .display
                .make_current(None, None, Some(context))
                .unwrap();
            gl::glad::load(Some(egl::get_proc_address)).unwrap();
            TestContext {
                device,
                context,
                _guard,
            }
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            gl::glad::unload();
            self.device.display.release_current();
            let _ = self.context.destroy(self.device.display);
            // The display is shared and externally managed; never
            // terminate it here.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not a Zig test: Device.zig has none.
    #[test]
    fn init_surfaceless() {
        let _guard = test_context::egl_lock();
        let device = Device::init(Options::default()).unwrap();
        // A second device reuses the (singleton) default display.
        let device2 = Device::init(Options {
            display: Some(device.display),
        })
        .unwrap();
        assert_eq!(device.display, device2.display);
        device2.deinit();
        device.deinit();
    }
}
