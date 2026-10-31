//! Port of `src/apprt/gtk/winproto/x11.zig`.
//!
//! STATUS: ported
//!
//! X11 window protocol implementation for the Ghostty GTK apprt.
//!
//! Zig's `Window` keeps a `*App` to reach the Xlib display and the atoms;
//! the port copies those (a raw pointer and two integers) into the window.

use std::os::raw::{c_int, c_long, c_uchar, c_uint, c_ulong};
use std::rc::Rc;

use gdk4_x11::prelude::*;
use gtk4::gdk;
use gtk4::prelude::*;
use x11::xlib;

use super::blur_region::{self, BlurRegion};
use super::{ApprtWindow, Error, GlobalShortcuts};
use crate::command::EnvMap;
use crate::config::config::{Config, WindowDecoration};
use crate::input::key_mods::Mods;

const LOG: &str = "gtk_x11";

// Constants from X11/XKBlib.h and X11/Xlib.h that the x11 crate lacks.
const XKB_MAJOR_VERSION: c_int = 1;
const XKB_MINOR_VERSION: c_int = 0;
const XKB_USE_CORE_KBD: c_uint = 0x0100;
const QUEUED_AFTER_READING: c_int = 1;
const CONTROL_MASK: c_uint = 1 << 2;
const MOD1_MASK: c_uint = 1 << 3;

pub struct App {
    pub display: *mut xlib::Display,
    pub base_event_code: c_int,
    atoms: Atoms,
}

impl App {
    pub fn init(gdk_display: &gdk::Display, app_id: &str, config: &Config) -> Result<Option<App>, Error> {
        // If the display isn't X11, then we don't need to do anything.
        let Some(gdk_x11_display) = gdk_display.downcast_ref::<gdk4_x11::X11Display>() else {
            return Ok(None);
        };

        // SAFETY: the display is an X11 display.
        let xlib_display = unsafe { gdk_x11_display.xdisplay() };

        let x11_program_name: &str = match &config.x11_instance_name {
            Some(pn) => pn.as_str(),
            None if cfg!(debug_assertions) => "ghostty-debug",
            None => "ghostty",
        };

        // Set the X11 window class property (WM_CLASS) if we are on an X11
        // display.
        //
        // Note that we also set the program name here using g_set_prgname.
        // This is how the instance name field for WM_CLASS is derived when
        // calling gdk_x11_display_set_program_class; there does not seem to be
        // a way to set it directly. It does not look like this is being set by
        // our other app initialization routines currently, but since we're
        // currently deriving its value from x11-instance-name effectively, I
        // feel like gating it behind an X11 check is better intent.
        //
        // This makes the property show up like so when using xprop:
        //
        //     WM_CLASS(STRING) = "ghostty", "com.mitchellh.ghostty"
        //
        // Append "-debug" on both when using the debug build.
        glib::set_prgname(Some(x11_program_name));
        gdk_x11_display.set_program_class(app_id);

        // XKB
        log::debug!(target: LOG, "Xkb.init: initializing Xkb");
        log::debug!(target: LOG, "Xkb.init: running XkbQueryExtension");
        let mut opcode: c_int = 0;
        let mut base_event_code: c_int = 0;
        let mut base_error_code: c_int = 0;
        let mut major = XKB_MAJOR_VERSION;
        let mut minor = XKB_MINOR_VERSION;
        // SAFETY: valid display and out pointers.
        if unsafe {
            xlib::XkbQueryExtension(
                xlib_display,
                &mut opcode,
                &mut base_event_code,
                &mut base_error_code,
                &mut major,
                &mut minor,
            )
        } == 0
        {
            log::error!(
                target: LOG,
                "Fatal: error initializing Xkb extension: error executing XkbQueryExtension"
            );
            return Err(Error::XkbInitializationError);
        }

        log::debug!(target: LOG, "Xkb.init: running XkbSelectEventDetails");
        // SAFETY: valid display.
        if unsafe {
            xlib::XkbSelectEventDetails(
                xlib_display,
                XKB_USE_CORE_KBD,
                xlib::XkbStateNotify as c_uint,
                xlib::XkbModifierStateMask,
                xlib::XkbModifierStateMask,
            )
        } == 0
        {
            log::error!(
                target: LOG,
                "Fatal: error initializing Xkb extension: error executing XkbSelectEventDetails"
            );
            return Err(Error::XkbInitializationError);
        }

        Ok(Some(App {
            display: xlib_display,
            base_event_code,
            atoms: Atoms::init(gdk_x11_display),
        }))
    }

    pub fn deinit(&mut self) {}

    /// Checks for an immediate pending XKB state update event, and returns the
    /// keyboard state based on if it finds any. This is necessary as the
    /// standard GTK X11 API (and X11 in general) does not include the current
    /// key pressed in any modifier state snapshot for that event (e.g. if the
    /// pressed key is a modifier, that is not necessarily reflected in the
    /// modifiers).
    ///
    /// Returns null if there is no event. In this case, the caller should fall
    /// back to the standard GDK modifier state (this likely means the key
    /// event did not result in a modifier change).
    pub fn event_mods(&mut self, _device: Option<&gdk::Device>, _gtk_mods: gdk::ModifierType) -> Option<Mods> {
        // Shoutout to Mozilla for figuring out a clean way to do this, this is
        // paraphrased from Firefox/Gecko in widget/gtk/nsGtkKeyUtils.cpp.
        // SAFETY: valid display; XPeekEvent fills the event union.
        unsafe {
            if xlib::XEventsQueued(self.display, QUEUED_AFTER_READING) == 0 {
                return None;
            }

            let mut next_event: xlib::XEvent = std::mem::zeroed();
            xlib::XPeekEvent(self.display, &mut next_event);
            if next_event.type_ != self.base_event_code {
                return None;
            }

            let xkb_event = &*(&next_event as *const xlib::XEvent as *const xlib::XkbAnyEvent);
            if xkb_event.xkb_type != xlib::XkbStateNotify {
                return None;
            }

            let xkb_state_notify_event =
                &*(&next_event as *const xlib::XEvent as *const xlib::XkbStateNotifyEvent);
            // Check the state according to XKB masks.
            let lookup_mods = xkb_state_notify_event.lookup_mods as c_uint;
            let mut mods = Mods::default();

            log::debug!(
                target: LOG,
                "X11: found extra XkbStateNotify event w/lookup_mods: {lookup_mods:b}"
            );
            if lookup_mods & xlib::ShiftMask != 0 {
                mods.shift = true;
            }
            if lookup_mods & CONTROL_MASK != 0 {
                mods.ctrl = true;
            }
            if lookup_mods & MOD1_MASK != 0 {
                mods.alt = true;
            }
            if lookup_mods & xlib::Mod4Mask != 0 {
                mods.super_ = true;
            }
            if lookup_mods & xlib::LockMask != 0 {
                mods.caps_lock = true;
            }

            Some(mods)
        }
    }

    pub fn bind_global_shortcuts(&mut self, _: Rc<dyn GlobalShortcuts>, _: &Config) -> bool {
        false
    }

    pub fn clear_global_shortcuts(&mut self) {}

    pub fn supports_quick_terminal(&self) -> bool {
        log::warn!(target: LOG, "quick terminal is not yet supported on X11");
        false
    }

    pub fn init_quick_terminal(&mut self, _: &dyn ApprtWindow) -> Result<(), Error> {
        Ok(())
    }
}

pub struct Window {
    display: *mut xlib::Display,
    atoms: Atoms,
    apprt_window: Rc<dyn ApprtWindow>,
    x11_surface: gdk4_x11::X11Surface,

    blur_region: BlurRegion,

    // Cache last applied values to avoid redundant X11 property updates.
    // Redundant property updates seem to cause some visual glitches
    // with some window managers: https://github.com/ghostty-org/ghostty/pull/8075
    last_applied_decoration_hints: Option<MotifWMHints>,
}

impl Window {
    pub fn init(app: &mut App, apprt_window: Rc<dyn ApprtWindow>) -> Result<Window, Error> {
        let surface = apprt_window
            .window()
            .upcast_ref::<gtk4::Native>()
            .surface()
            .ok_or(Error::NotX11Surface)?;

        let x11_surface = surface
            .downcast::<gdk4_x11::X11Surface>()
            .map_err(|_| Error::NotX11Surface)?;

        Ok(Window {
            display: app.display,
            atoms: app.atoms,
            apprt_window,
            x11_surface,
            blur_region: BlurRegion::EMPTY,
            last_applied_decoration_hints: None,
        })
    }

    pub fn deinit(&mut self) {
        self.blur_region.deinit();
    }

    pub fn resize_event(&mut self) -> Result<(), Error> {
        // The blur region must update with window resizes
        if let Err(err) = self.sync_blur() {
            log::error!(target: LOG, "failed to sync blur={err}");
        }
        Ok(())
    }

    pub fn sync_appearance(&mut self) -> Result<(), Error> {
        // The user could have toggled between CSDs and SSDs,
        // therefore we need to recalculate the blur region offset.
        if let Err(err) = self.sync_blur() {
            log::error!(target: LOG, "failed to sync blur={err}");
        }
        if let Err(err) = self.sync_decorations() {
            log::error!(target: LOG, "failed to sync decorations={err}");
        }
        Ok(())
    }

    pub fn client_side_decoration_enabled(&self) -> bool {
        match self.apprt_window.get_window_decoration() {
            WindowDecoration::Auto | WindowDecoration::Client => true,
            WindowDecoration::Server | WindowDecoration::None => false,
        }
    }

    fn sync_blur(&mut self) -> Result<(), Error> {
        let Some(config) = self.apprt_window.get_config() else {
            return Ok(());
        };

        // When blur is disabled, remove the property if it was previously set
        let blur = config.background_blur;

        let region = if blur.enabled() {
            BlurRegion::calc_for_window(
                &*self.apprt_window,
                self.client_side_decoration_enabled(),
                true,
            )
        } else {
            BlurRegion::EMPTY
        };

        // Only update X11 properties when the blur region actually changes
        if region.eql(&self.blur_region) {
            return Ok(());
        }

        if !region.slices.is_empty() {
            log::debug!(
                target: LOG,
                "set blur={:?}, window xid={}, region={}",
                blur,
                self.x11_surface.xid(),
                region,
            );

            self.change_property(
                self.atoms.kde_blur,
                xlib::XA_CARDINAL,
                PropertyFormat::_32,
                PropertyChangeMode::Replace,
                &region.slices,
            )?;
        } else {
            self.delete_property(self.atoms.kde_blur)?;
        }

        self.blur_region = region;
        Ok(())
    }

    fn sync_decorations(&mut self) -> Result<(), Error> {
        let mut hints = MotifWMHints::default();

        match self.get_window_property(
            self.atoms.motif_wm_hints,
            self.atoms.motif_wm_hints,
            PropertyFormat::_32,
            GetPropertyOptions::default(),
            &mut hints,
        ) {
            Ok(()) => {}
            // motif_wm_hints is already initialized, so this is fine
            Err(Error::PropertyNotFound) => {}
            Err(err) => return Err(err),
        }

        hints.set_flags_decorations(true);
        hints.set_decorations_all(match self.apprt_window.get_window_decoration() {
            WindowDecoration::Server => true,
            WindowDecoration::Auto | WindowDecoration::Client | WindowDecoration::None => false,
        });

        // Only update decoration hints when they actually change
        if let Some(last) = self.last_applied_decoration_hints {
            if hints == last {
                return Ok(());
            }
        }

        self.change_property(
            self.atoms.motif_wm_hints,
            self.atoms.motif_wm_hints,
            PropertyFormat::_32,
            PropertyChangeMode::Replace,
            &[hints],
        )?;
        self.last_applied_decoration_hints = Some(hints);
        Ok(())
    }

    pub fn add_subprocess_env(&mut self, env: &mut EnvMap) -> Result<(), Error> {
        let window_id = self.x11_surface.xid().to_string();

        env.insert("WINDOWID".to_owned(), window_id);
        Ok(())
    }

    pub fn set_urgent(&mut self, urgent: bool) -> Result<(), Error> {
        self.x11_surface.set_urgency_hint(urgent);
        Ok(())
    }

    fn get_window_property<T: Copy>(
        &self,
        name: xlib::Atom,
        typ: xlib::Atom,
        format: PropertyFormat,
        options: GetPropertyOptions,
        result: &mut T,
    ) -> Result<(), Error> {
        // FIXME: Maybe we should switch to libxcb one day.
        // Sounds like a much better idea than whatever this is
        let mut actual_type_return: xlib::Atom = 0;
        let mut actual_format_return: c_int = 0;
        let mut nitems_return: c_ulong = 0;
        let mut bytes_after_return: c_ulong = 0;
        let mut prop_return: *mut c_uchar = std::ptr::null_mut();

        // SAFETY: valid display and window; out pointers are valid.
        let code = unsafe {
            xlib::XGetWindowProperty(
                self.display,
                self.x11_surface.xid(),
                name,
                options.offset,
                options.length,
                options.delete as c_int,
                typ,
                &mut actual_type_return,
                &mut actual_format_return,
                &mut nitems_return,
                &mut bytes_after_return,
                &mut prop_return,
            )
        };
        if code != xlib::Success as c_int {
            return Err(Error::RequestFailed);
        }

        let free = |p: *mut c_uchar| {
            if !p.is_null() {
                // SAFETY: allocated by Xlib.
                unsafe { xlib::XFree(p as *mut _) };
            }
        };

        if actual_type_return == 0 {
            free(prop_return);
            return Err(Error::PropertyNotFound);
        }
        if typ != actual_type_return {
            free(prop_return);
            return Err(Error::PropertyTypeMismatch);
        }
        if format as c_int != actual_format_return {
            free(prop_return);
            return Err(Error::PropertyFormatMismatch);
        }

        // Xlib returns 32-bit data as `long`s; the buffer must hold a whole
        // `T` (Zig reads it unconditionally).
        let available = nitems_return as usize * format.elem_size();
        if prop_return.is_null() || available < std::mem::size_of::<T>() {
            free(prop_return);
            return Err(Error::PropertyFormatMismatch);
        }

        // SAFETY: the buffer holds at least size_of::<T>() bytes, aligned
        // for `long` by Xlib.
        *result = unsafe { std::ptr::read_unaligned(prop_return as *const T) };
        free(prop_return);
        Ok(())
    }

    fn change_property<T: Copy>(
        &self,
        name: xlib::Atom,
        typ: xlib::Atom,
        format: PropertyFormat,
        mode: PropertyChangeMode,
        values: &[T],
    ) -> Result<(), Error> {
        let data = values.as_ptr() as *const c_uchar;
        // The number of "words" that each element `T` occupies.
        debug_assert!(std::mem::size_of::<T>() % format.elem_size() == 0);
        let words_per_elem = std::mem::size_of::<T>() / format.elem_size();

        // SAFETY: `data` points to `values`, which spans
        // `words_per_elem * values.len()` elements of the format.
        let status = unsafe {
            xlib::XChangeProperty(
                self.display,
                self.x11_surface.xid(),
                name,
                typ,
                format as c_int,
                mode as c_int,
                data,
                (words_per_elem * values.len()) as c_int,
            )
        };

        // For some godforsaken reason Xlib alternates between
        // error values (0 = success) and booleans (1 = success), and they look exactly
        // the same in the signature (just `int`, since Xlib is written in C89)...
        if status == 0 {
            return Err(Error::RequestFailed);
        }
        Ok(())
    }

    fn delete_property(&self, name: xlib::Atom) -> Result<(), Error> {
        // SAFETY: valid display and window.
        let status = unsafe { xlib::XDeleteProperty(self.display, self.x11_surface.xid(), name) };
        if status == 0 {
            return Err(Error::RequestFailed);
        }
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.deinit();
    }
}

#[derive(Debug, Clone, Copy)]
struct GetPropertyOptions {
    offset: c_long,
    length: c_long,
    delete: bool,
}

impl Default for GetPropertyOptions {
    fn default() -> Self {
        GetPropertyOptions {
            offset: 0,
            length: c_long::MAX,
            delete: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Atoms {
    kde_blur: xlib::Atom,
    motif_wm_hints: xlib::Atom,
}

impl Atoms {
    fn init(display: &gdk4_x11::X11Display) -> Atoms {
        Atoms {
            kde_blur: gdk4_x11::x11_get_xatom_by_name_for_display(
                display,
                "_KDE_NET_WM_BLUR_BEHIND_REGION",
            ),
            motif_wm_hints: gdk4_x11::x11_get_xatom_by_name_for_display(
                display,
                "_MOTIF_WM_HINTS",
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum PropertyChangeMode {
    Replace = xlib::PropModeReplace,
    #[allow(dead_code)]
    Prepend = xlib::PropModePrepend,
    #[allow(dead_code)]
    Append = xlib::PropModeAppend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum PropertyFormat {
    #[allow(dead_code)]
    _8 = 8,
    #[allow(dead_code)]
    _16 = 16,
    _32 = 32,
}

impl PropertyFormat {
    /// `@sizeOf(elemType())`: Xlib's client-side element type for each
    /// format (`char`, `short`/`int`, `long`).
    fn elem_size(self) -> usize {
        match self {
            PropertyFormat::_8 => std::mem::size_of::<std::os::raw::c_char>(),
            PropertyFormat::_16 => std::mem::size_of::<c_int>(),
            PropertyFormat::_32 => std::mem::size_of::<c_long>(),
        }
    }
}

// See Xm/MwmUtil.h, packaged with the Motif Window Manager
//
// Zig uses packed structs for `flags` (bit 1: decorations) and
// `decorations` (bit 0: all); the port exposes accessors over the words.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MotifWMHints {
    flags: c_ulong,
    functions: c_ulong,
    decorations: c_ulong,
    input_mode: c_long,
    status: c_ulong,
}

impl MotifWMHints {
    const FLAGS_DECORATIONS: c_ulong = 1 << 1;
    const DECORATIONS_ALL: c_ulong = 1 << 0;

    fn set_flags_decorations(&mut self, v: bool) {
        if v {
            self.flags |= Self::FLAGS_DECORATIONS;
        } else {
            self.flags &= !Self::FLAGS_DECORATIONS;
        }
    }

    fn set_decorations_all(&mut self, v: bool) {
        if v {
            self.decorations |= Self::DECORATIONS_ALL;
        } else {
            self.decorations &= !Self::DECORATIONS_ALL;
        }
    }
}

// Keep the BlurRegion slice layout usable as format-32 property data.
const _: () = assert!(std::mem::size_of::<blur_region::Slice>() % std::mem::size_of::<c_long>() == 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motif_hints_bits() {
        let mut hints = MotifWMHints::default();
        hints.set_flags_decorations(true);
        hints.set_decorations_all(true);
        assert_eq!(hints.flags, 0b10);
        assert_eq!(hints.decorations, 0b1);
        hints.set_decorations_all(false);
        assert_eq!(hints.decorations, 0);
        assert_eq!(std::mem::size_of::<MotifWMHints>(), 5 * std::mem::size_of::<c_long>());
    }
}
