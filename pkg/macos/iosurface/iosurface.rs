//! Port of `src/macos/iosurface/iosurface.zig`.
//!
//! STATUS: ported

use std::ffi::{c_int, c_void};
use std::ptr;

use super::c;
use crate::pkg::macos::foundation;
use crate::pkg::macos::graphics;
use crate::pkg::macos::video;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `IOSurfaceRef`
    IOSurface
);

pub struct Properties<'a> {
    pub width: c_int,
    pub height: c_int,
    pub pixel_format: video::PixelFormat,
    pub bytes_per_element: c_int,
    pub colorspace: Option<&'a graphics::ColorSpace>,
}

impl IOSurface {
    pub fn init(properties: Properties<'_>) -> Result<&'static IOSurface, Error> {
        let w = foundation::Number::create(foundation::NumberType::Int, &properties.width)?;
        let h = foundation::Number::create(foundation::NumberType::Int, &properties.height)?;
        let pf =
            foundation::Number::create(foundation::NumberType::Int, &properties.pixel_format.0);
        let bpe =
            foundation::Number::create(foundation::NumberType::Int, &properties.bytes_per_element);

        let result = (|| {
            let pf = pf?;
            let bpe = bpe?;
            // SAFETY: reading framework constants.
            let keys = unsafe {
                [
                    c::kIOSurfaceWidth,
                    c::kIOSurfaceHeight,
                    c::kIOSurfacePixelFormat,
                    c::kIOSurfaceBytesPerElement,
                ]
            };
            let values: [*const c_void; 4] = [w.as_ptr(), h.as_ptr(), pf.as_ptr(), bpe.as_ptr()];
            let properties_dict = foundation::Dictionary::create(Some(&keys), Some(&values))?;

            // SAFETY: the dictionary is live.
            let surface: Result<&'static IOSurface, Error> =
                unsafe { created(c::IOSurfaceCreate(properties_dict.as_ptr())) };
            properties_dict.release();
            surface
        })();

        if let Ok(v) = bpe {
            v.release();
        }
        if let Ok(v) = pf {
            v.release();
        }
        h.release();
        w.release();

        let surface = result?;
        if let Some(space) = properties.colorspace {
            surface.set_color_space(space);
        }

        Ok(surface)
    }

    pub fn deinit(&self) {
        // We mark it purgeable so that it is immediately unloaded, so that we
        // don't have to wait for CoreFoundation garbage collection to trigger.
        // SAFETY: self is a live surface owned by the caller.
        unsafe {
            c::IOSurfaceSetPurgeable(self.as_ptr(), c::kIOSurfacePurgeableEmpty, ptr::null_mut());
            c::CFRelease(self.as_ptr());
        }
    }

    pub fn retain(&self) {
        // SAFETY: self is live.
        unsafe {
            c::CFRetain(self.as_ptr());
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live surface owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn set_color_space(&self, colorspace: &graphics::ColorSpace) {
        // SAFETY: colorspace is live.
        let serialized_colorspace = unsafe { c::CGColorSpaceCopyPropertyList(colorspace.as_ptr()) };
        assert!(!serialized_colorspace.is_null());

        // SAFETY: self is live; the property list is a CF object we own.
        unsafe {
            c::IOSurfaceSetValue(
                self.as_ptr(),
                c::kIOSurfaceColorSpace,
                serialized_colorspace,
            );
            c::CFRelease(serialized_colorspace);
        }
    }

    #[inline]
    pub fn lock(&self) {
        // SAFETY: self is live.
        unsafe {
            c::IOSurfaceLock(self.as_ptr(), 0, ptr::null_mut());
        }
    }

    #[inline]
    pub fn unlock(&self) {
        // SAFETY: self is live.
        unsafe {
            c::IOSurfaceUnlock(self.as_ptr(), 0, ptr::null_mut());
        }
    }

    #[inline]
    pub fn get_alloc_size(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetAllocSize(self.as_ptr()) }
    }

    #[inline]
    pub fn get_width(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetWidth(self.as_ptr()) }
    }

    #[inline]
    pub fn get_height(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetHeight(self.as_ptr()) }
    }

    #[inline]
    pub fn get_bytes_per_element(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetBytesPerElement(self.as_ptr()) }
    }

    #[inline]
    pub fn get_bytes_per_row(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetBytesPerRow(self.as_ptr()) }
    }

    #[inline]
    pub fn get_base_address(&self) -> Option<*mut u8> {
        // SAFETY: self is live.
        let ptr = unsafe { c::IOSurfaceGetBaseAddress(self.as_ptr()) } as *mut u8;
        (!ptr.is_null()).then_some(ptr)
    }

    #[inline]
    pub fn get_element_width(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetElementWidth(self.as_ptr()) }
    }

    #[inline]
    pub fn get_element_height(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::IOSurfaceGetElementHeight(self.as_ptr()) }
    }

    #[inline]
    pub fn get_pixel_format(&self) -> video::PixelFormat {
        // SAFETY: self is live.
        video::PixelFormat(unsafe { c::IOSurfaceGetPixelFormat(self.as_ptr()) } as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iosurface() {
        let cs = graphics::ColorSpace::create_device_rgb().unwrap();
        let surface = IOSurface::init(Properties {
            width: 16,
            height: 8,
            pixel_format: video::PixelFormat::_32BGRA,
            bytes_per_element: 4,
            colorspace: Some(cs),
        })
        .unwrap();
        assert_eq!(surface.get_width(), 16);
        assert_eq!(surface.get_height(), 8);
        assert_eq!(surface.get_bytes_per_element(), 4);
        assert_eq!(surface.get_pixel_format(), video::PixelFormat::_32BGRA);
        surface.lock();
        assert!(surface.get_base_address().is_some());
        surface.unlock();
        surface.deinit();
        cs.release();
    }
}
