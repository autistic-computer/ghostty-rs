//! Port of `pkg/opengl/glad.zig`.
//!
//! STATUS: ported

use std::cell::Cell;
use std::os::raw::{c_int, c_uint};

use super::c;

pub type Context = c::GladGLContext;

thread_local! {
    /// This is the current context. Set it with [`load`] prior to calling
    /// any of this package's functions. I know its nasty to have a global but
    /// this makes it match OpenGL API styles where it also operates on a
    /// threadlocal global.
    ///
    /// (Zig: `pub threadlocal var context: Context`.) Each [`load`]
    /// allocates a fresh context that is never freed, so references handed
    /// out by [`context`] stay valid even after [`unload`] or a reload; a
    /// context is a few KiB and loads are rare (once per renderer thread).
    static CONTEXT: Cell<Option<&'static Context>> = const { Cell::new(None) };
}

/// Returns the current thread's GL context.
///
/// Panics if [`load`] has not been called on this thread (the Zig global is
/// `undefined` then).
pub fn context() -> &'static Context {
    CONTEXT
        .with(Cell::get)
        .expect("opengl: glad context not loaded on this thread")
}

/// Calls a function of the current thread's GL context:
/// `call!(GenBuffers(1, &mut id))` is Zig's `glad.context.GenBuffers.?(1, &id)`.
///
/// Expands to an `unsafe` call; the caller is responsible for passing valid
/// arguments.
macro_rules! call {
    ($f:ident($($arg:expr),* $(,)?)) => {{
        let f = $crate::pkg::opengl::glad::context()
            .$f
            .expect(concat!("opengl: gl", stringify!($f), " is not loaded"));
        #[allow(unused_unsafe)]
        // SAFETY: function pointer loaded by glad for the current context.
        unsafe {
            f($($arg),*)
        }
    }};
}
pub(crate) use call;

/// Errors from [`load`] (Zig: `error.GLInitFailed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    GLInitFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Initialize Glad. This is guaranteed to succeed if no errors are returned.
///
/// The Zig version takes `anytype` so it can accept multiple forms of the
/// function: a `getProcAddress` function (e.g. glfw's or
/// [`egl::get_proc_address`](super::egl::get_proc_address)), or `null`,
/// which means that we are just loading the globally pointed gl functions
/// (glad's built-in dlopen loader).
pub fn load(get_proc_address: c::GLADloadfunc) -> Result<c_int, Error> {
    // SAFETY: GladGLContext is a plain C struct of nullable pointers.
    let mut ctx: Box<Context> = Box::new(unsafe { std::mem::zeroed() });
    // SAFETY: valid context storage; the loader is a valid C function.
    let res = unsafe {
        match get_proc_address {
            Some(_) => c::gladLoadGLContext(&mut *ctx, get_proc_address),
            None => c::gladLoaderLoadGLContext(&mut *ctx),
        }
    };
    if res == 0 {
        return Err(Error::GLInitFailed);
    }

    let ctx: &'static Context = Box::leak(ctx);
    CONTEXT.with(|c| c.set(Some(ctx)));
    Ok(res)
}

pub fn unload() {
    if let Some(ctx) = CONTEXT.with(|c| c.take()) {
        // gladLoaderUnloadGLContext mutates the context (it clears the
        // loader handle); unload a copy so outstanding `&'static Context`
        // references are never written through.
        // SAFETY: GladGLContext is a plain C struct (Copy).
        let mut copy: Context = *ctx;
        unsafe { c::gladLoaderUnloadGLContext(&mut copy) };
    }
}

pub fn version_major(res: c_uint) -> c_uint {
    // GLAD_VERSION_MAJOR(version) (version / 10000)
    res / 10000
}

pub fn version_minor(res: c_uint) -> c_uint {
    // GLAD_VERSION_MINOR(version) (version % 10000)
    res % 10000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(4, version_major(40003));
        assert_eq!(3, version_minor(40003));
    }

    #[test]
    #[should_panic(expected = "not loaded")]
    fn context_requires_load() {
        // A fresh test thread has no context.
        let _ = context();
    }

    // Loading against a real context is exercised by `egl::tests::gl_smoke`.
}
