//! Port of `src/renderer/metal/Device.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! The Metal device context.
//!
//! This holds the MTLDevice and metadata derived from it.
//! The device is shared by all surface renderers; each renderer
//! creates its own MTLCommandQueue.

use std::ffi::c_ulong;

use super::api as mtl;
use super::objc;
use super::shaders;
use super::Error;

const LOG: &str = "metal";

/// Options used when creating the Metal device.
/// There are currently no options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {}

#[derive(Debug)]
pub struct Device {
    /// MTLDevice
    pub device: objc::Object,

    /// The default storage mode to use for resources created with our device.
    ///
    /// This is based on whether the device is a discrete GPU or not, since
    /// discrete GPUs do not have unified memory and therefore do not support
    /// the "shared" storage mode, instead we have to use the "managed" mode.
    pub default_storage_mode: mtl::StorageMode,

    /// The maximum 2D texture width and height supported by the device.
    pub max_texture_size: u32,
}

// SAFETY: MTLDevice is thread-safe.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn init(options: Options) -> Result<Box<Device>, Error> {
        let _ = options;

        // Choose our MTLDevice.
        let device = choose_device()?;

        // Grab metadata about the device.
        // SAFETY: valid MTLDevice.
        let unified: objc::BOOL = unsafe { device.get_property(c"hasUnifiedMemory") };
        let default_storage_mode = if unified != objc::NO {
            mtl::StorageMode::Shared
        } else {
            mtl::StorageMode::Managed
        };
        let max_texture_size = query_max_texture_size(device);
        log::debug!(
            target: LOG,
            "device properties default_storage_mode={default_storage_mode:?} max_texture_size={max_texture_size}"
        );

        Ok(Box::new(Device {
            device,
            default_storage_mode,
            max_texture_size,
        }))
    }

    pub fn deinit(self: Box<Self>) {
        self.device.release();
    }

    /// Warm up the Metal device machinery. The first Metal device query in
    /// a process takes multiple milliseconds; once warm, subsequent queries
    /// are effectively free. Calling this early (e.g. on a background
    /// thread at app startup; Metal device queries are thread-safe) moves
    /// that one-time cost off the critical path of the first surface's
    /// renderer initialization.
    pub fn warmup() {
        let Ok(device) = choose_device() else {
            return;
        };

        // Create and release a command queue. The first command queue
        // created for a device pays additional one-time driver setup
        // costs; subsequent creations are much cheaper.
        // SAFETY: valid MTLDevice.
        let queue: objc::Object = unsafe { device.msg_send(objc::sel(c"newCommandQueue"), ()) };
        queue.release();

        // Build and discard our shader pipelines for both pixel formats we
        // may use (which one is used depends on the blending config). The
        // first pipeline state creation compiles shaders which is slow;
        // once warm, later creations hit driver and OS caches.
        for format in [
            mtl::MTLPixelFormat::Bgra8unormSrgb,
            mtl::MTLPixelFormat::Bgra8unorm,
        ] {
            match shaders::Shaders::init(device, &[], format) {
                Ok(mut s) => s.deinit(),
                Err(err) => log::warn!(target: LOG, "metal warmup shader init failed err={err}"),
            }
        }

        device.release();
    }
}

fn choose_device() -> Result<objc::Object, Error> {
    let mut chosen_device: Option<objc::Object> = None;

    // SAFETY: MTLCopyAllDevices returns a +1 NSArray of MTLDevices.
    unsafe {
        let devices = objc::Object::from_id(mtl::MTLCopyAllDevices());

        let count: c_ulong = devices.get_property(c"count");
        for i in 0..count {
            let device: objc::Object = devices.msg_send(objc::sel(c"objectAtIndex:"), (i,));

            // We want a GPU that’s connected to a display.
            let headless: objc::BOOL = device.get_property(c"isHeadless");
            if headless != objc::NO {
                continue;
            }
            chosen_device = Some(device);

            // If the user has an eGPU plugged in, they probably want
            // to use it. Otherwise, integrated GPUs are better for
            // battery life and thermals.
            let removable: objc::BOOL = device.get_property(c"isRemovable");
            let low_power: objc::BOOL = device.get_property(c"isLowPower");
            if removable != objc::NO || low_power != objc::NO {
                break;
            }
        }

        let device = chosen_device.map(|d| d.retain());
        devices.release();
        device.ok_or(Error::NoMetalDevice)
    }
}

/// Determines the maximum 2D texture size supported by the device.
/// We need to clamp our frame size to this if it's larger.
fn query_max_texture_size(device: objc::Object) -> u32 {
    // https://developer.apple.com/metal/Metal-Feature-Set-Tables.pdf

    // SAFETY: valid MTLDevice.
    unsafe {
        let supports = |family: mtl::MTLGPUFamily| -> bool {
            let r: objc::BOOL = device.msg_send(objc::sel(c"supportsFamily:"), (family as i64,));
            r != objc::NO
        };

        if supports(mtl::MTLGPUFamily::Apple10) {
            return 32768;
        }

        if supports(mtl::MTLGPUFamily::Apple3) {
            return 16384;
        }
    }

    8192
}
