//! Port of `src/renderer/Thread.zig`.
//!
//! STATUS: ported
//!
//! Represents the renderer thread logic. The renderer thread is able to
//! be woken up to render.
//!
//! Deviations from the Zig code:
//! * The allocator field is dropped; the mailbox is a `Box` owned by the
//!   thread (Zig: created/destroyed with the allocator).
//! * The surface, renderer and state are raw pointers like in Zig: the
//!   owner (the core surface) keeps them alive while the thread runs.
//! * The xev callbacks reach the loop through their `&mut Loop` argument
//!   rather than through `Thread.loop`, since the loop is borrowed by its
//!   own `run` while they execute. Helpers that arm timers therefore take
//!   the loop explicitly, and thread fields are accessed through the raw
//!   thread pointer (never a `&mut Thread` that would alias the loop).
//! * Valgrind is detected through its `LD_PRELOAD` preload libraries
//!   (Zig: `std.valgrind.runningOnValgrind()` client request).

use std::fmt;

use crate::config::Config;
use crate::crash;
use crate::datastruct::BlockingQueue;
use crate::global::xev;
use crate::lib::tinyio::TinyIo;
use crate::renderer::message::Message;
use crate::renderer::generic::AnimationWakeKind;
use crate::renderer::{Renderer, State};
use crate::terminal::mem::{can_reclaim, DecommitMode};
use crate::apprt;

const LOG: &str = "renderer_thread";

const CURSOR_BLINK_INTERVAL: u64 = 600;

/// The type used for sending messages to the IO thread. For now this is
/// hardcoded with a capacity. We can make this a comptime parameter in
/// the future if we want it configurable.
pub type Mailbox = BlockingQueue<Message, 64>;

/// Zig: `terminalpkg.compression_enabled`.
const COMPRESSION_ENABLED: bool = can_reclaim(DecommitMode::Strict);

/// Thread flags (Zig: a packed struct).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// This is true when a blinking cursor should be visible and false
    /// when it should not be visible. This is toggled on a timer by the
    /// thread automatically.
    pub cursor_blink_visible: bool,

    /// This is true when the inspector is active.
    pub has_inspector: bool,

    /// This is true when the view is visible. This is used to determine
    /// if we should be rendering or not.
    pub visible: bool,

    /// This is true when the view is focused. This defaults to true
    /// and it is up to the apprt to set the correct value.
    pub focused: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Flags {
            cursor_blink_visible: false,
            has_inspector: false,
            visible: true,
            focused: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DerivedConfig {
    pub scrollback_compression: bool,
}

impl DerivedConfig {
    pub fn init(config: &Config) -> DerivedConfig {
        DerivedConfig {
            scrollback_compression: config.scrollback_compression,
        }
    }
}

/// The renderer thread. Once [`Thread::thread_main`] runs, the thread must
/// not move: completions armed on its loop point into it.
pub struct Thread {
    /// The main event loop for the application.
    pub r#loop: xev::Loop,

    /// This can be used to wake up the renderer and force a render safely from
    /// any thread.
    pub wakeup: xev::Async,
    pub wakeup_c: xev::Completion,

    /// This can be used to stop the renderer on the next loop iteration.
    pub stop: xev::Async,
    pub stop_c: xev::Completion,

    /// The timer used for animations (custom shaders, Kitty graphics).
    /// Normal rendering is driven by wakeup messages instead.
    pub render_h: xev::Timer,
    pub render_c: xev::Completion,
    pub render_c_cancel: xev::Completion,

    /// The kind of work the currently scheduled animation wake needs,
    /// stored when the timer is armed.
    pub animation_wake: AnimationWakeKind,

    /// This async is used to force a draw immediately. This does not
    /// coalesce like the wakeup does.
    pub draw_now: xev::Async,
    pub draw_now_c: xev::Completion,

    /// The timer used for cursor blinking
    pub cursor_h: xev::Timer,
    pub cursor_c: xev::Completion,
    pub cursor_c_cancel: xev::Completion,

    /// Incremental scrollback compression scheduling (only initialized
    /// when compression is enabled).
    compression: Option<Compression>,

    /// The surface we're rendering to.
    pub surface: *mut apprt::Surface,

    /// The underlying renderer implementation.
    pub renderer: *mut Renderer,

    /// Pointer to the shared state that is used to generate the final render.
    pub state: *mut State,

    /// The mailbox that can be used to send this thread messages. Note
    /// this is a blocking queue so if it is full you will get errors (or block).
    pub mailbox: Box<Mailbox>,

    /// Configuration we need derived from the main config.
    pub config: DerivedConfig,

    pub flags: Flags,
}

// SAFETY: like Zig, the thread struct is handed to the renderer thread
// which is the only one touching it (other threads only use the
// thread-safe mailbox and async handles).
unsafe impl Send for Thread {}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("animation_wake", &self.animation_wake)
            .field("config", &self.config)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// Errors from [`Thread`] (Zig: inferred error sets).
pub type Error = crate::renderer::generic::Error;

pub use crate::terminal::terminal::{CompressionMode, CompressionResult};

impl Thread {
    /// Initialize the thread. This does not START the thread. This only sets
    /// up all the internal state necessary prior to starting the thread. It
    /// is up to the caller to start the thread with the threadMain entrypoint.
    pub fn init(
        config: &Config,
        surface: *mut apprt::Surface,
        renderer_impl: *mut Renderer,
        state: *mut State,
    ) -> Result<Thread, xev::Error> {
        // Create our event loop.
        let mut r#loop = xev::Loop::init(xev::Options::default())?;

        // This async handle is used to "wake up" the renderer and force a render.
        // This async handle is used to stop the loop and force the thread to end.
        // Draw now async, see comments.
        let asyncs = (|| -> Result<_, xev::Error> {
            let mut wakeup_h = xev::Async::init()?;
            let mut stop_h = match xev::Async::init() {
                Ok(h) => h,
                Err(e) => {
                    wakeup_h.deinit();
                    return Err(e);
                }
            };
            let draw_now = match xev::Async::init() {
                Ok(h) => h,
                Err(e) => {
                    stop_h.deinit();
                    wakeup_h.deinit();
                    return Err(e);
                }
            };
            Ok((wakeup_h, stop_h, draw_now))
        })();
        let (wakeup_h, stop_h, draw_now) = match asyncs {
            Ok(v) => v,
            Err(e) => {
                r#loop.deinit();
                return Err(e);
            }
        };

        // The primary timer for rendering.
        // Setup a timer for blinking the cursor
        let render_h = xev::Timer::init()?;
        let cursor_timer = xev::Timer::init()?;

        // The mailbox for messaging this thread
        let mailbox = Mailbox::create();

        let mut result = Thread {
            r#loop,
            wakeup: wakeup_h,
            wakeup_c: xev::Completion::default(),
            stop: stop_h,
            stop_c: xev::Completion::default(),
            render_h,
            render_c: xev::Completion::default(),
            render_c_cancel: xev::Completion::default(),
            animation_wake: AnimationWakeKind::Draw,
            draw_now,
            draw_now_c: xev::Completion::default(),
            cursor_h: cursor_timer,
            cursor_c: xev::Completion::default(),
            cursor_c_cancel: xev::Completion::default(),
            compression: None,
            surface,
            renderer: renderer_impl,
            state,
            mailbox,
            config: DerivedConfig::init(config),
            flags: Flags::default(),
        };

        // Only enable compression if we have it enabled... save some
        // minor resources.
        if COMPRESSION_ENABLED {
            result.compression = Some(Compression::init()?);
        }

        Ok(result)
    }

    /// Clean up the thread. This is only safe to call once the thread
    /// completes executing; the caller must join prior to this.
    pub fn deinit(&mut self) {
        self.stop.deinit();
        self.wakeup.deinit();
        self.render_h.deinit();
        self.draw_now.deinit();
        self.cursor_h.deinit();
        if let Some(c) = &mut self.compression {
            c.deinit();
        }
        self.r#loop.deinit();

        // Nothing can possibly access the mailbox anymore; it is freed
        // with the thread.
    }

    /// The main entrypoint for the thread.
    ///
    /// The renderer, state and surface must stay valid until this returns.
    /// Other threads may only use copies of the async handles and the
    /// mailbox while this runs.
    pub fn thread_main(&mut self) {
        let this: *mut Thread = self;
        // Call child function so we can use errors...
        // SAFETY: `this` is valid for the whole call.
        if let Err(err) = unsafe { Self::thread_main_(this) } {
            // In the future, we should expose this on the thread struct.
            log::warn!(target: LOG, "error in renderer err={err}");
        }
    }

    /// # Safety
    ///
    /// `this` and the pointers it holds must stay valid until this returns.
    unsafe fn thread_main_(this: *mut Thread) -> Result<(), Error> {
        struct ExitLog;
        impl Drop for ExitLog {
            fn drop(&mut self) {
                log::debug!(target: LOG, "renderer thread exited");
            }
        }
        let _exit_log = ExitLog;

        // Right now, on Darwin, `std.Thread.setName` can only name the current
        // thread, and we have no way to get the current thread from within it,
        // so instead we use this code to name the thread instead.
        #[cfg(target_os = "macos")]
        // SAFETY: a valid NUL-terminated name.
        unsafe {
            crate::os::macos::pthread_setname_np(c"renderer".as_ptr());
        }

        // SAFETY: caller guarantee (for all raw pointer accesses below).
        let renderer = unsafe { (*this).renderer };

        // Setup our crash metadata
        crash::sentry::set_thread_state(Some(crash::sentry::ThreadState {
            r#type: crash::sentry::ThreadType::Renderer,
            surface: unsafe { (*renderer).surface_mailbox.surface },
        }));
        struct CrashGuard;
        impl Drop for CrashGuard {
            fn drop(&mut self) {
                crash::sentry::set_thread_state(None);
            }
        }
        let _crash_guard = CrashGuard;

        // Setup our thread QoS
        unsafe { (*this).set_qos_class() };

        // Run our loop start/end callbacks if the renderer cares.
        unsafe { (*renderer).loop_enter(this)? };
        struct LoopGuard(*mut Renderer);
        impl Drop for LoopGuard {
            fn drop(&mut self) {
                // SAFETY: the renderer outlives the thread main.
                unsafe { (*self.0).loop_exit() };
            }
        }
        let _loop_guard = LoopGuard(renderer);

        // Run our thread start/end callbacks. This is important because some
        // renderers have to do per-thread setup. For example, OpenGL has to set
        // some thread-local state since that is how it works.
        unsafe { (*renderer).thread_enter((*this).surface)? };
        struct ThreadGuard(*mut Renderer);
        impl Drop for ThreadGuard {
            fn drop(&mut self) {
                // SAFETY: the renderer outlives the thread main.
                unsafe { (*self.0).thread_exit() };
            }
        }
        let _thread_guard = ThreadGuard(renderer);

        // Start the async handlers
        // SAFETY: the completions live in the thread, which outlives the
        // loop run (caller guarantee).
        unsafe {
            let t = &mut *this;
            t.wakeup
                .wait(&mut t.r#loop, &mut t.wakeup_c, this, wakeup_callback);
            t.stop.wait(&mut t.r#loop, &mut t.stop_c, this, stop_callback);
            t.draw_now
                .wait(&mut t.r#loop, &mut t.draw_now_c, this, draw_now_callback);
        }

        // Send an initial wakeup message so that we render right away.
        unsafe { (*this).wakeup.notify()? };

        // Start blinking the cursor.
        unsafe {
            let t = &mut *this;
            t.cursor_h.run(
                &mut t.r#loop,
                &mut t.cursor_c,
                cursor_blink_interval(),
                this,
                cursor_timer_callback,
            );
        }

        // Arm the animation timer in case the renderer already needs
        // animation wakes (e.g. custom shaders loaded at startup).
        unsafe { arm_animation_timer(this, &mut (*this).r#loop) };

        // Run
        log::debug!(target: LOG, "starting renderer thread");
        let res = unsafe { (*this).r#loop.run(xev::RunMode::UntilDone) };
        log::debug!(target: LOG, "starting renderer thread shutdown");
        res?;
        Ok(())
    }

    fn set_qos_class(&self) {
        // Thread QoS classes are only relevant on macOS.
        #[cfg(target_os = "macos")]
        {
            use crate::os::macos::QosClass;

            let class = if !self.flags.visible {
                // If we aren't visible (our view is fully occluded) then we
                // always drop our rendering priority down because it's just
                // mostly wasted work.
                //
                // The renderer itself should be doing this as well (for example
                // Metal will stop our DisplayLink) but this also helps with
                // general forced updates and CPU usage i.e. a rebuild cells call.
                QosClass::Utility
            } else if !self.flags.focused {
                // If we're not focused, but we're visible, then we set a higher
                // than default priority because framerates still matter but it isn't
                // as important as when we're focused.
                QosClass::UserInitiated
            } else {
                // We are focused and visible, we are the definition of user interactive.
                QosClass::UserInteractive
            };

            match crate::os::macos::set_qos_class(class) {
                Ok(()) => log::debug!(target: LOG, "thread QoS class set class={class:?}"),
                Err(err) => log::warn!(target: LOG, "error setting QoS class err={err:?}"),
            }
        }
    }

    fn change_config(&mut self, config: &DerivedConfig) {
        // A newly enabled scheduler must reconsider existing history even when no
        // terminal activity occurred while compression was disabled.
        if COMPRESSION_ENABLED && !self.config.scrollback_compression && config.scrollback_compression {
            if let Some(c) = &mut self.compression {
                c.activity = None;
            }
        }

        self.config = *config;
    }
}

/// Drain the mailbox.
///
/// # Safety
///
/// Called on the renderer thread with a valid thread pointer; `l` is the
/// thread's loop.
unsafe fn drain_mailbox(t: *mut Thread, l: &mut xev::Loop) -> Result<(), Error> {
    // This is effectively an @autoreleasepool{} block, which we need in
    // order to ensure that autoreleased objects are properly released.
    #[cfg(target_os = "macos")]
    struct Pool(*mut std::ffi::c_void);
    #[cfg(target_os = "macos")]
    impl Drop for Pool {
        fn drop(&mut self) {
            // SAFETY: the pool was pushed in this scope.
            unsafe { crate::os::macos::objc::objc_autoreleasePoolPop(self.0) };
        }
    }
    #[cfg(target_os = "macos")]
    // SAFETY: plain runtime call.
    let _pool = Pool(unsafe { crate::os::macos::objc::objc_autoreleasePoolPush() });

    // SAFETY: caller guarantee (for all raw pointer accesses below).
    let mailbox: *const Mailbox = unsafe { &*(*t).mailbox };
    let renderer: *mut Renderer = unsafe { (*t).renderer };

    while let Some(message) = unsafe { (*mailbox).pop() } {
        log::debug!(target: LOG, "mailbox message={}", message_tag(&message));
        match message {
            Message::Crash => panic!("crash request, crashing intentionally"),

            Message::Visible(v) => unsafe {
                // If our state didn't change we do nothing.
                if (*t).flags.visible != v {
                    // Set our visible state
                    (*t).flags.visible = v;

                    // Visibility affects our QoS class
                    (*t).set_qos_class();

                    // If we became visible then we immediately rebuild cells
                    // (renderCallback skips updateFrame while invisible) and
                    // draw. Going through renderCallback also reschedules
                    // any Kitty graphics animation wakeup that lapsed
                    // while we were invisible.
                    if v {
                        let _ = render_callback_(t, l);
                    }

                    // Notify the renderer so it can update any state.
                    (*renderer).set_visible(v);

                    // Note that we're explicitly today not stopping any
                    // cursor timers, draw timers, etc. These things have very
                    // little resource cost and properly maintaining their active
                    // state across different transitions is going to be bug-prone,
                    // so its easier to just let them keep firing and have them
                    // check the visible state themselves to control their behavior.
                }
            },

            Message::Focus(v) => unsafe {
                // If our state didn't change we do nothing.
                if (*t).flags.focused != v {
                    // Set our state
                    (*t).flags.focused = v;

                    // Focus affects our QoS class
                    (*t).set_qos_class();

                    // Set it on the renderer
                    (*renderer).set_focus(v)?;

                    // Focus gates custom shader animation, so re-arm
                    // the animation timer for the new state.
                    arm_animation_timer(t, l);

                    if !v {
                        // If we're not focused, then we stop the cursor blink
                        if (*t).cursor_c.state() == xev::CompletionState::Active
                            && (*t).cursor_c_cancel.state() == xev::CompletionState::Dead
                        {
                            (*t).cursor_h.cancel(
                                l,
                                &mut (*t).cursor_c,
                                &mut (*t).cursor_c_cancel,
                                std::ptr::null_mut::<()>(),
                                cursor_cancel_callback,
                            );
                        }
                    } else {
                        // If we're focused, we immediately show the cursor again
                        // and then restart the timer.
                        if (*t).cursor_c.state() != xev::CompletionState::Active {
                            (*t).flags.cursor_blink_visible = true;
                            (*t).cursor_h.run(
                                l,
                                &mut (*t).cursor_c,
                                cursor_blink_interval(),
                                t,
                                cursor_timer_callback,
                            );
                        }
                    }
                }
            },

            Message::ResetCursorBlink => unsafe {
                (*t).flags.cursor_blink_visible = true;
                if (*t).cursor_c.state() == xev::CompletionState::Active {
                    (*t).cursor_h.reset(
                        l,
                        &mut (*t).cursor_c,
                        &mut (*t).cursor_c_cancel,
                        cursor_blink_interval(),
                        t,
                        cursor_timer_callback,
                    );
                }
            },

            Message::FontGrid { grid, set, old_key, .. } => {
                unsafe { (*renderer).set_font_grid(grid) };
                // SAFETY: the grid set outlives every grid it hands out.
                unsafe { (*set).deref(&old_key) };
            }

            Message::PresentationHealth(v) => unsafe { (*renderer).set_presentation_health(v) },

            Message::Resize(v) => unsafe { (*renderer).set_screen_size(v) },

            Message::ChangeConfig { thread, mut impl_ } => {
                unsafe { (*t).change_config(&thread) };
                unsafe { (*renderer).change_config(&mut impl_)? };

                // The config affects what animation wakes the
                // renderer needs (custom shaders, animation mode).
                unsafe { arm_animation_timer(t, l) };
            }

            Message::SearchViewportMatches(v) => unsafe {
                // The previous value is freed on drop.
                (*renderer).search_matches = Some(v);
                (*renderer).search_matches_dirty = true;
            },

            Message::SearchSelectedMatch(v) => unsafe {
                // The previous value is freed on drop.
                (*renderer).search_selected_match = v;
                (*renderer).search_matches_dirty = true;
            },

            Message::Inspector(v) => unsafe {
                (*t).flags.has_inspector = v;
            },

            Message::MacosDisplayId(v) => unsafe {
                (*renderer).set_macos_display_id(v, &mut (*t).draw_now)?;
            },
        }
    }

    Ok(())
}

/// A short name for a message, for logging (Zig formats the union).
fn message_tag(message: &Message) -> &'static str {
    match message {
        Message::Crash => "crash",
        Message::Focus(_) => "focus",
        Message::Visible(_) => "visible",
        Message::ResetCursorBlink => "reset_cursor_blink",
        Message::FontGrid { .. } => "font_grid",
        Message::Resize(_) => "resize",
        Message::ChangeConfig { .. } => "change_config",
        Message::SearchViewportMatches(_) => "search_viewport_matches",
        Message::SearchSelectedMatch(_) => "search_selected_match",
        Message::Inspector(_) => "inspector",
        Message::MacosDisplayId(_) => "macos_display_id",
        Message::PresentationHealth(_) => "presentation_health",
    }
}

/// Trigger a draw. This will not update frame data or anything, it will
/// just trigger a draw/paint.
///
/// # Safety
///
/// Valid thread pointer.
unsafe fn draw_frame(t: *mut Thread, now: bool) {
    // SAFETY: caller guarantee.
    unsafe {
        // If we're invisible, we do not draw.
        if !(*t).flags.visible {
            return;
        }

        let renderer = &mut *(*t).renderer;

        // If the renderer is managing a vsync on its own, we only draw
        // when we're forced to via `now`.
        if !now && renderer.has_vsync() {
            return;
        }

        if let Err(err) = renderer.draw_frame(false) {
            log::warn!(target: LOG, "error drawing err={err}");
        }
    }
}

fn wakeup_callback(
    self_: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if let Err(err) = r {
        log::error!(target: LOG, "error in wakeup err={err}");
        return xev::CallbackAction::Rearm;
    }

    let t = self_;
    debug_assert!(!t.is_null());

    // When we wake up, we check the mailbox. Mailbox producers should
    // wake up our thread after publishing.
    // SAFETY: the thread outlives the loop run.
    if let Err(err) = unsafe { drain_mailbox(t, l) } {
        log::error!(target: LOG, "error draining mailbox err={err}");
    }

    // Render immediately
    // SAFETY: as above.
    let _ = unsafe { render_callback_(t, l) };

    // PageList mutations maintain their own compression dirty state. Checking
    // it here covers output, resize, and viewport scrolling uniformly.
    // SAFETY: as above.
    unsafe { Compression::wake(t, l) };

    // The below is not used anymore but if we ever want to introduce
    // a configuration to introduce a delay to coalesce renders, we can
    // use this (start `render_h` with `render_callback` if it is not
    // already active).

    xev::CallbackAction::Rearm
}

fn draw_now_callback(
    self_: *mut Thread,
    _: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if let Err(err) = r {
        log::error!(target: LOG, "error in draw now err={err}");
        return xev::CallbackAction::Rearm;
    }

    // Draw immediately
    // SAFETY: the thread outlives the loop run.
    unsafe { draw_frame(self_, true) };

    xev::CallbackAction::Rearm
}

/// The xev timer callback form of `renderCallback`.
#[allow(dead_code)]
fn render_callback(
    self_: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) => {}
        // Sent when a scheduled animation wakeup is superseded by a
        // newer one (Timer.reset cancels the pending run). Nothing to
        // do; the replacement timer carries on.
        Err(xev::timer::RunError::Canceled) => return xev::CallbackAction::Disarm,
        Err(_) => unreachable!(),
    }
    if self_.is_null() {
        // This shouldn't happen so we log it.
        log::warn!(target: LOG, "render callback fired without data set");
        return xev::CallbackAction::Disarm;
    }

    // SAFETY: the thread outlives the loop run.
    unsafe { render_callback_(self_, l) }
}

/// The body of `renderCallback` (Zig calls it directly with `undefined`
/// loop/completion arguments).
///
/// # Safety
///
/// Valid thread pointer; `l` is the thread's loop.
unsafe fn render_callback_(t: *mut Thread, l: &mut xev::Loop) -> xev::CallbackAction {
    // SAFETY: caller guarantee.
    unsafe {
        let renderer = &mut *(*t).renderer;

        // If the display is now unrealized, release GPU resources now
        // we're on the render thread, and do not try to update and draw
        // this frame.
        if !renderer.display_realized {
            renderer.draw_mutex.lock_uncancelable(TinyIo::INIT);
            renderer.release_gpu_resources();
            renderer.draw_mutex.unlock(TinyIo::INIT);
            return xev::CallbackAction::Disarm;
        }

        // If we're not visible there's no point spending CPU rebuilding cells —
        // we'll catch up when the .visible mailbox message flips us back on.
        // Kitty graphics animations pause with us and resume on visibility.
        if !(*t).flags.visible {
            return xev::CallbackAction::Disarm;
        }

        // Update our frame data
        if let Err(err) = renderer.update_frame(&*(*t).state, (*t).flags.cursor_blink_visible) {
            log::warn!(target: LOG, "error rendering err={err}");
        }

        // Draw
        draw_frame(t, false);

        // Schedule the next animation wake, if the renderer needs one.
        arm_animation_timer(t, l);
    }

    xev::CallbackAction::Disarm
}

/// Schedule the animation timer for the renderer's next animation
/// wake, if it needs one.
///
/// This is called after every frame update or animation draw and
/// whenever the wake inputs change (focus, config, visibility
/// regain). Resetting a pending timer is always safe: every call
/// recomputes the wake, so the deadline only ever moves toward the
/// actual next wake.
///
/// # Safety
///
/// Valid thread pointer; `l` is the thread's loop.
unsafe fn arm_animation_timer(t: *mut Thread, l: &mut xev::Loop) {
    // SAFETY: caller guarantee; the completions live in the thread.
    unsafe {
        let Some(wake) = (*(*t).renderer).animation_wake() else {
            return;
        };
        (*t).animation_wake = wake.kind;
        (*t).render_h.reset(
            l,
            &mut (*t).render_c,
            &mut (*t).render_c_cancel,
            wake.delay_ms,
            t,
            animation_timer_callback,
        );
    }
}

fn animation_timer_callback(
    self_: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) => {}
        // Sent when a scheduled animation wake is superseded by a
        // newer one (Timer.reset cancels the pending run). Nothing to
        // do; the replacement timer carries on.
        Err(xev::timer::RunError::Canceled) => return xev::CallbackAction::Disarm,
        Err(_) => unreachable!(),
    }
    let t = self_;
    if t.is_null() {
        // This shouldn't happen so we log it.
        log::warn!(target: LOG, "animation callback fired without data set");
        return xev::CallbackAction::Disarm;
    }

    // SAFETY: the thread outlives the loop run.
    unsafe {
        // Animations pause entirely while we're invisible; the .visible
        // mailbox message re-arms us when we can be seen again.
        if !(*t).flags.visible {
            return xev::CallbackAction::Disarm;
        }

        match (*t).animation_wake {
            // Frame data must be updated (a Kitty animation frame is
            // due). renderCallback updates, draws, and re-arms us.
            AnimationWakeKind::Update => render_callback_(t, l),

            // A redraw alone suffices (custom shader time uniform).
            // Draw calls don't update from the terminal state so they
            // are much cheaper than a frame update.
            AnimationWakeKind::Draw => {
                draw_frame(t, false);
                arm_animation_timer(t, l);
                xev::CallbackAction::Disarm
            }
        }
    }
}

fn cursor_timer_callback(
    self_: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) => {}
        // This is sent when our timer is canceled. That's fine.
        Err(xev::timer::RunError::Canceled) => return xev::CallbackAction::Disarm,
        Err(err) => {
            log::warn!(target: LOG, "error in cursor timer callback err={err}");
            unreachable!();
        }
    }

    let t = self_;
    if t.is_null() {
        // This shouldn't happen so we log it.
        log::warn!(target: LOG, "render callback fired without data set");
        return xev::CallbackAction::Disarm;
    }

    // SAFETY: the thread outlives the loop run; the completion lives in it.
    unsafe {
        (*t).flags.cursor_blink_visible = !(*t).flags.cursor_blink_visible;
        let _ = (*t).wakeup.notify();

        (*t).cursor_h.run(
            l,
            &mut (*t).cursor_c,
            cursor_blink_interval(),
            t,
            cursor_timer_callback,
        );
    }
    xev::CallbackAction::Disarm
}

fn cursor_cancel_callback(
    _: *mut (),
    _: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::timer::CancelError>,
) -> xev::CallbackAction {
    // This makes it easier to work across platforms where different platforms
    // support different sets of errors, so we just unify it.
    match r {
        Ok(()) => {}                    // success
        Err(xev::Error::NotFound) => {} // completed before it could cancel
        Err(err) => {
            log::warn!(target: LOG, "error in cursor cancel callback err={err}");
            unreachable!();
        }
    }

    xev::CallbackAction::Disarm
}

fn stop_callback(
    _self: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    r.unwrap_or_else(|_| unreachable!());
    l.stop();
    xev::CallbackAction::Disarm
}

/// Whether we're running under Valgrind (approximated by its preload
/// libraries; Zig uses a Valgrind client request).
fn running_on_valgrind() -> bool {
    std::env::var_os("LD_PRELOAD")
        .is_some_and(|v| v.to_string_lossy().contains("vgpreload"))
}

/// Returns the interval for the blinking cursor in milliseconds.
fn cursor_blink_interval() -> u64 {
    if running_on_valgrind() {
        // If we're running under Valgrind, the cursor blink adds enough
        // churn that it makes some stalls annoying unless you're on a
        // super powerful computer, so we delay it.
        //
        // This is a hack, we should change some of our cursor timer
        // logic to be more efficient:
        // https://github.com/ghostty-org/ghostty/issues/8003
        return CURSOR_BLINK_INTERVAL * 5;
    }

    CURSOR_BLINK_INTERVAL
}

/// Schedules incremental terminal compression after renderer activity stops.
///
/// This owns all renderer-specific compression state. The terminal decides
/// when compression-relevant activity changes and performs the actual work;
/// the renderer only provides idle scheduling and avoids waiting for the
/// terminal lock.
struct Compression {
    timer: xev::Timer,
    completion: xev::Completion,
    reset_completion: xev::Completion,
    activity: Option<u64>,
}

impl Compression {
    const IDLE_INTERVAL: u64 = 250;
    const STEP_INTERVAL: u64 = 1;

    fn init() -> Result<Compression, xev::Error> {
        Ok(Compression {
            timer: xev::Timer::init()?,
            completion: xev::Completion::default(),
            reset_completion: xev::Completion::default(),
            activity: None,
        })
    }

    fn deinit(&mut self) {
        self.timer.deinit();
    }

    /// Start or postpone compression after a renderer wake.
    ///
    /// # Safety
    ///
    /// Valid thread pointer; `l` is the thread's loop.
    unsafe fn wake(thread: *mut Thread, l: &mut xev::Loop) {
        // If we have no compression then don't do anything.
        if !COMPRESSION_ENABLED {
            return;
        }
        // SAFETY: caller guarantee.
        unsafe {
            if !(*thread).config.scrollback_compression {
                return;
            }
            let Some(this) = (*thread).compression.as_mut() else {
                return;
            };
            let state = &*(*thread).state;

            // PageList activity, rather than a generic renderer wake, restarts the
            // idle interval. In particular, the inspector wakes the renderer every
            // frame without changing terminal contents and must not starve this
            // timer indefinitely.
            let mutex = state.mutex.as_ref();
            if mutex.try_lock() {
                let activity = state.terminal.as_ref().compression_activity();
                mutex.unlock(TinyIo::INIT);
                if this.activity == Some(activity) {
                    return;
                }
                this.activity = Some(activity);
            } else if this.completion.state() == xev::CompletionState::Active {
                // Contention doesn't prove that compression-relevant activity
                // changed. Keep an existing deadline so frequent inspector frames
                // cannot postpone compression forever. The timer rechecks both the
                // activity token and lock availability before doing any work.
                return;
            }

            // Contention may mean parsing is active. Scheduling is a harmless
            // false positive when no compression work is actually pending, but is
            // necessary when no timer is already active.
            Self::schedule(thread, l, Self::IDLE_INTERVAL);
        }
    }

    /// Start the one-shot timer, or move its deadline if it is already active.
    ///
    /// # Safety
    ///
    /// Valid thread pointer with compression initialized; `l` is the
    /// thread's loop.
    unsafe fn schedule(thread: *mut Thread, l: &mut xev::Loop, delay_ms: u64) {
        // SAFETY: caller guarantee; the completions live in the thread.
        unsafe {
            let this = (*thread).compression.as_mut().unwrap();
            this.timer.reset(
                l,
                &mut this.completion,
                &mut this.reset_completion,
                delay_ms,
                thread,
                Self::timer_callback,
            );
        }
    }

    fn timer_callback(
        thread_: *mut Thread,
        l: &mut xev::Loop,
        _: &mut xev::Completion,
        result: Result<(), xev::timer::RunError>,
    ) -> xev::CallbackAction {
        match result {
            Ok(()) => {}
            Err(xev::timer::RunError::Canceled) => return xev::CallbackAction::Disarm,
            Err(err) => {
                log::warn!(target: LOG, "error in compression timer err={err}");
                return xev::CallbackAction::Disarm;
            }
        }

        let thread = thread_;
        if thread.is_null() {
            return xev::CallbackAction::Disarm;
        }

        // SAFETY: the thread outlives the loop run.
        unsafe {
            if let Some(delay) = Self::step(thread) {
                Self::schedule(thread, l, delay);
            }
        }
        xev::CallbackAction::Disarm
    }

    /// Try one bounded step without waiting for the terminal lock. The return
    /// value is the delay before another attempt, or null when work is done.
    ///
    /// # Safety
    ///
    /// Valid thread pointer with compression initialized.
    unsafe fn step(thread: *mut Thread) -> Option<u64> {
        // SAFETY: caller guarantee.
        unsafe {
            if !(*thread).config.scrollback_compression {
                return None;
            }

            let this = (*thread).compression.as_mut().unwrap();
            let state = &*(*thread).state;
            let mutex = state.mutex.as_ref();
            if !mutex.try_lock() {
                return Some(Self::IDLE_INTERVAL);
            }
            struct Unlock<'a>(&'a crate::lib::tinyio::Mutex);
            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    self.0.unlock(TinyIo::INIT);
                }
            }
            let _unlock = Unlock(mutex);

            let terminal = &mut *state.terminal.as_ptr();
            let activity = terminal.compression_activity();
            if this.activity != Some(activity) {
                this.activity = Some(activity);
                return Some(Self::IDLE_INTERVAL);
            }

            match terminal.compress(CompressionMode::Incremental) {
                CompressionResult::Pending => Some(Self::STEP_INTERVAL),
                CompressionResult::Unsupported | CompressionResult::Complete => None,
            }
        }
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    // Not a Zig test: Thread.zig has none. This runs the thread with the
    // OpenGL renderer against a surfaceless EGL device.
    use std::ptr::NonNull;

    use super::*;
    use crate::datastruct::blocking_queue::Timeout;
    use crate::lib::tinyio::Mutex;
    use crate::renderer::message::SearchMatches;
    use crate::renderer::opengl::device::{self, test_context::egl_lock, Device};
    use crate::app::MailboxQueue as AppQueue;
    use crate::renderer::opengl::test_util::test_options_with_mailbox;
    use crate::renderer::{Health, ScreenSize, Size};
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    struct SendPtr(*mut Thread);
    // SAFETY: the test only reads thread-safe parts until it joins.
    unsafe impl Send for SendPtr {}

    #[test]
    fn run_messages_stop() {
        let _guard = egl_lock();
        let mut device = Device::init(device::Options::default()).unwrap();
        let app = apprt::App::default();
        let queue = AppQueue::create();
        let opts = test_options_with_mailbox(&mut *device, &app, &*queue);
        let mut renderer = Renderer::init(opts).unwrap();

        let mut mutex = Mutex::new();
        let mut terminal = Terminal::init(TerminalOptions::new(80, 24)).unwrap();
        let mut state = State::new(NonNull::from(&mut mutex), NonNull::from(&mut terminal));
        let mut surface = apprt::Surface::default();

        let config = Config::default();
        let mut thread = Thread::init(&config, &mut surface, &mut renderer, &mut state).unwrap();

        // Queue messages before starting; the initial wakeup drains them.
        let size = Size {
            screen: ScreenSize {
                width: 100,
                height: 50,
            },
            ..Default::default()
        };
        let mailbox: *const Mailbox = &*thread.mailbox;
        // SAFETY: the mailbox is thread-safe and outlives the thread run.
        let mailbox = unsafe { &*mailbox };
        mailbox.push(Message::Focus(false), Timeout::Forever);
        mailbox.push(Message::Resize(size), Timeout::Forever);
        mailbox.push(Message::Inspector(true), Timeout::Forever);
        mailbox.push(Message::SearchViewportMatches(SearchMatches::default()), Timeout::Forever);
        mailbox.push(Message::ResetCursorBlink, Timeout::Forever);
        mailbox.push(Message::Visible(true), Timeout::Forever);
        // A marker: the thread is done draining once this is applied.
        mailbox.push(Message::PresentationHealth(Health::Unhealthy), Timeout::Forever);

        let stop = thread.stop;
        let renderer_ptr: *const Renderer = &renderer;
        let ptr = SendPtr(&mut thread);
        let handle = std::thread::spawn(move || {
            let ptr = ptr;
            // SAFETY: the main thread waits for this thread to finish.
            unsafe { (*ptr.0).thread_main() };
        });

        // Wait until the mailbox is drained, then stop the thread.
        let start = std::time::Instant::now();
        // SAFETY: only the atomic health is read while the thread runs.
        while unsafe { (*renderer_ptr).presentation_health.load() } != Health::Unhealthy {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "timeout");
            std::thread::yield_now();
        }
        stop.notify().unwrap();
        handle.join().unwrap();

        assert!(!thread.flags.focused);
        assert!(thread.flags.has_inspector);
        assert!(thread.flags.cursor_blink_visible);
        assert!(!renderer.focused);
        assert!(renderer.search_matches.is_some());
        // (`search_matches_dirty` is not checked: the frame update that
        // follows the message may already have consumed it.)
        assert_eq!(Health::Unhealthy, renderer.presentation_health());
        assert_eq!(100, renderer.size.screen.width);
        // threadExit marks the display unrealized.
        assert!(!renderer.display_realized);

        thread.deinit();
        renderer.deinit();
        device.deinit();
    }
}
