//! Port of `src/apprt/gtk/class/imgui_widget.zig`.
//!
//! STATUS: ported
//!
//! Only built with the `inspector` Cargo feature (Dear ImGui is only
//! compiled for the inspector).
//!
//! The `setup`/`render` virtual methods (Zig: `C.defineVirtualMethod`) are
//! function pointers in [`ImguiWidgetClass`]; subclasses override them by
//! implementing [`ImguiWidgetImpl`], whose `IsSubclassable` installs
//! trampolines in their class struct — the gtk-rs equivalent of Zig's
//! `virtual_methods.setup.implement(class, f)`.
#![cfg(all(feature = "gtk", feature = "inspector"))]

use std::cell::{Cell, RefCell};
use std::mem::MaybeUninit;
use std::time::{Duration, Instant};

use glib::prelude::*;
use glib::subclass::prelude::*;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, Blueprint};
use crate::apprt::gtk::key;
use crate::input::key::Action as InputAction;
use crate::pkg::dcimgui as cimgui;
use crate::pkg::dcimgui::c;

const LOG: &str = "gtk_ghostty_imgui_widget";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "imgui-widget" };

/// The class structure (Zig: `ImguiWidget.Class`).
#[repr(C)]
pub struct ImguiWidgetClass {
    pub parent_class: libadwaita::ffi::AdwBinClass,

    /// Function pointers for virtual methods.
    pub setup: Option<fn(&ImguiWidget)>,
    pub render: Option<fn(&ImguiWidget)>,
}

unsafe impl ClassStruct for ImguiWidgetClass {
    type Type = imp::ImguiWidget;
}

mod imp {
    use super::*;

    #[derive(gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/imgui-widget.ui")]
    pub struct ImguiWidget {
        /// GL area where we display the Dear ImGui application.
        #[template_child]
        pub gl_area: TemplateChild<gtk4::GLArea>,

        /// GTK input method context
        #[template_child]
        pub im_context: TemplateChild<gtk4::IMMulticontext>,

        /// Dear ImGui context. We create a context per widget so that we can
        /// have multiple active imgui views in the same application.
        pub ig_context: Cell<*mut c::ImGuiContext>,

        /// Our previous instant used to calculate delta time for animations.
        pub instant: Cell<Option<Instant>>,

        /// Tick callback ID for timed updates.
        pub tick_callback_id: RefCell<Option<gtk4::TickCallbackId>>,

        /// Last render time for throttling to 30 FPS.
        pub last_render_time: Cell<Option<Instant>>,
    }

    impl Default for ImguiWidget {
        fn default() -> Self {
            ImguiWidget {
                gl_area: TemplateChild::default(),
                im_context: TemplateChild::default(),
                ig_context: Cell::new(std::ptr::null_mut()),
                instant: Cell::new(None),
                tick_callback_id: RefCell::new(None),
                last_render_time: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImguiWidget {
        const NAME: &'static str = "GhosttyImguiWidget";
        type Type = super::ImguiWidget;
        type ParentType = libadwaita::Bin;
        type Class = super::ImguiWidgetClass;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Initialize our virtual methods with default functions.
            klass.setup = Some(super::default_setup);
            klass.render = Some(super::default_render);

            type W = super::ImguiWidget;

            // Template Callbacks
            class::bind_template_callback(klass, "realize", |args| {
                args.template::<W>().gl_area_realize();
                None
            });
            class::bind_template_callback(klass, "unrealize", |args| {
                args.template::<W>().gl_area_unrealize();
                None
            });
            class::bind_template_callback(klass, "resize", |args| {
                // (area, width: c_int, height: c_int, self)
                let area: gtk4::GLArea = args.get(0);
                args.template::<W>().gl_area_resize(&area, args.get(1), args.get(2));
                None
            });
            class::bind_template_callback(klass, "render", |args| {
                Some(args.template::<W>().gl_area_render().to_value())
            });
            class::bind_template_callback(klass, "focus_enter", |args| {
                args.template::<W>().ec_focus(true);
                None
            });
            class::bind_template_callback(klass, "focus_leave", |args| {
                args.template::<W>().ec_focus(false);
                None
            });
            class::bind_template_callback(klass, "key_pressed", |args| {
                // (ec_key, keyval, keycode, gtk_mods, self) -> c_int
                let ec: gtk4::EventControllerKey = args.get(0);
                let r = args.template::<W>().key_event(
                    InputAction::Press,
                    &ec,
                    args.get(1),
                    args.get(2),
                    args.get(3),
                );
                Some(r.to_value())
            });
            class::bind_template_callback(klass, "key_released", |args| {
                let ec: gtk4::EventControllerKey = args.get(0);
                let _ = args.template::<W>().key_event(
                    InputAction::Release,
                    &ec,
                    args.get(1),
                    args.get(2),
                    args.get(3),
                );
                None
            });
            class::bind_template_callback(klass, "mouse_pressed", |args| {
                let gesture: gtk4::GestureClick = args.get(0);
                args.template::<W>().ec_mouse_button(&gesture, true);
                None
            });
            class::bind_template_callback(klass, "mouse_released", |args| {
                let gesture: gtk4::GestureClick = args.get(0);
                args.template::<W>().ec_mouse_button(&gesture, false);
                None
            });
            class::bind_template_callback(klass, "mouse_motion", |args| {
                args.template::<W>().ec_mouse_motion(args.get(1), args.get(2));
                None
            });
            class::bind_template_callback(klass, "scroll", |args| {
                Some(args.template::<W>().ec_mouse_scroll(args.get(1), args.get(2)).to_value())
            });
            class::bind_template_callback(klass, "im_commit", |args| {
                // (im, bytes: [*:0]u8, self)
                let bytes: String = args.get(1);
                args.template::<W>().im_commit(&bytes);
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ImguiWidget {
        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for ImguiWidget {}
    impl BinImpl for ImguiWidget {}
}

glib::wrapper! {
    /// A widget for embedding a Dear ImGui application.
    ///
    /// It'd be a lot cleaner to use inheritance here but zig-gobject doesn't
    /// currently have a way to define virtual methods, so we have to use
    /// composition and signals instead.
    pub struct ImguiWidget(ObjectSubclass<imp::ImguiWidget>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

/// The virtual methods of [`ImguiWidget`] for subclasses
/// (Zig: `ImguiWidget.virtual_methods`).
pub trait ImguiWidgetImpl: BinImpl
where
    <Self as ObjectSubclass>::Type: IsA<ImguiWidget>,
{
    /// This virtual method will be called to allow the Dear ImGui
    /// application to do one-time setup of the context. The correct context
    /// will be current when the virtual method is called.
    fn setup(&self) {
        default_setup(self.obj().upcast_ref());
    }

    /// This virtual method will be called at each frame to allow the Dear
    /// ImGui application to draw the application. The correct context will
    /// be current when the virtual method is called.
    fn render(&self) {
        default_render(self.obj().upcast_ref());
    }
}

unsafe impl<T: ImguiWidgetImpl> IsSubclassable<T> for ImguiWidget
where
    <T as ObjectSubclass>::Type: IsA<ImguiWidget>,
{
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.setup = Some(setup_trampoline::<T>);
        klass.render = Some(render_trampoline::<T>);
    }
}

fn setup_trampoline<T: ImguiWidgetImpl>(obj: &ImguiWidget)
where
    <T as ObjectSubclass>::Type: IsA<ImguiWidget>,
{
    let this = obj.dynamic_cast_ref::<T::Type>().unwrap();
    T::from_obj(this).setup();
}

fn render_trampoline<T: ImguiWidgetImpl>(obj: &ImguiWidget)
where
    <T as ObjectSubclass>::Type: IsA<ImguiWidget>,
{
    let this = obj.dynamic_cast_ref::<T::Type>().unwrap();
    T::from_obj(this).render();
}

/// `error{ContextNotInitialized}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextNotInitialized;

/// Translate a GTK mouse button to a Dear ImGui mouse button.
fn translate_mouse_button(button: u32) -> Option<i32> {
    match button {
        1 => Some(c::ImGuiMouseButton_Left as i32),
        2 => Some(c::ImGuiMouseButton_Middle as i32),
        3 => Some(c::ImGuiMouseButton_Right as i32),
        _ => None,
    }
}

/// Default setup function. Does nothing but log a warning.
fn default_setup(_: &ImguiWidget) {
    log::warn!(target: LOG, "default Dear ImGui setup called, this is a bug.");
}

/// Default render function. Does nothing but log a warning.
fn default_render(_: &ImguiWidget) {
    log::warn!(target: LOG, "default Dear ImGui render called, this is a bug.");
}

impl ImguiWidget {
    //---------------------------------------------------------------
    // Public methods

    /// This should be called anytime the underlying data for the UI changes
    /// so that the UI can be refreshed.
    pub fn queue_render(&self) {
        self.imp().gl_area.queue_render();
    }

    //---------------------------------------------------------------
    // Public wrappers for virtual methods

    /// This virtual method will be called to allow the Dear ImGui application
    /// to do one-time setup of the context. The correct context will be current
    /// when the virtual method is called.
    pub fn setup(&self) {
        let class: &ImguiWidgetClass = self.class().as_ref();
        if let Some(f) = class.setup {
            f(self);
        }
    }

    /// This virtual method will be called at each frame to allow the Dear ImGui
    /// application to draw the application. The correct context will be current
    /// when the virtual method is called.
    pub fn render(&self) {
        let class: &ImguiWidgetClass = self.class().as_ref();
        if let Some(f) = class.render {
            f(self);
        }
    }

    //---------------------------------------------------------------
    // Private Methods

    /// Set our imgui context to be current, or return an error. This must be
    /// called before any Dear ImGui API calls so that they're made against
    /// the proper context.
    fn set_current_context(&self) -> Result<(), ContextNotInitialized> {
        let ig_context = self.imp().ig_context.get();
        if ig_context.is_null() {
            log::warn!(target: LOG, "Dear ImGui context not initialized");
            return Err(ContextNotInitialized);
        }
        // SAFETY: a live context created by `ImGui_CreateContext`.
        unsafe { c::ImGui_SetCurrentContext(ig_context) };
        Ok(())
    }

    /// The IO of the current context.
    fn io(&self) -> &mut c::ImGuiIO {
        // SAFETY: only called after `set_current_context` succeeded.
        unsafe { &mut *c::ImGui_GetIO() }
    }

    /// Initialize the frame. Expects that the context is already current.
    fn new_frame(&self) {
        let priv_ = self.imp();
        let io = self.io();

        let now = Instant::now();

        // Determine our delta time
        io.DeltaTime = match priv_.instant.get() {
            Some(prev) => {
                let since_s = now.duration_since(prev).as_secs_f64() as f32;
                since_s.max(0.00001)
            }
            None => 1.0 / 60.0,
        };

        priv_.instant.set(Some(now));
    }

    /// Handle key press/release events.
    fn key_event(
        &self,
        action: InputAction,
        ec_key: &gtk4::EventControllerKey,
        keyval: u32,
        _keycode: u32,
        gtk_mods: gdk::ModifierType,
    ) -> bool {
        self.queue_render();

        if self.set_current_context().is_err() {
            return false;
        }

        let io = self.io() as *mut c::ImGuiIO;

        let mods = key::translate_mods(gtk_mods);
        // SAFETY: `io` is the current context's IO.
        unsafe {
            c::ImGuiIO_AddKeyEvent(io, c::ImGuiKey_LeftShift as c::ImGuiKey, mods.shift);
            c::ImGuiIO_AddKeyEvent(io, c::ImGuiKey_LeftCtrl as c::ImGuiKey, mods.ctrl);
            c::ImGuiIO_AddKeyEvent(io, c::ImGuiKey_LeftAlt as c::ImGuiKey, mods.alt);
            c::ImGuiIO_AddKeyEvent(io, c::ImGuiKey_LeftSuper as c::ImGuiKey, mods.super_);
        }

        // If our keyval has a key, then we send that key event
        if let Some(inputkey) = key::key_from_keyval(keyval) {
            if let Some(imgui_key) = inputkey.imgui_key() {
                // SAFETY: as above.
                unsafe { c::ImGuiIO_AddKeyEvent(io, imgui_key, action == InputAction::Press) };
            }
        }

        // Try to process the event as text
        if let Some(event) = ec_key.current_event() {
            let _ = self.imp().im_context.filter_keypress(&event);
        }

        true
    }

    /// Get the scale factor that the display is operating at.
    fn get_scale_factor(&self) -> f64 {
        self.imp().gl_area.scale_factor() as f64
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn gl_area_realize(&self) {
        let priv_ = self.imp();
        debug_assert!(priv_.ig_context.get().is_null());

        priv_.gl_area.make_current();
        if let Some(err) = priv_.gl_area.error() {
            log::warn!(target: LOG, "GLArea for Dear ImGui widget failed to realize: {}", err.message());
            return;
        }

        // SAFETY: plain Dear ImGui calls on the GTK thread.
        let ctx = unsafe { c::ImGui_CreateContext(std::ptr::null_mut()) };
        if ctx.is_null() {
            log::warn!(target: LOG, "unable to initialize Dear ImGui context");
            return;
        }
        priv_.ig_context.set(ctx);
        if self.set_current_context().is_err() {
            return;
        }

        // Setup some basic config
        let io = self.io();
        io.BackendPlatformName = c"ghostty_gtk".as_ptr();

        // Realize means that our OpenGL context is ready, so we can now
        // initialize the ImgUI OpenGL backend for our context.
        // GTK may use GLES for its shared context. ImGui's default shader
        // version is for desktop GL, so select GLSL ES explicitly in that case.
        let context = priv_.gl_area.context().unwrap();
        let glsl_version: *const std::ffi::c_char =
            if context.uses_es() { c"#version 300 es".as_ptr() } else { std::ptr::null() };
        // SAFETY: the GL context is current.
        if !unsafe { cimgui::ImGui_ImplOpenGL3_Init(glsl_version) } {
            log::warn!(target: LOG, "unable to initialize Dear ImGui OpenGL backend");
            // SAFETY: the context we just created.
            unsafe { c::ImGui_DestroyContext(priv_.ig_context.get()) };
            priv_.ig_context.set(std::ptr::null_mut());
            return;
        }

        // Call the virtual method to setup the UI.
        self.setup();

        // Add a tick callback to drive timed updates via the frame clock.
        let id = self.add_tick_callback(|widget, _| tick_callback(widget));
        priv_.tick_callback_id.replace(Some(id));
    }

    /// Handle a request to unrealize the GLArea
    fn gl_area_unrealize(&self) {
        let priv_ = self.imp();
        debug_assert!(!priv_.ig_context.get().is_null());

        // Remove the tick callback if it was registered.
        if let Some(id) = priv_.tick_callback_id.take() {
            id.remove();
        }

        // Unrealize is not guaranteed to be called with a current GL context,
        // so we make it current for ImGui cleanup.
        priv_.gl_area.make_current();
        if let Some(err) = priv_.gl_area.error() {
            log::warn!(target: LOG, "GLArea for Dear ImGui widget failed to realize: {}", err.message());
            return;
        }

        if self.set_current_context().is_err() {
            return;
        }
        // SAFETY: the current context with an initialized backend.
        unsafe {
            cimgui::ImGui_ImplOpenGL3_ShutdownWithLoaderCleanup();
            c::ImGui_DestroyContext(priv_.ig_context.get());
        }
        priv_.ig_context.set(std::ptr::null_mut());
    }

    /// Handle a request to resize the GLArea
    fn gl_area_resize(&self, area: &gtk4::GLArea, width: i32, height: i32) {
        if self.set_current_context().is_err() {
            return;
        }
        let io = self.io();
        let scale_factor = area.scale_factor();

        // Our display size is always unscaled. We'll do the scaling in the
        // style instead. This creates crisper looking fonts.
        io.DisplaySize = c::ImVec2 { x: width as f32, y: height as f32 };
        io.DisplayFramebufferScale = c::ImVec2 { x: 1.0, y: 1.0 };

        // Setup a new style and scale it appropriately. We must use the
        // ImGuiStyle constructor to get proper default values (e.g.,
        // CurveTessellationTol) rather than zero-initialized values.
        // SAFETY: the constructor initializes the whole struct.
        unsafe {
            let mut style = MaybeUninit::<c::ImGuiStyle>::uninit();
            cimgui::ext::ImGuiStyle_ImGuiStyle(style.as_mut_ptr());
            let mut style = style.assume_init();
            c::ImGuiStyle_ScaleAllSizes(&mut style, scale_factor as f32);
            let active_style = c::ImGui_GetStyle();
            *active_style = style;
        }
    }

    /// Handle a request to render the contents of our GLArea
    fn gl_area_render(&self) -> bool {
        if self.set_current_context().is_err() {
            return false;
        }

        // Update last render time for tick callback throttling.
        self.imp().last_render_time.set(Some(Instant::now()));

        // Setup our frame. We render twice because some ImGui behaviors
        // take multiple renders to process. I don't know how to make this
        // more efficient.
        for _ in 0..2 {
            // SAFETY: the current context with an initialized backend.
            unsafe { cimgui::ImGui_ImplOpenGL3_NewFrame() };
            self.new_frame();
            // SAFETY: as above.
            unsafe { c::ImGui_NewFrame() };

            // Call the virtual method to draw the UI.
            self.render();

            // Render
            // SAFETY: as above.
            unsafe { c::ImGui_Render() };
        }

        // SAFETY: as above.
        unsafe { cimgui::ImGui_ImplOpenGL3_RenderDrawData(c::ImGui_GetDrawData()) };

        true
    }

    /// `ecFocusEnter` / `ecFocusLeave`
    fn ec_focus(&self, focused: bool) {
        if self.set_current_context().is_err() {
            return;
        }
        // SAFETY: the current context's IO.
        unsafe { c::ImGuiIO_AddFocusEvent(self.io(), focused) };
        self.queue_render();
    }

    /// `ecMousePressed` / `ecMouseReleased`
    fn ec_mouse_button(&self, gesture: &gtk4::GestureClick, pressed: bool) {
        self.queue_render();
        if self.set_current_context().is_err() {
            return;
        }
        let gdk_button = gesture.current_button();
        if let Some(button) = translate_mouse_button(gdk_button) {
            // SAFETY: the current context's IO.
            unsafe { c::ImGuiIO_AddMouseButtonEvent(self.io(), button, pressed) };
        }
    }

    fn ec_mouse_motion(&self, x: f64, y: f64) {
        self.queue_render();
        if self.set_current_context().is_err() {
            return;
        }
        let scale_factor = self.get_scale_factor();
        // SAFETY: the current context's IO.
        unsafe {
            c::ImGuiIO_AddMousePosEvent(
                self.io(),
                (x * scale_factor) as f32,
                (y * scale_factor) as f32,
            )
        };
    }

    fn ec_mouse_scroll(&self, x: f64, y: f64) -> bool {
        self.queue_render();
        if self.set_current_context().is_err() {
            return false;
        }
        // SAFETY: the current context's IO.
        unsafe { c::ImGuiIO_AddMouseWheelEvent(self.io(), x as f32, -y as f32) };
        true
    }

    fn im_commit(&self, bytes: &str) {
        self.queue_render();
        if self.set_current_context().is_err() {
            return;
        }
        let Ok(bytes) = std::ffi::CString::new(bytes) else { return };
        // SAFETY: the current context's IO and a NUL-terminated string.
        unsafe { c::ImGuiIO_AddInputCharactersUTF8(self.io(), bytes.as_ptr()) };
    }
}

/// Tick callback for timed updates. This drives periodic redraws.
/// Redraws are limited to 30 FPS max since our imgui widgets don't
/// usually need higher frame rates than that.
fn tick_callback(widget: &ImguiWidget) -> glib::ControlFlow {
    let priv_ = widget.imp();

    // Throttle to 30 FPS (~33ms between frames)
    let frame_time = Duration::from_nanos(1_000_000_000 / 30);
    let should_render = match priv_.last_render_time.get() {
        Some(last) => last.elapsed() >= frame_time,
        None => true,
    };

    if should_render {
        widget.queue_render();
    }

    glib::ControlFlow::Continue // Continue the tick callback
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.0/imgui-widget.ui");
    }

    #[test]
    fn mouse_buttons() {
        assert_eq!(translate_mouse_button(1), Some(c::ImGuiMouseButton_Left as i32));
        assert_eq!(translate_mouse_button(3), Some(c::ImGuiMouseButton_Right as i32));
        assert_eq!(translate_mouse_button(9), None);
    }

    #[test]
    fn instantiate_without_context() {
        test_gtk::run(|| {
            let w: ImguiWidget = glib::Object::new();
            // Default virtual methods only log.
            w.setup();
            w.render();
            // Without a context nothing is forwarded to Dear ImGui.
            assert!(!w.ec_mouse_scroll(0.0, 1.0));
            assert!(!w.gl_area_render());
        });
    }
}
