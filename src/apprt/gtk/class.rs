//! Port of `src/apprt/gtk/class.zig`.
//!
//! STATUS: ported
//!
//! This files contains all the GObject classes for the GTK apprt
//! along with helpers to work with them.
//!
//! # Conventions for GObject classes in the Rust port
//!
//! Zig defines every class by hand with zig-gobject (`defineClass`,
//! `defineProperty`, `defineSignal`, a `Private` struct and the [`Common`]
//! helpers below). The port builds each class on gtk-rs' `glib::subclass`
//! with the same shape, so that a Zig class reads one-to-one against its
//! Rust port:
//!
//! | Zig | Rust |
//! | --- | --- |
//! | `pub const Foo = extern struct { ... }` + `getGObjectType` | `glib::wrapper! { pub struct Foo(ObjectSubclass<imp::Foo>) @extends ...; }` |
//! | `defineClass(.{ .name = "GhosttyFoo" })` | `impl ObjectSubclass for imp::Foo { const NAME = "GhosttyFoo"; }` (same GType name: templates and CSS refer to it) |
//! | `Private` struct (`self.private()`) | the `imp::Foo` struct with `Cell`/`RefCell` fields (`self.imp()`, see [`Common::private`]) |
//! | `instanceInit` / `initTemplate` | `ObjectSubclass::instance_init` calling `obj.init_template()` |
//! | `Class.init` | `ObjectSubclass::class_init` |
//! | `setTemplateFromResource(gresource.blueprint(..))` | `#[derive(gtk4::CompositeTemplate)]` + `#[template(resource = "...")]` and [`ensure_resources`] before `klass.bind_template()`; each class checks its literal against [`blueprint`] in a test |
//! | `bindTemplateChildPrivate("name")` | `#[template_child] pub name: TemplateChild<T>` |
//! | `bindTemplateCallback("name", &func)` | [`bind_template_callback`] with a [`TemplateArgs`] closure (same argument order as the C callback) |
//! | `properties.foo` + `registerProperties` | `ObjectImpl::properties()` built from `glib::ParamSpec*::builder`, names in a `pub mod properties` of string constants |
//! | `privateShallowFieldAccessor`, `privateObjFieldAccessor`, `privateBoxedFieldAccessor`, `privateStringFieldAccessor` | [`accessor`] helpers on `Cell`/`RefCell` private fields, used from `property`/`set_property` |
//! | `typedAccessor(.{ .getter = f, .setter = g })` | `property`/`set_property` arms calling the Rust getter/setter |
//! | `notifyByPspec(properties.foo.impl.param_spec)` | `obj.notify(properties::FOO)` |
//! | `signals.foo` + `defineSignal` + `impl.emit` | `ObjectImpl::signals()` with `glib::subclass::Signal::builder`, `obj.emit_by_name::<()>(signals::FOO, &[..])` |
//! | `signals.foo.connect(obj, T, cb, ud, .{})` | a typed `connect_foo(&self, f)` method on the wrapper |
//! | pointer signal parameters (`*const Binding.Action`) | `G_TYPE_POINTER` parameters, see [`pointer_value`] / [`pointer_from_value`] |
//! | `defineEnum` | `#[derive(glib::Enum)] #[enum_type(name = "...")]` (nicks are the Zig tag names) |
//! | `defineBoxed` | `#[derive(glib::Boxed)] #[boxed_type(name = "...")]` |
//! | `dispose` + `disposeTemplate` | `ObjectImpl::dispose` calling `self.dispose_template()`; the parent class' dispose is chained by gtk-rs |
//! | `finalize` | `Drop` of the private struct (gtk-rs chains the parent `finalize`) |
//! | `defineVirtualMethod("name")` | a `#[repr(C)]` class struct with function pointer fields plus `unsafe impl IsSubclassable` (see `class/imgui_widget.rs`) |
//! | `ref`/`refSink`/`unref` | `clone()` / [`Common::ref_sink`] / `drop` |
//!
//! A complete, minimal example of a templated widget following these
//! conventions (the shape of `class/debug_warning.rs` plus one property,
//! one signal and one template callback):
//!
//! ```ignore
//! mod imp {
//!     #[derive(Default, gtk4::CompositeTemplate)]
//!     #[template(resource = "/com/mitchellh/ghostty/ui/1.0/example.ui")]
//!     pub struct Example {
//!         // Private data (Zig `Private`)
//!         pub flag: Cell<bool>,
//!         // Template bindings (Zig `bindTemplateChildPrivate("label")`)
//!         #[template_child]
//!         pub label: TemplateChild<gtk4::Label>,
//!     }
//!
//!     #[glib::object_subclass]
//!     impl ObjectSubclass for Example {
//!         const NAME: &'static str = "GhosttyExample";
//!         type Type = super::Example;
//!         type ParentType = libadwaita::Bin;
//!
//!         fn class_init(klass: &mut Self::Class) {
//!             class::ensure_resources();
//!             klass.bind_template();
//!             // Zig: class.bindTemplateCallback("clicked", &clicked)
//!             class::bind_template_callback(klass, "clicked", |args| {
//!                 // (button: *gtk.Button, self: *Self)
//!                 let this: super::Example = args.template();
//!                 this.emit_by_name::<()>(signals::ACTIVATED, &[]);
//!                 None
//!             });
//!         }
//!
//!         fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
//!             obj.init_template();
//!         }
//!     }
//!
//!     impl ObjectImpl for Example {
//!         fn properties() -> &'static [glib::ParamSpec] { /* ParamSpecBoolean "flag" */ }
//!         fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
//!             match pspec.name() {
//!                 properties::FLAG => accessor::get_shallow(&self.flag),
//!                 _ => unreachable!(),
//!             }
//!         }
//!         fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
//!             match pspec.name() {
//!                 properties::FLAG => accessor::set_shallow(&self.flag, value),
//!                 _ => unreachable!(),
//!             }
//!         }
//!         fn signals() -> &'static [Signal] { /* Signal::builder("activated") */ }
//!         fn dispose(&self) {
//!             self.dispose_template();
//!         }
//!     }
//!     impl WidgetImpl for Example {}
//!     impl BinImpl for Example {}
//! }
//!
//! glib::wrapper! {
//!     pub struct Example(ObjectSubclass<imp::Example>)
//!         @extends libadwaita::Bin, gtk4::Widget,
//!         @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
//! }
//! ```
//!
//! GTK may only be used from one thread: smoke tests instantiate widgets on
//! the shared GTK test thread (`ext::test_gtk::run`).
#![cfg_attr(not(feature = "gtk"), allow(unused))]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "class/application.rs"]
pub mod application;
#[path = "class/clipboard_confirmation_dialog.rs"]
pub mod clipboard_confirmation_dialog;
#[path = "class/close_confirmation_dialog.rs"]
pub mod close_confirmation_dialog;
#[path = "class/command_palette.rs"]
pub mod command_palette;
#[path = "class/config.rs"]
pub mod config;
#[path = "class/config_errors_dialog.rs"]
pub mod config_errors_dialog;
#[path = "class/debug_warning.rs"]
pub mod debug_warning;
#[path = "class/global_shortcuts.rs"]
pub mod global_shortcuts;
#[path = "class/imgui_widget.rs"]
pub mod imgui_widget;
#[path = "class/inspector_widget.rs"]
pub mod inspector_widget;
#[path = "class/inspector_window.rs"]
pub mod inspector_window;
#[path = "class/key_state_overlay.rs"]
pub mod key_state_overlay;
#[path = "class/overrides.rs"]
pub mod overrides;
#[path = "class/render_surface.rs"]
pub mod render_surface;
#[path = "class/resize_overlay.rs"]
pub mod resize_overlay;
#[path = "class/search_overlay.rs"]
pub mod search_overlay;
#[path = "class/split_tree.rs"]
pub mod split_tree;
#[path = "class/surface.rs"]
pub mod surface;
#[path = "class/surface_child_exited.rs"]
pub mod surface_child_exited;
#[path = "class/surface_scrolled_window.rs"]
pub mod surface_scrolled_window;
#[path = "class/tab.rs"]
pub mod tab;
#[path = "class/title_dialog.rs"]
pub mod title_dialog;
#[path = "class/window.rs"]
pub mod window;
// @generated-mods-end

#[cfg(feature = "gtk")]
pub use self::gtk_impl::*;

#[cfg(feature = "gtk")]
mod gtk_impl {
    use std::cell::{Cell, RefCell};

    use glib::prelude::*;
    use glib::translate::*;
    use gtk4::subclass::prelude::*;

    pub use crate::apprt::gtk::build::gresource::{blueprint, Blueprint};

    pub use super::application::Application;
    pub use super::config::Config;
    pub use super::render_surface::RenderSurface;
    pub use super::surface::Surface;
    pub use super::window::Window;

    /// Common methods for all GObject classes we create.
    ///
    /// Zig's `Common(Self, Private)` returns a namespace of helpers that
    /// each class re-exports. In Rust they are a blanket extension trait
    /// over every GObject wrapper type.
    pub trait Common: ObjectType {
        /// Upcast our type to a parent type or interface. This will fail at
        /// compile time if the cast isn't 100% safe. For unsafe casts,
        /// use `downcast` instead. We don't have a helper for that
        /// because its uncommon and unsafe behavior should be noisier.
        fn as_<T: ObjectType>(&self) -> &T
        where
            Self: IsA<T>,
        {
            self.upcast_ref::<T>()
        }

        /// Increase the reference count of the object.
        fn ref_(&self) -> Self {
            self.clone()
        }

        /// If the reference count is 1 and the object is floating, clear the
        /// floating attribute. Otherwise, increase the reference count by 1.
        ///
        /// (gtk-rs never hands out floating references, so in practice this
        /// takes a new reference.)
        fn ref_sink(&self) -> Self {
            // SAFETY: `self` is a valid object; `g_object_ref_sink` returns
            // the same object with a reference we own (wrappers never hold
            // a floating reference).
            unsafe {
                let ptr = glib::gobject_ffi::g_object_ref_sink(
                    self.as_ptr() as *mut glib::gobject_ffi::GObject,
                );
                let obj: glib::Object = from_glib_full(ptr);
                obj.unsafe_cast()
            }
        }

        /// Decrease the reference count of the object.
        fn unref(self) {
            drop(self);
        }

        /// Access the private data of the object. This should be forwarded
        /// via a non-pub const usually. (gtk-rs: `imp()`.)
        fn private(&self) -> &<Self as glib::object::ObjectSubclassIs>::Subclass
        where
            Self: ObjectSubclassIsExt,
        {
            self.imp()
        }

        /// Get the class for the object.
        ///
        /// Every GObject instance starts with a `GTypeInstance` whose first
        /// field points to its class structure, which is what gtk-rs'
        /// `ObjectExt::class` reads.
        fn get_class(&self) -> &glib::Class<Self>
        where
            Self: glib::object::IsClass,
        {
            ObjectExt::class(self)
        }
    }

    impl<T: ObjectType> Common for T {}

    /// Helpers that implement properties reading and writing private
    /// fields (Zig: `privateShallowFieldAccessor`,
    /// `privateObjFieldAccessor`, `privateBoxedFieldAccessor` and
    /// `privateStringFieldAccessor`).
    ///
    /// Reference counting and freeing of the previous value is done by the
    /// Rust types themselves (`Option<Object>` unrefs on replace, boxed
    /// and string values are owned copies), so all non-shallow accessors
    /// share one implementation.
    pub mod accessor {
        use super::*;

        /// A property that reads and writes a private field with only
        /// shallow copies. This is good for primitives such as bools,
        /// numbers, etc.
        pub fn get_shallow<T: ToValue + Copy>(field: &Cell<T>) -> glib::Value {
            field.get().to_value()
        }

        /// See [`get_shallow`].
        pub fn set_shallow<T>(field: &Cell<T>, value: &glib::Value)
        where
            T: for<'a> glib::value::FromValue<'a> + Copy,
        {
            field.set(value.get::<T>().expect("property value type mismatch"));
        }

        /// A property that reads and writes a private object (reference
        /// counted), boxed or string field. Reading the property copies or
        /// refs the value; writing it releases the previous value.
        pub fn get<T: ToValue>(field: &RefCell<T>) -> glib::Value {
            field.borrow().to_value()
        }

        /// See [`get`].
        pub fn set<T>(field: &RefCell<T>, value: &glib::Value)
        where
            T: for<'a> glib::value::FromValue<'a>,
        {
            let v = value.get::<T>().expect("property value type mismatch");
            // Drop the old value outside of the borrow.
            let _old = field.replace(v);
        }
    }

    /// Registers the GResource bundle (UI templates, CSS, icons) before a
    /// class loads its template from it. In Zig the bundle is linked into
    /// the binary and registered by a constructor before any class exists.
    pub fn ensure_resources() {
        ensure_resources_impl();
    }

    /// `i18n._(msgid)` as a `&str` for the gtk-rs setters (translations are
    /// UTF-8; an invalid one falls back to the untranslated message).
    pub fn i18n_(msgid: &'static std::ffi::CStr) -> &'static str {
        let translated = crate::os::i18n::gettext(msgid);
        translated
            .to_str()
            .unwrap_or_else(|_| msgid.to_str().unwrap_or_default())
    }

    fn ensure_resources_impl() {
        #[cfg(ghostty_gresource)]
        crate::apprt::gtk::build::gresource::register();
    }

    /// The arguments a template callback is invoked with.
    ///
    /// The values are in the order of the C callback that Zig binds with
    /// `bindTemplateCallback`: for signal handlers `[emitter, signal
    /// arguments..., self]`, and for `<closure>` functions `[self,
    /// arguments..., self]`. [`TemplateArgs::get`]`(i)` returns the `i`th
    /// parameter of the Zig callback and [`TemplateArgs::template`] the
    /// trailing user-data `self`.
    pub struct TemplateArgs<'a> {
        values: &'a [glib::Value],
    }

    impl<'a> TemplateArgs<'a> {
        pub fn new(values: &'a [glib::Value]) -> Self {
            TemplateArgs { values }
        }

        /// The number of values, including the trailing template object.
        pub fn len(&self) -> usize {
            self.values.len()
        }

        pub fn is_empty(&self) -> bool {
            self.values.is_empty()
        }

        /// The raw `i`th value.
        pub fn value(&self, i: usize) -> &'a glib::Value {
            &self.values[i]
        }

        /// The `i`th value converted to `T`. Panics on a type mismatch, like
        /// a C callback with the wrong signature would misbehave.
        pub fn get<T>(&self, i: usize) -> T
        where
            T: for<'b> glib::value::FromValue<'b>,
        {
            let v = self.values.get(i).unwrap_or_else(|| {
                panic!("template callback argument {i} missing (have {})", self.values.len())
            });
            v.get::<T>()
                .unwrap_or_else(|e| panic!("template callback argument {i}: {e:?}"))
        }

        /// The template object (the callback's `self` user data).
        pub fn template<T: IsA<glib::Object>>(&self) -> T {
            let obj: glib::Object = self.get(self.values.len() - 1);
            obj.downcast::<T>()
                .unwrap_or_else(|o| panic!("template object has type {}", o.type_()))
        }
    }

    /// Bind a function to a template callback symbol (Zig:
    /// `Class.bindTemplateCallback`). The function returns the C return
    /// value as a `glib::Value` (`None` for `void`). Booleans are returned
    /// as `bool` values (Zig returns `c_int` for the same ABI reason).
    pub fn bind_template_callback<C, F>(klass: &mut C, name: &str, func: F)
    where
        C: WidgetClassExt,
        F: Fn(&TemplateArgs<'_>) -> Option<glib::Value> + 'static,
    {
        klass
            .rust_template_scope()
            .add_callback(name, move |values| func(&TemplateArgs::new(values)));
    }

    /// Wraps a reference as a `G_TYPE_POINTER` value, for signals whose Zig
    /// parameter is a plain pointer (e.g. `*const input.Binding.Action`).
    /// The pointer is only valid for the duration of the emission.
    pub fn pointer_value<T>(v: &T) -> glib::Value {
        (v as *const T as glib::Pointer).to_value()
    }

    /// The reverse of [`pointer_value`].
    ///
    /// # Safety
    /// `value` must hold a pointer created by [`pointer_value`] from a `T`
    /// that is still alive (i.e. this is called during the emission).
    pub unsafe fn pointer_from_value<'a, T>(value: &glib::Value) -> &'a T {
        let ptr = value.get::<glib::Pointer>().expect("pointer value");
        &*(ptr as *const T)
    }

    /// GObject wrappers for apprt structs that Zig makes GObject types via
    /// `getGObjectType` in `apprt/structs.zig` and `apprt/surface.zig`.
    pub mod boxed {
        /// `apprt.ClipboardRequest` as the boxed type
        /// `GhosttyClipboardRequest`.
        #[derive(Debug, Clone, PartialEq, Eq, glib::Boxed)]
        #[boxed_type(name = "GhosttyClipboardRequest", nullable)]
        pub struct ClipboardRequest(pub crate::apprt::structs::ClipboardRequest);

        /// `apprt.surface.Message.ChildExited` as the boxed type
        /// `GhosttyApprtChildExited`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, glib::Boxed)]
        #[boxed_type(name = "GhosttyApprtChildExited", nullable)]
        pub struct ChildExited(pub crate::apprt::surface::ChildExited);
    }
}

#[cfg(all(test, feature = "gtk"))]
mod tests {
    use super::*;
    use glib::prelude::*;

    #[test]
    fn common_helpers() {
        let obj = glib::Object::new::<glib::Object>();
        let r = obj.ref_();
        assert_eq!(obj.ref_count(), 2);
        let s = obj.ref_sink();
        assert_eq!(obj.ref_count(), 3);
        s.unref();
        r.unref();
        assert_eq!(obj.ref_count(), 1);
        let _: &glib::Object = obj.as_();
        assert_eq!(obj.get_class().type_(), glib::Object::static_type());
    }

    #[test]
    fn pointer_values() {
        let x = 42u32;
        let v = pointer_value(&x);
        assert_eq!(v.type_(), glib::Type::POINTER);
        assert_eq!(*unsafe { pointer_from_value::<u32>(&v) }, 42);
    }

    #[test]
    fn template_args() {
        let values = [1i32.to_value(), "x".to_value(), glib::Object::new::<glib::Object>().to_value()];
        let args = TemplateArgs::new(&values);
        assert_eq!(args.len(), 3);
        assert_eq!(args.get::<i32>(0), 1);
        assert_eq!(args.get::<String>(1), "x");
        let _: glib::Object = args.template();
    }

    #[test]
    fn accessors() {
        use std::cell::{Cell, RefCell};
        let c = Cell::new(false);
        accessor::set_shallow(&c, &true.to_value());
        assert!(accessor::get_shallow(&c).get::<bool>().unwrap());
        let s: RefCell<Option<String>> = RefCell::new(None);
        accessor::set(&s, &Some("hi").to_value());
        assert_eq!(accessor::get(&s).get::<Option<String>>().unwrap().as_deref(), Some("hi"));
    }
}
