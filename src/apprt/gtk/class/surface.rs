//! Port of `src/apprt/gtk/class/surface.zig`.
//!
//! STATUS: ported
//!
//! Rust notes:
//! * The core surface is reached through a raw pointer like Zig
//!   (`core_surface: ?*CoreSurface`); it is created in `init_surface` and
//!   freed in finalize (the `Drop` of the private struct).
//! * The apprt surface handed to the core lives in the private struct
//!   (`rt_surface`), which has a stable address for the object's lifetime.
//! * Enum properties use GObject enum mirrors registered under the Zig
//!   type names ([`MouseShape`] = `GhosttyMouseShape`, [`ClipboardType`] =
//!   `GhosttyApprtClipboard`); the mouse shape nicks are the CSS cursor names.
//! * Clipboard reads keep the surface alive by moving a strong reference
//!   into the async callbacks (Zig: `self.ref()` in the userdata).
//! * The Kitty clipboard read operation state is a heap struct moved
//!   between the async callbacks (Zig: `KittyReadOp`).
#![cfg(feature = "gtk")]

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::{CStr, CString};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use glib::prelude::*;
use glib::subclass::Signal;
use glib::translate::IntoGlib;
use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt;
use crate::apprt::action::{self as rt_action, CommandFinished, Inspector as InspectorAction, KeySequence, KeyTable};
use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::clipboard_confirmation_dialog::{self, ClipboardConfirmationDialog, ClipboardPart};
#[cfg(feature = "inspector")]
use crate::apprt::gtk::class::inspector_window::InspectorWindow;
use crate::apprt::gtk::class::key_state_overlay::KeyStateOverlay;
use crate::apprt::gtk::class::overrides::Overrides;
use crate::apprt::gtk::class::render_surface::RenderSurface;
use crate::apprt::gtk::class::resize_overlay::{self, ResizeOverlay};
use crate::apprt::gtk::class::search_overlay::{self, SearchOverlay};
use crate::apprt::gtk::class::split_tree::SplitTree;
use crate::apprt::gtk::class::surface_child_exited::SurfaceChildExited;
use crate::apprt::gtk::class::surface_scrolled_window::SurfaceScrolledWindow;
use crate::apprt::gtk::class::title_dialog::{self, TitleDialog};
use crate::apprt::gtk::class::window::Window;
use crate::apprt::gtk::class::{self, accessor, boxed, Blueprint, Config};
use crate::apprt::gtk::desktop_notifications::DesktopNotifications;
use crate::apprt::gtk::ext;
use crate::apprt::gtk::ext::actions::Action as GtkAction;
use crate::apprt::gtk::ext::StringList;
use crate::apprt::gtk::gsettings;
use crate::apprt::gtk::gtk_version;
use crate::apprt::gtk::key as gtk_key;
use crate::apprt::gtk::scale as scale_util;
use crate::apprt::structs::{
    Clipboard, ClipboardContent, ClipboardReadResult, ClipboardRequest, ContentScale, CursorPos,
    SurfaceSize,
};
use crate::apprt::surface::{ChildExited, NewSurfaceContext};
use crate::build_config;
use crate::command::EnvMap;
use crate::config::config::{
    BackgroundBlur, Command, DragHandle, NotifyOnCommandFinish, ResizeOverlay as ResizeOverlayMode,
    ResizeOverlayPosition, ShellIntegration, WorkingDirectory,
};
use crate::config::Config as CoreConfig;
use crate::datastruct::split_tree as dst;
use crate::font;
use crate::font::face::DesiredSize;
use crate::global;
use crate::input;
use crate::input::binding::{Action as BindingAction, NavigateSearch};
use crate::input::{KeyEvent, MouseButton, MouseButtonState};
use crate::os as internal_os;
use crate::renderer;
use crate::surface::{self as core_surface_mod, CompleteClipboard, InputEffect, Surface as CoreSurface};
use crate::terminal;
use crate::terminal::kitty::clipboard as kitty_clipboard;
use crate::terminal::mouse::Shape as TerminalMouseShape;
use crate::terminal::osc::{ProgressReport, ProgressReportState};
use crate::terminal::page_list::Scrollbar;

const LOG: &str = "gtk_ghostty_surface";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "surface" };

/// Property names (Zig: `properties`).
pub mod properties {
    /// This property is set to true when the bell is ringing. Note that
    /// this property will only emit a changed signal when there is a
    /// full state change. If a bell is ringing and another bell event
    /// comes through, the change notification will NOT be emitted.
    ///
    /// If you need to know every scenario the bell is triggered,
    /// listen to the `bell` signal instead.
    pub const BELL_RINGING: &str = "bell-ringing";
    pub const CONFIG: &str = "config";
    pub const CHILD_EXITED: &str = "child-exited";
    pub const DEFAULT_SIZE: &str = "default-size";
    pub const ERROR: &str = "error";
    pub const FONT_SIZE_REQUEST: &str = "font-size-request";
    pub const FOCUSED: &str = "focused";
    pub const MAPPED: &str = "mapped";
    pub const MIN_SIZE: &str = "min-size";
    pub const MOUSE_HIDDEN: &str = "mouse-hidden";
    pub const MOUSE_SHAPE: &str = "mouse-shape";
    pub const MOUSE_HOVER_URL: &str = "mouse-hover-url";
    pub const PWD: &str = "pwd";
    pub const TITLE: &str = "title";
    pub const TITLE_OVERRIDE: &str = "title-override";
    pub const ZOOM: &str = "zoom";
    pub const IS_SPLIT: &str = "is-split";
    pub const HADJUSTMENT: &str = "hadjustment";
    pub const VADJUSTMENT: &str = "vadjustment";
    pub const HSCROLL_POLICY: &str = "hscroll-policy";
    pub const VSCROLL_POLICY: &str = "vscroll-policy";
    pub const KEY_SEQUENCE: &str = "key-sequence";
    pub const KEY_TABLE: &str = "key-table";
    pub const READONLY: &str = "readonly";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted whenever the bell event is received. Unlike the
    /// `bell-ringing` property, this is emitted every time the event
    /// is received and not just on state changes.
    pub const BELL: &str = "bell";
    /// Emitted whenever the surface would like to be closed for any
    /// reason.
    ///
    /// The surface view does NOT handle its own close confirmation.
    /// If there is a process alive then the boolean parameter will
    /// specify it and the parent widget should handle this request.
    ///
    /// This signal lets the containing widget decide how closure works.
    /// This lets this Surface widget be used as a split, tab, etc.
    /// without it having to be aware of its own semantics.
    pub const CLOSE_REQUEST: &str = "close-request";
    /// Emitted whenever the clipboard has been written. The string
    /// is the text representation of the written contents, empty
    /// when there is none (e.g. an image-only Kitty clipboard
    /// protocol write). The boolean is true when the write cleared
    /// the clipboard, i.e. it carried no non-empty representation.
    pub const CLIPBOARD_WRITE: &str = "clipboard-write";
    /// Emitted whenever the surface reads the clipboard.
    pub const CLIPBOARD_READ: &str = "clipboard-read";
    /// Emitted after the surface is initialized.
    pub const INIT: &str = "init";
    /// Emitted just prior to the context menu appearing.
    pub const MENU: &str = "menu";
    /// Emitted when the focus wants to be brought to the top and
    /// focused.
    pub const PRESENT_REQUEST: &str = "present-request";
    /// Emitted when this surface requests its container to toggle its
    /// fullscreen state.
    pub const TOGGLE_FULLSCREEN: &str = "toggle-fullscreen";
    /// Emitted when this surface requests its container to toggle its
    /// maximized state.
    pub const TOGGLE_MAXIMIZE: &str = "toggle-maximize";
}

/// Simple dimensions struct for the surface used by various properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Boxed)]
#[boxed_type(name = "GhosttySurfaceSize", nullable)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// `font.face.DesiredSize` as the boxed type `GhosttyFontDesiredSize`.
#[derive(Debug, Clone, Copy, PartialEq, glib::Boxed)]
#[boxed_type(name = "GhosttyFontDesiredSize", nullable)]
pub struct FontSizeRequest(pub DesiredSize);

/// `apprt.Clipboard` as the enum type `GhosttyApprtClipboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "GhosttyApprtClipboard")]
pub enum ClipboardType {
    #[default]
    Standard,
    Selection,
    Primary,
}

impl From<Clipboard> for ClipboardType {
    fn from(v: Clipboard) -> Self {
        match v {
            Clipboard::Standard => ClipboardType::Standard,
            Clipboard::Selection => ClipboardType::Selection,
            Clipboard::Primary => ClipboardType::Primary,
        }
    }
}

impl From<ClipboardType> for Clipboard {
    fn from(v: ClipboardType) -> Self {
        match v {
            ClipboardType::Standard => Clipboard::Standard,
            ClipboardType::Selection => Clipboard::Selection,
            ClipboardType::Primary => Clipboard::Primary,
        }
    }
}

macro_rules! mouse_shapes {
    ($($v:ident => $nick:literal),* $(,)?) => {
        /// `terminal.MouseShape` as the enum type `GhosttyMouseShape`. The
        /// nicks are the CSS cursor names used by `propMouseShape`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
        #[enum_type(name = "GhosttyMouseShape")]
        pub enum MouseShape {
            #[default]
            $(#[enum_value(nick = $nick)] $v,)*
        }

        impl From<TerminalMouseShape> for MouseShape {
            fn from(v: TerminalMouseShape) -> Self {
                match v { $(TerminalMouseShape::$v => MouseShape::$v,)* }
            }
        }

        impl MouseShape {
            /// The CSS cursor name for this shape.
            pub fn cursor_name(self) -> &'static str {
                match self { $(MouseShape::$v => $nick,)* }
            }
        }
    };
}

mouse_shapes! {
    Default => "default",
    ContextMenu => "context-menu",
    Help => "help",
    Pointer => "pointer",
    Progress => "progress",
    Wait => "wait",
    Cell => "cell",
    Crosshair => "crosshair",
    Text => "text",
    VerticalText => "vertical-text",
    Alias => "alias",
    Copy => "copy",
    Move => "move",
    NoDrop => "no-drop",
    NotAllowed => "not-allowed",
    Grab => "grab",
    Grabbing => "grabbing",
    AllScroll => "all-scroll",
    ColResize => "col-resize",
    RowResize => "row-resize",
    NResize => "n-resize",
    EResize => "e-resize",
    SResize => "s-resize",
    WResize => "w-resize",
    NeResize => "ne-resize",
    NwResize => "nw-resize",
    SeResize => "se-resize",
    SwResize => "sw-resize",
    EwResize => "ew-resize",
    NsResize => "ns-resize",
    NeswResize => "nesw-resize",
    NwseResize => "nwse-resize",
    ZoomIn => "zoom-in",
    ZoomOut => "zoom-out",
}

/// The state of the key event while we're doing IM composition.
/// See gtkKeyPressed for detailed descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IMKeyEvent {
    /// Not in a key event.
    #[default]
    False,

    /// In a key event but im_composing was either true or false
    /// prior to the calling IME processing. This is important to
    /// work around different input methods calling commit and
    /// preedit end in a different order.
    Composing,
    NotComposing,
}

/// The input method buffer (Zig: `im_buf: [128]u8`).
#[derive(Debug, Clone, Copy)]
pub struct ImBuf(pub [u8; 128]);

impl Default for ImBuf {
    fn default() -> Self {
        ImBuf([0; 128])
    }
}

/// A `Gtk.ScrollablePolicy` with the GTK default (`natural`).
#[derive(Debug, Clone, Copy)]
pub struct Policy(pub gtk4::ScrollablePolicy);

impl Default for Policy {
    fn default() -> Self {
        Policy(gtk4::ScrollablePolicy::Natural)
    }
}

/// The command overrides stored until the core surface is created.
#[derive(Debug, Clone, Default)]
pub struct PrivateOverrides {
    pub command: Option<Command>,
    pub shell_integration: Option<ShellIntegration>,
    pub working_directory: Option<String>,
}

/// A SplitTree implementation that stores surfaces (Zig: `Surface.Tree`,
/// boxed as `GhosttySplitTreeSurface`).
#[derive(glib::Boxed)]
#[boxed_type(name = "GhosttySplitTreeSurface", nullable)]
pub struct Tree(pub dst::SplitTree<Surface>);

impl Clone for Tree {
    fn clone(&self) -> Self {
        Tree(self.0.clone_tree())
    }
}

impl std::ops::Deref for Tree {
    type Target = dst::SplitTree<Surface>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Tree {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl dst::View for Surface {
    fn eql(&self, other: &Self) -> bool {
        self == other
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/surface.ui")]
    pub struct Surface {
        /// The configuration that this surface is using.
        pub config: RefCell<Option<Config>>,

        /// The default size for a window that embeds this surface.
        pub default_size: RefCell<Option<Size>>,

        /// The minimum size for this surface. Embedders enforce this,
        /// not the surface itself.
        pub min_size: RefCell<Option<Size>>,

        /// The smallest size, in logical pixels, that this surface can be
        /// resized to within a split: one cell plus the window padding.
        pub split_min_size: Cell<Size>,

        /// The size of a cell, in pixels, from the last cell_size action.
        pub cell_size: Cell<Option<Size>>,

        /// The requested font size. This only applies to initialization
        /// and has no effect later.
        pub font_size_request: RefCell<Option<FontSizeRequest>>,

        /// The mouse shape to show for the surface.
        pub mouse_shape: Cell<MouseShape>,

        /// Whether the mouse should be hidden or not as requested externally.
        pub mouse_hidden: Cell<bool>,

        /// The URL that the mouse is currently hovering over.
        pub mouse_hover_url: RefCell<Option<String>>,

        /// The current working directory. This has to be reported
        /// externally, usually by shell integration which then talks to
        /// libghostty which triggers this property.
        ///
        /// If this is set prior to initialization then the surface will
        /// start in this pwd. If it is set after, it has no impact on the
        /// core surface.
        pub pwd: RefCell<Option<String>>,

        /// The title of this surface, if any has been set.
        pub title: RefCell<Option<CString>>,

        /// The manually overridden title of this surface from `promptTitle`.
        pub title_override: RefCell<Option<String>>,

        /// The current focus state of the terminal based on the
        /// focus events.
        pub focused: Cell<bool>,

        /// Whether the RenderSurface widget is mapped. Some operations like
        /// grabbing focus only work if a widget is mapped.
        pub mapped: Cell<bool>,

        /// Whether this surface is "zoomed" or not. A zoomed surface
        /// shows up taking the full bounds of a split view.
        pub zoom: Cell<bool>,

        /// The apprt Surface.
        pub rt_surface: OnceCell<apprt::Surface>,

        /// The core surface backing this GTK surface.
        ///
        /// This starts out null and unrealized and is initialized eagerly
        /// when we get our first resize event, since we don't know what
        /// size GTK will allocate for this widget beforehand. This will
        /// then be realized when the widget itself is realized.
        pub core_surface: Cell<Option<*mut CoreSurface>>,

        /// Cached metrics for libghostty callbacks
        pub size: Cell<SurfaceSize>,
        pub cursor_pos: Cell<CursorPos>,

        /// Various input method state. All related to key input.
        pub in_keyevent: Cell<IMKeyEvent>,
        pub im_composing: Cell<bool>,
        pub im_buf: Cell<ImBuf>,
        pub im_len: Cell<u8>,

        /// True when we have a precision scroll in progress
        pub precision_scroll: Cell<bool>,

        /// True when the child has exited.
        pub child_exited: Cell<bool>,

        // Progress bar
        pub progress_bar_timer: RefCell<Option<glib::SourceId>>,

        // Desktop notifications and timers owned by this surface.
        pub desktop_notifications: RefCell<DesktopNotifications>,

        // True while the bell is ringing. This will be set to false (after
        // true) under various scenarios, but can also manually be set to
        // false by a parent widget.
        pub bell_ringing: Cell<bool>,

        /// True if this surface is in an error state. This is currently
        /// a simple boolean with no additional information on WHAT the
        /// error state is, because we don't yet need it or use it. For now,
        /// if this is true, then it means the terminal is non-functional.
        pub error: Cell<bool>,

        /// The source that handles setting our child property.
        pub idle_rechild: RefCell<Option<glib::SourceId>>,

        /// A weak reference to an inspector window.
        #[cfg(feature = "inspector")]
        pub inspector: glib::WeakRef<InspectorWindow>,

        // True if the current surface is a split, this is used to apply
        // unfocused-split-* options
        pub is_split: Cell<bool>,
        pub is_split_binding: RefCell<Option<glib::Binding>>,

        pub action_group: RefCell<Option<gio::SimpleActionGroup>>,

        // Gtk.Scrollable interface adjustments
        pub hadj: RefCell<Option<gtk4::Adjustment>>,
        pub vadj: RefCell<Option<gtk4::Adjustment>>,
        pub hscroll_policy: Cell<Policy>,
        pub vscroll_policy: Cell<Policy>,
        pub vadj_signal_group: RefCell<Option<glib::SignalGroup>>,

        // Key state tracking for key sequences and tables
        pub key_sequence: RefCell<Vec<CString>>,
        pub key_tables: RefCell<Vec<CString>>,

        /// The context for this surface (window, tab, or split); `None`
        /// is the default, `window`.
        pub context: Cell<Option<NewSurfaceContext>>,

        /// Whether primary paste (middle-click paste) is enabled.
        pub gtk_enable_primary_paste: Cell<bool>,

        /// True when a left mouse down was consumed purely for a focus
        /// change, and the matching left mouse release should also be
        /// suppressed.
        pub suppress_left_mouse_release: Cell<bool>,

        /// How much pending horizontal scroll do we have?
        pub pending_horizontal_scroll: Cell<f64>,

        /// Timer to reset the amount of horizontal scroll if the user
        /// stops scrolling.
        pub pending_horizontal_scroll_reset: RefCell<Option<glib::SourceId>>,

        pub overrides: RefCell<PrivateOverrides>,

        // Template binds
        /// The RenderSurface that displays the rendered output of the surface.
        #[template_child]
        pub render_surface: TemplateChild<RenderSurface>,
        /// The labels for the left/right sides of the URL hover tooltip.
        #[template_child]
        pub url_left: TemplateChild<gtk4::Label>,
        #[template_child]
        pub url_right: TemplateChild<gtk4::Label>,
        /// The resize overlay
        #[template_child]
        pub resize_overlay: TemplateChild<ResizeOverlay>,
        /// The search overlay
        #[template_child]
        pub search_overlay: TemplateChild<SearchOverlay>,
        /// The key state overlay
        #[template_child]
        pub key_state_overlay: TemplateChild<KeyStateOverlay>,
        #[template_child]
        pub child_exited_overlay: TemplateChild<SurfaceChildExited>,
        #[template_child]
        pub context_menu: TemplateChild<gtk4::PopoverMenu>,
        #[template_child]
        pub drop_target: TemplateChild<gtk4::DropTarget>,
        #[template_child]
        pub surface_drop_target: TemplateChild<gtk4::DropTarget>,
        #[template_child]
        pub drag_handle: TemplateChild<gtk4::Widget>,
        #[template_child]
        pub drop_overlay: TemplateChild<gtk4::Widget>,
        #[template_child]
        pub progress_bar_overlay: TemplateChild<gtk4::ProgressBar>,
        #[template_child]
        pub error_page: TemplateChild<libadwaita::StatusPage>,
        #[template_child]
        pub terminal_page: TemplateChild<gtk4::Overlay>,
        #[template_child]
        pub im_context: TemplateChild<gtk4::IMMulticontext>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Surface {
        const NAME: &'static str = "GhosttySurface";
        type Type = super::Surface;
        type ParentType = libadwaita::Bin;
        type Interfaces = (gtk4::Scrollable,);

        fn class_init(klass: &mut Self::Class) {
            RenderSurface::ensure_type();
            ResizeOverlay::ensure_type();
            SearchOverlay::ensure_type();
            KeyStateOverlay::ensure_type();
            SurfaceChildExited::ensure_type();
            class::ensure_resources();
            klass.bind_template();
            super::bind_template_callbacks(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
            // SAFETY: the instance is initialized (as in `init_template`).
            unsafe { obj.as_ref() }.init();
        }
    }

    impl ObjectImpl for Surface {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecBoolean::builder(properties::BELL_RINGING).build(),
                    glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build(),
                    glib::ParamSpecBoolean::builder(properties::CHILD_EXITED).build(),
                    glib::ParamSpecBoxed::builder::<Size>(properties::DEFAULT_SIZE).build(),
                    glib::ParamSpecBoolean::builder(properties::ERROR).build(),
                    glib::ParamSpecBoxed::builder::<FontSizeRequest>(properties::FONT_SIZE_REQUEST)
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::FOCUSED).build(),
                    glib::ParamSpecBoolean::builder(properties::MAPPED).build(),
                    glib::ParamSpecBoxed::builder::<StringList>(properties::KEY_SEQUENCE)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoxed::builder::<StringList>(properties::KEY_TABLE)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoxed::builder::<Size>(properties::MIN_SIZE).build(),
                    glib::ParamSpecEnum::builder_with_default(properties::MOUSE_SHAPE, MouseShape::Text)
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::MOUSE_HIDDEN).build(),
                    glib::ParamSpecString::builder(properties::MOUSE_HOVER_URL).build(),
                    glib::ParamSpecString::builder(properties::PWD).build(),
                    glib::ParamSpecString::builder(properties::TITLE).build(),
                    glib::ParamSpecString::builder(properties::TITLE_OVERRIDE).build(),
                    glib::ParamSpecBoolean::builder(properties::ZOOM).build(),
                    glib::ParamSpecBoolean::builder(properties::IS_SPLIT).build(),
                    glib::ParamSpecBoolean::builder(properties::READONLY).read_only().build(),
                    // For Gtk.Scrollable
                    glib::ParamSpecOverride::for_interface::<gtk4::Scrollable>(properties::HADJUSTMENT),
                    glib::ParamSpecOverride::for_interface::<gtk4::Scrollable>(properties::VADJUSTMENT),
                    glib::ParamSpecOverride::for_interface::<gtk4::Scrollable>(
                        properties::HSCROLL_POLICY,
                    ),
                    glib::ParamSpecOverride::for_interface::<gtk4::Scrollable>(
                        properties::VSCROLL_POLICY,
                    ),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::BELL_RINGING => accessor::get_shallow(&self.bell_ringing),
                properties::CONFIG => accessor::get(&self.config),
                properties::CHILD_EXITED => accessor::get_shallow(&self.child_exited),
                properties::DEFAULT_SIZE => accessor::get(&self.default_size),
                properties::ERROR => accessor::get_shallow(&self.error),
                properties::FONT_SIZE_REQUEST => accessor::get(&self.font_size_request),
                properties::FOCUSED => accessor::get_shallow(&self.focused),
                properties::MAPPED => accessor::get_shallow(&self.mapped),
                properties::KEY_SEQUENCE => obj.get_key_sequence().to_value(),
                properties::KEY_TABLE => obj.get_key_table().to_value(),
                properties::MIN_SIZE => accessor::get(&self.min_size),
                properties::MOUSE_SHAPE => obj.get_mouse_shape().to_value(),
                properties::MOUSE_HIDDEN => obj.get_mouse_hidden().to_value(),
                properties::MOUSE_HOVER_URL => accessor::get(&self.mouse_hover_url),
                properties::PWD => accessor::get(&self.pwd),
                properties::TITLE => self
                    .title
                    .borrow()
                    .as_ref()
                    .map(|t| t.to_string_lossy().into_owned())
                    .to_value(),
                properties::TITLE_OVERRIDE => accessor::get(&self.title_override),
                properties::ZOOM => accessor::get_shallow(&self.zoom),
                properties::IS_SPLIT => accessor::get_shallow(&self.is_split),
                properties::READONLY => obj.get_readonly().to_value(),
                properties::HADJUSTMENT => obj.get_h_adjustment().to_value(),
                properties::VADJUSTMENT => obj.get_v_adjustment().to_value(),
                properties::HSCROLL_POLICY => self.hscroll_policy.get().0.to_value(),
                properties::VSCROLL_POLICY => self.vscroll_policy.get().0.to_value(),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                properties::BELL_RINGING => accessor::set_shallow(&self.bell_ringing, value),
                properties::CONFIG => accessor::set(&self.config, value),
                properties::CHILD_EXITED => accessor::set_shallow(&self.child_exited, value),
                properties::DEFAULT_SIZE => accessor::set(&self.default_size, value),
                properties::ERROR => accessor::set_shallow(&self.error, value),
                properties::FONT_SIZE_REQUEST => accessor::set(&self.font_size_request, value),
                properties::FOCUSED => accessor::set_shallow(&self.focused, value),
                properties::MAPPED => accessor::set_shallow(&self.mapped, value),
                properties::MIN_SIZE => accessor::set(&self.min_size, value),
                properties::MOUSE_SHAPE => obj.set_mouse_shape(value.get().unwrap()),
                properties::MOUSE_HIDDEN => obj.set_mouse_hidden(value.get().unwrap()),
                properties::MOUSE_HOVER_URL => accessor::set(&self.mouse_hover_url, value),
                properties::PWD => accessor::set(&self.pwd, value),
                properties::TITLE => {
                    let v: Option<String> = value.get().unwrap();
                    self.title.replace(v.map(cstring_lossy));
                }
                properties::TITLE_OVERRIDE => accessor::set(&self.title_override, value),
                properties::ZOOM => accessor::set_shallow(&self.zoom, value),
                properties::IS_SPLIT => accessor::set_shallow(&self.is_split, value),
                properties::HADJUSTMENT => obj.set_h_adjustment(value.get().unwrap()),
                properties::VADJUSTMENT => obj.set_v_adjustment(value.get().unwrap()),
                properties::HSCROLL_POLICY => self.hscroll_policy.set(Policy(value.get().unwrap())),
                properties::VSCROLL_POLICY => self.vscroll_policy.set(Policy(value.get().unwrap())),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signals::BELL).build(),
                    Signal::builder(signals::CLOSE_REQUEST).build(),
                    Signal::builder(signals::CLIPBOARD_READ).build(),
                    Signal::builder(signals::CLIPBOARD_WRITE)
                        .param_types([
                            ClipboardType::static_type(),
                            String::static_type(),
                            bool::static_type(),
                        ])
                        .build(),
                    Signal::builder(signals::INIT).build(),
                    Signal::builder(signals::MENU).build(),
                    Signal::builder(signals::PRESENT_REQUEST).build(),
                    Signal::builder(signals::TOGGLE_FULLSCREEN).build(),
                    Signal::builder(signals::TOGGLE_MAXIMIZE).build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            let obj = self.obj();
            obj.clear_desktop_notifications();

            self.config.replace(None);

            if let Some(binding) = self.is_split_binding.take() {
                binding.unbind();
            }

            if let Some(group) = self.vadj_signal_group.take() {
                group.set_target(None::<&glib::Object>);
            }

            self.hadj.replace(None);
            self.vadj.replace(None);

            if let Some(timer) = self.progress_bar_timer.take() {
                timer.remove();
            }

            if let Some(v) = self.idle_rechild.take() {
                v.remove();
            }

            if let Some(v) = self.pending_horizontal_scroll_reset.take() {
                v.remove();
            }

            // This works around a GTK double-free bug where if you bind
            // to a top-level template child, it frees twice if the widget is
            // also the root child of the template. By unsetting the child
            // here, we avoid the double-free.
            obj.set_child(None::<&gtk4::Widget>);

            self.dispose_template();
        }
    }

    impl Drop for Surface {
        // Zig: `finalize`.
        fn drop(&mut self) {
            if let Some(v) = self.core_surface.get() {
                // Remove ourselves from the list of known surfaces in the
                // app. We do this before deinit in case a callback triggers
                // searching for this surface.
                if let Some(app) = Application::try_default() {
                    if let Some(rt) = self.rt_surface.get() {
                        app.core().delete_surface(rt as *const apprt::Surface as *mut apprt::Surface);
                    }
                }

                // NOTE: We must deinit the surface in the finalize call and
                // NOT the dispose call because the inspector widget relies on
                // this behavior with a weakRef to properly deactivate.

                // Deinit the surface
                // SAFETY: the core surface was created by `init_surface`
                // and is freed exactly once, here.
                unsafe {
                    (*v).deinit();
                    drop(Box::from_raw(v));
                }

                self.core_surface.set(None);
            }

            // Clean up key sequence and key table state (dropped with the
            // vectors) and the desktop notifications.
            self.desktop_notifications.get_mut().deinit();
        }
    }

    impl WidgetImpl for Surface {
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            self.obj().snapshot_impl(snapshot);
            self.parent_snapshot(snapshot);
        }
    }

    impl BinImpl for Surface {}
    impl ScrollableImpl for Surface {}
}

glib::wrapper! {
    pub struct Surface(ObjectSubclass<imp::Surface>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Scrollable;
}

fn cstring_lossy(s: impl Into<Vec<u8>>) -> CString {
    let mut v: Vec<u8> = s.into();
    if let Some(i) = v.iter().position(|&b| b == 0) {
        v.truncate(i);
    }
    CString::new(v).expect("no interior NUL")
}

/// Logs a warning once for the given call site. It may log duplicates if
/// called from multiple threads but will eventually stop.
macro_rules! log_warn_once {
    ($($arg:tt)*) => {{
        static FLAG: AtomicBool = AtomicBool::new(false);
        if !FLAG.swap(true, Ordering::Relaxed) {
            log::warn!(target: LOG, $($arg)*);
        }
    }};
}

impl Surface {
    pub fn new(overrides: &Overrides) -> Surface {
        let this: Surface = glib::Object::builder()
            .property(properties::TITLE_OVERRIDE, overrides.title.as_deref())
            .build();
        this.imp().overrides.replace(PrivateOverrides {
            command: overrides.command.clone(),
            shell_integration: overrides.shell_integration,
            working_directory: overrides.working_directory.clone(),
        });
        this
    }

    /// The core surface, if initialized.
    pub fn core(&self) -> Option<*mut CoreSurface> {
        self.imp().core_surface.get()
    }

    /// The core surface as a mutable reference.
    fn core_mut(&self) -> Option<&mut CoreSurface> {
        // SAFETY: the core surface is alive while set and is only used on
        // the main thread (like the Zig pointer).
        self.core().map(|p| unsafe { &mut *p })
    }

    pub fn rt(&self) -> *mut apprt::Surface {
        let rt = self.imp().rt_surface.get().expect("rt surface initialized");
        rt as *const apprt::Surface as *mut apprt::Surface
    }

    /// Set the parent of this surface. This will extract the information
    /// required to initialize this surface with the proper values but
    /// doesn't retain any memory.
    ///
    /// If the surface is already realized this does nothing.
    pub fn set_parent(&self, parent: &CoreSurface, context: NewSurfaceContext) {
        let priv_ = self.imp();

        // This is a mistake! We can only set a parent before surface
        // realization. We log this because this is probably a logic error.
        if priv_.core_surface.get().is_some() {
            log::warn!(target: LOG, "setParent called after surface is already realized");
            return;
        }

        // Store the context so initSurface can use it
        priv_.context.set(Some(context));

        // Setup our font size
        priv_.font_size_request.replace(Some(FontSizeRequest(parent.font_size)));
        self.notify(properties::FONT_SIZE_REQUEST);

        // Remainder needs a config. If there is no config we just assume
        // we aren't inheriting any of these values.
        let config_obj = priv_.config.borrow().clone();
        if let Some(config_obj) = config_obj {
            // Setup our cwd if configured to inherit
            if apprt::surface::should_inherit_working_directory(context, &config_obj.get()) {
                // SAFETY: the parent is a live GTK core surface.
                let parent_gobj = unsafe { crate::apprt::gtk::gobj_of(parent) };
                if let Some(pwd) = parent_gobj.get_pwd() {
                    priv_.pwd.replace(Some(pwd));
                    self.notify(properties::PWD);
                }
            }
        }
    }

    /// The context stored by [`Surface::set_parent`].
    pub fn parent_context(&self) -> NewSurfaceContext {
        self.imp().context.get().unwrap_or(NewSurfaceContext::Window)
    }

    /// Force the surface to redraw itself. Ghostty often will only redraw
    /// the terminal in reaction to internal changes. If there are external
    /// events that invalidate the surface, such as the widget moving
    /// parents, then we should force a redraw.
    pub fn redraw(&self) {
        self.imp().render_surface.queue_draw();
    }

    pub fn bind_is_split(&self, tree: &SplitTree) {
        let priv_ = self.imp();
        if let Some(binding) = priv_.is_split_binding.take() {
            binding.unbind();
        }

        let binding = tree
            .bind_property(properties::IS_SPLIT, self, properties::IS_SPLIT)
            .sync_create()
            .build();
        // The ref created by bindProperty is owned by the binding itself.
        // We keep another ref to prevent the binding object from being
        // freed if the source object (SplitTree) is finalized.
        priv_.is_split_binding.replace(Some(binding));
    }

    pub fn toggle_fullscreen(&self) {
        self.emit_by_name::<()>(signals::TOGGLE_FULLSCREEN, &[]);
    }

    pub fn toggle_maximize(&self) {
        self.emit_by_name::<()>(signals::TOGGLE_MAXIMIZE, &[]);
    }

    pub fn toggle_command_palette(&self) -> bool {
        // TODO: pass the surface with the action
        self.activate_action("win.toggle-command-palette", None).is_ok()
    }

    #[cfg(not(feature = "inspector"))]
    pub fn control_inspector(&self, value: InspectorAction) -> bool {
        // The terminal inspector needs Dear ImGui (the `inspector` Cargo
        // feature).
        let _ = value;
        log::warn!(target: LOG, "the terminal inspector requires the `inspector` feature");
        false
    }

    #[cfg(feature = "inspector")]
    pub fn control_inspector(&self, value: InspectorAction) -> bool {
        // Let's see if we have an inspector already.
        let priv_ = self.imp();
        if let Some(inspector) = priv_.inspector.upgrade() {
            match value {
                InspectorAction::Show => {}
                // Our weak ref will set our private value to null
                InspectorAction::Toggle | InspectorAction::Hide => inspector.destroy(),
            }
        } else {
            match value {
                InspectorAction::Toggle | InspectorAction::Show => {
                    let inspector = InspectorWindow::new(self);
                    inspector.present();
                    priv_.inspector.set(Some(&inspector));
                }
                InspectorAction::Hide => {}
            }
        }

        true
    }

    /// Redraw our inspector, if there is one associated with this surface.
    pub fn redraw_inspector(&self) {
        #[cfg(feature = "inspector")]
        if let Some(v) = self.imp().inspector.upgrade() {
            v.queue_render();
        }
    }

    /// Handle a key sequence action from the apprt.
    pub fn key_sequence_action(&self, value: &KeySequence) {
        let priv_ = self.imp();

        let _guard = self.freeze_notify();
        self.notify(properties::KEY_SEQUENCE);

        match value {
            KeySequence::Trigger(trigger) => {
                // Convert the trigger to a human-readable label
                let mut buf = String::new();
                match gtk_key::label_from_trigger(&mut buf, *trigger) {
                    Ok(true) => {}
                    _ => return,
                }

                // Copy and append
                priv_.key_sequence.borrow_mut().push(cstring_lossy(buf));
            }
            KeySequence::End => {
                // Free all the stored strings and clear
                priv_.key_sequence.borrow_mut().clear();
            }
        }
    }

    /// Handle a key table action from the apprt.
    pub fn key_table_action(&self, value: &KeyTable<'_>) {
        let priv_ = self.imp();

        let _guard = self.freeze_notify();
        self.notify(properties::KEY_TABLE);

        match value {
            KeyTable::Activate(name) => {
                // Duplicate the name string and push onto stack
                priv_.key_tables.borrow_mut().push(cstring_lossy(name.to_vec()));
            }
            KeyTable::Deactivate => {
                // Pop and free the top table
                priv_.key_tables.borrow_mut().pop();
            }
            KeyTable::DeactivateAll => {
                // Free all tables and clear
                priv_.key_tables.borrow_mut().clear();
            }
        }
    }

    pub fn show_on_screen_keyboard(&self, event: Option<&gdk::Event>) -> bool {
        self.imp().im_context.activate_osk(event)
    }

    /// Set the scrollbar state for this surface. This will setup the
    /// properties for our Gtk.Scrollable interface properly.
    pub fn set_scrollbar(&self, scrollbar: Scrollbar) {
        // Update existing adjustment in-place. If we don't have an
        // adjustment then we do nothing because we're not part of a
        // scrolled window.
        let Some(vadj) = self.get_v_adjustment() else { return };

        // Check if values match existing adjustment and skip update if so
        let value = scrollbar.offset as f64;
        let upper = scrollbar.total as f64;
        let page_size = scrollbar.len as f64;

        if (vadj.value() - value).abs() <= 0.001
            && (vadj.upper() - upper).abs() <= 0.001
            && (vadj.page_size() - page_size).abs() <= 0.001
        {
            return;
        }

        // If we have a vadjustment we MUST have the signal group since
        // it is setup in the prop handler.
        let group = self.imp().vadj_signal_group.borrow().clone().expect("vadj signal group");

        // During manual scrollbar changes from Ghostty core we don't
        // want to emit value-changed signals so we block them. This would
        // cause a waste of resources at best and infinite loops at worst.
        group.block();
        vadj.configure(
            value,     // value: current scroll position
            0.0,       // lower: minimum value
            upper,     // upper: maximum value (total scrollable area)
            1.0,       // step_increment: amount to scroll on arrow click
            page_size, // page_increment: amount to scroll on page up/down
            page_size, // page_size: size of visible area
        );
        group.unblock();
    }

    /// Set the current progress report state.
    pub fn set_progress_report(&self, value: ProgressReport) {
        let priv_ = self.imp();

        // No matter what, we stop the timer because if we're removing
        // then we're done and otherwise we restart it.
        if let Some(timer) = priv_.progress_bar_timer.take() {
            timer.remove();
        }

        let progress_bar = priv_.progress_bar_overlay.get();
        if let Some(config) = priv_.config.borrow().as_ref() {
            if !config.get().progress_style {
                log::debug!(target: LOG, "progress_report action blocked by config");
                progress_bar.set_visible(false);
                return;
            }
        }

        match value.state {
            // Remove the progress bar
            ProgressReportState::Remove => {
                // Set a value to stop the pulse animation, if there is one.
                progress_bar.set_fraction(0.0);
                progress_bar.set_visible(false);
                return;
            }

            // Set the progress bar to a fixed value if one was provided,
            // otherwise pulse. Remove the `error` CSS class so that the
            // progress bar shows as normal.
            ProgressReportState::Set => {
                progress_bar.remove_css_class("error");
                match value.progress {
                    Some(progress) => progress_bar.set_fraction(compute_fraction(progress)),
                    None => progress_bar.pulse(),
                }
            }

            // Set the progress bar to a fixed value if one was provided,
            // otherwise pulse. Set the `error` CSS class so that the
            // progress bar shows as an error color.
            ProgressReportState::Error => {
                progress_bar.add_css_class("error");
                match value.progress {
                    Some(progress) => progress_bar.set_fraction(compute_fraction(progress)),
                    None => progress_bar.pulse(),
                }
            }

            // The state of progress is unknown, so pulse the progress bar
            // to indicate that things are still happening.
            ProgressReportState::Indeterminate => progress_bar.pulse(),

            // If a progress value was provided, set the progress bar to
            // that value. Don't pulse the progress bar as that would
            // indicate that things were happening. Otherwise this is mainly
            // used to keep the progress bar on screen instead of timing out.
            ProgressReportState::Pause => {
                if let Some(progress) = value.progress {
                    progress_bar.set_fraction(compute_fraction(progress));
                }
            }
        }

        // Assume all states lead to visibility
        debug_assert!(value.state != ProgressReportState::Remove);
        progress_bar.set_visible(true);

        // Start our timer to remove bad actor programs that stall
        // the progress bar.
        const PROGRESS_BAR_TIMEOUT_SECONDS: u64 = 15;
        debug_assert!(priv_.progress_bar_timer.borrow().is_none());
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_secs(PROGRESS_BAR_TIMEOUT_SECONDS),
            move || {
                // The progress bar hasn't been updated by the TUI recently,
                // remove it.
                if let Some(this) = weak.upgrade() {
                    this.imp().progress_bar_timer.replace(None);
                    this.set_progress_report(ProgressReport::new(ProgressReportState::Remove));
                }
            },
        );
        priv_.progress_bar_timer.replace(Some(id));
    }

    /// Request that this terminal come to the front and become focused.
    /// It is up to the embedding widget to react to this.
    pub fn present(&self) {
        self.emit_by_name::<()>(signals::PRESENT_REQUEST, &[]);
    }

    pub fn command_finished(&self, value: CommandFinished) -> bool {
        let priv_ = self.imp();

        let notify_next_command_finish = (|| {
            let group = priv_.action_group.borrow().clone()?;
            let state = group.action_state("notify-on-next-command-finish")?;
            let notify = state.get::<bool>()?;
            group.change_action_state("notify-on-next-command-finish", &false.to_variant());
            Some(notify)
        })()
        .unwrap_or(false);

        let Some(config) = priv_.config.borrow().clone() else { return false };
        let cfg = config.get();

        if !notify_next_command_finish {
            if cfg.notify_on_command_finish == NotifyOnCommandFinish::Never {
                return true;
            }
            if cfg.notify_on_command_finish == NotifyOnCommandFinish::Unfocused && self.get_focused() {
                return true;
            }
        }

        if value.duration.lte(cfg.notify_on_command_finish_after) {
            return true;
        }

        let action = cfg.notify_on_command_finish_action;
        drop(cfg);

        if action.bell {
            self.set_bell_ringing(true);
        }

        if action.notify {
            let title = match value.exit_code {
                None => class::i18n_(c"Command Finished"),
                Some(0) => class::i18n_(c"Command Succeeded"),
                Some(_) => class::i18n_(c"Command Failed"),
            };
            let duration = value.duration.round(1_000_000);
            let body = match value.exit_code {
                None => format!("Command took {duration}."),
                Some(code) => format!("Command took {duration} and exited with code {code}."),
            };

            self.send_desktop_notification(title, &body);
        }

        true
    }

    /// Get the readonly state from the core surface.
    pub fn get_readonly(&self) -> bool {
        match self.core_mut() {
            Some(surface) => surface.readonly,
            None => false,
        }
    }

    /// Notify anyone interested that the readonly status has changed.
    pub fn set_readonly(&self, _: rt_action::Readonly) -> bool {
        self.notify(properties::READONLY);
        true
    }

    /// Key press event (press or release).
    ///
    /// At a high level, we want to construct an `input.KeyEvent` and
    /// pass that to `keyCallback`. At a low level, this is more complicated
    /// than it appears because we need to construct all of this information
    /// and its not given to us.
    ///
    /// For all events, we run the GdkEvent through the input method context.
    /// This allows the input method to capture the event and trigger
    /// callbacks such as preedit, commit, etc.
    ///
    /// There are a couple important aspects to the prior paragraph: we must
    /// send ALL events through the input method context. This is because
    /// input methods use both key press and key release events to determine
    /// the state of the input method. For example, fcitx uses key release
    /// events on modifiers (i.e. ctrl+shift) to switch the input method.
    ///
    /// We set some state to note we're in a key event (self.in_keyevent)
    /// because some of the input method callbacks change behavior based on
    /// this state. For example, we don't want to send character events
    /// like "a" via the input "commit" event if we're actively processing
    /// a keypress because we'd lose access to the keycode information.
    /// However, a "commit" event may still happen outside of a keypress
    /// event from e.g. a tablet or on-screen keyboard.
    ///
    /// Finally, we take all of the information in order to determine if we
    /// have a unicode character or if we have to map the keyval to a code to
    /// get the underlying logical key, etc.
    ///
    /// Then we can emit the keyCallback.
    pub fn key_event(
        &self,
        action: input::Action,
        ec_key: &gtk4::EventControllerKey,
        keyval: u32,
        keycode: u32,
        gtk_mods: gdk::ModifierType,
    ) -> bool {
        let Some(event) = ec_key.current_event() else { return false };
        let Some(key_event) = event.downcast_ref::<gdk::KeyEvent>() else { return false };
        let priv_ = self.imp();

        // The block below is all related to input method handling. See the
        // function comment for some high level details and then the
        // comments within the block for more specifics.
        {
            // This can trigger an input method so we need to notify the im
            // context where the cursor is so it can render the dropdowns in
            // the correct place.
            self.update_ime_position();

            // We note that we're in a keypress because we want some logic to
            // depend on this. For example, we don't want to send character
            // events like "a" via the input "commit" event if we're actively
            // processing a keypress because we'd lose access to the keycode
            // information.
            //
            // We have to maintain some additional state here of whether we
            // were composing because different input methods call the
            // callbacks in different orders. For example, ibus calls commit
            // THEN preedit end but simple calls preedit end THEN commit.
            priv_.in_keyevent.set(if priv_.im_composing.get() {
                IMKeyEvent::Composing
            } else {
                IMKeyEvent::NotComposing
            });

            // Pass the event through the input method which returns true if
            // handled. Confusingly, not all events handled by the input
            // method result in this returning true so we have to maintain
            // some additional state about whether we were composing or not
            // to determine if we should proceed with key encoding.
            //
            // Cases where the input method does not mark the event as
            // handled:
            //
            // - If we change the input method via keypress while we have
            //   preedit text, the input method will commit the pending text
            //   but will not mark it as handled. We use the `.composing`
            //   state to detect this case.
            //
            // - If we switch input methods (i.e. via ctrl+shift with fcitx),
            //   the input method will handle the key release event but will
            //   not mark it as handled. I don't know any way to detect this
            //   case so it will result in a key event being sent to the key
            //   callback. For Kitty text encoding, this will result in
            //   modifiers being triggered despite being technically
            //   consumed. At the time of writing, both Kitty and Alacritty
            //   have the same behavior. I know of no way to fix this.
            let im_handled = priv_.im_context.filter_keypress(&event);
            let in_keyevent = priv_.in_keyevent.replace(IMKeyEvent::False);

            // If the input method handled the event, you would think we
            // would never proceed with key encoding for Ghostty but that is
            // not the case. Input methods will handle basic character
            // encoding like typing "a" and we want to associate that with
            // the key event. So we have to check additional state to
            // determine if we exit.
            if im_handled {
                // If we are composing then we're in a preedit state and do
                // not want to encode any keys. For example: type a deadkey
                // such as single quote on a US international keyboard layout.
                if priv_.im_composing.get() {
                    return true;
                }

                // If we were composing and now we're not, it means that we
                // committed the text. We also don't want to encode a key
                // event for this. Example: enable Japanese input method,
                // press "konn" and then press enter. The final enter should
                // not be encoded and "konn" (in hiragana) should be written
                // as "こん".
                if in_keyevent == IMKeyEvent::Composing {
                    return true;
                }

                // Not composing and our input method buffer is empty. This
                // could mean that the input method reacted to this event by
                // activating an onscreen keyboard or something equivalent.
                // We don't know. But the input method handled it and didn't
                // give us text so we will just assume we should not encode
                // this. This handles a real scenario when ibus starts the
                // emoji input method (super+.).
                if priv_.im_len.get() == 0 {
                    return true;
                }
            }

            // At this point, for the sake of explanation of internal state:
            // it is possible that im_len > 0 and im_composing == false. This
            // means that we received a commit event from the input method
            // that we want associated with the key event. This is common:
            // its how basic character translation for simple inputs like "a"
            // work.
        }

        // We always reset the length of the im buffer. There's only one
        // scenario we reach this point with im_len > 0 and that's if we
        // received a commit event from the input method. We don't want to
        // keep that state around since we've handled it here.
        struct ResetImLen<'a>(&'a Cell<u8>);
        impl Drop for ResetImLen<'_> {
            fn drop(&mut self) {
                self.0.set(0);
            }
        }
        let _reset = ResetImLen(&priv_.im_len);

        // Get the keyvals for this event.
        // SAFETY: plain FFI call.
        let keyval_unicode = unsafe { gdk::ffi::gdk_keyval_to_unicode(keyval) };
        let keyval_unicode_unshifted =
            gtk_key::keyval_unicode_unshifted(&*priv_.render_surface, key_event, keycode);

        // We want to get the physical unmapped key to process physical
        // keybinds. (These are keybinds explicitly marked as requesting
        // physical mapping).
        let physical_key = {
            let w3c_key = input::keycodes::ENTRIES
                .iter()
                .find(|entry| entry.native == keycode)
                .map(|entry| entry.key)
                .unwrap_or(input::Key::Unidentified);

            gtk_key::remap_key(w3c_key, keyval, key_event.is_modifier())
        };

        // Get our modifier for the event
        let app = Application::default();
        let mods = app
            .with_winproto(|wp| gtk_key::event_mods(&event, physical_key, gtk_mods, action, wp))
            .unwrap_or_else(|| gtk_key::translate_mods(gtk_mods));

        // Get our consumed modifiers
        let consumed_mods = {
            let masked = key_event.consumed_modifiers() & gdk::MODIFIER_MASK;
            gtk_key::translate_mods(masked)
        };

        // If we have no UTF-8 text, we try to convert our keyval to
        // a text value. We have to do this because GTK will not process
        // "Ctrl+Shift+1" (on US keyboards) as "Ctrl+!" but instead as "".
        // But the keyval is set correctly so we can at least extract that.
        if priv_.im_len.get() == 0 && keyval_unicode > 0 {
            if let Some(cp) = char::from_u32(keyval_unicode) {
                // We don't want to send control characters as IM
                // text. Control characters are handled already by
                // the encoder directly.
                if (cp as u32) >= 0x20 {
                    let mut buf = priv_.im_buf.get().0;
                    let len = cp.encode_utf8(&mut buf).len();
                    priv_.im_buf.set(ImBuf(buf));
                    priv_.im_len.set(len as u8);
                }
            }
        }

        // Invoke the core Ghostty logic to handle this input.
        let Some(surface) = self.core_mut() else { return false };
        let im_buf = priv_.im_buf.get().0;
        let effect = surface.key_callback(KeyEvent {
            action,
            key: physical_key,
            mods,
            consumed_mods,
            composing: priv_.im_composing.get(),
            utf8: &im_buf[..priv_.im_len.get() as usize],
            unshifted_codepoint: keyval_unicode_unshifted,
        });
        let effect = match effect {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error in key callback err={err}");
                return false;
            }
        };

        match effect {
            InputEffect::Closed => return true,
            InputEffect::Ignored => {}
            InputEffect::Consumed => {
                if action == input::Action::Press || action == input::Action::Repeat {
                    // If we were in the composing state then we reset our
                    // context. We do NOT want to reset if we're not in the
                    // composing state because there is other IME state that
                    // we want to preserve, such as quotation mark ordering
                    // for Chinese input.
                    if priv_.im_composing.get() {
                        priv_.im_context.reset();
                        if let Some(surface) = self.core_mut() {
                            let _ = surface.preedit_callback(None);
                        }
                    }

                    // Bell stops ringing when any key is pressed that is used
                    // by the core in any way.
                    self.set_bell_ringing(false);

                    return true;
                }
            }
        }

        false
    }

    /// Prompt for a manual title change for the surface.
    pub fn prompt_title(&self) {
        let priv_ = self.imp();
        let initial = priv_
            .title_override
            .borrow()
            .clone()
            .or_else(|| priv_.title.borrow().as_ref().map(|t| t.to_string_lossy().into_owned()));
        let dialog = TitleDialog::new(title_dialog::Target::Surface, initial.as_deref());
        let weak = self.downgrade();
        dialog.connect_set(move |_, title| {
            if let Some(this) = weak.upgrade() {
                this.set_title_override(if title.is_empty() { None } else { Some(title) });
            }
        });

        dialog.present(self.upcast_ref());
    }

    /// Scale x/y by the GDK device scale.
    fn scaled_coordinates(&self, x: f64, y: f64) -> (f64, f64) {
        let scale_factor = scale_util::widget_surface_scale(&*self.imp().render_surface);
        (x * scale_factor, y * scale_factor)
    }

    //---------------------------------------------------------------
    // Libghostty Callbacks

    pub fn close(&self) {
        self.emit_by_name::<()>(signals::CLOSE_REQUEST, &[]);
    }

    pub fn child_exited(&self, data: ChildExited) -> bool {
        // Keep our property up to date for anyone listening.
        let priv_ = self.imp();
        priv_.child_exited.set(true);
        self.notify(properties::CHILD_EXITED);

        priv_.child_exited_overlay.set_data(Some(&data));
        true
    }

    pub fn get_content_scale(&self) -> ContentScale {
        let widget = &*self.imp().render_surface;

        let gtk_scale: f32 = {
            let surface_scale = scale_util::widget_surface_scale(widget);
            if surface_scale <= 0.0 {
                log_warn_once!("widget surface scale was non-positive: {}", surface_scale);
                1.0
            } else {
                surface_scale as f32
            }
        };

        // Also scale using font-specific DPI, which is often exposed to the
        // user via DE accessibility settings (see
        // https://docs.gtk.org/gtk4/class.Settings.html).
        let xft_dpi_scale: f32 = 'xft_scale: {
            // gtk-xft-dpi is font DPI multiplied by 1024. See
            // https://docs.gtk.org/gtk4/property.Settings.gtk-xft-dpi.html
            let Some(gtk_xft_dpi) = gsettings::get::<gsettings::GtkXftDpi>() else {
                log_warn_once!("gtk-xft-dpi was not set, using default value");
                break 'xft_scale 1.0;
            };

            // Use a value of 1.0 for the XFT DPI scale if the setting is <= 0
            // See:
            // https://gitlab.gnome.org/GNOME/libadwaita/-/commit/a7738a4d269bfdf4d8d5429ca73ccdd9b2450421
            // https://gitlab.gnome.org/GNOME/libadwaita/-/commit/9759d3fd81129608dd78116001928f2aed974ead
            if gtk_xft_dpi <= 0 {
                // -1 is a valid value which specifies default scale.
                // https://docs.gtk.org/gtk4/property.Settings.gtk-xft-dpi.html
                if gtk_xft_dpi != -1 {
                    log_warn_once!("gtk-xft-dpi has invalid value ({}), using default", gtk_xft_dpi);
                }
                break 'xft_scale 1.0;
            }

            // As noted above gtk-xft-dpi is multiplied by 1024, so we divide
            // by 1024, then divide by the default value (96) to derive a
            // scale. Note gtk-xft-dpi can be fractional, so we use floating
            // point math here.
            let xft_dpi = gtk_xft_dpi as f32 / 1024.0;
            xft_dpi / 96.0
        };

        let scale = gtk_scale * xft_dpi_scale;
        ContentScale { x: scale, y: scale }
    }

    pub fn get_size(&self) -> SurfaceSize {
        // By the time this is called, we should be in a widget tree.
        // This should not be called before that. We ensure this by
        // initializing the surface in `renderSurfaceResize`. This is VERY
        // important because it avoids the pty having an incorrect initial
        // size.
        self.imp().size.get()
    }

    pub fn get_cursor_pos(&self) -> CursorPos {
        self.imp().cursor_pos.get()
    }

    pub fn default_termio_env(&self) -> Result<EnvMap, crate::apprt::Error> {
        let app = Application::default();
        let mut env = if internal_os::is_flatpak() {
            EnvMap::new()
        } else {
            global::environ_map()
        };

        match app.saved_language() {
            Some(language) => {
                env.insert("LANG".to_owned(), language);
            }
            None => {
                env.remove("LANG");
            }
        }

        // Don't leak these GTK environment variables to child processes.
        for k in ["GDK_DEBUG", "GDK_DISABLE", "GSK_RENDERER"] {
            env.remove(k);
        }

        // Remove some environment variables that are set when Ghostty is
        // launched from a `.desktop` file, by D-Bus activation, or systemd.
        for k in [
            "GIO_LAUNCHED_DESKTOP_FILE",
            "GIO_LAUNCHED_DESKTOP_FILE_PID",
            "DBUS_STARTER_ADDRESS",
            "DBUS_STARTER_BUS_TYPE",
            "INVOCATION_ID",
            "JOURNAL_STREAM",
            "NOTIFY_SOCKET",
        ] {
            env.remove(k);
        }

        // Unset environment varies set by snaps if we're running in a snap.
        // This allows Ghostty to further launch additional snaps.
        if build_config::SNAP && env.get("SNAP").is_some() {
            filter_snap_paths(&mut env);
        }

        // This is a hack because it ties ourselves (optionally) to the
        // Window class. The right solution we should do is emit a signal
        // here where the handler can modify our EnvMap, but boxing the
        // EnvMap is a bit annoying so I'm punting it.
        if let Some(window) = ext::get_ancestor::<Window>(self) {
            window
                .with_winproto(|wp| wp.add_subprocess_env(&mut env))
                .map_err(|e| crate::apprt::Error(format!("{e:?}")))?;

            if window.is_quick_terminal() {
                env.insert("GHOSTTY_QUICK_TERMINAL".to_owned(), "1".to_owned());
            }
        }

        Ok(env)
    }

    pub fn clipboard_request(&self, clipboard_type: Clipboard, state: &ClipboardRequest) -> ClipboardReadResult {
        clipboard::request(self, clipboard_type, state)
    }

    pub fn set_clipboard(&self, clipboard_type: Clipboard, contents: &[ClipboardContent<'_>], confirm: bool) {
        clipboard::set(self, clipboard_type, contents, confirm);
    }

    /// Focus this surface. This properly focuses the input part of
    /// our surface.
    pub fn grab_focus(&self) {
        self.imp().render_surface.grab_focus();
    }

    pub fn send_desktop_notification(&self, title: &str, body: &str) {
        let priv_ = self.imp();

        if priv_.core_surface.get().is_none() {
            log::warn!(target: LOG, "can't send notification because there is no core surface");
            return;
        }

        priv_
            .desktop_notifications
            .borrow_mut()
            .send(priv_.focused.get(), title, body);
    }

    fn clear_desktop_notifications(&self) {
        self.imp().desktop_notifications.borrow_mut().clear();
    }

    pub fn render_device_options(&self) -> renderer::DeviceOptions {
        // build_config.renderer is OpenGL on Linux.
        let Some(gtk_native) = self.native() else { return Default::default() };
        let Some(gdk_surface) = gtk_native.surface() else { return Default::default() };
        let gdk_display = gdk_surface.display();

        let mut ptr: glib::ffi::gpointer = std::ptr::null_mut();
        if let Some(wl) = gdk_display.downcast_ref::<gdk4_wayland::WaylandDisplay>() {
            // SAFETY: plain FFI getter on a live display.
            ptr = unsafe { gdk4_wayland::ffi::gdk_wayland_display_get_egl_display(wl.as_ptr()) };
        } else if let Some(x) = gdk_display.downcast_ref::<gdk4_x11::X11Display>() {
            // SAFETY: plain FFI getter on a live display.
            ptr = unsafe { gdk4_x11::ffi::gdk_x11_display_get_egl_display(x.as_ptr()) };
        }

        match std::ptr::NonNull::new(ptr) {
            // SAFETY: GDK returns an initialized EGLDisplay (or null).
            Some(p) => renderer::DeviceOptions {
                display: Some(unsafe { crate::pkg::opengl::egl::Display::from_raw(p) }),
            },
            None => Default::default(),
        }
    }

    //---------------------------------------------------------------
    // Virtual Methods

    fn init(&self) {
        // Initialize our actions
        self.init_action_map();

        let priv_ = self.imp();

        // Initialize some private fields so they aren't undefined
        let _ = priv_
            .rt_surface
            .set(crate::apprt::gtk::wrap_surface(crate::apprt::gtk::Surface::new(self)));
        priv_.split_min_size.set(Size { width: 1, height: 1 });
        priv_.precision_scroll.set(false);
        priv_.cursor_pos.set(CursorPos { x: 0.0, y: 0.0 });
        priv_.mouse_shape.set(MouseShape::Text);
        priv_.mouse_hidden.set(false);
        priv_.focused.set(true);
        priv_.mapped.set(false);
        priv_.size.set(SurfaceSize { width: 0, height: 0 });
        priv_.hscroll_policy.set(Policy(gtk4::ScrollablePolicy::Natural));
        priv_.vscroll_policy.set(Policy(gtk4::ScrollablePolicy::Natural));

        // If our configuration is null then we get the configuration
        // from the application.
        if priv_.config.borrow().is_none() {
            let app = Application::default();
            priv_.config.replace(app.get_config());
        }

        // Setup our input method state
        priv_.in_keyevent.set(IMKeyEvent::False);
        priv_.im_composing.set(false);
        priv_.im_len.set(0);

        // Read GTK primary paste setting
        priv_
            .gtk_enable_primary_paste
            .set(gsettings::get::<gsettings::GtkEnablePrimaryPaste>().unwrap_or(true));

        // Set up to handle items being dropped on our surface. Files can be
        // dropped from Nautilus and strings can be dropped from many
        // programs. The order of these types matter.
        priv_.drop_target.set_types(&[
            gdk::FileList::static_type(),
            gio::File::static_type(),
            String::static_type(),
        ]);

        // Also have to set up the surface drop target to accept other
        // surfaces (in particular, their surface IDs)
        priv_.surface_drop_target.set_types(&[u64::static_type()]);

        // Setup properties we can't set from our Blueprint file.
        priv_.render_surface.set_cursor_from_name(Some("text"));
        priv_.drag_handle.set_cursor_from_name(Some("grab"));

        // Initialize our config
        self.prop_config();
    }

    fn init_action_map(&self) {
        let actions = [
            GtkAction::init("prompt-title", Self::action_prompt_title, None),
            GtkAction::init_stateful(
                "notify-on-next-command-finish",
                Self::action_notify_on_next_command_finish,
                None,
                false.to_variant(),
            ),
        ];

        let group = ext::actions::add_as_group(self, "surface", &actions);
        self.imp().action_group.replace(Some(group));
    }

    fn snapshot_impl(&self, snap: &gtk4::Snapshot) {
        let priv_ = self.imp();

        let blur: Option<f32> = 'blur: {
            // Native GTK blur is only available since GTK 4.23.3
            if gtk_version::runtime_until(4, 23, 3) {
                break 'blur None;
            }
            let Some(config) = priv_.config.borrow().clone() else { break 'blur None };

            let v = match config.get().background_blur {
                BackgroundBlur::Radius(v) => Some(v as f32),
                BackgroundBlur::True | BackgroundBlur::MacosGlassRegular | BackgroundBlur::MacosGlassClear => {
                    Some(20.0)
                }
                BackgroundBlur::False => None,
            };
            v
        };

        // pushCopy and appendPaste are only supported since 4.22. These two
        // functions are crucial for the blur function and it cannot be
        // implemented otherwise, so if you compile Ghostty on an older GTK
        // version (as the Rust port's gtk-rs bindings target 4.14), you
        // won't get blur. Sorry.
        if blur.is_some() && !gtk_version::at_least(4, 22, 0) {
            log_warn_once!("background-blur requires GTK 4.22, ignoring");
        }

        // Drawing the children normally is done by the caller (the parent
        // snapshot).
        let _ = snap;
    }

    //---------------------------------------------------------------
    // Properties

    /// Returns the title property without a copy.
    pub fn title_ptr(&self) -> Option<*const std::ffi::c_char> {
        self.imp().title.borrow().as_ref().map(|t| t.as_ptr())
    }

    /// Returns the title property.
    pub fn get_title(&self) -> Option<String> {
        self.imp()
            .title
            .borrow()
            .as_ref()
            .map(|t| t.to_string_lossy().into_owned())
    }

    /// Returns the effective title: the user-overridden title if set,
    /// otherwise the terminal-set title.
    pub fn get_effective_title(&self) -> Option<String> {
        let priv_ = self.imp();
        priv_.title_override.borrow().clone().or_else(|| self.get_title())
    }

    /// Copies the effective title to the clipboard.
    pub fn copy_title_to_clipboard(&self) -> bool {
        let Some(title) = self.get_effective_title() else { return false };
        if title.is_empty() {
            return false;
        }
        self.set_clipboard(
            Clipboard::Standard,
            &[ClipboardContent {
                mime: b"text/plain",
                data: title.as_bytes(),
            }],
            false,
        );
        true
    }

    /// Set the title for this surface, copies the value. This should always
    /// be the title as set by the terminal program, not any manually set
    /// title. For manually set titles see `setTitleOverride`.
    pub fn set_title(&self, title: Option<&CStr>) {
        self.imp().title.replace(title.map(|t| t.to_owned()));
        self.notify(properties::TITLE);
    }

    /// Overridden title. This will be generally be shown over the title
    /// unless this is unset (null).
    pub fn set_title_override(&self, title: Option<&str>) {
        self.imp().title_override.replace(title.map(str::to_owned));
        self.notify(properties::TITLE_OVERRIDE);
    }

    /// Returns the pwd property.
    pub fn get_pwd(&self) -> Option<String> {
        self.imp().pwd.borrow().clone()
    }

    /// Set the pwd for this surface, copies the value.
    pub fn set_pwd(&self, pwd: Option<&str>) {
        self.imp().pwd.replace(pwd.map(str::to_owned));
        self.notify(properties::PWD);
    }

    /// Returns the focus state of this surface.
    pub fn get_focused(&self) -> bool {
        self.imp().focused.get()
    }

    /// Returns true if the GLArea of this surface is mapped.
    pub fn get_mapped(&self) -> bool {
        self.imp().mapped.get()
    }

    /// The configuration of this surface (a new reference).
    pub fn get_config(&self) -> Option<Config> {
        self.imp().config.borrow().clone()
    }

    /// Change the configuration for this surface.
    pub fn set_config(&self, config: &Config) {
        self.imp().config.replace(Some(config.clone()));
        self.notify(properties::CONFIG);
    }

    /// Return the default size, if set.
    pub fn get_default_size(&self) -> Option<Size> {
        *self.imp().default_size.borrow()
    }

    /// Set the default size for a window that contains this surface.
    /// This is up to the embedding widget to respect this. Generally, only
    /// the first surface in a window respects this.
    pub fn set_default_size(&self, size: Size) {
        self.imp().default_size.replace(Some(size));
        self.notify(properties::DEFAULT_SIZE);
    }

    /// Estimate and set the initial window size from config and font
    /// metrics. This can be called before the core surface exists to set up
    /// the window size before presenting. This is an estimate because it
    /// does not take into account any padding that may need to be added to
    /// the window.
    pub fn estimate_initial_size(&self) {
        let Some(config_obj) = self.get_config() else { return };
        let config = config_obj.get();

        // Both dimensions must be configured
        if config.window_height == 0 || config.window_width == 0 {
            return;
        }

        let app = Application::default();

        // Get content scale and compute DPI
        let content_scale = self.get_content_scale();
        let x_dpi = content_scale.x * font::face::DEFAULT_DPI as f32;
        let y_dpi = content_scale.y * font::face::DEFAULT_DPI as f32;

        let font_size = DesiredSize {
            points: config.font_size,
            xdpi: x_dpi.round() as u16,
            ydpi: y_dpi.round() as u16,
        };

        // Get font grid for cell metrics
        let derived_config = font::shared_grid_set::DerivedConfig::init(&config);

        let core_app = app.core();
        let Ok((font_grid_key, font_grid)) = core_app.font_grid_set.r#ref(&derived_config, font_size) else {
            return;
        };
        let cell = font_grid.cell_size();
        core_app.font_grid_set.deref(&font_grid_key);

        let width = core_surface_mod::MIN_WINDOW_WIDTH_CELLS.max(config.window_width) * cell.width;
        let height = core_surface_mod::MIN_WINDOW_HEIGHT_CELLS.max(config.window_height) * cell.height;

        let final_width = (width as f32 / content_scale.x).ceil() as u32;
        let final_height = (height as f32 / content_scale.y).ceil() as u32;

        drop(config);
        self.set_default_size(Size {
            width: final_width,
            height: final_height,
        });
    }

    /// Get the key sequence list. Full transfer.
    fn get_key_sequence(&self) -> Option<StringList> {
        let seq = self.imp().key_sequence.borrow();
        let refs: Vec<&CStr> = seq.iter().map(|s| s.as_c_str()).collect();
        Some(*StringList::create(&refs))
    }

    /// Get the key table list. Full transfer.
    fn get_key_table(&self) -> Option<StringList> {
        let tables = self.imp().key_tables.borrow();
        let refs: Vec<&CStr> = tables.iter().map(|s| s.as_c_str()).collect();
        Some(*StringList::create(&refs))
    }

    /// Return the smallest size, in logical pixels, that this surface can
    /// be resized to within a split.
    pub fn get_split_min_size(&self) -> Size {
        self.imp().split_min_size.get()
    }

    /// Set the size of a cell, in pixels. This determines the smallest size
    /// that this surface can be resized to within a split, so that a split
    /// is never so small that none of its terminal is visible.
    pub fn set_cell_size(&self, width: u32, height: u32) {
        self.imp().cell_size.set(Some(Size { width, height }));
        self.sync_split_min_size();
    }

    /// Update the smallest size that this surface can be resized to within
    /// a split, from the cell size and the window padding.
    fn sync_split_min_size(&self) {
        let priv_ = self.imp();
        let Some(cell_size) = priv_.cell_size.get() else { return };
        let Some(config_obj) = self.get_config() else { return };

        // The cell size is in pixels but widget sizes are in logical pixels.
        let content_scale = self.get_content_scale();
        let cell_width = (cell_size.width as f32 / content_scale.x).ceil() as u32;
        let cell_height = (cell_size.height as f32 / content_scale.y).ceil() as u32;

        // Padding is in points, and is rounded down like scaledPadding in
        // the core surface.
        fn padding(points: u32) -> u32 {
            (points as f32 * font::face::DEFAULT_DPI as f32 / 72.0).floor() as u32
        }
        let (padding_x, padding_y) = {
            let config = config_obj.get();
            (config.window_padding_x, config.window_padding_y)
        };

        priv_.split_min_size.set(Size {
            width: cell_width + padding(padding_x.top_left) + padding(padding_x.bottom_right),
            height: cell_height + padding(padding_y.top_left) + padding(padding_y.bottom_right),
        });

        if let Some(window) = ext::get_ancestor::<SurfaceScrolledWindow>(self) {
            window.sync_split_min_size();
        }
    }

    /// Test hook: set the split minimum size directly.
    #[cfg(test)]
    pub fn set_split_min_size(&self, size: Size) {
        self.imp().split_min_size.set(size);
    }

    /// Return the min size, if set.
    pub fn get_min_size(&self) -> Option<Size> {
        *self.imp().min_size.borrow()
    }

    /// Set the min size for a window that contains this surface.
    /// This is up to the embedding widget to respect this. Generally, only
    /// the first surface in a window respects this.
    pub fn set_min_size(&self, size: Size) {
        self.imp().min_size.replace(Some(size));
        self.notify(properties::MIN_SIZE);
    }

    pub fn get_mouse_shape(&self) -> MouseShape {
        self.imp().mouse_shape.get()
    }

    pub fn set_mouse_shape(&self, shape: MouseShape) {
        self.imp().mouse_shape.set(shape);
        self.notify(properties::MOUSE_SHAPE);
    }

    pub fn get_mouse_hidden(&self) -> bool {
        self.imp().mouse_hidden.get()
    }

    pub fn set_mouse_hidden(&self, hidden: bool) {
        self.imp().mouse_hidden.set(hidden);
        self.notify(properties::MOUSE_HIDDEN);
    }

    pub fn set_mouse_hover_url(&self, url: Option<&str>) {
        self.imp().mouse_hover_url.replace(url.map(str::to_owned));
        self.notify(properties::MOUSE_HOVER_URL);
    }

    pub fn get_bell_ringing(&self) -> bool {
        self.imp().bell_ringing.get()
    }

    pub fn set_bell_ringing(&self, ringing: bool) {
        // Prevent duplicate change notifications if the signals we emit
        // in this function cause this state to change again.
        let _guard = self.freeze_notify();

        // Logic around bell reaction happens on every event even if we're
        // already in the ringing state.
        if ringing {
            self.ring_bell();
            // focus clears ringing state, so we should not change state to
            // ringing if we're already focused
            if self.get_focused() {
                return;
            }
        }

        // Property change only happens on actual state change
        let priv_ = self.imp();
        if priv_.bell_ringing.get() == ringing {
            return;
        }
        priv_.bell_ringing.set(ringing);
        self.notify(properties::BELL_RINGING);
    }

    pub fn set_error(&self, v: bool) {
        self.imp().error.set(v);
        self.notify(properties::ERROR);
    }

    pub fn set_search_active(&self, active: bool, needle: &str) {
        let priv_ = self.imp();
        priv_.search_overlay.set_property(search_overlay::properties::ACTIVE, active);

        if !needle.is_empty() {
            priv_.search_overlay.set_search_contents(needle);
        }

        if active {
            priv_.search_overlay.grab_focus();
        }
    }

    pub fn set_search_total(&self, total: Option<usize>) {
        self.imp().search_overlay.set_search_total(total);
    }

    pub fn set_search_selected(&self, selected: Option<usize>) {
        self.imp().search_overlay.set_search_selected(selected);
    }

    fn prop_config(&self) {
        let priv_ = self.imp();
        let Some(config_obj) = self.get_config() else { return };

        // window-padding-x, window-padding-y
        self.sync_split_min_size();

        let config = config_obj.get();

        // resize-overlay-duration
        {
            let ms = config.resize_overlay_duration.as_milliseconds();
            priv_
                .resize_overlay
                .set_property(resize_overlay::properties::DURATION, ms as u32);
        }

        // resize-overlay-position
        {
            use gtk4::Align;
            let (halign, valign) = match config.resize_overlay_position {
                ResizeOverlayPosition::Center => (Align::Center, Align::Center),
                ResizeOverlayPosition::TopLeft => (Align::Start, Align::Start),
                ResizeOverlayPosition::TopRight => (Align::End, Align::Start),
                ResizeOverlayPosition::TopCenter => (Align::Center, Align::Start),
                ResizeOverlayPosition::BottomLeft => (Align::Start, Align::End),
                ResizeOverlayPosition::BottomRight => (Align::End, Align::End),
                ResizeOverlayPosition::BottomCenter => (Align::Center, Align::End),
            };

            priv_
                .resize_overlay
                .set_property(resize_overlay::properties::OVERLAY_HALIGN, halign);
            priv_
                .resize_overlay
                .set_property(resize_overlay::properties::OVERLAY_VALIGN, valign);
        }
    }

    fn prop_error(&self) {
        let priv_ = self.imp();
        if priv_.error.get() {
            // Ensure we have an opaque background. The window will NOT set
            // this if we have transparency set and we need an opaque
            // background for the error message to be readable.
            self.add_css_class("background");
        } else {
            // Regardless of transparency setting, we remove the background
            // CSS class from this widget. Parent widgets will set it
            // appropriately (see window.zig for example).
            self.remove_css_class("background");
        }

        // We need to set our child property on an idle tick, because the
        // error property can be triggered by signals that are in the middle
        // of widget mapping and changing our child during that time
        // results in a hard gtk crash.
        if priv_.idle_rechild.borrow().is_none() {
            let weak = self.downgrade();
            let id = glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.on_idle_rechild();
                }
            });
            priv_.idle_rechild.replace(Some(id));
        }
    }

    fn on_idle_rechild(&self) {
        let priv_ = self.imp();
        priv_.idle_rechild.replace(None);
        if priv_.error.get() {
            self.set_child(Some(&*priv_.error_page));
        } else {
            self.set_child(Some(&*priv_.terminal_page));
        }
    }

    fn prop_mouse_hover_url(&self) {
        let priv_ = self.imp();
        let visible = priv_.mouse_hover_url.borrow().as_ref().is_some_and(|v| !v.is_empty());
        priv_.url_left.set_visible(visible);
    }

    fn prop_mouse_hidden(&self) {
        let priv_ = self.imp();

        // If we're hidden we set it to "none"
        if priv_.mouse_hidden.get() {
            priv_.render_surface.set_cursor_from_name(Some("none"));
            return;
        }

        // If we're not hidden we just trigger the mouse shape
        // prop notification to handle setting the proper mouse shape.
        self.prop_mouse_shape();
    }

    fn prop_mouse_shape(&self) {
        let priv_ = self.imp();

        // If our mouse should be hidden currently then we don't
        // do anything.
        if priv_.mouse_hidden.get() {
            return;
        }

        // Set our new cursor.
        let name = priv_.mouse_shape.get().cursor_name();
        priv_.render_surface.set_cursor_from_name(Some(name));
    }

    fn vadj_value_changed(&self, adj: &gtk4::Adjustment) {
        // This will trigger for every single pixel change in the
        // adjustment, but our core surface handles the noise from this so
        // that identical rows are cheap.
        let Some(core_surface) = self.core_mut() else { return };
        let row = adj.value().round() as usize;
        if let Err(err) = core_surface.perform_binding_action(BindingAction::ScrollToRow(row)) {
            log::error!(target: LOG, "error performing scroll_to_row action err={err}");
        }
    }

    fn prop_v_adjustment(&self) {
        let priv_ = self.imp();

        // When vadjustment is first set, we setup the signal group lazily.
        // This makes it so that if we don't use scrollbars, we never
        // pay the memory cost of this.
        let group = priv_.vadj_signal_group.borrow().clone();
        let group = match group {
            Some(g) => g,
            None => {
                let group = glib::SignalGroup::with_type(gtk4::Adjustment::static_type());
                let weak = self.downgrade();
                group.connect_local("value-changed", false, move |args| {
                    let adj: gtk4::Adjustment = args[0].get().ok()?;
                    if let Some(this) = weak.upgrade() {
                        this.vadj_value_changed(&adj);
                    }
                    None
                });
                priv_.vadj_signal_group.replace(Some(group.clone()));
                group
            }
        };

        // Setup our signal group target
        group.set_target(priv_.vadj.borrow().as_ref());
    }

    /// Handle bell features that need to happen every time a BEL is
    /// received. Currently this is audio and system but this could change
    /// in the future.
    fn ring_bell(&self) {
        // Emit the signal
        self.emit_by_name::<()>(signals::BELL, &[]);

        // Activate actions if they exist
        let _ = self.activate_action("tab.ring-bell", None);
        let _ = self.activate_action("win.ring-bell", None);
        let _ = self.activate_action("app.ring-bell", None);
    }

    //---------------------------------------------------------------
    // Gtk.Scrollable interface implementation

    pub fn get_h_adjustment(&self) -> Option<gtk4::Adjustment> {
        self.imp().hadj.borrow().clone()
    }

    pub fn set_h_adjustment(&self, adj: Option<gtk4::Adjustment>) {
        let _guard = self.freeze_notify();
        self.notify(properties::HADJUSTMENT);
        self.imp().hadj.replace(adj);
    }

    pub fn get_v_adjustment(&self) -> Option<gtk4::Adjustment> {
        self.imp().vadj.borrow().clone()
    }

    pub fn set_v_adjustment(&self, adj: Option<gtk4::Adjustment>) {
        let _guard = self.freeze_notify();
        self.notify(properties::VADJUSTMENT);
        self.imp().vadj.replace(adj);
    }

    //---------------------------------------------------------------
    // Signal Handlers

    pub fn action_prompt_title(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let Some(surface) = self_.core_mut() else { return };
        if let Err(err) = surface.perform_binding_action(BindingAction::PromptSurfaceTitle) {
            log::warn!(target: LOG, "unable to perform prompt title action err={err}");
        }
    }

    pub fn action_notify_on_next_command_finish(
        action: &gio::SimpleAction,
        _: Option<&glib::Variant>,
        _: &Self,
    ) {
        let state = action.state().unwrap_or_else(|| false.to_variant());
        let Some(value) = state.get::<bool>() else { return };
        action.set_state(&(!value).to_variant());
    }

    fn update_focus(&self, focused: bool) {
        let priv_ = self.imp();
        priv_.focused.set(focused);

        let ctx = &*priv_.im_context;
        if focused {
            self.update_ime_position();
            ctx.focus_in();
        } else {
            ctx.focus_out();
        }

        // The focus callback must be triggered on an idle loop source
        // because there are actions within libghostty callbacks (such as
        // showing close confirmation dialogs) that can trigger focus loss
        // and cause a deadlock because the lock may be held during the
        // callback.
        let this = self.clone();
        glib::idle_add_local_once(move || {
            let priv_ = this.imp();
            let Some(surface) = this.core_mut() else { return };
            if let Err(err) = surface.focus_callback(priv_.focused.get()) {
                log::warn!(target: LOG, "error in focus callback err={err}");
            }
        });
        self.notify(properties::FOCUSED);

        // Bell state and desktop notifications clear when we gain focus.
        if focused {
            self.set_bell_ringing(false);
            self.clear_desktop_notifications();
        }
    }

    fn gc_mouse_down(&self, gesture: &gtk4::GestureClick, x: f64, y: f64) {
        let Some(event) = gesture.current_event() else { return };

        // Bell stops ringing if any mouse button is pressed.
        self.set_bell_ringing(false);

        // Get our surface. If we don't have one, ignore this.
        let priv_ = self.imp();
        if priv_.core_surface.get().is_none() {
            return;
        }

        // If we don't have focus, grab it.
        let widget = &*priv_.render_surface;
        let had_focus = widget.has_focus();
        if !had_focus {
            widget.grab_focus();
        }

        // Report the event
        let button = translate_mouse_button(gesture.current_button());

        // If this click is only transitioning split focus, suppress it so
        // it doesn't get forwarded to the terminal as a mouse event.
        if !had_focus && button == MouseButton::Left {
            priv_.suppress_left_mouse_release.set(true);
            return;
        }

        let Some(core_surface) = self.core_mut() else { return };
        if button == MouseButton::Middle
            && !priv_.gtk_enable_primary_paste.get()
            && !core_surface.mouse_reporting_active()
        {
            return;
        }

        let consumed = {
            let gtk_mods = event.modifier_state();
            let mods = gtk_key::translate_mods(gtk_mods);
            match core_surface.mouse_button_callback(MouseButtonState::Press, button, mods) {
                Ok(v) => v,
                Err(err) => {
                    log::warn!(target: LOG, "error in key callback err={err}");
                    false
                }
            }
        };

        // If a right click isn't consumed, mouseButtonCallback selects the
        // hovered word and returns false. We can use this to handle the
        // context menu opening under normal scenarios.
        if !consumed && button == MouseButton::Right {
            self.emit_by_name::<()>(signals::MENU, &[]);

            let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            let popover = &*priv_.context_menu;
            popover.set_pointing_to(Some(&rect));
            popover.popup();
        }
    }

    fn gc_mouse_up(&self, gesture: &gtk4::GestureClick) {
        let Some(event) = gesture.current_event() else { return };

        let priv_ = self.imp();
        let Some(surface) = self.core_mut() else { return };
        let gtk_mods = event.modifier_state();
        let button = translate_mouse_button(gesture.current_button());

        if button == MouseButton::Left && priv_.suppress_left_mouse_release.get() {
            priv_.suppress_left_mouse_release.set(false);
            return;
        }

        if button == MouseButton::Middle
            && !priv_.gtk_enable_primary_paste.get()
            && !surface.mouse_reporting_active()
        {
            return;
        }

        let mods = gtk_key::translate_mods(gtk_mods);
        let consumed = match surface.mouse_button_callback(MouseButtonState::Release, button, mods) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error in key callback err={err}");
                return;
            }
        };

        // Trigger the on-screen keyboard if we have no selection,
        // and that the mouse event hasn't been intercepted by the callback.
        //
        // It's better to do this here rather than within the core callback
        // since we have direct access to the underlying gdk.Event here.
        if !consumed && button == MouseButton::Left && !surface.has_selection() {
            if !self.show_on_screen_keyboard(Some(&event)) {
                log::warn!(target: LOG, "failed to activate the on-screen keyboard");
            }
        }
    }

    fn ec_mouse_motion(&self, ec: &gtk4::EventControllerMotion, x: f64, y: f64) {
        let Some(event) = ec.current_event() else { return };
        let priv_ = self.imp();

        let (sx, sy) = self.scaled_coordinates(x, y);
        let pos = CursorPos {
            x: sx as f32,
            y: sy as f32,
        };

        // There seem to be at least two cases where GTK issues a mouse
        // motion event without the cursor actually moving:
        // 1. GLArea is resized under the mouse. This has the unfortunate
        //    side effect of causing focus to potentially change when
        //    `focus-follows-mouse` is enabled.
        // 2. The window title is updated. This can cause the mouse to unhide
        //    incorrectly when hide-mouse-when-typing is enabled.
        // To prevent incorrect behavior, we'll only grab focus and
        // continue with callback logic if the cursor has actually moved.
        let old = priv_.cursor_pos.get();
        let is_cursor_still = (old.x - pos.x).abs() < 1.0 && (old.y - pos.y).abs() < 1.0;
        if is_cursor_still {
            return;
        }

        // If we don't have focus, and we want it, grab it.
        if let Some(config) = self.get_config() {
            let widget = &*priv_.render_surface;
            if !widget.has_focus() && config.get().focus_follows_mouse {
                widget.grab_focus();
            }
        }

        // Our pos changed, update
        priv_.cursor_pos.set(pos);

        // Notify the callback
        if let Some(surface) = self.core_mut() {
            let gtk_mods = event.modifier_state();
            let mods = gtk_key::translate_mods(gtk_mods);
            if let Err(err) = surface.cursor_pos_callback(pos, Some(mods)) {
                log::warn!(target: LOG, "error in cursor pos callback err={err}");
            }
        }
    }

    fn ec_mouse_leave(&self, ec_motion: &gtk4::EventControllerMotion) {
        let Some(event) = ec_motion.current_event() else { return };

        // Get our modifiers
        if let Some(surface) = self.core_mut() {
            // If we have a core surface then we can send the cursor pos
            // callback with an invalid position to indicate the mouse left.
            let gtk_mods = event.modifier_state();
            let mods = gtk_key::translate_mods(gtk_mods);
            if let Err(err) = surface.cursor_pos_callback(CursorPos { x: -1.0, y: -1.0 }, Some(mods)) {
                log::warn!(target: LOG, "error in cursor pos callback err={err}");
            }
        }
    }

    fn ec_mouse_scroll_vertical(&self, x: f64, y: f64) -> bool {
        let priv_ = self.imp();
        let Some(surface) = self.core_mut() else { return false };

        // Multiply precision scrolls by 10 to get a better response from
        // touchpad scrolling
        let multiplier = if priv_.precision_scroll.get() { 10.0 } else { 1.0 };
        let scroll_mods = input::ScrollMods {
            precision: priv_.precision_scroll.get(),
            ..Default::default()
        };

        let (sx, sy) = self.scaled_coordinates(x, y);
        // We invert because we apply natural scrolling to the values.
        // This behavior has existed for years without Linux users
        // complaining but I suspect we'll have to make this configurable in
        // the future or read a system setting.
        if let Err(err) = surface.scroll_callback(sx * -1.0 * multiplier, sy * -1.0 * multiplier, scroll_mods) {
            log::warn!(target: LOG, "error in scroll callback err={err}");
            return false;
        }

        true
    }

    fn ec_mouse_scroll_horizontal(&self, ec: &gtk4::EventControllerScroll, x: f64) -> bool {
        let priv_ = self.imp();

        // Check if horizontal tab scrolling is enabled and this is a
        // touchpad surface scroll. If not, forward to the terminal.
        let tab_scroll_enabled = self
            .get_config()
            .map(|c| c.get().gtk_horizontal_tab_scroll)
            .unwrap_or(true);

        let is_surface_scroll = ec.unit() == gdk::ScrollUnit::Surface;

        if tab_scroll_enabled && is_surface_scroll {
            priv_
                .pending_horizontal_scroll
                .set(priv_.pending_horizontal_scroll.get() + x);

            if priv_.pending_horizontal_scroll.get().abs() < 120.0 {
                if let Some(v) = priv_.pending_horizontal_scroll_reset.take() {
                    v.remove();
                }
                let weak = self.downgrade();
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    if let Some(this) = weak.upgrade() {
                        let priv_ = this.imp();
                        priv_.pending_horizontal_scroll.set(0.0);
                        priv_.pending_horizontal_scroll_reset.replace(None);
                    }
                });
                priv_.pending_horizontal_scroll_reset.replace(Some(id));
                return true;
            }

            let _ = self.activate_action(
                if priv_.pending_horizontal_scroll.get() < 0.0 {
                    "tab.next-page"
                } else {
                    "tab.previous-page"
                },
                None,
            );

            if let Some(v) = priv_.pending_horizontal_scroll_reset.take() {
                v.remove();
            }

            priv_.pending_horizontal_scroll.set(0.0);

            return true;
        }

        // Forward horizontal scroll to the terminal (e.g. for neovim).
        let Some(surface) = self.core_mut() else { return false };
        let (sx, _) = self.scaled_coordinates(x, 0.0);
        if let Err(err) = surface.scroll_callback(sx * -1.0, 0.0, input::ScrollMods::default()) {
            log::warn!(target: LOG, "error in scroll callback err={err}");
            return false;
        }

        true
    }

    /// Tell GTK where our cursor is so that the IME can place the candidate
    /// window at the right position.
    fn update_ime_position(&self) {
        let priv_ = self.imp();
        let Some(surface) = self.core_mut() else { return };

        // GTK wants widget coordinates, so we divide by the surface scale
        // only and leave out the font DPI scale (gtk-xft-dpi).
        let surface_scale = scale_util::widget_surface_scale(&*priv_.render_surface) as f32;
        let ime_point = surface.ime_point_scaled(ContentScale {
            x: surface_scale,
            y: surface_scale,
        });

        // Snap outward to whole pixels; an empty rectangle means "unset".
        let top = (ime_point.y - ime_point.height).floor();
        let bottom = ime_point.y.ceil();
        priv_.im_context.set_cursor_location(&gdk::Rectangle::new(
            ime_point.x.floor() as i32,
            top as i32,
            1,
            (bottom - top).max(1.0) as i32,
        ));
    }

    fn im_preedit_start(&self) {
        // Start our composing state for the input method and reset our
        // input buffer to empty.
        let priv_ = self.imp();
        priv_.im_composing.set(true);
        priv_.im_len.set(0);

        self.update_ime_position();
    }

    fn im_preedit_changed(&self, ctx: &gtk4::IMMulticontext) {
        let priv_ = self.imp();

        // Any preedit change should mark that we're composing. Its possible
        // this is false using fcitx5-hangul and typing "dkssud<space>"
        // ("안녕"). The second "s" results in a "commit" for "안" which sets
        // composing to false, but then immediately sends a preedit change
        // for the next symbol. With composing set to false we won't commit
        // this text. Therefore, we must ensure it is set here.
        priv_.im_composing.set(true);

        // We can't set our preedit on our surface unless we're realized.
        // We do this now because we want to still keep our input method
        // state coherent.
        let Some(surface) = self.core_mut() else { return };

        // Get our pre-edit string that we'll use to show the user.
        let (buf, _, _) = ctx.preedit_string();

        // Update our preedit state in Ghostty core
        if let Err(err) = surface.preedit_callback(Some(buf.as_bytes())) {
            log::warn!(target: LOG, "error in preedit callback err={err}");
        }

        self.update_ime_position();
    }

    fn im_preedit_end(&self) {
        // End our composing state for GTK, allowing us to commit the text.
        let priv_ = self.imp();
        priv_.im_composing.set(false);

        // End our preedit state in Ghostty core
        let Some(surface) = self.core_mut() else { return };
        if let Err(err) = surface.preedit_callback(None) {
            log::warn!(target: LOG, "error in preedit callback err={err}");
        }
    }

    fn im_commit(&self, s: &str) {
        let priv_ = self.imp();
        let bytes = s.as_bytes();

        // We need to handle commit specially if we're in a key event.
        // Specifically, GTK will send us a commit event for basic key
        // encodings like "a" (on a US layout keyboard). We don't want
        // to treat this as IME committed text because we want to associate
        // it with a key event (i.e. "a" key press).
        match priv_.in_keyevent.get() {
            // If we're not in a key event then this commit is from
            // some other source (i.e. on-screen keyboard, tablet, etc.)
            // and we want to commit the text to the core surface.
            IMKeyEvent::False => {}

            // If we're in a composing state and in a key event then this
            // key event is resulting in a commit of multiple keypresses
            // and we don't want to encode it alongside the keypress.
            IMKeyEvent::Composing => {}

            // If we're not composing then this commit is just a normal
            // key encoding and we want our key event to handle it so
            // that Ghostty can be aware of the key event alongside
            // the text.
            IMKeyEvent::NotComposing => {
                let mut buf = priv_.im_buf.get().0;
                if bytes.len() > buf.len() {
                    log::warn!(target: LOG, "not enough buffer space for input method commit");
                    return;
                }

                // Copy our committed text to the buffer
                buf[..bytes.len()].copy_from_slice(bytes);
                priv_.im_buf.set(ImBuf(buf));
                priv_.im_len.set(bytes.len() as u8);
                return;
            }
        }

        // If we reach this point from above it means we're composing OR
        // not in a keypress. In either case, we want to commit the text
        // given to us because that's what GTK is asking us to do. If we're
        // not in a keypress it means that this commit came via a
        // non-keyboard event (i.e. on-screen keyboard, tablet of some kind,
        // etc.).

        // Committing ends composing state
        priv_.im_composing.set(false);

        // We can't set our preedit on our surface unless we're realized.
        // We do this now because we want to still keep our input method
        // state coherent.
        if let Some(surface) = self.core_mut() {
            // End our preedit state. Well-behaved input methods do this for
            // us by triggering a preedit-end event but some do not (ibus
            // 1.5.29).
            if let Err(err) = surface.preedit_callback(None) {
                log::warn!(target: LOG, "error in preedit callback err={err}");
            }

            // Send the text to the core surface, associated with no key (an
            // invalid key, which should produce no PTY encoding).
            if let Err(err) = surface.key_callback(KeyEvent {
                action: input::Action::Press,
                key: input::Key::Unidentified,
                mods: Default::default(),
                consumed_mods: Default::default(),
                composing: false,
                utf8: bytes,
                ..Default::default()
            }) {
                log::warn!(target: LOG, "error in key callback err={err}");
            }
        }
    }

    fn render_surface_realize(&self) {
        log::debug!(target: LOG, "render surface realize");

        // Notify our core surface that it should realize.
        if let Some(v) = self.core_mut() {
            if let Err(err) = v.display_realized() {
                log::warn!(target: LOG, "core displayRealized failed err={err}");
            }
            self.redraw();
        }

        // Setup our input method. We do this here because this will
        // create a strong reference back to ourself and we want to be
        // able to release that in unrealize.
        self.imp().im_context.set_client_widget(Some(self));
    }

    fn render_surface_unrealize(&self) {
        log::debug!(target: LOG, "render surface unrealize");

        if let Some(surface) = self.core_mut() {
            surface.display_unrealized();
        }

        // Unset our input method
        self.imp().im_context.set_client_widget(None::<&gtk4::Widget>);
    }

    fn update_mapped(&self, mapped: bool) {
        self.imp().mapped.set(mapped);
        self.notify(properties::MAPPED);
    }

    /// Update the core surface visibility based on both GTK widget and
    /// toplevel state. This is public so the window can call it when its
    /// suspended state changes.
    pub fn update_occlusion(&self) {
        let Some(surface) = self.core_mut() else { return };
        let visible = self.imp().mapped.get() && !self.window_suspended();
        if let Err(err) = surface.occlusion_callback(visible) {
            log::warn!(target: LOG, "error in occlusion callback err={err}");
        }
    }

    fn window_suspended(&self) -> bool {
        let Some(native) = self.native() else { return false };
        let Some(window) = native.downcast_ref::<gtk4::Window>() else { return false };
        window.is_suspended()
    }

    fn render_surface_resize(&self, width: i32, height: i32) {
        // Some debug output to help understand what GTK is telling us.
        {
            let widget = &*self.imp().render_surface;
            log::debug!(
                target: LOG,
                "gl resize width={} height={} scale={} scale_factor={}",
                width,
                height,
                scale_util::widget_surface_scale(widget),
                widget.scale_factor()
            );
        }

        // Store our cached size
        let priv_ = self.imp();

        let new_size = SurfaceSize {
            width: width as u32,
            height: height as u32,
        };
        let changed = priv_.size.get() != new_size;
        priv_.size.set(new_size);

        // If our surface is initialized, we send callbacks.
        if let Some(surface) = self.core_mut() {
            // We also update the content scale because there is no signal
            // for content scale change and it seems to trigger a resize
            // event.
            if let Err(err) = surface.content_scale_callback(self.get_content_scale()) {
                log::warn!(target: LOG, "error in content scale callback err={err}");
            }

            if changed {
                if let Some(surface) = self.core_mut() {
                    if let Err(err) = surface.size_callback(new_size) {
                        log::warn!(target: LOG, "error in size callback err={err}");
                    }
                }
                // Setup our resize overlay if configured
                self.resize_overlay_schedule();
            }
        } else {
            // If we haven't initalized a surface yet, now's the time to
            // do so. We cannot do this any earlier since GTK defers the
            // layouting and size allocation until after widget
            // initialization. Note that initialization is different from
            // realization: `initSurface` will start the core surface in an
            // unrealized state and wait for the `realize` signal, unless the
            // widget is somehow realized before the first resize signal ever
            // fires.
            if let Err(err) = self.init_surface() {
                log::warn!(target: LOG, "surface failed to initialize err={err}");
            }
        }
    }

    fn init_surface(&self) -> Result<(), InitError> {
        let priv_ = self.imp();
        debug_assert!(priv_.core_surface.get().is_none());

        let app = Application::default();
        let core_app = app.core();

        // Add ourselves to the list of surfaces on the app.
        core_app.add_surface(self.rt());

        let result = (|| -> Result<Box<CoreSurface>, InitError> {
            // Initialize our surface configuration.
            let config_obj = self.get_config().ok_or(InitError::SurfaceError)?;
            let mut config = apprt::surface::new_config(core_app, &config_obj.get(), self.parent_context());

            {
                let overrides = priv_.overrides.borrow();
                apply_command_overrides(&mut config, overrides.command.as_ref(), overrides.shell_integration);
                if let Some(wd) = &overrides.working_directory {
                    let mut wd_val = WorkingDirectory::Path(wd.clone());
                    wd_val.finalize();
                    config.working_directory = Some(wd_val);
                }
            }

            // Properties that can impact surface init
            if let Some(size) = *priv_.font_size_request.borrow() {
                config.font_size = size.0.points;
            }
            if let Some(pwd) = priv_.pwd.borrow().clone() {
                let mut wd_val = WorkingDirectory::Path(pwd);
                wd_val.finalize();
                config.working_directory = Some(wd_val);
            }

            let content_scale = self.get_content_scale();
            log::info!(
                target: LOG,
                "surface content scale: x={} y={} dpi={}x{} font_size={}",
                content_scale.x,
                content_scale.y,
                (content_scale.x * font::face::DEFAULT_DPI as f32).round(),
                (content_scale.y * font::face::DEFAULT_DPI as f32).round(),
                config.font_size
            );

            // Initialize the surface
            CoreSurface::init(&config, core_app as *mut _, app.rt(), self.rt()).map_err(|err| {
                log::warn!(target: LOG, "failed to initialize surface err={err}");
                InitError::SurfaceError
            })
        })();

        let surface = match result {
            Ok(s) => s,
            Err(err) => {
                core_app.delete_surface(self.rt());
                return Err(err);
            }
        };

        // Store it!
        let surface = Box::into_raw(surface);
        priv_.core_surface.set(Some(surface));
        // SAFETY: just created.
        let id = unsafe { (*surface).id };
        priv_.desktop_notifications.replace(DesktopNotifications::init(id));

        // Give the render surface a pointer to the core surface so it
        // can pull presents from the renderer in its snapshot handler.
        priv_.render_surface.set_core_surface(Some(surface));

        // If the widget isn't realized yet, start the renderer in an
        // unrealized state so it waits for `displayRealized` (called from
        // `renderSurfaceRealize`) before building GPU resources.
        if !priv_.render_surface.is_realized() {
            if let Some(s) = self.core_mut() {
                s.display_unrealized();
            }
        }

        // Emit the signal that we initialized the surface.
        self.emit_by_name::<()>(signals::INIT, &[]);

        self.update_focus(priv_.focused.get());
        Ok(())
    }

    fn resize_overlay_schedule(&self) {
        let priv_ = self.imp();
        let Some(surface) = self.core_mut() else { return };

        // Only show the resize overlay if its enabled
        let Some(config) = self.get_config() else { return };
        match config.get().resize_overlay {
            ResizeOverlayMode::Always | ResizeOverlayMode::AfterFirst => {}
            ResizeOverlayMode::Never => return,
        }

        // If we have resize overlays enabled, setup an idler
        // to show that. We do this in an idle tick because doing it
        // during the resize results in flickering.
        let grid_size = surface.size.grid();
        let text = format!("{} x {}", grid_size.columns, grid_size.rows);
        priv_.resize_overlay.set_label(Some(&text));
        priv_.resize_overlay.schedule();
    }

    fn search_stop(&self) {
        let Some(surface) = self.core_mut() else { return };
        if let Err(err) = surface.perform_binding_action(BindingAction::EndSearch) {
            log::warn!(target: LOG, "unable to perform end_search action err={err}");
        }
        self.imp().render_surface.grab_focus();
    }

    fn search_changed(&self, needle: Option<&str>) {
        let Some(surface) = self.core_mut() else { return };
        let needle = needle.unwrap_or("").as_bytes().to_vec();
        if let Err(err) = surface.perform_binding_action(BindingAction::Search(needle)) {
            log::warn!(target: LOG, "unable to perform search action err={err}");
        }
    }

    fn search_navigate(&self, to: NavigateSearch) {
        let Some(surface) = self.core_mut() else { return };
        if let Err(err) = surface.perform_binding_action(BindingAction::NavigateSearch(to)) {
            log::warn!(target: LOG, "unable to perform navigate_search action err={err}");
        }
    }

    fn surface_drag_prepare(&self) -> gdk::ContentProvider {
        // TODO: Use a static content provider once we make `self.core()`
        // available immediately when constructing the surface widget
        let id = self.core_mut().map(|c| c.id).unwrap_or(0);
        gdk::ContentProvider::for_value(&id.to_value())
    }

    fn surface_drag_begin(&self, src: &gtk4::DragSource) {
        // The scale of the preview
        let preview_scale: f32 = 0.2;

        // Snapshot the entire surface widget as the icon preview
        let paintable = gtk4::WidgetPaintable::new(Some(self));

        // Center the preview
        let width = self.width();
        let height = self.height();
        let mid_x = width as f32 / 2.0;
        let mid_y = height as f32 / 2.0;

        // Create a snapshot to render the scaled paintable
        let snap = gtk4::Snapshot::new();
        snap.scale(preview_scale, preview_scale);
        paintable.snapshot(&snap, width as f64, height as f64);

        if let Some(scaled) = snap.to_paintable(None) {
            src.set_icon(
                Some(&scaled),
                (mid_x * preview_scale) as i32,
                (mid_y * preview_scale) as i32,
            );
        } else {
            // The scaling process somehow failed.
            // Use the original paintable as a fail-safe
            log::warn!(target: LOG, "preview scaling failed - falling back to original paintable");
            src.set_icon(Some(&paintable), mid_x as i32, mid_y as i32);
        }
    }

    fn surface_drop(&self, value: &glib::Value, x: f64, y: f64) -> bool {
        // Clean up overlay state
        struct Cleanup<'a>(&'a Surface);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                self.0.set_drop_overlay_direction(None);
            }
        }
        let _cleanup = Cleanup(self);

        let Ok(dropped_id) = value.get::<u64>() else { return false };
        let Some(core) = self.core_mut() else { return false };
        // SAFETY: the app outlives the surface.
        let Some(dropped) = (unsafe { &*core.app }).find_surface_by_id(dropped_id) else {
            return false;
        };
        // SAFETY: find_surface_by_id returns live GTK core surfaces.
        let from = unsafe { crate::apprt::gtk::gobj_of(dropped) };

        let Some(st) = ext::get_ancestor::<SplitTree>(self) else {
            log::warn!(target: LOG, "surface is not placed in a split tree");
            return false;
        };

        let dir = self.calc_drop_direction(x, y);

        // The only error that could happen here is an OOM,
        // and in that case we're already milliseconds away from crashing, so...
        st.move_split(&from, self, dir).is_ok()
    }

    fn prop_drop_value(&self, tgt: &gtk4::DropTarget) {
        // Reject the drop if we're dropping a surface onto itself.
        // Note that we cannot implement this via the `accept` signal,
        // since the decision of whether to accept or deny a drop is
        // dependent on the payload (i.e. the surface being dropped). This is
        // well-documented in GTK docs.
        let Some(core_surface) = self.core_mut() else { return };
        let Some(value) = tgt.value() else { return };
        let Ok(surface_id) = value.get::<u64>() else { return };
        if core_surface.id == surface_id {
            tgt.reject();
        }
    }

    fn set_drop_overlay_direction(&self, dir: Option<dst::Direction>) {
        let priv_ = self.imp();
        for c in ["drop-top", "drop-left", "drop-right", "drop-bottom"] {
            priv_.drop_overlay.remove_css_class(c);
        }

        if let Some(d) = dir {
            priv_.drop_overlay.add_css_class(match d {
                dst::Direction::Up => "drop-top",
                dst::Direction::Left => "drop-left",
                dst::Direction::Right => "drop-right",
                dst::Direction::Down => "drop-bottom",
            });
        }
    }

    fn calc_drop_direction(&self, x: f64, y: f64) -> dst::Direction {
        calc_drop_direction(self.width() as f64, self.height() as f64, x, y)
    }

    fn dt_drop(&self, value: &glib::Value) -> bool {
        if let Ok(list) = value.get::<gdk::FileList>() {
            let mut out = internal_os::ShellEscapeWriter::init(String::new());
            for file in list.files() {
                let Some(path) = file.path() else { continue };
                use std::fmt::Write as _;
                if let Err(err) = write!(out, "{}", path.display()) {
                    log::error!(target: LOG, "unable to write path to buffer: {err}");
                    continue;
                }
                if let Err(err) = out.write_str("\n") {
                    log::error!(target: LOG, "unable to write to buffer: {err}");
                    continue;
                }
            }

            let string = out.into_inner();
            clipboard::paste(self, string.as_bytes());
            return true;
        }

        if let Ok(file) = value.get::<gio::File>() {
            let Some(path) = file.path() else { return false };
            let mut out = internal_os::ShellEscapeWriter::init(String::new());
            use std::fmt::Write as _;
            if let Err(err) = write!(out, "{}", path.display()) {
                log::error!(target: LOG, "unable to write path to buffer: {err}");
                return false;
            }
            if let Err(err) = out.write_str("\n") {
                log::error!(target: LOG, "unable to write to buffer: {err}");
                return false;
            }
            // Zig builds the string but (a long-standing upstream quirk)
            // never pastes it.
            let _string = out.into_inner();
            return true;
        }

        if let Ok(Some(string)) = value.get::<Option<String>>() {
            clipboard::paste(self, string.as_bytes());
            return true;
        }

        true
    }

}

/// Errors from `init_surface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    SurfaceError,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// Filter out environment variables that start with forbidden prefixes.
fn filter_snap_paths(env_map: &mut EnvMap) {
    let snap_vars = ["SNAP", "SNAP_USER_COMMON", "SNAP_USER_DATA", "SNAP_DATA", "SNAP_COMMON"];

    let mut env_to_remove: Vec<String> = Vec::new();
    let mut env_to_update: Vec<(String, String)> = Vec::new();

    for (key, value) in env_map.iter() {
        // Ignore fields we set ourself
        if key == "TERMINFO" {
            continue;
        }
        if key.starts_with("GHOSTTY") {
            continue;
        }

        // Any env var starting with SNAP must be removed
        if key.starts_with("SNAP_") {
            env_to_remove.push(key.to_owned());
            continue;
        }

        let mut filtered_paths: Vec<&str> = Vec::new();
        let mut modified = false;
        for path in value.split(':') {
            let mut include = true;
            for k in snap_vars {
                if let Some(snap_path) = env_map.get(k) {
                    if snap_path.is_empty() {
                        continue;
                    }
                    if path.starts_with(snap_path.as_str()) {
                        include = false;
                        modified = true;
                        break;
                    }
                }
            }
            if include {
                filtered_paths.push(path);
            }
        }

        if modified {
            if !filtered_paths.is_empty() {
                env_to_update.push((key.to_owned(), filtered_paths.join(":")));
            } else {
                env_to_remove.push(key.to_owned());
            }
        }
    }

    for (k, v) in env_to_update {
        env_map.insert(k, v);
    }
    for k in env_to_remove {
        env_map.remove(&k);
    }
}

fn calc_drop_direction(width: f64, height: f64, x: f64, y: f64) -> dst::Direction {
    let l_dist = x / width;
    let t_dist = y / height;
    let r_dist = 1.0 - l_dist;
    let b_dist = 1.0 - t_dist;
    let min = l_dist.min(t_dist).min(r_dist).min(b_dist);

    if min == l_dist {
        return dst::Direction::Left;
    }
    if min == r_dist {
        return dst::Direction::Right;
    }
    if min == t_dist {
        return dst::Direction::Up;
    }
    dst::Direction::Down
}

fn translate_mouse_button(button: u32) -> MouseButton {
    match button {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        4 => MouseButton::Four,
        5 => MouseButton::Five,
        6 => MouseButton::Six,
        7 => MouseButton::Seven,
        8 => MouseButton::Eight,
        9 => MouseButton::Nine,
        10 => MouseButton::Ten,
        11 => MouseButton::Eleven,
        _ => MouseButton::Unknown,
    }
}

/// Callback used to determine whether border should be shown around the
/// surface.
fn closure_should_border_be_shown(config: Option<&Config>, bell_ringing: bool) -> bool {
    // If the bell isn't ringing exit early because when the surface is
    // first created there's a race between this code being run and the
    // config being set on the surface. That way we don't overwhelm people
    // with the warning that we issue if the config isn't set and overwhelm
    // ourselves with large numbers of bug reports.
    if !bell_ringing {
        return false;
    }

    let Some(config) = config else {
        log::warn!(target: LOG, "config unavailable for computing whether border should be shown, likely bug");
        return false;
    };

    config.get().bell_features.border
}

/// Callback used to determine whether unfocused-split-fill /
/// unfocused-split-opacity should be applied to the surface
fn closure_should_unfocused_split_be_shown(search_active: bool, focused: bool, is_split: bool) -> bool {
    !search_active && !focused && is_split
}

fn closure_should_drag_handle_be_shown(config: Option<&Config>, is_split: bool) -> bool {
    let Some(config) = config else { return false };
    match config.get().drag_handle {
        DragHandle::Always => true,
        DragHandle::Auto => is_split,
        DragHandle::Never => false,
    }
}

fn bind_template_callbacks(klass: &mut <imp::Surface as ObjectSubclass>::Class) {
    use class::bind_template_callback as cb;

    fn this(args: &class::TemplateArgs<'_>) -> Surface {
        args.template()
    }

    cb(klass, "focus_enter", |args| {
        this(args).update_focus(true);
        None
    });
    cb(klass, "focus_leave", |args| {
        this(args).update_focus(false);
        None
    });
    cb(klass, "key_pressed", |args| {
        let ec: gtk4::EventControllerKey = args.get(0);
        let r = this(args).key_event(
            input::Action::Press,
            &ec,
            args.get(1),
            args.get(2),
            args.get(3),
        );
        Some(r.to_value())
    });
    cb(klass, "key_released", |args| {
        let ec: gtk4::EventControllerKey = args.get(0);
        this(args).key_event(
            input::Action::Release,
            &ec,
            args.get(1),
            args.get(2),
            args.get(3),
        );
        None
    });
    cb(klass, "mouse_down", |args| {
        let g: gtk4::GestureClick = args.get(0);
        this(args).gc_mouse_down(&g, args.get(2), args.get(3));
        None
    });
    cb(klass, "mouse_up", |args| {
        let g: gtk4::GestureClick = args.get(0);
        this(args).gc_mouse_up(&g);
        None
    });
    cb(klass, "mouse_motion", |args| {
        let ec: gtk4::EventControllerMotion = args.get(0);
        this(args).ec_mouse_motion(&ec, args.get(1), args.get(2));
        None
    });
    cb(klass, "mouse_leave", |args| {
        let ec: gtk4::EventControllerMotion = args.get(0);
        this(args).ec_mouse_leave(&ec);
        None
    });
    cb(klass, "scroll_vertical", |args| {
        Some(this(args).ec_mouse_scroll_vertical(args.get(1), args.get(2)).to_value())
    });
    cb(klass, "scroll_vertical_begin", |args| {
        this(args).imp().precision_scroll.set(true);
        None
    });
    cb(klass, "scroll_vertical_end", |args| {
        this(args).imp().precision_scroll.set(false);
        None
    });
    cb(klass, "scroll_horizontal", |args| {
        let ec: gtk4::EventControllerScroll = args.get(0);
        Some(this(args).ec_mouse_scroll_horizontal(&ec, args.get(1)).to_value())
    });
    cb(klass, "drop", |args| {
        let value: glib::Value = args.get(1);
        Some(this(args).dt_drop(&value).to_value())
    });
    cb(klass, "render_surface_realize", |args| {
        this(args).render_surface_realize();
        None
    });
    cb(klass, "render_surface_unrealize", |args| {
        this(args).render_surface_unrealize();
        None
    });
    cb(klass, "render_surface_map", |args| {
        let s = this(args);
        s.update_mapped(true);
        s.update_occlusion();
        None
    });
    cb(klass, "render_surface_unmap", |args| {
        let s = this(args);
        s.update_mapped(false);
        s.update_occlusion();
        None
    });
    cb(klass, "render_surface_resize", |args| {
        this(args).render_surface_resize(args.get(1), args.get(2));
        None
    });
    cb(klass, "im_preedit_start", |args| {
        this(args).im_preedit_start();
        None
    });
    cb(klass, "im_preedit_changed", |args| {
        let ctx: gtk4::IMMulticontext = args.get(0);
        this(args).im_preedit_changed(&ctx);
        None
    });
    cb(klass, "im_preedit_end", |args| {
        this(args).im_preedit_end();
        None
    });
    cb(klass, "im_commit", |args| {
        let s: String = args.get(1);
        this(args).im_commit(&s);
        None
    });
    cb(klass, "url_mouse_enter", |args| {
        this(args).imp().url_right.set_visible(true);
        None
    });
    cb(klass, "url_mouse_leave", |args| {
        this(args).imp().url_right.set_visible(false);
        None
    });
    cb(klass, "child_exited_close", |args| {
        // This closes the surface with no confirmation.
        this(args).close();
        None
    });
    cb(klass, "context_menu_closed", |args| {
        // When the context menu closes, it moves focus back to the tab
        // bar if there are tabs. That's not correct. We need to grab it
        // on the surface.
        this(args).grab_focus();
        None
    });
    cb(klass, "notify_config", |args| {
        this(args).prop_config();
        None
    });
    cb(klass, "notify_error", |args| {
        this(args).prop_error();
        None
    });
    cb(klass, "notify_mouse_hover_url", |args| {
        this(args).prop_mouse_hover_url();
        None
    });
    cb(klass, "notify_mouse_hidden", |args| {
        this(args).prop_mouse_hidden();
        None
    });
    cb(klass, "notify_mouse_shape", |args| {
        this(args).prop_mouse_shape();
        None
    });
    cb(klass, "notify_vadjustment", |args| {
        this(args).prop_v_adjustment();
        None
    });
    cb(klass, "should_border_be_shown", |args| {
        let config: Option<Config> = args.get(1);
        let bell: bool = args.get(2);
        Some(closure_should_border_be_shown(config.as_ref(), bell).to_value())
    });
    cb(klass, "should_unfocused_split_be_shown", |args| {
        Some(closure_should_unfocused_split_be_shown(args.get(1), args.get(2), args.get(3)).to_value())
    });
    cb(klass, "search_stop", |args| {
        this(args).search_stop();
        None
    });
    cb(klass, "search_changed", |args| {
        let needle: Option<String> = args.get(1);
        this(args).search_changed(needle.as_deref());
        None
    });
    cb(klass, "search_next_match", |args| {
        this(args).search_navigate(NavigateSearch::Next);
        None
    });
    cb(klass, "search_previous_match", |args| {
        this(args).search_navigate(NavigateSearch::Previous);
        None
    });
    cb(klass, "should_drag_handle_be_shown", |args| {
        let config: Option<Config> = args.get(1);
        Some(closure_should_drag_handle_be_shown(config.as_ref(), args.get(2)).to_value())
    });
    cb(klass, "surface_drag_prepare", |args| Some(this(args).surface_drag_prepare().to_value()));
    cb(klass, "surface_drag_begin", |args| {
        let src: gtk4::DragSource = args.get(0);
        this(args).surface_drag_begin(&src);
        None
    });
    cb(klass, "surface_drop", |args| {
        let value: glib::Value = args.get(1);
        Some(this(args).surface_drop(&value, args.get(2), args.get(3)).to_value())
    });
    cb(klass, "surface_drop_leave", |args| {
        // Hide overlay
        this(args).set_drop_overlay_direction(None);
        None
    });
    cb(klass, "surface_drop_motion", |args| {
        // Recalculate the drop region
        let s = this(args);
        let dir = s.calc_drop_direction(args.get(1), args.get(2));
        s.set_drop_overlay_direction(Some(dir));
        Some(gdk::DragAction::MOVE.to_value())
    });
    cb(klass, "notify_drop_value", |args| {
        let tgt: gtk4::DropTarget = args.get(0);
        this(args).prop_drop_value(&tgt);
        None
    });
}

/// A namespace for our clipboard-related functions so Surface isn't SO
/// large.
mod clipboard {
    use super::*;

    /// Set the clipboard contents.
    pub fn set(self_: &Surface, clipboard_type: Clipboard, contents: &[ClipboardContent<'_>], confirm: bool) {
        let priv_ = self_.imp();

        // Grab our plaintext content for use in confirmation dialogs
        // and signals. Kitty clipboard protocol writes may carry no
        // text representation at all.
        let text: &[u8] = contents
            .iter()
            .find(|c| terminal::clipboard::is_text_mime(c.mime))
            .map(|c| c.data)
            .unwrap_or(b"");

        // If no confirmation is necessary, set the clipboard.
        if !confirm {
            let Some(clipboard) = get(priv_.render_surface.upcast_ref(), clipboard_type) else {
                return;
            };

            let mut providers: Vec<gdk::ContentProvider> = Vec::with_capacity(contents.len());
            for content in contents {
                let bytes = glib::Bytes::from(content.data);
                if content.mime == b"text/plain" {
                    // Add an explicit UTF-8 encoding parameter to the
                    // text/plain type. The default charset when there is
                    // none is ASCII, and lots of things look for UTF-8
                    // specifically.
                    // The specs are not clear about the order here, but
                    // some clients apparently pick the first match in the
                    // order we set here then garble up bare 'text/plain'
                    // with non-ASCII UTF-8 content, so offer UTF-8 first.
                    //
                    // Note that under X11, GTK automatically adds the
                    // UTF8_STRING atom when this is present.
                    let text_providers: Vec<gdk::ContentProvider> = ["text/plain;charset=utf-8", "text/plain"]
                        .iter()
                        .map(|atom| gdk::ContentProvider::for_bytes(atom, &bytes))
                        .collect();
                    providers.push(gdk::ContentProvider::new_union(&text_providers));
                } else {
                    let mime = String::from_utf8_lossy(content.mime);
                    providers.push(gdk::ContentProvider::for_bytes(&mime, &bytes));
                }
            }

            let all = gdk::ContentProvider::new_union(&providers);
            if clipboard.set_content(Some(&all)).is_err() {
                // If we fail to set the content, we can at least set the
                // text content.
                clipboard.set_text(&String::from_utf8_lossy(text));
            }

            // A write that carries no non-empty representation clears
            // the clipboard.
            let cleared = contents.iter().all(|c| c.data.is_empty());

            self_.emit_by_name::<()>(
                signals::CLIPBOARD_WRITE,
                &[
                    &ClipboardType::from(clipboard_type),
                    &String::from_utf8_lossy(text).into_owned(),
                    &cleared,
                ],
            );

            return;
        }

        show_clipboard_confirmation(
            self_,
            &ClipboardRequest::Osc52Write(clipboard_type),
            &[ClipboardContent {
                mime: b"text/plain",
                data: text,
            }],
        );
    }

    /// Request data from the clipboard (read the clipboard). A started
    /// request completes asynchronously and will call the
    /// `completeClipboardRequest` core surface API when done.
    pub fn request(self_: &Surface, clipboard_type: Clipboard, state: &ClipboardRequest) -> ClipboardReadResult {
        // Get our requested clipboard
        let Some(clipboard) = get(self_.imp().render_surface.upcast_ref(), clipboard_type) else {
            return ClipboardReadResult::Unsupported;
        };

        match state {
            // Kitty clipboard protocol writes carry their own committed
            // contents and read nothing from the clipboard, so the
            // request completes immediately. A completion that requires
            // confirmation diverts into the confirmation dialog flow.
            ClipboardRequest::KittyWrite(_) => {
                complete_kitty_write(self_, state);
                return ClipboardReadResult::Started;
            }

            // A mode 5522 paste event only needs the listing of
            // available MIME types, which the advertised content
            // formats provide synchronously.
            ClipboardRequest::List(_) => {
                complete_list(self_, &clipboard, state);
                return ClipboardReadResult::Started;
            }

            // Kitty clipboard protocol reads gather each requested
            // representation from the clipboard asynchronously.
            ClipboardRequest::KittyRead(_) => {
                return kitty_read_start(self_, &clipboard, state.clone(), false, false);
            }

            ClipboardRequest::Paste(_) | ClipboardRequest::Osc52Read(_) | ClipboardRequest::Osc52Write(_) => {}
        }

        // For paste requests, check if clipboard has text format available.
        // This is a synchronous check that allows performable keybinds to
        // pass through when the clipboard contains non-text content (e.g.,
        // images).
        if matches!(state, ClipboardRequest::Paste(_)) {
            let formats = clipboard.formats();
            if !formats.contains_type(String::static_type()) {
                log::debug!(target: LOG, "clipboard has no text format, not starting paste request");
                return ClipboardReadResult::Unavailable;
            }
        }

        // Important: we ref self here so that we can't free memory
        // while we have an outstanding clipboard read.
        let this = self_.clone();
        let req = state.clone();
        clipboard.read_text_async(None::<&gio::Cancellable>, move |res| {
            clipboard_read_text(this, req, res);
        });

        ClipboardReadResult::Started
    }

    /// Complete a MIME type listing request (a mode 5522 paste event).
    /// The listing reads no clipboard data; it only reports the types
    /// the clipboard advertises.
    fn complete_list(self_: &Surface, clipboard: &gdk::Clipboard, state: &ClipboardRequest) {
        let Some(surface) = self_.core_mut() else { return };

        let mimes = available_mimes(clipboard);
        let refs: Vec<&[u8]> = mimes.iter().map(|m| m.as_bytes()).collect();
        if let Err(err) = surface.complete_clipboard_request(
            state,
            CompleteClipboard {
                available: &refs,
                ..Default::default()
            },
        ) {
            log::warn!(target: LOG, "failed to complete clipboard request err={err}");
        }
    }

    /// Complete a committed Kitty clipboard protocol write. The
    /// authoritative contents live in the request itself so the apprt
    /// contributes nothing, but a completion that requires confirmation
    /// diverts into the confirmation dialog flow, which keeps the
    /// request state alive for the asynchronous prompt.
    fn complete_kitty_write(self_: &Surface, state: &ClipboardRequest) {
        let ClipboardRequest::KittyWrite(kitty) = state else { unreachable!() };
        let Some(surface) = self_.core_mut() else { return };

        match surface.complete_clipboard_request(state, CompleteClipboard::default()) {
            Ok(()) => {}
            Err(core_surface_mod::Error::UnauthorizedPaste) => {
                show_clipboard_confirmation(self_, state, &kitty.contents);
            }
            Err(err) => log::warn!(target: LOG, "failed to complete clipboard request err={err}"),
        }
    }

    /// Collect the MIME types the clipboard advertises, capped at the
    /// Kitty listing limit.
    fn available_mimes(clipboard: &gdk::Clipboard) -> Vec<glib::GString> {
        let mut mimes = clipboard.formats().mime_types();
        mimes.truncate(kitty_clipboard::MAX_LISTING_MIMES);
        mimes
    }

    /// State for one in-flight Kitty clipboard protocol read against
    /// the GDK clipboard. The requested representations are read one
    /// at a time; representations the clipboard can't serve are
    /// skipped, which is how the protocol reports an unavailable
    /// representation.
    struct KittyReadOp {
        /// Reffed so the surface can't be disposed mid-operation.
        self_: Surface,

        /// The clipboard being read. Owned by the GDK display, which
        /// outlives the operation.
        clipboard: gdk::Clipboard,

        /// The request being served. Always `.kitty_read`.
        state: ClipboardRequest,

        /// The representations read so far (MIME index, data). Bounded by
        /// the request's MIME types, which the sender capped at
        /// max_read_mimes.
        contents: Vec<(usize, Vec<u8>)>,

        /// Index of the requested MIME type currently being read.
        idx: usize,

        /// Confirmation state forwarded to the completion. Set when the
        /// read is rerun after the user confirmed the request.
        confirmed: bool,
        remember: bool,
    }

    impl KittyReadOp {
        fn kitty(&self) -> &crate::apprt::structs::KittyRead {
            match &self.state {
                ClipboardRequest::KittyRead(k) => k,
                _ => unreachable!(),
            }
        }
    }

    /// Start a Kitty clipboard protocol read. The operation owns a
    /// surface reference until it completes.
    pub(super) fn kitty_read_start(
        self_: &Surface,
        clipboard: &gdk::Clipboard,
        state: ClipboardRequest,
        confirmed: bool,
        remember: bool,
    ) -> ClipboardReadResult {
        let op = Box::new(KittyReadOp {
            self_: self_.clone(),
            clipboard: clipboard.clone(),
            state,
            contents: Vec::new(),
            idx: 0,
            confirmed,
            remember,
        });
        kitty_read_next(op);
        ClipboardReadResult::Started
    }

    /// Start reading the next requested representation, or complete
    /// the request once they have all been read.
    fn kitty_read_next(op: Box<KittyReadOp>) {
        let kitty = op.kitty();
        if op.idx >= kitty.mimes.len() {
            kitty_read_finish(op);
            return;
        }

        let mime = kitty.mimes[op.idx].clone();
        let clipboard = op.clipboard.clone();

        // Text-like types are served through GDK's text reading so
        // they pick up the same conversions ordinary pastes use.
        if terminal::clipboard::is_text_mime(mime.to_bytes()) {
            clipboard.read_text_async(None::<&gio::Cancellable>, move |res| {
                let mut op = op;
                match res {
                    Ok(Some(text)) => kitty_read_store(&mut op, text.as_bytes()),
                    Ok(None) => {}
                    // An unavailable representation is expected: it is
                    // simply never served.
                    Err(err) => log::debug!(target: LOG, "failed to read clipboard text err={}", err.message()),
                }
                op.idx += 1;
                kitty_read_next(op);
            });
            return;
        }

        let mime_str = mime.to_string_lossy().into_owned();
        clipboard.read_async(&[mime_str.as_str()], glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |res| {
            let op = op;
            let stream = match res {
                Ok((stream, _)) => stream,
                Err(err) => {
                    // An unavailable representation is expected: it is
                    // simply never served.
                    log::debug!(target: LOG, "failed to read clipboard representation err={}", err.message());
                    let mut op = op;
                    op.idx += 1;
                    kitty_read_next(op);
                    return;
                }
            };

            // Splice the representation into memory.
            let output = gio::MemoryOutputStream::new_resizable();
            let output2 = output.clone();
            output.splice_async(
                &stream,
                gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                glib::Priority::DEFAULT,
                None::<&gio::Cancellable>,
                move |res| {
                    let mut op = op;
                    match res {
                        Ok(_) => {
                            // The splice closed the stream, so the bytes are
                            // complete.
                            let bytes = output2.steal_as_bytes();
                            kitty_read_store(&mut op, &bytes);
                        }
                        Err(err) => log::debug!(
                            target: LOG,
                            "failed to read clipboard representation err={}",
                            err.message()
                        ),
                    }
                    op.idx += 1;
                    kitty_read_next(op);
                },
            );
        });
    }

    /// Record data for the representation currently being read, under
    /// the MIME type the program requested.
    fn kitty_read_store(op: &mut KittyReadOp, data: &[u8]) {
        let idx = op.idx;
        op.contents.push((idx, data.to_vec()));
    }

    /// Complete the read request with everything gathered. A completion
    /// that requires confirmation keeps the request state alive and
    /// diverts into the confirmation dialog flow; the read is rerun
    /// with the confirmation attached if the user allows it.
    fn kitty_read_finish(op: Box<KittyReadOp>) {
        let self_ = op.self_.clone();
        let kitty = op.kitty();

        let Some(surface) = self_.core_mut() else { return };

        // Gather the available type listing when it was requested. The
        // listing is available synchronously from the advertised
        // content formats.
        let available_owned: Vec<glib::GString> =
            if kitty.list { available_mimes(&op.clipboard) } else { Vec::new() };
        let available: Vec<&[u8]> = available_owned.iter().map(|m| m.as_bytes()).collect();

        let contents: Vec<terminal::clipboard::Content<'_>> = op
            .contents
            .iter()
            .map(|(i, data)| terminal::clipboard::Content {
                mime: kitty.mimes[*i].to_bytes(),
                data,
            })
            .collect();

        match surface.complete_clipboard_request(
            &op.state,
            CompleteClipboard {
                contents: &contents,
                available: &available,
                confirmed: op.confirmed,
                remember: op.remember,
            },
        ) {
            Ok(()) => {}
            Err(core_surface_mod::Error::UnauthorizedPaste) => {
                show_clipboard_confirmation(&self_, &op.state, &contents);
                return;
            }
            Err(err) => {
                log::warn!(target: LOG, "failed to complete clipboard request err={err}");
                return;
            }
        }

        self_.emit_by_name::<()>(signals::CLIPBOARD_READ, &[]);
    }

    /// Paste explicit text directly into the surface, regardless of the
    /// actual clipboard contents.
    pub fn paste(self_: &Surface, text: &[u8]) {
        if text.is_empty() {
            return;
        }

        let Some(surface) = self_.core_mut() else { return };
        let req = ClipboardRequest::Paste(Clipboard::Standard);
        let content = [terminal::clipboard::Content {
            mime: b"text/plain",
            data: text,
        }];
        match surface.complete_clipboard_request(
            &req,
            CompleteClipboard {
                contents: &content,
                ..Default::default()
            },
        ) {
            Ok(()) => {}
            Err(core_surface_mod::Error::UnsafePaste | core_surface_mod::Error::UnauthorizedPaste) => {
                show_clipboard_confirmation(self_, &req, &content);
            }
            Err(err) => log::warn!(target: LOG, "failed to complete clipboard request err={err}"),
        }
    }

    /// Get the specific type of clipboard for a widget.
    pub fn get(widget: &gtk4::Widget, clipboard: Clipboard) -> Option<gdk::Clipboard> {
        Some(match clipboard {
            Clipboard::Standard => widget.clipboard(),
            Clipboard::Selection | Clipboard::Primary => widget.primary_clipboard(),
        })
    }

    /// Prompt for a clipboard request, previewing every representation
    /// it carries.
    fn show_clipboard_confirmation<P: ClipboardPart>(self_: &Surface, req: &ClipboardRequest, contents: &[P]) {
        let can_remember = match req {
            ClipboardRequest::Osc52Read(_) | ClipboardRequest::Osc52Write(_) => true,

            // Kitty clipboard protocol requests can only be
            // remembered as a session grant, which requires a
            // session password.
            ClipboardRequest::KittyRead(k) => !k.pw.is_empty(),
            ClipboardRequest::KittyWrite(k) => !k.pw.is_empty(),

            ClipboardRequest::Paste(_) | ClipboardRequest::List(_) => false,
        };
        let dialog: ClipboardConfirmationDialog = glib::Object::builder()
            .property(
                clipboard_confirmation_dialog::properties::REQUEST,
                boxed::ClipboardRequest(req.clone()),
            )
            .property(clipboard_confirmation_dialog::properties::CAN_REMEMBER, can_remember)
            .build();

        dialog.set_parts(contents);

        let weak = self_.downgrade();
        dialog.connect_confirm(move |dialog, remember| {
            if let Some(this) = weak.upgrade() {
                clipboard_confirmation_confirm(dialog, remember, &this);
            }
        });
        let weak = self_.downgrade();
        dialog.connect_deny(move |dialog, remember| {
            if let Some(this) = weak.upgrade() {
                clipboard_confirmation_deny(dialog, remember, &this);
            }
        });

        dialog.present(Some(self_.upcast_ref()));
    }

    fn clipboard_confirmation_confirm(dialog: &ClipboardConfirmationDialog, remember: bool, self_: &Surface) {
        let Some(surface) = self_.core_mut() else { return };
        let Some(req) = dialog.get_request().as_ref().map(|r| r.0.clone()) else { return };

        // Handle remember. OSC 52 remembers by changing the configured
        // policy; Kitty clipboard protocol requests remember via a
        // session grant recorded by the completion below.
        if remember {
            match req {
                ClipboardRequest::Osc52Read(_) => {
                    surface.config.clipboard_read = crate::config::config::ClipboardAccess::Allow
                }
                ClipboardRequest::Osc52Write(_) => {
                    surface.config.clipboard_write = crate::config::config::ClipboardAccess::Allow
                }
                ClipboardRequest::Paste(_)
                | ClipboardRequest::List(_)
                | ClipboardRequest::KittyRead(_)
                | ClipboardRequest::KittyWrite(_) => {}
            }
        }

        match &req {
            // A confirmed Kitty read must serve the actual requested
            // representations rather than the text preview shown in
            // the dialog, so the read is rerun with the confirmation
            // attached.
            ClipboardRequest::KittyRead(kitty) => {
                let Some(clipboard) = get(self_.imp().render_surface.upcast_ref(), kitty.location) else {
                    surface.deny_clipboard_request(&req);
                    return;
                };
                kitty_read_start(self_, &clipboard, req.clone(), true, remember);
                return;
            }

            // A confirmed Kitty write carries its committed contents
            // in the request itself.
            ClipboardRequest::KittyWrite(_) => {
                if let Err(err) = surface.complete_clipboard_request(
                    &req,
                    CompleteClipboard {
                        confirmed: true,
                        remember,
                        ..Default::default()
                    },
                ) {
                    log::warn!(target: LOG, "failed to complete clipboard request err={err}");
                }
                return;
            }

            ClipboardRequest::Paste(_)
            | ClipboardRequest::Osc52Read(_)
            | ClipboardRequest::Osc52Write(_)
            | ClipboardRequest::List(_) => {}
        }

        // Get our text
        let Some(text_buf) = dialog.get_clipboard_contents() else { return };
        let text: Option<String> = text_buf.property("text");
        let Some(text) = text else { return };

        let content = [terminal::clipboard::Content {
            mime: b"text/plain",
            data: text.as_bytes(),
        }];
        if let Err(err) = surface.complete_clipboard_request(
            &req,
            CompleteClipboard {
                contents: &content,
                confirmed: true,
                ..Default::default()
            },
        ) {
            log::warn!(target: LOG, "failed to complete clipboard request: {err}");
        }
    }

    fn clipboard_confirmation_deny(dialog: &ClipboardConfirmationDialog, remember: bool, self_: &Surface) {
        let Some(surface) = self_.core_mut() else { return };
        let Some(req) = dialog.get_request().as_ref().map(|r| r.0.clone()) else { return };

        // The denial consumes the request, and protocols whose client
        // waits on a reply are answered: an OSC 52 read replies with
        // empty contents and the Kitty clipboard protocol reports
        // EPERM. The other request types simply don't happen.
        //
        // This has to happen before the policy is changed below, because
        // the OSC 52 reply is written by the same path that serves an
        // allowed read, and that path asserts reads aren't denied.
        surface.deny_clipboard_request(&req);

        // Handle remember. Kitty clipboard protocol session grants only
        // record allowed requests, so a denied request is never
        // remembered.
        if remember {
            match req {
                ClipboardRequest::Osc52Read(_) => {
                    surface.config.clipboard_read = crate::config::config::ClipboardAccess::Deny
                }
                ClipboardRequest::Osc52Write(_) => {
                    surface.config.clipboard_write = crate::config::config::ClipboardAccess::Deny
                }
                ClipboardRequest::KittyRead(_) | ClipboardRequest::KittyWrite(_) => {}
                ClipboardRequest::Paste(_) | ClipboardRequest::List(_) => {
                    panic!("request should not be able to be remembered")
                }
            }
        }
    }

    fn clipboard_read_text(self_: Surface, req: ClipboardRequest, res: Result<Option<glib::GString>, glib::Error>) {
        let text = match res {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "failed to read clipboard err={}", err.message());

                // The requester may be waiting on a reply, e.g. an OSC 52
                // read of a clipboard with no text representation. Denying
                // the request answers the protocols that expect one.
                if let Some(surface) = self_.core_mut() {
                    surface.deny_clipboard_request(&req);
                }
                return;
            }
        };
        let Some(text) = text else { return };
        let bytes = text.as_bytes();

        let Some(surface) = self_.core_mut() else { return };
        let content = [terminal::clipboard::Content {
            mime: b"text/plain",
            data: bytes,
        }];
        match surface.complete_clipboard_request(
            &req,
            CompleteClipboard {
                contents: &content,
                ..Default::default()
            },
        ) {
            Ok(()) => {}
            Err(core_surface_mod::Error::UnsafePaste | core_surface_mod::Error::UnauthorizedPaste) => {
                show_clipboard_confirmation(&self_, &req, &content);
                return;
            }
            Err(err) => {
                log::warn!(target: LOG, "failed to complete clipboard request err={err}");
                return;
            }
        }

        self_.emit_by_name::<()>(signals::CLIPBOARD_READ, &[]);
    }
}

/// Compute a fraction [0.0, 1.0] from the supplied progress, which is
/// clamped to [0, 100].
fn compute_fraction(progress: u8) -> f64 {
    progress.clamp(0, 100) as f64 / 100.0
}

/// Apply command and shell integration overrides received from the CLI.
/// Explicit commands should only receive shell integration when their
/// executable can be detected as a supported shell. An explicit shell
/// integration override is also valid without a command.
fn apply_command_overrides(
    config: &mut CoreConfig,
    command: Option<&Command>,
    shell_integration: Option<ShellIntegration>,
) {
    if let Some(value) = command {
        config.command = Some(value.clone());

        if let Some(integration) = shell_integration {
            config.shell_integration = integration;
        } else if config.shell_integration != ShellIntegration::None {
            config.shell_integration = ShellIntegration::Detect;
        }
    } else if let Some(value) = shell_integration {
        config.shell_integration = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_fraction_() {
        assert_eq!(1.0, compute_fraction(100));
        assert_eq!(1.0, compute_fraction(255));
        assert_eq!(0.0, compute_fraction(0));
        assert_eq!(0.5, compute_fraction(50));
    }

    #[test]
    fn command_and_shell_integration_overrides() {
        let mut config = CoreConfig::default();

        config.shell_integration = ShellIntegration::Nushell;
        apply_command_overrides(&mut config, Some(&Command::Shell("vim".into())), None);
        assert_eq!(ShellIntegration::Detect, config.shell_integration);

        config.shell_integration = ShellIntegration::None;
        apply_command_overrides(&mut config, Some(&Command::Shell("vim".into())), None);
        assert_eq!(ShellIntegration::None, config.shell_integration);

        apply_command_overrides(&mut config, Some(&Command::Shell("nu".into())), Some(ShellIntegration::Nushell));
        assert_eq!(ShellIntegration::Nushell, config.shell_integration);

        apply_command_overrides(&mut config, Some(&Command::Shell("vim".into())), Some(ShellIntegration::None));
        assert_eq!(ShellIntegration::None, config.shell_integration);

        config.shell_integration = ShellIntegration::Nushell;
        apply_command_overrides(&mut config, None, None);
        assert_eq!(ShellIntegration::Nushell, config.shell_integration);

        config.shell_integration = ShellIntegration::None;
        apply_command_overrides(&mut config, None, Some(ShellIntegration::Nushell));
        assert_eq!(ShellIntegration::Nushell, config.shell_integration);

        apply_command_overrides(&mut config, None, Some(ShellIntegration::None));
        assert_eq!(ShellIntegration::None, config.shell_integration);
    }

    #[test]
    fn drop_direction() {
        assert_eq!(calc_drop_direction(100.0, 100.0, 1.0, 50.0), dst::Direction::Left);
        assert_eq!(calc_drop_direction(100.0, 100.0, 99.0, 50.0), dst::Direction::Right);
        assert_eq!(calc_drop_direction(100.0, 100.0, 50.0, 1.0), dst::Direction::Up);
        assert_eq!(calc_drop_direction(100.0, 100.0, 50.0, 99.0), dst::Direction::Down);
    }

    #[test]
    fn mouse_shape_names() {
        assert_eq!(MouseShape::from(TerminalMouseShape::ContextMenu).cursor_name(), "context-menu");
        assert_eq!(MouseShape::from(TerminalMouseShape::NwseResize).cursor_name(), "nwse-resize");
        assert_eq!(MouseShape::from(TerminalMouseShape::Text).cursor_name(), "text");
    }

    #[test]
    fn mouse_buttons() {
        assert_eq!(translate_mouse_button(1), MouseButton::Left);
        assert_eq!(translate_mouse_button(2), MouseButton::Middle);
        assert_eq!(translate_mouse_button(3), MouseButton::Right);
        assert_eq!(translate_mouse_button(11), MouseButton::Eleven);
        assert_eq!(translate_mouse_button(12), MouseButton::Unknown);
    }

    #[test]
    fn snap_paths_filtered() {
        let mut env = EnvMap::new();
        for (k, v) in [
            ("SNAP", "/snap/ghostty/1"),
            ("SNAP_DATA", "/var/snap"),
            ("PATH", "/snap/ghostty/1/bin:/usr/bin"),
            ("XDG", "/snap/ghostty/1/share"),
            ("GHOSTTY_X", "/snap/ghostty/1"),
        ] {
            env.insert(k.to_owned(), v.to_owned());
        }
        filter_snap_paths(&mut env);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("XDG"), None);
        assert_eq!(env.get("SNAP_DATA"), None);
        assert_eq!(env.get("GHOSTTY_X").map(String::as_str), Some("/snap/ghostty/1"));
    }
}
