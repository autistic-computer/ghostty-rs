//! Port of `src/apprt/gtk/scale.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::os::raw::c_int;

use gtk4::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CssPoint {
    pub x: f64,
    pub y: f64,
}

/// Scale, GdkSurface-space origin, and snapped device size for one widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub scale: f64,
    pub origin: CssPoint,
    pub size: DeviceSize,
}

impl Layout {
    /// CSS offset that moves widget (0,0) onto the nearest device pixel.
    pub fn snap_origin(self) -> CssPoint {
        CssPoint {
            x: snap_offset(self.origin.x, self.scale),
            y: snap_offset(self.origin.y, self.scale),
        }
    }
}

pub fn widget_surface_scale(widget: &impl IsA<gtk4::Widget>) -> f64 {
    if let Some(native) = widget.native() {
        if let Some(surface) = native.surface() {
            let scale = surface.scale();
            if scale > 0.0 {
                return scale;
            }
        }
    }

    let scale = widget.scale_factor();
    if scale <= 0 {
        return 1.0;
    }
    scale as f64
}

/// Widget (0,0) in GdkSurface CSS coordinates.
///
/// `computePoint` to the native widget is CSS inside the window. CSD shadows
/// live outside that, so add `gtk_native_get_surface_transform` to get the
/// origin on the GdkSurface.
pub fn widget_surface_origin(widget: &impl IsA<gtk4::Widget>) -> CssPoint {
    let zero = CssPoint { x: 0.0, y: 0.0 };
    let Some(native) = widget.native() else { return zero };
    let Some(native_widget) = native.dynamic_cast_ref::<gtk4::Widget>() else {
        return zero;
    };

    let Some(native_origin) =
        widget.compute_point(native_widget, &gtk4::graphene::Point::new(0.0, 0.0))
    else {
        return zero;
    };

    let (tx, ty) = native.surface_transform();

    CssPoint {
        x: native_origin.x() as f64 + tx,
        y: native_origin.y() as f64 + ty,
    }
}

pub fn widget_layout(widget: &impl IsA<gtk4::Widget>) -> Layout {
    widget_layout_for_size(widget, widget.width(), widget.height())
}

pub fn widget_layout_for_size(
    widget: &impl IsA<gtk4::Widget>,
    css_width: c_int,
    css_height: c_int,
) -> Layout {
    let scale = widget_surface_scale(widget);
    let origin = widget_surface_origin(widget);
    Layout {
        scale,
        origin,
        size: snapped_device_size(origin.x, origin.y, css_width, css_height, scale),
    }
}

pub fn device_size(css_width: c_int, css_height: c_int, scale: f64) -> DeviceSize {
    snapped_device_size(0.0, 0.0, css_width, css_height, scale)
}

pub fn snapped_device_size(
    origin_x: f64,
    origin_y: f64,
    css_width: c_int,
    css_height: c_int,
    scale: f64,
) -> DeviceSize {
    DeviceSize {
        width: snapped_axis(origin_x, css_width, scale),
        height: snapped_axis(origin_y, css_height, scale),
    }
}

/// CSS delta that lands a surface-relative origin on the nearest device pixel.
///
/// Tab chrome can put widget (0,0) on a fractional device coordinate, so GTK
/// linearly samples every texel. Shift by less than 0.5 device px without
/// changing content size.
pub fn snap_offset(css_origin: f64, scale: f64) -> f64 {
    if !(scale > 0.0) {
        return 0.0;
    }
    let device_origin = css_origin * scale;
    (device_origin.round() - device_origin) / scale
}

/// Device coverage of one axis after snapping both edges.
///
/// `round(end) - round(start)`, not `ceil(css × scale)`, so the buffer spans
/// the same snapped pixels the texture is drawn into.
fn snapped_axis(css_origin: f64, css: c_int, scale: f64) -> u32 {
    if css <= 0 || !(scale > 0.0) {
        return 0;
    }
    let css_size = css as f64;
    let start = (css_origin * scale).round();
    let end = ((css_origin + css_size) * scale).round();
    if end <= start {
        return 0;
    }
    (end - start) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(expected: f64, actual: f64, tolerance: f64) {
        assert!(
            (expected - actual).abs() <= tolerance,
            "expected {expected}, found {actual}"
        );
    }

    #[test]
    fn device_size_integer_scale() {
        assert_eq!(DeviceSize { width: 800, height: 600 }, device_size(800, 600, 1.0));
        assert_eq!(DeviceSize { width: 1600, height: 1200 }, device_size(800, 600, 2.0));
    }

    #[test]
    fn device_size_fractional_scale() {
        assert_eq!(DeviceSize { width: 1000, height: 750 }, device_size(800, 600, 1.25));
        assert_eq!(DeviceSize { width: 1200, height: 900 }, device_size(800, 600, 1.5));
        assert_eq!(DeviceSize { width: 126, height: 126 }, device_size(101, 101, 1.25));
    }

    #[test]
    fn device_size_rejects_non_positive_inputs() {
        assert_eq!(DeviceSize { width: 0, height: 15 }, device_size(0, 10, 1.5));
        assert_eq!(DeviceSize { width: 0, height: 0 }, device_size(10, 10, 0.0));
        assert_eq!(DeviceSize { width: 0, height: 15 }, device_size(-1, 10, 1.5));
    }

    #[test]
    fn snap_offset_aligns_fractional_device_origins() {
        approx(0.0, snap_offset(0.0, 1.75), 0.000001);
        approx(-1.0 / 7.0, snap_offset(47.0, 1.75), 0.000001);
        approx(0.2, snap_offset(47.0, 1.25), 0.000001);
        approx(0.25, snap_offset(47.0, 4.0 / 3.0), 0.000001);
    }

    #[test]
    fn snap_offset_rejects_non_positive_scale() {
        assert_eq!(0.0, snap_offset(47.0, 0.0));
        assert_eq!(0.0, snap_offset(47.0, -1.25));
    }

    #[test]
    fn snapped_device_size_uses_origin_aware_device_spans() {
        let scale_4_3 = 4.0 / 3.0;
        assert_eq!(
            DeviceSize { width: 1066, height: 1066 },
            snapped_device_size(47.0, 47.0, 800, 800, scale_4_3),
        );
        assert_eq!(
            DeviceSize { width: 1097, height: 628 },
            snapped_device_size(0.0, 0.0, 823, 471, scale_4_3),
        );
        assert_eq!(
            DeviceSize { width: 126, height: 126 },
            snapped_device_size(0.0, 0.0, 101, 101, 1.25),
        );
    }

    #[test]
    fn snapped_device_size_integer_scale_with_origin() {
        assert_eq!(
            DeviceSize { width: 800, height: 600 },
            snapped_device_size(47.0, 11.0, 800, 600, 1.0),
        );
        assert_eq!(
            DeviceSize { width: 1600, height: 1200 },
            snapped_device_size(0.25, 0.75, 800, 600, 2.0),
        );
    }

    #[test]
    fn unrealized_widget_layout() {
        crate::apprt::gtk::ext::test_gtk::run(|| {
            let label = gtk4::Label::new(None);
            assert_eq!(widget_surface_origin(&label), CssPoint { x: 0.0, y: 0.0 });
            assert_eq!(widget_surface_scale(&label), 1.0);
            let layout = widget_layout_for_size(&label, 10, 20);
            assert_eq!(layout.size, DeviceSize { width: 10, height: 20 });
            assert_eq!(layout.snap_origin(), CssPoint { x: 0.0, y: 0.0 });
        });
    }
}
