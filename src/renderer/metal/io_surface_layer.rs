//! Port of `src/renderer/metal/IOSurfaceLayer.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! A wrapper around a CALayer with a utility method
//! for settings its `contents` to an IOSurface.

use std::ffi::{c_ulong, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::pkg::macos;
use crate::pkg::macos::iosurface::IOSurface;

use super::objc;
use super::Error;

const LOG: &str = "IOSurfaceLayer";

#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}

/// We subclass CALayer with a custom display handler, we only need
/// to make the subclass once, and then we can use it as a singleton.
static SUBCLASS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Debug)]
pub struct IOSurfaceLayer {
    /// The underlying CALayer
    pub layer: objc::Object,
}

/// Zig: `?*align(8) const fn (?*anyopaque) void`.
pub type DisplayCallback = Option<fn(*mut c_void)>;

impl IOSurfaceLayer {
    pub fn init() -> Result<IOSurfaceLayer, Error> {
        // The layer returned by `[CALayer layer]` is autoreleased, which means
        // that at the end of the current autorelease pool it will be deallocated
        // if it isn't retained, so we retain it here manually an extra time.
        let subclass = get_subclass()?;
        // SAFETY: valid class; `layer` returns a CALayer.
        let layer = unsafe {
            let layer: objc::Object = subclass.msg_send(objc::sel(c"layer"), ());
            layer.retain()
        };

        // SAFETY: valid layer of our subclass (which has the ivars).
        unsafe {
            // The layer gravity is set to top-left so that the contents aren't
            // stretched during resize operations before a new frame has been drawn.
            layer.set_property(c"contentsGravity", macos::animation::kCAGravityTopLeft);

            layer.set_instance_variable(c"display_cb", objc::Object::from_id(std::ptr::null_mut()));
            layer.set_instance_variable(c"display_ctx", objc::Object::from_id(std::ptr::null_mut()));
        }

        Ok(IOSurfaceLayer { layer })
    }

    pub fn release(&mut self) {
        // The layer may be retained by the view after we release our reference.
        // Clear the callback first so that a later display pass can't access the
        // renderer that owned this wrapper after it has been freed.
        self.set_display_callback(None, std::ptr::null_mut());
        self.layer.release();
    }

    /// Sets the layer's `contents` to the provided IOSurface.
    ///
    /// Makes sure to do so on the main thread to avoid visual artifacts.
    #[inline]
    pub fn set_surface(&mut self, surface: &IOSurface) -> Result<(), Error> {
        // We retain the surface to make sure it's not GC'd
        // before we can set it as the contents of the layer.
        //
        // We release in the callback after setting the contents.
        surface.retain();
        // NOTE: Since `self.layer` is captured as an object, it's
        //       automatically retained when the block is copied, so we
        //       don't need to retain it ourselves like with the surface.

        let mut block = objc::Block::init(
            SetSurfaceCaptures {
                layer: self.layer.value,
                surface: surface as *const IOSurface,
            },
            set_surface_callback as *const c_void,
            &SET_SURFACE_DESCRIPTOR,
        );

        // We check if we're on the main thread and run the block directly if so.
        let ns_thread = objc::get_class(c"NSThread").ok_or(Error::ObjCFailed)?;
        // SAFETY: `isMainThread` is a BOOL class property.
        let is_main: objc::BOOL = unsafe { ns_thread.msg_send(objc::sel(c"isMainThread"), ()) };
        if is_main != objc::NO {
            // SAFETY: the block is valid.
            unsafe { set_surface_callback(&block) };
        } else {
            // NOTE: The block will be copied when we pass it to dispatch_async,
            //       and then automatically be deallocated by the objc runtime
            //       once it's executed.
            // SAFETY: a valid stack block; dispatch_async copies it.
            unsafe {
                macos::dispatch::dispatch_async(
                    macos::dispatch::queue::get_main(),
                    (&mut block as *mut objc::Block<SetSurfaceCaptures>).cast(),
                );
            }
        }
        Ok(())
    }

    /// Sets the layer's `contents` to the provided IOSurface.
    ///
    /// Does not ensure this happens on the main thread.
    #[inline]
    pub fn set_surface_sync(&mut self, surface: &IOSurface) {
        // SAFETY: valid layer; `contents` takes an object.
        unsafe { self.layer.set_property(c"contents", surface.as_ptr()) };
    }

    pub fn set_display_callback(&mut self, display_cb: DisplayCallback, display_ctx: *mut c_void) {
        let cb_ptr: objc::Id = match display_cb {
            Some(f) => f as *mut c_void,
            None => std::ptr::null_mut(),
        };
        // SAFETY: our subclass has these ivars.
        unsafe {
            self.layer
                .set_instance_variable(c"display_cb", objc::Object::from_id(cb_ptr));
            self.layer
                .set_instance_variable(c"display_ctx", objc::Object::from_id(display_ctx));
        }
    }
}

/// Zig: `SetSurfaceBlock` captures.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SetSurfaceCaptures {
    pub layer: objc::Id,
    pub surface: *const IOSurface,
}

unsafe extern "C" fn set_surface_copy(dst: *mut c_void, src: *const c_void) {
    let dst = dst.cast::<objc::Block<SetSurfaceCaptures>>();
    let src = src.cast::<objc::Block<SetSurfaceCaptures>>();
    // SAFETY: called by the blocks runtime with valid blocks.
    unsafe { objc::block_retain_object(&mut (*dst).captures.layer, (*src).captures.layer) };
}

unsafe extern "C" fn set_surface_dispose(src: *const c_void) {
    let src = src.cast::<objc::Block<SetSurfaceCaptures>>();
    // SAFETY: called by the blocks runtime with a valid block.
    unsafe { objc::block_release_object((*src).captures.layer) };
}

static SET_SURFACE_DESCRIPTOR: objc::BlockDescriptor = objc::BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<objc::Block<SetSurfaceCaptures>>() as c_ulong,
    copy_helper: Some(set_surface_copy),
    dispose_helper: Some(set_surface_dispose),
};

unsafe extern "C" fn set_surface_callback(block: *const objc::Block<SetSurfaceCaptures>) {
    // SAFETY: called with a valid block whose surface was retained.
    unsafe {
        let layer = objc::Object::from_id((*block).captures.layer);
        let surface: &IOSurface = &*(*block).captures.surface;

        // See explanation of why we retain and release in `setSurface`.
        struct Release<'a>(&'a IOSurface);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.release();
            }
        }
        let _release = Release(surface);

        // We check to see if the surface is the appropriate size for
        // the layer, if it's not then we discard it. This is because
        // asynchronously drawn frames can sometimes finish just after
        // a synchronously drawn frame during a resize, and if we don't
        // discard the improperly sized surface it creates jank.
        let bounds: macos::graphics::Rect = layer.get_property(c"bounds");
        let scale: f64 = layer.get_property(c"contentsScale");
        let width = (bounds.size.width * scale) as usize;
        let height = (bounds.size.height * scale) as usize;
        if width != surface.get_width() || height != surface.get_height() {
            log::debug!(
                target: LOG,
                "setSurfaceCallback(): surface is wrong size for layer, discarding. surface = {}x{}, layer = {}x{}",
                surface.get_width(),
                surface.get_height(),
                width,
                height,
            );
            return;
        }

        layer.set_property(c"contents", surface.as_ptr());
    }
}

/// `-[IOSurfaceLayer display]`
unsafe extern "C" fn display(target: objc::Id, _sel: objc::SEL) {
    let this = objc::Object::from_id(target);
    // SAFETY: our subclass has these ivars; `display_cb` holds a
    // `DisplayCallback` function pointer or null.
    unsafe {
        let cb_ptr = this.get_instance_variable(c"display_cb").value;
        if !cb_ptr.is_null() {
            let cb: fn(*mut c_void) = std::mem::transmute::<*mut c_void, fn(*mut c_void)>(cb_ptr);
            cb(this.get_instance_variable(c"display_ctx").value);
        }
    }
}

/// `-[IOSurfaceLayer actionForKey:]`
unsafe extern "C" fn action_for_key(_target: objc::Id, _sel: objc::SEL, _key: objc::Id) -> objc::Id {
    // SAFETY: NSNull is always available.
    unsafe {
        objc::get_class(c"NSNull")
            .expect("NSNull")
            .msg_send(objc::sel(c"null"), ())
    }
}

fn get_subclass() -> Result<objc::Class, Error> {
    let existing = SUBCLASS.load(Ordering::Acquire);
    if !existing.is_null() {
        return Ok(objc::Class { value: existing });
    }

    let ca_layer = objc::get_class(c"CALayer").ok_or(Error::ObjCFailed)?;

    let subclass = objc::allocate_class_pair(ca_layer, c"IOSurfaceLayer").ok_or(Error::ObjCFailed)?;

    if !subclass.add_ivar(c"display_cb") || !subclass.add_ivar(c"display_ctx") {
        objc::dispose_class_pair(subclass);
        return Err(Error::ObjCFailed);
    }

    // SAFETY: the implementations match the type encodings.
    unsafe {
        subclass.replace_method(c"display", display as *const c_void, c"v@:");

        // Disable all animations for this layer by returning null for all actions.
        subclass.replace_method(c"actionForKey:", action_for_key as *const c_void, c"@@:@");
    }

    objc::register_class_pair(subclass);

    SUBCLASS.store(subclass.value, Ordering::Release);

    Ok(subclass)
}
