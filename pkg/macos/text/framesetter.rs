//! Port of `src/macos/text/framesetter.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use super::frame::Frame;
use crate::pkg::macos::foundation;
use crate::pkg::macos::graphics;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CTFramesetterRef`
    Framesetter
);

impl Framesetter {
    pub fn create_with_attributed_string(
        str: &foundation::AttributedString,
    ) -> Result<&'static Framesetter, Error> {
        // SAFETY: str is live.
        unsafe { created(c::CTFramesetterCreateWithAttributedString(str.as_ptr())) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn create_frame(
        &self,
        range: foundation::Range,
        path: &graphics::Path,
        attrs: Option<&foundation::Dictionary>,
    ) -> Result<&'static Frame, Error> {
        // SAFETY: self, path (and attrs) are live.
        unsafe {
            created(c::CTFramesetterCreateFrame(
                self.as_ptr(),
                range.cval(),
                path.as_ptr(),
                attrs.map_or(ptr::null(), |a| a.as_ptr()),
            ))
        }
        .map_err(|_| Error::FrameCreateFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framesetter() {
        let str = foundation::MutableAttributedString::create(0).unwrap();
        {
            let rep = foundation::String::create_with_bytes(
                b"hello",
                foundation::StringEncoding::Utf8,
                false,
            )
            .unwrap();
            str.replace_string(foundation::Range::init(0, 0), rep);
            rep.release();
        }

        let fs = Framesetter::create_with_attributed_string(str.as_attributed_string()).unwrap();

        let path =
            graphics::Path::create_with_rect(graphics::Rect::init(0.0, 0.0, 100.0, 200.0), None)
                .unwrap();
        let frame = fs
            .create_frame(foundation::Range::init(0, 0), path, None)
            .unwrap();

        {
            let mut points = [graphics::Point::default(); 1];
            frame.get_line_origins(foundation::Range::init(0, 1), &mut points);
        }

        frame.release();
        path.release();
        fs.release();
        str.release();
    }
}
