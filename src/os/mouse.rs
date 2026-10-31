//! Port of `src/os/mouse.zig`.
//!
//! STATUS: ported

/// The system-configured double-click interval if its available.
pub fn click_interval() -> Option<u32> {
    // On macOS, we can ask the system.
    #[cfg(target_os = "macos")]
    {
        use super::macos::objc;
        let Some(ns_event) = objc::get_class(c"NSEvent") else {
            log::error!(target: "os", "NSEvent class not found. Can't get click interval.");
            return None;
        };

        // Get the interval and convert to ms
        // SAFETY: `doubleClickInterval` is a class method returning a double.
        let interval: f64 =
            unsafe { objc::msg_send_f64(ns_event, objc::sel(c"doubleClickInterval")) };
        let ms = (interval * 1000.0).ceil() as u32;
        return Some(ms);
    }

    #[allow(unreachable_code)]
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn click_interval_platform() {
        let v = super::click_interval();
        if !cfg!(target_os = "macos") {
            assert_eq!(None, v);
        }
    }
}
