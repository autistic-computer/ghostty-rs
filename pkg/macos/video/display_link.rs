//! Port of `src/macos/video/display_link.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

use super::c;
use crate::pkg::macos::{opaque_type, Error};

opaque_type!(
    /// `CVDisplayLinkRef`
    DisplayLink
);

/// The output callback type for [`DisplayLink::set_output_callback`].
pub type OutputCallback<Userdata> = fn(display_link: &DisplayLink, userdata: Option<&mut Userdata>);

impl DisplayLink {
    pub fn create_with_active_cg_displays() -> Result<&'static DisplayLink, Error> {
        let mut result: c::CVDisplayLinkRef = std::ptr::null_mut();
        // SAFETY: result is a valid out pointer.
        if unsafe { c::CVDisplayLinkCreateWithActiveCGDisplays(&mut result) } != c::kCVReturnSuccess
        {
            return Err(Error::CreationFailed);
        }

        // SAFETY: a non-null result is a display link (+1).
        unsafe { (result as *const DisplayLink).as_ref() }.ok_or(Error::CreationFailed)
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CVDisplayLinkRelease(self.as_ptr()) }
    }

    pub fn start(&self) -> Result<(), Error> {
        // SAFETY: self is live.
        if unsafe { c::CVDisplayLinkStart(self.as_ptr()) } != c::kCVReturnSuccess {
            return Err(Error::InvalidOperation);
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), Error> {
        // SAFETY: self is live.
        if unsafe { c::CVDisplayLinkStop(self.as_ptr()) } != c::kCVReturnSuccess {
            return Err(Error::InvalidOperation);
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        // SAFETY: self is live.
        unsafe { c::CVDisplayLinkIsRunning(self.as_ptr()) != 0 }
    }

    pub fn set_current_cg_display(&self, display_id: c::CGDirectDisplayID) -> Result<(), Error> {
        // SAFETY: self is live.
        if unsafe { c::CVDisplayLinkSetCurrentCGDisplay(self.as_ptr(), display_id) }
            != c::kCVReturnSuccess
        {
            return Err(Error::InvalidOperation);
        }
        Ok(())
    }

    // Note: this purposely throws away a ton of arguments I didn't need.
    // It would be trivial to refactor this into Zig types and properly
    // pass this through.
    //
    // The Zig version takes the callback as a comptime parameter and
    // generates a C trampoline for it. Rust can't generate a distinct
    // `extern "C"` function per callback value, so the callback and the
    // userdata are boxed together and the box is passed as the user info.
    // The box is leaked (the callback stays registered for the life of the
    // display link, like in Zig).
    pub fn set_output_callback<Userdata: 'static>(
        &self,
        callback_fn: OutputCallback<Userdata>,
        userinfo: Option<&'static mut Userdata>,
    ) -> Result<(), Error> {
        struct Trampoline<Userdata: 'static> {
            callback_fn: OutputCallback<Userdata>,
            userinfo: *mut Userdata,
        }

        unsafe extern "C" fn callback<Userdata: 'static>(
            display_link: c::CVDisplayLinkRef,
            _in_now: *const c::CVTimeStamp,
            _in_output_time: *const c::CVTimeStamp,
            _flags_in: c::CVOptionFlags,
            _flags_out: *mut c::CVOptionFlags,
            inner_userinfo: *mut c_void,
        ) -> c::CVReturn {
            // SAFETY: inner_userinfo is the leaked Trampoline below and
            // display_link is the live display link.
            unsafe {
                let t = &*(inner_userinfo as *const Trampoline<Userdata>);
                (t.callback_fn)(&*(display_link as *const DisplayLink), t.userinfo.as_mut());
            }
            c::kCVReturnSuccess
        }

        let trampoline = Box::into_raw(Box::new(Trampoline {
            callback_fn,
            userinfo: userinfo.map_or(std::ptr::null_mut(), |u| u as *mut Userdata),
        }));

        // SAFETY: self is live; the trampoline lives forever.
        if unsafe {
            c::CVDisplayLinkSetOutputCallback(
                self.as_ptr(),
                Some(callback::<Userdata>),
                trampoline as *mut c_void,
            )
        } != c::kCVReturnSuccess
        {
            // SAFETY: not registered, so reclaim it.
            drop(unsafe { Box::from_raw(trampoline) });
            return Err(Error::InvalidOperation);
        }
        Ok(())
    }
}
