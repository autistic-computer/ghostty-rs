//! Port of `src/renderer/DisplayLink.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! A CVDisplayLink that is only ever started, stopped, or re-targeted
//! from the main thread.
//!
//! ## Why start and stop must happen on the main thread
//!
//! CoreVideo's `CVDisplayLinkStop` clears the link's running flag and
//! then waits, once and unconditionally, for the link's IO thread to
//! notice and exit. While it waits, the link's mutex is released. If any
//! thread calls `CVDisplayLinkStart` during that wait, the flag is set
//! again and a second IO thread is created. The first IO thread wakes up,
//! sees the flag set, and keeps running. Nothing ever signals the waiting
//! `CVDisplayLinkStop`, so it blocks forever.
//!
//! CoreVideo itself performs exactly this stop and start sequence, on the
//! main thread, inside its display reconfiguration callback. That fires
//! whenever the display the link is bound to changes mode, which happens
//! on display sleep and wake, resolution changes, and hotplug.
//! `CVDisplayLinkSetCurrentCGDisplay` performs the same sequence. None of
//! these calls take a lock that protects against each other.
//!
//! So a start or stop issued from the render thread can collide with
//! CoreVideo's own, and whichever side loses is stuck in
//! `CVDisplayLinkStop` forever. In Ghostty that first wedged the render
//! thread, then the main thread as soon as it waited on the renderer, and
//! the whole app froze. See
//! https://github.com/ghostty-org/ghostty/discussions/14150.
//!
//! The fix is to issue every state change from the main thread, where it
//! is serialized with CoreVideo's own reconfiguration handling simply by
//! running on the same thread.
//!
//! Deviations: the allocator is dropped (the struct is a `Box` leaked to
//! a raw pointer, freed by `close` or by a queued sync); the `Requested`
//! packed struct is stored in an `AtomicU64`.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::pkg::macos;

const LOG: &str = "display_link";

extern "C" {
    fn dispatch_async_f(queue: *mut c_void, context: *mut c_void, work: unsafe extern "C" fn(*mut c_void));
}

pub struct DisplayLink {
    /// The underlying CVDisplayLink. Its state is only changed on the
    /// main thread.
    pub link: &'static macos::video::DisplayLink,

    /// Set by `close`. A sync that was already queued when `close` ran
    /// sees this, skips its work, and frees the struct. Main thread only.
    pub closed: bool,

    /// The display the link was last bound to with `setDisplay`, or zero
    /// if `setDisplay` was never called. Main thread only.
    pub display: u32,

    /// The state the owner has asked for. Written by `setRunning` and
    /// `setDisplay` on the owner's thread and read by the sync on the
    /// main thread. Packing both fields into one atomic means the sync
    /// always sees them together as they were last requested.
    requested: AtomicU64,

    /// True while a sync is queued on the main thread and has not yet
    /// started. Used to collapse many requests into one sync, and by
    /// `close` to know whether a queued sync still needs this struct.
    sync_queued: AtomicBool,
}

/// The state the owner wants the link in. This is a single 64-bit
/// value so that it can be read and written atomically as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Requested {
    /// Whether the link should be running.
    running: bool,

    /// The CGDirectDisplayID the link should be bound to, or zero if
    /// no display has been requested.
    display: u32,
}

impl Requested {
    /// `packed struct(u64)`: running in bit 0, 31 bits padding, display
    /// in the high 32 bits.
    fn to_bits(self) -> u64 {
        (self.running as u64) | ((self.display as u64) << 32)
    }

    fn from_bits(v: u64) -> Requested {
        Requested {
            running: v & 1 != 0,
            display: (v >> 32) as u32,
        }
    }
}

impl std::fmt::Debug for DisplayLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayLink")
            .field("link", &self.link.as_ptr())
            .field("closed", &self.closed)
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

impl DisplayLink {
    /// Create a display link bound to the active displays, with the given
    /// output callback. The link starts out stopped. Call `setRunning` to
    /// start it.
    ///
    /// `callbackFn` is invoked by CoreVideo on its own thread once per
    /// display refresh while the link is running, with `userdata` as its
    /// argument. `userdata` must stay valid until `close` returns.
    ///
    /// This can be called from any thread. Creating a link and setting its
    /// callback do not change its running state, so they do not collide
    /// with CoreVideo's reconfiguration handling.
    ///
    /// Fails if no display is available, which can happen briefly while
    /// the session is locked or a display is asleep. Callers can continue
    /// without vsync and try again later.
    pub fn create<Userdata: 'static>(
        callback_fn: macos::video::display_link::OutputCallback<Userdata>,
        userdata: Option<&'static mut Userdata>,
    ) -> Result<*mut DisplayLink, macos::Error> {
        let link = macos::video::DisplayLink::create_with_active_cg_displays()?;
        if let Err(err) = link.set_output_callback(callback_fn, userdata) {
            link.release();
            return Err(err);
        }

        Ok(Box::into_raw(Box::new(DisplayLink {
            link,
            closed: false,
            display: 0,
            requested: AtomicU64::new(Requested::default().to_bits()),
            sync_queued: AtomicBool::new(false),
        })))
    }

    /// Stop and release the display link. The pointer is invalid after
    /// this call.
    ///
    /// Must be called on the main thread. Stopping waits for CoreVideo's
    /// IO thread to exit, and that is only safe on the main thread, where
    /// CoreVideo's own stop and start also run and so cannot restart the
    /// link while we wait. Our apprts always destroy surfaces on the main
    /// thread.
    ///
    /// The link is stopped before this returns, so the output callback is
    /// not invoked again and whatever `userdata` pointed at can be freed.
    ///
    /// No other method may be called on this link after or concurrently
    /// with `close`. In practice the render thread that makes those calls
    /// has already exited by the time the renderer is deinitialized.
    ///
    /// # Safety
    ///
    /// `this` must come from [`DisplayLink::create`] and not be closed.
    pub unsafe fn close(this: *mut DisplayLink) {
        // SAFETY: caller guarantee.
        let self_ = unsafe { &mut *this };
        debug_assert!(!self_.closed);
        self_.closed = true;

        if self_.link.is_running() {
            if let Err(err) = self_.link.stop() {
                log::warn!(target: LOG, "error stopping display link err={err:?}");
            }
        }
        self_.link.release();

        // If a sync is queued on the main thread it runs after we return
        // and still needs this struct, so it frees it instead of us.
        if !self_.sync_queued.load(Ordering::SeqCst) {
            // SAFETY: caller guarantee.
            unsafe { Self::destroy(this) };
        }
    }

    /// # Safety
    ///
    /// `this` must come from [`DisplayLink::create`].
    unsafe fn destroy(this: *mut DisplayLink) {
        // SAFETY: caller guarantee.
        drop(unsafe { Box::from_raw(this) });
    }

    /// Returns true if the link is running right now, meaning CoreVideo is
    /// invoking the output callback on every display refresh.
    ///
    /// This reports the link's actual state, not the state most recently
    /// requested with `setRunning`. The two differ briefly after a request,
    /// and also when a start failed or CoreVideo stopped the link itself
    /// during a display reconfiguration. Can be called from any thread.
    pub fn is_running(&self) -> bool {
        self.link.is_running()
    }

    /// Request that the link run, or stop running. Never blocks.
    ///
    /// The change is applied shortly after on the main thread. Until then
    /// `isRunning` keeps reporting the old state. Calling this repeatedly
    /// with the same value is cheap, and calling it with alternating values
    /// in quick succession results in at most one real change.
    ///
    /// This and `setDisplay` must be called from a single thread, which
    /// for the renderer is the render thread. That thread is the only
    /// writer of the requested state, so no compare-and-swap is needed.
    pub fn set_running(&mut self, running: bool) {
        let mut requested = Requested::from_bits(self.requested.load(Ordering::SeqCst));
        requested.running = running;
        self.requested.store(requested.to_bits(), Ordering::SeqCst);
        if running != self.link.is_running() {
            self.queue_sync();
        }
    }

    /// Request that the link be bound to the given CGDirectDisplayID, so
    /// that it follows that display's refresh rate. Never blocks.
    ///
    /// Call this when the window moves to a different screen. A display ID
    /// of zero means no display and is ignored. See `setRunning` for the
    /// threading requirement.
    pub fn set_display(&mut self, display_id: u32) {
        if display_id == 0 {
            return;
        }
        let mut requested = Requested::from_bits(self.requested.load(Ordering::SeqCst));
        requested.display = display_id;
        self.requested.store(requested.to_bits(), Ordering::SeqCst);
        self.queue_sync();
    }

    /// Queue a sync onto the main thread, unless one is already queued and
    /// has not started yet.
    ///
    /// A sync that has already started clears `sync_queued` before it reads
    /// the requested state, so a request that arrives too late for it will
    /// queue another sync rather than be lost.
    fn queue_sync(&mut self) {
        if self.sync_queued.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: the struct stays alive until the sync runs (see `close`).
        unsafe {
            dispatch_async_f(
                macos::dispatch::queue::get_main(),
                (self as *mut DisplayLink).cast(),
                sync_callback,
            );
        }
    }

    /// Make the link's actual state match the requested state. Main thread
    /// only. Every CoreVideo call that changes the link's state is made
    /// from here or from `close`.
    fn sync(&mut self) {
        let requested = Requested::from_bits(self.requested.load(Ordering::SeqCst));

        if requested.display != 0 && requested.display != self.display {
            // Record the display even if binding fails so that we don't
            // retry on every sync. The next display change tries again.
            self.display = requested.display;
            log::info!(target: LOG, "updating display link display id={}", requested.display);

            // If the link is running, CoreVideo stops and restarts it
            // internally. That is safe here for the same reason our own
            // stop below is.
            if let Err(err) = self.link.set_current_cg_display(requested.display) {
                log::warn!(target: LOG, "error setting display link display id err={err:?}");
            }
        }

        let running = self.link.is_running();
        if requested.running && !running {
            // If this fails the link stays stopped. The renderer notices
            // through isRunning and draws without vsync, and the next
            // request retries. This is logged at debug level because a
            // display can be unavailable for a while, such as during a
            // locked session, and we would otherwise log once per frame.
            if let Err(err) = self.link.start() {
                log::debug!(target: LOG, "error starting display link err={err:?}");
            }
        } else if !requested.running && running {
            if let Err(err) = self.link.stop() {
                log::warn!(target: LOG, "error stopping display link err={err:?}");
            }
        }
    }
}

unsafe extern "C" fn sync_callback(ud: *mut c_void) {
    let this = ud.cast::<DisplayLink>();
    // SAFETY: queued by `queue_sync` with a live struct (see `close`).
    unsafe {
        // Clear this before reading the requested state. See queueSync.
        (*this).sync_queued.store(false, Ordering::SeqCst);

        // If the owner closed the link while we were queued, it left the
        // struct for us to free. See close.
        if (*this).closed {
            DisplayLink::destroy(this);
            return;
        }

        (*this).sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not a Zig test: the packed `Requested` layout.
    #[test]
    fn requested_bits() {
        let r = Requested {
            running: true,
            display: 7,
        };
        assert_eq!((7u64 << 32) | 1, r.to_bits());
        assert_eq!(r, Requested::from_bits(r.to_bits()));
    }
}
