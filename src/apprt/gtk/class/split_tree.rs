//! Port of `src/apprt/gtk/class/split_tree.zig`.
//!
//! STATUS: ported
//!
//! Rust notes: the tree is the boxed [`Tree`] (`GhosttySplitTreeSurface`).
//! The `changed` signal carries copies of the old and new trees (Zig passes
//! pointers to them for the duration of the emission).
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::close_confirmation_dialog::{self, CloseConfirmationDialog};
use crate::apprt::gtk::class::overrides::Overrides;
use crate::apprt::gtk::class::surface::{Surface, Tree};
use crate::apprt::gtk::class::surface_scrolled_window::SurfaceScrolledWindow;
use crate::apprt::gtk::class::{self, Blueprint};
use crate::apprt::gtk::ext;
use crate::apprt::gtk::ext::actions::Action as GtkAction;
use crate::apprt::surface::NewSurfaceContext;
use crate::datastruct::split_tree::{self as dst, f16, Direction, Goto, Handle, Layout, Node};

const LOG: &str = "gtk_ghostty_split_tree";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "split-tree" };

/// The template of [`SplitTreeSplit`].
pub const SPLIT_BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "split-tree-split" };

/// Property names (Zig: `properties`).
pub mod properties {
    /// The active surface is the surface that should be receiving all
    /// surface-targeted actions. This is usually the focused surface,
    /// but may also not be focused if the user has selected a non-surface
    /// widget.
    pub const ACTIVE_SURFACE: &str = "active-surface";
    pub const HAS_SURFACES: &str = "has-surfaces";
    pub const IS_ZOOMED: &str = "is-zoomed";
    pub const TREE: &str = "tree";
    pub const IS_SPLIT: &str = "is-split";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted whenever the tree property has changed, with access
    /// to the previous and new values.
    pub const CHANGED: &str = "changed";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/split-tree.ui")]
    pub struct SplitTree {
        /// The tree datastructure containing all of our surface views.
        pub tree: RefCell<Option<Tree>>,

        /// Last focused surface in the tree. We need this to handle various
        /// tree change states.
        pub last_focused: glib::WeakRef<Surface>,

        /// The source that we use to rebuild the tree. This is also
        /// used to debounce updates.
        pub rebuild_source: RefCell<Option<glib::SourceId>>,

        /// The source that we use to restore focus. With enough nested
        /// splits, some surfaces might initially be allocated a width or
        /// height of 0 which causes them to get unmapped and lose focus.
        /// We can reliably restore focus to the last focused surface only
        /// once it is mapped again.
        pub restore_focus_source: RefCell<Option<glib::SourceId>>,

        /// Used to store state about a pending surface close for the
        /// close dialog.
        pub pending_close: Cell<Option<Handle>>,

        /// Signal handlers connected to the surfaces of the current tree.
        pub surface_handlers: RefCell<Vec<(glib::WeakRef<Surface>, glib::SignalHandlerId)>>,

        // Template bindings
        #[template_child]
        pub tree_bin: TemplateChild<libadwaita::Bin>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SplitTree {
        const NAME: &'static str = "GhosttySplitTree";
        type Type = super::SplitTree;
        type ParentType = gtk4::Box;

        fn class_init(klass: &mut Self::Class) {
            Surface::ensure_type();
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "notify_tree", |args| {
                let this: super::SplitTree = args.template();
                this.prop_tree();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // SAFETY: the instance is initialized (as in `init_template`).
            let this = unsafe { obj.as_ref() };

            // Initialize our actions
            this.init_action_map();

            // Initialize some basic state
            this.imp().pending_close.set(None);
        }
    }

    impl ObjectImpl for SplitTree {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Surface>(properties::ACTIVE_SURFACE)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_SURFACES).read_only().build(),
                    glib::ParamSpecBoolean::builder(properties::IS_ZOOMED).read_only().build(),
                    glib::ParamSpecBoxed::builder::<Tree>(properties::TREE).build(),
                    glib::ParamSpecBoolean::builder(properties::IS_SPLIT).read_only().build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::ACTIVE_SURFACE => obj.get_active_surface().to_value(),
                properties::HAS_SURFACES => obj.get_has_surfaces().to_value(),
                properties::IS_ZOOMED => obj.get_is_zoomed().to_value(),
                properties::TREE => obj.get_tree().to_value(),
                properties::IS_SPLIT => obj.get_is_split().to_value(),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::TREE => {
                    let tree: Option<Tree> = value.get().unwrap();
                    self.obj().set_tree(tree.as_ref());
                }
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder(signals::CHANGED)
                    .param_types([Tree::static_type(), Tree::static_type()])
                    .build()]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.last_focused.set(None);
            if let Some(v) = self.rebuild_source.take() {
                v.remove();
            }
            if let Some(v) = self.restore_focus_source.take() {
                v.remove();
            }

            self.dispose_template();
        }

        // finalize: the tree is dropped with the private struct.
    }

    impl WidgetImpl for SplitTree {}
    impl BoxImpl for SplitTree {}
}

glib::wrapper! {
    pub struct SplitTree(ObjectSubclass<imp::SplitTree>)
        @extends gtk4::Box, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget,
            gtk4::Orientable;
}

impl Default for SplitTree {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Errors from tree operations (Zig: `Allocator.Error`).
pub type Error = dst::Error;

impl SplitTree {
    fn init_action_map(&self) {
        let actions = [
            // All of these will eventually take a target surface parameter.
            // For now all our targets originate from the focused surface.
            GtkAction::init("new-split", Self::action_new_split, Some(glib::VariantTy::STRING)),
            GtkAction::init("equalize", Self::action_equalize, None),
            GtkAction::init("zoom", Self::action_zoom, None),
            GtkAction::init("close-split", Self::action_close_split, None),
        ];

        ext::actions::add_as_group(self, "split-tree", &actions);
    }

    /// Create a new split in the given direction from the currently
    /// active surface.
    ///
    /// If the tree is empty this will create a new tree with a new surface
    /// and ignore the direction.
    ///
    /// The parent will be used as the parent of the surface regardless of
    /// if that parent is in this split tree or not. This allows inheriting
    /// surface properties from anywhere.
    pub fn new_split(&self, direction: Direction, parent: Option<&Surface>, overrides: &Overrides) -> Result<(), Error> {
        // Create our new surface.
        let surface = Surface::new(overrides);

        // Inherit properly if we were asked to.
        if let Some(p) = parent {
            if let Some(core) = p.core() {
                // SAFETY: the parent's core surface is live.
                surface.set_parent(unsafe { &*core }, NewSurfaceContext::Split);
            }
        }

        // Bind is-split property for new surface
        surface.bind_is_split(self);

        // Create our tree
        let single_tree = dst::SplitTree::init(&surface);

        // We want to move our focus to the new surface no matter what.
        // But we need to be careful to restore state if we fail.
        let priv_ = self.imp();
        let old_last_focused = priv_.last_focused.upgrade();
        priv_.last_focused.set(Some(&surface));

        // If we have no tree yet, then this becomes our tree and we're done.
        let Some(old_tree) = self.get_tree() else {
            self.set_tree(Some(&Tree(single_tree)));
            return Ok(());
        };

        // The handle we create the split relative to. Today this is the
        // active surface but this might be the handle of the given parent
        // if we want.
        let handle = self.get_active_surface_handle().unwrap_or(Handle::ROOT);

        // Create our split!
        let new_tree = match old_tree.split(
            handle,
            direction,
            f16::from_f32(0.5), // Always split equally for new splits
            &single_tree,
        ) {
            Ok(t) => t,
            Err(err) => {
                priv_.last_focused.set(old_last_focused.as_ref());
                return Err(err);
            }
        };
        log::debug!(
            target: LOG,
            "new split at={} direction={:?} old_tree={} new_tree={}",
            handle,
            direction,
            *old_tree,
            new_tree
        );

        // Replace our tree
        self.set_tree(Some(&Tree(new_tree)));
        Ok(())
    }

    pub fn resize(&self, direction: Direction, amount: u16) -> Result<bool, Error> {
        // Avoid useless work
        if amount == 0 {
            return Ok(false);
        }

        let Some(old_tree) = self.get_tree() else { return Ok(false) };
        let Some(active) = self.get_active_surface_handle() else { return Ok(false) };

        // Get all our dimensions we're going to need to turn our
        // amount into a percentage.
        let priv_ = self.imp();
        let width = priv_.tree_bin.width();
        let height = priv_.tree_bin.height();
        if width == 0 || height == 0 {
            return Ok(false);
        }
        let width_f64 = width as f64;
        let height_f64 = height as f64;
        let amount_f64 = amount as f64;

        // Get our ratio and use positive/neg for directions. The amount can
        // be larger than the split, which can only resize it as far as the
        // edge, so the ratio is clamped.
        let ratio = match direction {
            Direction::Right => amount_f64 / width_f64,
            Direction::Left => -(amount_f64 / width_f64),
            Direction::Down => amount_f64 / height_f64,
            Direction::Up => -(amount_f64 / height_f64),
        }
        .clamp(-1.0, 1.0);

        let layout = match direction {
            Direction::Left | Direction::Right => Layout::Horizontal,
            Direction::Up | Direction::Down => Layout::Vertical,
        };

        // If the split is held at its minimum size, resize from the ratio
        // it shows rather than the one in the tree.
        if let Node::Leaf(leaf) = &old_tree.nodes()[active.idx()] {
            if let Some(split) = find_parent_split(leaf, layout) {
                split.sync_split_ratio(SyncDirection::ClampedToTree);
            }
        }

        // The tree may have been updated in place above.
        let Some(old_tree) = self.get_tree() else { return Ok(false) };
        let new_tree = old_tree.resize(active, layout, f16::from_f64(ratio));
        self.set_tree(Some(&Tree(new_tree)));
        Ok(true)
    }

    /// Move focus from the currently focused surface to the given
    /// direction. Returns true if focus switched to a new surface.
    pub fn goto(&self, to: Goto) -> bool {
        let Some(tree) = self.get_tree() else { return false };
        let Some(active) = self.get_active_surface_handle() else { return false };
        let Some(target) = tree.goto(active, to) else { return false };

        // If we aren't changing targets then we did nothing.
        if active == target {
            return false;
        }

        // Get the surface at the target location and grab focus.
        let Node::Leaf(surface) = &tree.nodes()[target.idx()] else { return false };
        surface.grab_focus();

        // We also need to setup our last_focused to this because if we
        // trigger a tree change like below, the grab focus above never
        // actually triggers in time to set this and this ensures we
        // grab focus to the right thing.
        let priv_ = self.imp();
        priv_.last_focused.set(Some(surface));

        if tree.zoomed.is_some() {
            let app = Application::default();
            let navigation = app
                .get_config()
                .map(|c| c.get().split_preserve_zoom.navigation)
                .unwrap_or(false);

            {
                let mut tree = priv_.tree.borrow_mut();
                if let Some(tree) = tree.as_mut() {
                    if !navigation {
                        tree.zoomed = None;
                    } else {
                        tree.zoom(Some(target));
                    }
                }
            }

            // When the zoom state changes our tree state changes and
            // we need to send the proper notifications to trigger
            // relayout.
            self.notify(properties::TREE);
            self.notify(properties::IS_ZOOMED);
        }

        true
    }

    /// Move the source split onto the target split in a given direction.
    /// The target split must be located within this tree.
    pub fn move_split(&self, source: &Surface, target: &Surface, dir: Direction) -> Result<(), Error> {
        let Some(target_tree) = self.get_tree() else { return Ok(()) };

        // This really shouldn't fail, but just in case
        let Some(target_handle) = target_tree.locate(target) else {
            log::warn!(target: LOG, "target is not placed in a split tree");
            return Ok(());
        };

        // Try to find the source within the current tree.
        // If it exists, then it's a local move; otherwise the logic gets
        // more complicated
        let source_handle = target_tree.locate(source);

        // First add the source into this tree.
        // We have to do this no matter what
        let branch = dst::SplitTree::init(source);

        let after_split = target_tree.split(target_handle, dir, f16::from_f32(0.5), &branch)?;

        if let Some(handle) = source_handle {
            // Happy path: source and target are located within the same
            // tree. In that case, we just remove the existing source from
            // this tree
            let after_remove = after_split.remove(handle);
            self.set_tree(Some(&Tree(after_remove)));
        } else {
            // :( Cross-tree moves are a bit more complicated.

            // TODO: Find a better way to access the split tree from here
            let Some(source_tree_widget) = ext::get_ancestor::<SplitTree>(source) else {
                log::warn!(target: LOG, "source is not placed in a split tree");
                return Ok(());
            };
            let Some(source_tree) = source_tree_widget.get_tree() else { return Ok(()) };

            // Remove the source from its own tree. This also moves focus
            // in the source tree to another surface, as closing it would.
            let Some(handle) = source_tree.locate(source) else { return Ok(()) };
            source_tree_widget.remove_handle(handle)?;

            // Finally, set the final tree structure for this tree widget
            self.set_tree(Some(&Tree(after_split)));

            // Re-bind vital properties like `is-split`
            source.bind_is_split(self);
        }

        Ok(())
    }

    fn disconnect_surface_handlers(&self) {
        for (surface, id) in self.imp().surface_handlers.take() {
            if let Some(surface) = surface.upgrade() {
                surface.disconnect(id);
            }
        }
    }

    fn connect_surface_handlers(&self) {
        let Some(tree) = self.get_tree() else { return };
        let mut handlers = self.imp().surface_handlers.borrow_mut();
        for entry in tree.iterator() {
            let surface = entry.view;
            let weak = self.downgrade();
            let id = surface.connect_local(
                crate::apprt::gtk::class::surface::signals::CLOSE_REQUEST,
                false,
                move |args| {
                    let surface: Surface = args[0].get().ok()?;
                    if let Some(this) = weak.upgrade() {
                        this.surface_close_request(&surface);
                    }
                    None
                },
            );
            handlers.push((surface.downgrade(), id));
            let weak = self.downgrade();
            let id = surface.connect_notify_local(Some("focused"), move |surface, _| {
                if let Some(this) = weak.upgrade() {
                    this.prop_surface_focused(surface);
                }
            });
            handlers.push((surface.downgrade(), id));
            let weak = self.downgrade();
            let id = surface.connect_notify_local(Some("mapped"), move |surface, _| {
                if let Some(this) = weak.upgrade() {
                    this.prop_surface_mapped(surface);
                }
            });
            handlers.push((surface.downgrade(), id));
        }
    }

    //---------------------------------------------------------------
    // Properties

    /// Returns true if this split tree needs confirmation before quitting
    /// based on the various Ghostty configurations.
    pub fn get_needs_confirm_quit(&self) -> bool {
        let Some(tree) = self.get_tree() else { return false };
        for entry in tree.iterator() {
            if let Some(core) = entry.view.core() {
                // SAFETY: the core surface is live while the GTK surface is.
                if unsafe { (*core).needs_confirm_quit() } {
                    return true;
                }
            }
        }

        false
    }

    /// Get the currently active surface. See the "active-surface" property.
    pub fn get_active_surface(&self) -> Option<Surface> {
        let tree = self.get_tree()?;
        let handle = self.get_active_surface_handle()?;
        match &tree.nodes()[handle.idx()] {
            Node::Leaf(v) => Some(v.clone()),
            Node::Split(_) => None,
        }
    }

    fn get_active_surface_handle(&self) -> Option<Handle> {
        let tree = self.get_tree()?;
        for entry in tree.iterator() {
            if entry.view.get_focused() {
                return Some(entry.handle);
            }
        }

        // If none are currently focused, the most previously focused
        // surface (if it exists) is our active surface. This lets things
        // like apprt actions and bell ringing continue to work in the
        // background.
        if let Some(v) = self.imp().last_focused.upgrade() {
            // We need to find the handle of the last focused surface.
            for entry in tree.iterator() {
                if *entry.view == v {
                    return Some(entry.handle);
                }
            }
        }

        None
    }

    /// Returns the last focused surface in the tree.
    pub fn get_last_focused_surface(&self) -> Option<Surface> {
        self.imp().last_focused.upgrade()
    }

    pub fn get_has_surfaces(&self) -> bool {
        self.imp().tree.borrow().as_ref().is_some_and(|t| !t.is_empty())
    }

    pub fn get_is_zoomed(&self) -> bool {
        self.imp().tree.borrow().as_ref().is_some_and(|t| t.zoomed.is_some())
    }

    /// Get the tree data model that we're showing in this widget. This
    /// returns a copy of the tree (the views are reference counted).
    pub fn get_tree(&self) -> Option<Tree> {
        self.imp().tree.borrow().clone()
    }

    /// Set the tree data model that we're showing in this widget. This
    /// will clone the given tree.
    pub fn set_tree(&self, tree: Option<&Tree>) {
        let priv_ = self.imp();

        // We always normalize our tree parameter so that empty trees
        // become null so that we don't have to deal with callers being
        // confused about that.
        let tree = tree.filter(|t| !t.is_empty());

        // Emit the signal so that handlers can witness both the before and
        // after values of the tree.
        let old = priv_.tree.borrow().clone();
        self.emit_by_name::<()>(signals::CHANGED, &[&old, &tree.cloned()]);

        if priv_.tree.borrow().is_some() {
            self.disconnect_surface_handlers();
            priv_.tree.replace(None);
        }

        if let Some(new_tree) = tree {
            debug_assert!(priv_.tree.borrow().is_none());
            debug_assert!(!new_tree.is_empty());
            priv_.tree.replace(Some(new_tree.clone()));
            self.connect_surface_handlers();
        }

        self.notify(properties::TREE);
    }

    pub fn get_is_split(&self) -> bool {
        let tree = self.imp().tree.borrow();
        let Some(tree) = tree.as_ref() else { return false };
        if tree.is_empty() {
            return false;
        }

        match &tree.nodes()[Handle::ROOT.idx()] {
            Node::Leaf(_) => false,
            Node::Split(_) => true,
        }
    }

    //---------------------------------------------------------------
    // Signal handlers

    pub fn action_new_split(_: &gio::SimpleAction, args: Option<&glib::Variant>, self_: &Self) {
        let Some(args) = args else {
            log::warn!(target: LOG, "split-tree.new-split called without a parameter");
            return;
        };

        let Some(dir) = args.str() else { return };

        let direction = match dir {
            "left" => Direction::Left,
            "right" => Direction::Right,
            "down" => Direction::Down,
            "up" => Direction::Up,
            _ => {
                // Need to be defensive here since actions can be triggered
                // externally.
                log::warn!(target: LOG, "invalid split direction for split-tree.new-split: {dir}");
                return;
            }
        };

        if let Err(err) = self_.new_split(direction, self_.get_active_surface().as_ref(), &Overrides::NONE) {
            log::warn!(target: LOG, "new split failed error={err}");
        }
    }

    pub fn action_equalize(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let Some(old_tree) = self_.get_tree() else { return };
        let new_tree = old_tree.equalize();
        self_.set_tree(Some(&Tree(new_tree)));
    }

    pub fn action_zoom(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        {
            let active = self_.get_active_surface_handle();
            let mut tree = self_.imp().tree.borrow_mut();
            let Some(tree) = tree.as_mut() else { return };
            if tree.zoomed.is_some() {
                tree.zoomed = None;
            } else {
                let Some(active) = active else { return };
                if tree.zoomed == Some(active) {
                    return;
                }
                tree.zoom(Some(active));
            }
        }

        self_.notify(properties::TREE);
    }

    pub fn action_close_split(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let Some(surface) = self_.get_active_surface() else { return };
        surface.close();
    }

    fn surface_close_request(&self, surface: &Surface) {
        let Some(core) = surface.core() else { return };

        // Reset our pending close state
        let priv_ = self.imp();
        priv_.pending_close.set(None);

        // Find the surface in the tree to verify this is valid and
        // set our pending close handle.
        let Some(tree) = self.get_tree() else { return };
        let Some(handle) = tree.locate(surface) else { return };
        priv_.pending_close.set(Some(handle));

        // If we don't need to confirm then just close immediately.
        // SAFETY: the core surface is live while the GTK surface is.
        if !unsafe { (*core).needs_confirm_quit() } {
            self.close_confirmation_close();
            return;
        }

        // Show a confirmation dialog
        let dialog = CloseConfirmationDialog::new(close_confirmation_dialog::Target::Surface);
        let weak = self.downgrade();
        dialog.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.close_confirmation_close();
            }
        });
        dialog.present(Some(self.upcast_ref()));
    }

    fn close_confirmation_close(&self) {
        // Get the handle we're closing
        let priv_ = self.imp();
        let Some(handle) = priv_.pending_close.take() else { return };

        if let Err(err) = self.remove_handle(handle) {
            log::warn!(target: LOG, "unable to remove surface from tree: {err}");
        }
    }

    /// Remove the node with the given handle from the tree, making the
    /// surface next to it the last focused surface of the tree.
    fn remove_handle(&self, handle: Handle) -> Result<(), Error> {
        // Figure out our next focus target. The next focus target is
        // always the "previous" surface unless we're the leftmost then
        // its the next.
        let Some(old_tree) = self.get_tree() else { return Ok(()) };
        let next_focus: Option<Surface> = (|| {
            let next_handle = old_tree
                .goto(handle, Goto::Previous)
                .or_else(|| old_tree.goto(handle, Goto::Next))?;
            if next_handle == handle {
                return None;
            }

            // Note: we don't need to ref this or anything because its
            // guaranteed to remain in the new tree since its not part
            // of the handle we're removing.
            match &old_tree.nodes()[next_handle.idx()] {
                Node::Leaf(v) => Some(v.clone()),
                Node::Split(_) => None,
            }
        })();

        // Remove it from the tree.
        let new_tree = old_tree.remove(handle);
        self.set_tree(Some(&Tree(new_tree)));

        // Grab focus. We have to set this on the "last focused" because our
        // focus will be set when the tree is redrawn.
        if let Some(v) = next_focus {
            self.imp().last_focused.set(Some(&v));
        }
        Ok(())
    }

    fn prop_surface_focused(&self, surface: &Surface) {
        // We never CLEAR our last_focused because the property is
        // specifically the last focused surface. We let the weakref clear
        // itself when the surface is destroyed.
        if !surface.get_focused() {
            return;
        }
        self.imp().last_focused.set(Some(surface));

        // Our active surface probably changed
        self.notify(properties::ACTIVE_SURFACE);
    }

    fn prop_surface_mapped(&self, surface: &Surface) {
        if !surface.get_mapped() {
            return;
        }

        // We could add the idle callback only if this is actually the last
        // focused surface. But we can avoid that check because usually all
        // the surfaces get mapped at once, so the idle callback will run
        // only once anyway.
        let priv_ = self.imp();
        if priv_.restore_focus_source.borrow().is_none() {
            let weak = self.downgrade();
            let id = glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.on_restore_focus();
                }
            });
            priv_.restore_focus_source.replace(Some(id));
        }
    }

    fn prop_tree(&self) {
        let priv_ = self.imp();

        // No matter what we notify
        let _guard = self.freeze_notify();
        self.notify(properties::HAS_SURFACES);
        self.notify(properties::IS_ZOOMED);

        // If we were planning a rebuild or focus restore, always remove
        // that so we can start from a clean slate.
        if let Some(v) = priv_.rebuild_source.take() {
            v.remove();
        }
        if let Some(v) = priv_.restore_focus_source.take() {
            v.remove();
        }

        // If we transitioned to an empty tree, clear immediately instead of
        // waiting for an idle callback. Delaying teardown can keep the last
        // surface alive during shutdown if the main loop exits first.
        if priv_.tree.borrow().is_none() {
            priv_.tree_bin.set_child(None::<&gtk4::Widget>);
            return;
        }

        // Build on an idle callback so rapid tree changes are debounced.
        // We keep the existing tree attached until the rebuild runs,
        // which avoids transient empty frames.
        debug_assert!(priv_.rebuild_source.borrow().is_none());
        let weak = self.downgrade();
        let id = glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.on_rebuild();
            }
        });
        priv_.rebuild_source.replace(Some(id));
    }

    fn on_rebuild(&self) {
        // Always mark our rebuild source as null since we're done.
        let priv_ = self.imp();
        priv_.rebuild_source.replace(None);

        // Rebuild our tree
        let tree = self.get_tree();
        match tree {
            Some(tree) if !tree.is_empty() => {
                let built = self.build_tree(&tree, tree.zoomed.unwrap_or(Handle::ROOT));
                priv_.tree_bin.set_child(Some(&built.widget));
            }
            _ => priv_.tree_bin.set_child(None::<&gtk4::Widget>),
        }

        // Replacing our tree widget hierarchy can reset focus state.
        // If we have a last-focused surface, restore focus to it.
        if let Some(v) = priv_.last_focused.upgrade() {
            v.grab_focus();
        }

        // Our split status may have changed
        self.notify(properties::IS_SPLIT);

        // Our active surface may have changed
        self.notify(properties::ACTIVE_SURFACE);
    }

    fn on_restore_focus(&self) {
        // Always mark our source as null since we're done.
        let priv_ = self.imp();
        priv_.restore_focus_source.replace(None);

        // If we have a last-focused surface and it is mapped, restore focus
        // to it. Depending on the available size, the surface might already
        // have focus because it never got unmapped. In that case grabbing
        // focus will have no effect.
        if let Some(v) = priv_.last_focused.upgrade() {
            if v.get_mapped() {
                v.grab_focus();
            }
        }
    }

    fn build_tree(&self, tree: &Tree, current: Handle) -> BuildTreeResult {
        match &tree.nodes()[current.idx()] {
            Node::Leaf(v) => match ext::get_ancestor::<SurfaceScrolledWindow>(v) {
                // The surface isn't in a window already so we don't
                // have to worry about reuse.
                None => {
                    let w: SurfaceScrolledWindow =
                        glib::Object::builder().property("surface", v).build();
                    BuildTreeResult::init_new(w.upcast())
                }

                // Keep this widget alive while we detach it from the
                // old tree and adopt it into the new one.
                Some(window) => BuildTreeResult::init_reused(window.upcast()),
            },
            Node::Split(s) => {
                let left = self.build_tree(tree, s.left);
                let right = self.build_tree(tree, s.right);

                BuildTreeResult::init_new(SplitTreeSplit::new(current, s, &left.widget, &right.widget).upcast())
            }
        }
    }
}

/// Builds the widget tree associated with a surface split tree.
///
/// Returned widgets are expected to be attached to a parent by the caller.
/// A reused widget is held by the strong reference in `widget` while it is
/// detached and re-parented (Zig: `release_ref`).
struct BuildTreeResult {
    widget: gtk4::Widget,
}

impl BuildTreeResult {
    fn init_new(widget: gtk4::Widget) -> BuildTreeResult {
        BuildTreeResult { widget }
    }

    fn init_reused(widget: gtk4::Widget) -> BuildTreeResult {
        // We hold a ref to the widget to ensure it doesn't get destroyed
        // while we're rebuilding the tree and detaching it from its old
        // parent. Detach after we ref it so that this doesn't mark the
        // widget for destruction.
        detach_widget(&widget);
        BuildTreeResult { widget }
    }
}

/// Detach a split widget from its current parent.
///
/// We intentionally use parent-specific child APIs when possible
/// (`GtkPaned.setStartChild/setEndChild`, `AdwBin.setChild`) instead of
/// calling `gtk.Widget.unparent` directly. Container implementations track
/// child pointers/properties internally, and those setters are the path
/// that keeps container state and notifications in sync.
fn detach_widget(widget: &gtk4::Widget) {
    let Some(parent) = widget.parent() else { return };

    // Surface will be in a paned when it is split.
    if let Some(paned) = parent.downcast_ref::<gtk4::Paned>() {
        if paned.start_child().as_ref() == Some(widget) {
            paned.set_start_child(None::<&gtk4::Widget>);
            return;
        }

        if paned.end_child().as_ref() == Some(widget) {
            paned.set_end_child(None::<&gtk4::Widget>);
            return;
        }
    }

    // Surface will be in a bin when it is not split.
    if let Some(bin) = parent.downcast_ref::<libadwaita::Bin>() {
        if bin.child().as_ref() == Some(widget) {
            bin.set_child(None::<&gtk4::Widget>);
            return;
        }
    }

    // Fallback for unexpected parents where we don't have a typed
    // container API available.
    widget.unparent();
}

/// Find the nearest SplitTreeSplit ancestor of the surface with the
/// given layout. This is the split that Surface.Tree.resize resizes.
fn find_parent_split(surface: &Surface, layout: Layout) -> Option<SplitTreeSplit> {
    let orientation = match layout {
        Layout::Horizontal => gtk4::Orientation::Horizontal,
        Layout::Vertical => gtk4::Orientation::Vertical,
    };

    let mut widget: gtk4::Widget = surface.clone().upcast();
    while let Some(split) = ext::get_ancestor::<SplitTreeSplit>(&widget) {
        let paned = split.imp().paned.get();
        if paned.orientation() == orientation {
            return Some(split);
        }
        widget = split.parent()?;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncDirection {
    // Update gtk.Paned widget to match ratio from split tree node.
    TreeToWidget,
    // Update split tree node to match ratio from gtk.Paned widget.
    WidgetToTree,
    // Like widget_to_tree, but only if the split is held at its minimum.
    ClampedToTree,
}

mod split_imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/split-tree-split.ui")]
    pub struct SplitTreeSplit {
        /// The handle of the node in the tree that this split represents.
        /// Assumed to be correct.
        pub handle: Cell<Option<Handle>>,

        /// Cache the split ratio. Looking it up is relatively expensive
        /// because we have to first walk the widget tree to find the
        /// SplitTree widget.
        pub ratio: Cell<f64>,

        /// Source to handle repositioning the split when properties change.
        pub idle: RefCell<Option<glib::SourceId>>,

        /// Whether the min-position/max-position or position property of the
        /// gtk.Paned widget changed. We use these to distinguish between a
        /// resize and the user manually moving the split divider. See the
        /// "onIdle" function.
        pub limits_changed: Cell<bool>,
        pub pos_changed: Cell<bool>,

        // Template bindings
        #[template_child]
        pub paned: TemplateChild<gtk4::Paned>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SplitTreeSplit {
        const NAME: &'static str = "GhosttySplitTreeSplit";
        type Type = super::SplitTreeSplit;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "notify_position_limits", |args| {
                let this: super::SplitTreeSplit = args.template();
                this.prop_position_limits();
                None
            });
            class::bind_template_callback(klass, "notify_position", |args| {
                let this: super::SplitTreeSplit = args.template();
                this.prop_position();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SplitTreeSplit {
        fn dispose(&self) {
            if let Some(v) = self.idle.take() {
                v.remove();
            }

            self.dispose_template();
        }
    }

    impl WidgetImpl for SplitTreeSplit {}
    impl BinImpl for SplitTreeSplit {}
}

glib::wrapper! {
    /// This is an internal-only widget that represents a split in the
    /// split tree. This is a wrapper around gtk.Paned that allows us to
    /// handle ratio (0 to 1) based positioning of the split, and also allows
    /// us to write back the updated ratio to the split tree when the user
    /// manually adjusts the split position.
    ///
    /// Since this is internal, it expects to be nested within a SplitTree
    /// and will use `getAncestor` to find the SplitTree it belongs to.
    ///
    /// This is an _immutable_ widget. It isn't meant to be updated after
    /// creation. As such, there are no properties or APIs to change the
    /// split, access the paned, etc.
    pub struct SplitTreeSplit(ObjectSubclass<split_imp::SplitTreeSplit>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl SplitTreeSplit {
    /// Create a new split.
    ///
    /// The reason we don't use GObject properties here is because this is
    /// an immutable widget and we don't want to deal with the overhead of
    /// all the boilerplate for properties, signals, bindings, etc.
    pub fn new(handle: Handle, split: &dst::Split, start_child: &gtk4::Widget, end_child: &gtk4::Widget) -> SplitTreeSplit {
        let this: SplitTreeSplit = glib::Object::new();
        let priv_ = this.imp();
        priv_.handle.set(Some(handle));
        priv_.ratio.set(split.ratio.to_f64());

        // Setup our paned fields
        let paned = &*priv_.paned;
        paned.set_start_child(Some(start_child));
        paned.set_end_child(Some(end_child));
        paned.set_orientation(match split.layout {
            Layout::Horizontal => gtk4::Orientation::Horizontal,
            Layout::Vertical => gtk4::Orientation::Vertical,
        });

        // Signals and so on are setup in the template.

        this
    }

    // Sync split ratio between the gtk.Paned widget and the split tree.
    fn sync_split_ratio(&self, direction: SyncDirection) {
        let priv_ = self.imp();
        let paned = &*priv_.paned;

        // Current, min, and max positions as pixels.
        let pos = paned.position();
        let min: i32 = paned.property("min-position");
        let max: i32 = paned.property("max-position");

        // The size of the split as pixels, which ratios are relative to.
        // Since gtk.Paned doesn't shrink the end child below its minimum
        // size, max is reduced by that minimum size, so add it back.
        let size = match paned.end_child() {
            Some(end_child) => {
                let (end_min, _, _, _) = end_child.measure(paned.orientation(), -1);
                max + end_min
            }
            None => max,
        };

        // If our size is zero then we can't do any math. I don't know
        // if this is possible but I suspect it can be if you make a nested
        // split completely minimized.
        if size == 0 {
            return;
        }

        // Determine our current ratio.
        let current_ratio = pos as f64 / size as f64;
        let desired_ratio = priv_.ratio.get();

        // If our ratio is close enough to our desired ratio, then
        // we ignore the update. This is to avoid constant split updates
        // for lossy floating point math.
        if (current_ratio - desired_ratio).abs() <= 0.001 {
            return;
        }

        // The position in gtk.Paned matching the desired ratio. Note that
        // if the size is small, it might not be possible to accurately set
        // the desired ratio. E.g. with a size of 2 you can only have
        // ratios 0, 0.5 and 1.
        let desired_pos = (size as f64 * desired_ratio).round() as i32;
        let clamped_pos = desired_pos.clamp(min, min.max(max));

        match direction {
            // gtk.Paned would clamp this too, but a frame later. The tree
            // keeps the desired ratio for when the split grows again.
            SyncDirection::TreeToWidget => paned.set_position(clamped_pos),
            SyncDirection::WidgetToTree => self.set_tree_ratio(current_ratio),
            SyncDirection::ClampedToTree => {
                if clamped_pos != desired_pos {
                    self.set_tree_ratio(clamped_pos as f64 / size as f64);
                }
            }
        }
    }

    /// Update ratio of the split tree node. This is the most dangerous
    /// part of this widget. We assume that this widget is always a child
    /// of a SplitTree, we assume that our handle is valid, and we assume
    /// the handle is always a split node.
    fn set_tree_ratio(&self, ratio: f64) {
        let priv_ = self.imp();
        let Some(split_tree) = ext::get_ancestor::<SplitTree>(self) else {
            log::warn!(target: LOG, "SplitTree ancestor not found");
            return;
        };

        // A pending rebuild means our handle is for the old tree.
        if split_tree.imp().rebuild_source.borrow().is_some() {
            return;
        }

        let mut tree = split_tree.imp().tree.borrow_mut();
        let Some(tree) = tree.as_mut() else {
            log::warn!(target: LOG, "no tree data model set");
            return;
        };
        let Some(handle) = priv_.handle.get() else { return };
        debug_assert!(handle.idx() < tree.nodes().len());
        debug_assert!(matches!(tree.nodes()[handle.idx()], Node::Split(_)));
        tree.resize_in_place(handle, f16::from_f64(ratio));
        priv_.ratio.set(ratio);
    }

    // We need to keep the split ratios from the tree datastructure and
    // widget tree in sync. Using the min-position, max-position and position
    // properties of the gtk.Paned widget, we can distinguish a resize from a
    // manual update (e.g. the user dragging the divider). If min-position or
    // max-position changes, we always have a widget resize (of the split or
    // of a child's minimum size) and we sync the ratio directly from
    // the propPositionLimits function. Usually position will change as well
    // but it might not if the size change is small enough. If only position
    // changes, we have a manual human update and we sync the ratio here.
    //
    // This is a hack, it relies on the timing of property notifcations.
    // From looking at the GTK source code, it should not be possible that
    // we interpret a position change from a resize as a manual update.
    // When a gtk.Paned is resized, internally the gtk_paned_calc_position
    // function will change min-position, max-position and position and
    // synchronously call our propPositionLimits and propPosition functions.
    // I.e. when the widget is resized, it should not be possible for onIdle
    // to run before we have been notified of all property changes.
    fn on_idle(&self) {
        let priv_ = self.imp();

        // Clear source and fields.
        let limits_changed = priv_.limits_changed.replace(false);
        let pos_changed = priv_.pos_changed.replace(false);
        priv_.idle.replace(None);

        if limits_changed {
            // Nothing to do, syncSplitRatio was already called
            // directly from propPositionLimits.
            return;
        }
        if !pos_changed {
            return;
        }

        // If only position changed, this is a manual human update and
        // we need to write our update back to the tree.
        self.sync_split_ratio(SyncDirection::WidgetToTree);
    }

    fn schedule_idle(&self) {
        let priv_ = self.imp();
        if priv_.idle.borrow().is_none() {
            let weak = self.downgrade();
            let id = glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.on_idle();
                }
            });
            priv_.idle.replace(Some(id));
        }
    }

    //---------------------------------------------------------------
    // Signal handlers

    fn prop_position_limits(&self) {
        self.imp().limits_changed.set(true);

        // Widget or the minimum size of a child got resized, update
        // position to match desired ratio.
        //
        // Syncing the split ratio directly here ensures that every surface
        // gets the correct size allocated immediately. This works because
        // this callback runs during size allocation, so that we update the
        // divider position right after the gtk.Paned widget computes it and
        // right before it will use the position to compute the sizes of its
        // children. See the size_allocate and calc_position functions of
        // the gtk.Paned widget.
        //
        // If we synced the ratio in an idle callback, there might be some
        // flickering as surfaces are shown with the wrong size for a few
        // frames until our idle callback runs. Depending on the split layout
        // it might take multiple rounds of size allocation and idle
        // callbacks for every widget to get the correct size.
        //
        // Note that gtk.Paned calls Widget.setChildVisible(true/false) for
        // both children based on the initial divider position it computes.
        // If we then update the position here, we might sometimes have to
        // call setChildVisible again. This would be necessary if we updated
        // the position from initially 0 (max), where start (end) child is
        // invisible, to a value > 0 (< max). However, we can skip this
        // because set_position already queues another round of size
        // allocation, during which gtk.Paned will keep the correct position
        // we set manually and then call setChildVisible again based on that.
        // The only problematic situation is when we update the position from
        // 0 to a value > 0, because setChildVisible(false) will unmap the
        // start child widget. It will not be visible for a single frame
        // which introduces some flickering. This can't happen because
        // surfaces always have a minimum size (see Surface.setCellSize) that
        // gtk.Paned doesn't shrink below.
        self.sync_split_ratio(SyncDirection::TreeToWidget);

        // We still need the idle callback to clear the limits_changed field
        // later, because propPosition might or might not still get called.
        self.schedule_idle();
    }

    fn prop_position(&self) {
        self.imp().pos_changed.set(true);
        self.schedule_idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprints_match() {
        assert_eq!(BLUEPRINT.name, "split-tree");
        assert_eq!(SPLIT_BLUEPRINT.name, "split-tree-split");
    }

    #[test]
    fn split_tree_smoke() {
        let Some(()) = test_gtk::run(|| {
            let _app = Application::default();
            let tree = SplitTree::default();
            assert!(!tree.get_has_surfaces());
            assert!(!tree.get_is_split());
            assert!(tree.get_active_surface().is_none());

            // First split creates a single-leaf tree.
            tree.new_split(Direction::Right, None, &Overrides::NONE).unwrap();
            assert!(tree.get_has_surfaces());
            assert!(!tree.get_is_split());
            let first = tree.get_last_focused_surface().unwrap();
            assert_eq!(tree.get_active_surface().as_ref(), Some(&first));

            // A second split makes it a split with the new surface active.
            tree.new_split(Direction::Down, Some(&first), &Overrides::NONE).unwrap();
            assert!(tree.get_is_split());
            let second = tree.get_last_focused_surface().unwrap();
            assert_ne!(first, second);
            assert_eq!(tree.get_tree().unwrap().iterator().count(), 2);

            // Zoom toggles.
            SplitTree::action_zoom(&gio::SimpleAction::new("x", None), None, &tree);
            assert!(tree.get_is_zoomed());
            SplitTree::action_zoom(&gio::SimpleAction::new("x", None), None, &tree);
            assert!(!tree.get_is_zoomed());

            // Closing (no core surface means no confirmation) removes it.
            tree.surface_close_request(&second);
            // Without a core surface the close request is ignored, like Zig.
            assert!(tree.get_is_split());
            let handle = tree.get_tree().unwrap().locate(&second).unwrap();
            tree.remove_handle(handle).unwrap();
            assert!(!tree.get_is_split());
            assert_eq!(tree.get_last_focused_surface().as_ref(), Some(&first));
        }) else {
            return;
        };
    }
}
