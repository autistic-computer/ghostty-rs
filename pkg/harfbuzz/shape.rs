//! Port of `pkg/harfbuzz/shape.zig`.
//!
//! STATUS: ported

use std::os::raw::c_uint;

use super::buffer::Buffer;
use super::c;
use super::common::Feature;
use super::font::Font;

/// Shapes buffer using font turning its Unicode characters content to
/// positioned glyphs. If features is not NULL, it will be used to control
/// the features applied during shaping. If two features have the same tag
/// but overlapping ranges the value of the feature with the higher index
/// takes precedence.
pub fn shape(font: Font, buf: Buffer, features: Option<&[Feature]>) {
    let hb_feats: *const c::hb_feature_t = match features {
        Some(fs) if !fs.is_empty() => fs.as_ptr() as *const c::hb_feature_t,
        _ => std::ptr::null(),
    };

    // SAFETY: valid handles; Feature has the layout of hb_feature_t.
    unsafe {
        c::hb_shape(
            font.handle,
            buf.handle,
            hb_feats,
            features.map_or(0, |f| f.len() as c_uint),
        );
    }
}
