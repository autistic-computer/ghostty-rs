//! Port of `src/apprt/gtk/class/clipboard_confirmation_dialog.zig`.
//!
//! STATUS: ported
//!
//! Zig's `setParts(contents: anytype)` duck-types over element types with
//! `mime` and `data` fields; the port expresses that with the
//! [`ClipboardPart`] trait.
#![cfg(feature = "gtk")]

use std::cell::{Cell, Ref, RefCell};
use std::fmt;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, boxed, i18n_, Blueprint};
use crate::apprt::structs::{ClipboardContent, ClipboardContentBuf, ClipboardRequest};

const LOG: &str = "gtk_ghostty_clipboard_confirmation";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint =
    Blueprint { major: 1, minor: 5, name: "clipboard-confirmation-dialog" };

/// The height of the preview area, matching the `height-request` the
/// template gives the stack. An image preview is clamped to it so that
/// the image's own size can't decide how tall the dialog is.
const PREVIEW_HEIGHT: i32 = 200;

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CAN_REMEMBER: &str = "can-remember";
    pub const REQUEST: &str = "request";
    pub const BLUR: &str = "blur";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    pub const DENY: &str = "deny";
    pub const CONFIRM: &str = "confirm";
}

/// One clipboard representation as [`ClipboardConfirmationDialog::set_parts`]
/// reads it: only the MIME type and the data.
pub trait ClipboardPart {
    fn mime(&self) -> &[u8];
    fn data(&self) -> &[u8];
}

impl ClipboardPart for ClipboardContent<'_> {
    fn mime(&self) -> &[u8] {
        self.mime
    }
    fn data(&self) -> &[u8] {
        self.data
    }
}

impl ClipboardPart for ClipboardContentBuf {
    fn mime(&self) -> &[u8] {
        self.mime.to_bytes()
    }
    fn data(&self) -> &[u8] {
        &self.data
    }
}

impl ClipboardPart for crate::terminal::clipboard::Content<'_> {
    fn mime(&self) -> &[u8] {
        self.mime
    }
    fn data(&self) -> &[u8] {
        self.data
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.5/clipboard-confirmation-dialog.ui")]
    pub struct ClipboardConfirmationDialog {
        /// The request that this dialog is for.
        pub request: RefCell<Option<boxed::ClipboardRequest>>,

        /// The buffer holding the first text representation, which is
        /// what a confirmed request that carries one value sends.
        pub clipboard_contents: RefCell<Option<gtk4::TextBuffer>>,

        /// Whether the contents should be blurred.
        pub blur: Cell<bool>,

        /// Whether the user can remember the choice.
        pub can_remember: Cell<bool>,

        // Template bindings
        #[template_child]
        pub parts_stack: TemplateChild<gtk4::Stack>,
        #[template_child]
        pub parts_dropdown: TemplateChild<gtk4::DropDown>,
        #[template_child]
        pub reveal_button: TemplateChild<gtk4::Button>,
        #[template_child]
        pub hide_button: TemplateChild<gtk4::Button>,
        #[template_child]
        pub remember_choice: TemplateChild<libadwaita::SwitchRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipboardConfirmationDialog {
        const NAME: &'static str = "GhosttyClipboardConfirmationDialog";
        type Type = super::ClipboardConfirmationDialog;
        type ParentType = libadwaita::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "parts_selected", |args| {
                // (dropdown, pspec, self)
                let dropdown: gtk4::DropDown = args.get(0);
                let this: super::ClipboardConfirmationDialog = args.template();
                this.parts_selected(&dropdown);
                None
            });
            class::bind_template_callback(klass, "reveal_clicked", |args| {
                let this: super::ClipboardConfirmationDialog = args.template();
                this.reveal_button_clicked();
                None
            });
            class::bind_template_callback(klass, "hide_clicked", |args| {
                let this: super::ClipboardConfirmationDialog = args.template();
                this.hide_button_clicked();
                None
            });
            class::bind_template_callback(klass, "notify_blur", |args| {
                let this: super::ClipboardConfirmationDialog = args.template();
                this.prop_blur();
                None
            });
            class::bind_template_callback(klass, "notify_request", |args| {
                let this: super::ClipboardConfirmationDialog = args.template();
                this.prop_request();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // Trigger initial values
            // SAFETY: the instance is initialized (as in `init_template`).
            let this = unsafe { obj.as_ref() };
            this.prop_blur();
            this.prop_request();
        }
    }

    impl ObjectImpl for ClipboardConfirmationDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecBoolean::builder(properties::BLUR).default_value(false).build(),
                    glib::ParamSpecBoolean::builder(properties::CAN_REMEMBER)
                        .default_value(false)
                        .build(),
                    glib::ParamSpecBoxed::builder::<boxed::ClipboardRequest>(properties::REQUEST)
                        .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::BLUR => accessor::get_shallow(&self.blur),
                properties::CAN_REMEMBER => accessor::get_shallow(&self.can_remember),
                properties::REQUEST => accessor::get(&self.request),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::BLUR => accessor::set_shallow(&self.blur, value),
                properties::CAN_REMEMBER => accessor::set_shallow(&self.can_remember, value),
                properties::REQUEST => accessor::set(&self.request, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signals::CONFIRM).param_types([bool::static_type()]).build(),
                    Signal::builder(signals::DENY).param_types([bool::static_type()]).build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.clipboard_contents.replace(None);
            self.dispose_template();
        }

        // finalize: `request` is dropped with the private struct.
    }

    impl WidgetImpl for ClipboardConfirmationDialog {}
    impl AdwDialogImpl for ClipboardConfirmationDialog {}

    impl AdwAlertDialogImpl for ClipboardConfirmationDialog {
        fn response(&self, response_id: &str) {
            let remember = self.remember_choice.is_active();

            if response_id == "cancel" {
                self.obj().emit_by_name::<()>(signals::DENY, &[&remember]);
            } else if response_id == "ok" {
                self.obj().emit_by_name::<()>(signals::CONFIRM, &[&remember]);
            }
        }
    }
}

glib::wrapper! {
    pub struct ClipboardConfirmationDialog(ObjectSubclass<imp::ClipboardConfirmationDialog>)
        @extends libadwaita::AlertDialog, libadwaita::Dialog, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for ClipboardConfirmationDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// What a preview page shows, which is only as much as we could make of
/// the representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Image,
    Text,
    None,
}

impl fmt::Display for PartKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PartKind::Image => "image",
            PartKind::Text => "text",
            PartKind::None => "none",
        })
    }
}

/// One representation's preview page.
struct Part {
    widget: gtk4::Widget,

    /// What the page shows, which is only as much as we could make
    /// of the representation.
    kind: PartKind,

    /// The buffer behind the page when the representation is shown
    /// as text, so the dialog can hand that text back on confirm.
    buffer: Option<gtk4::TextBuffer>,
}

impl ClipboardConfirmationDialog {
    pub fn new() -> ClipboardConfirmationDialog {
        glib::Object::new()
    }

    pub fn present(&self, parent: Option<&gtk4::Widget>) {
        AdwDialogExt::present(self, parent);
    }

    /// Get the clipboard request without copying.
    pub fn get_request(&self) -> Ref<'_, Option<boxed::ClipboardRequest>> {
        self.imp().request.borrow()
    }

    /// Get the clipboard contents without copying.
    pub fn get_clipboard_contents(&self) -> Option<gtk4::TextBuffer> {
        self.imp().clipboard_contents.borrow().clone()
    }

    /// Fill the preview with one page per clipboard representation, so
    /// that a multipart payload can be inspected in full rather than
    /// through whichever single representation we happened to pick.
    /// The pages are named by MIME type and the dropdown above them
    /// is shown only when there's more than one.
    ///
    /// The first text representation is kept as the dialog contents,
    /// since that's the one value a confirmed OSC 52 write or unsafe
    /// paste sends on.
    ///
    /// Everything is copied into widgets here, so the contents need
    /// only live for this call.
    pub fn set_parts<P: ClipboardPart>(&self, contents: &[P]) {
        let priv_ = self.imp();

        let mut count: usize = 0;
        for (i, content) in contents.iter().enumerate() {
            let part = preview_part(content.mime(), content.data());

            // The page name only addresses the page; the title is what
            // the dropdown shows.
            let name = format!("part-{i}");
            if name.len() >= 32 {
                continue;
            }

            let title = truncate_z(content.mime(), 256);

            priv_.parts_stack.add_titled(&part.widget, Some(&name), &title);
            count += 1;

            log::debug!(
                target: LOG,
                "clipboard confirmation preview mime={} bytes={} shown as {}",
                title,
                content.data().len(),
                part.kind,
            );

            // The value a confirmed single-value request sends.
            if priv_.clipboard_contents.borrow().is_none() {
                if let Some(buffer) = part.buffer {
                    priv_.clipboard_contents.replace(Some(buffer));
                }
            }
        }

        priv_.parts_dropdown.set_visible(count > 1);

        log::debug!(
            target: LOG,
            "clipboard confirmation preview parts={} text={}",
            count,
            priv_.clipboard_contents.borrow().is_some(),
        );
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn prop_blur(&self) {
        let priv_ = self.imp();
        if priv_.blur.get() {
            priv_.parts_stack.set_sensitive(false);
            priv_.parts_stack.add_css_class("blurred");
            priv_.reveal_button.set_visible(true);
            priv_.hide_button.set_visible(false);
        } else {
            priv_.parts_stack.set_sensitive(true);
            priv_.parts_stack.remove_css_class("blurred");
            priv_.reveal_button.set_visible(false);
            priv_.hide_button.set_visible(false);
        }
    }

    fn prop_request(&self) {
        let priv_ = self.imp();
        let req = priv_.request.borrow();
        let Some(req) = req.as_ref() else { return };
        match &req.0 {
            ClipboardRequest::Osc52Write(_) | ClipboardRequest::KittyWrite(_) => {
                self.set_heading(Some(i18n_(c"Authorize Clipboard Access")));
                self.set_body(i18n_(c"An application is attempting to write to the clipboard. The current clipboard contents are shown below."));
            }
            ClipboardRequest::Osc52Read(_) | ClipboardRequest::KittyRead(_) => {
                self.set_heading(Some(i18n_(c"Authorize Clipboard Access")));
                self.set_body(i18n_(c"An application is attempting to read from the clipboard. The current clipboard contents are shown below."));
            }
            ClipboardRequest::Paste(_) => {
                self.set_heading(Some(i18n_(c"Warning: Potentially Unsafe Paste")));
                self.set_body(i18n_(c"Pasting this text into the terminal may be dangerous as it looks like some commands may be executed."));
            }
            ClipboardRequest::List(_) => unreachable!(),
        }

        // The remember switch means different things for different
        // request types: OSC 52 remembers by changing the configured
        // policy, while Kitty clipboard protocol requests record a
        // session grant for the password supplied by the program.
        match &req.0 {
            ClipboardRequest::KittyRead(_) | ClipboardRequest::KittyWrite(_) => {
                // TODO: mark these strings for translation with
                // i18n._() and regenerate the translation files.
                priv_.remember_choice.set_title("Remember choice for this terminal session");
                priv_
                    .remember_choice
                    .set_subtitle("Future requests with the same token will be allowed");
            }
            ClipboardRequest::Osc52Read(_)
            | ClipboardRequest::Osc52Write(_)
            | ClipboardRequest::Paste(_) => {}
            ClipboardRequest::List(_) => unreachable!(),
        }
    }

    /// Show the representation picked from the dropdown. Its model is
    /// the stack's own page list, so the selection is a `gtk.StackPage`.
    fn parts_selected(&self, dropdown: &gtk4::DropDown) {
        let Some(item) = dropdown.selected_item() else { return };
        let Ok(page) = item.downcast::<gtk4::StackPage>() else { return };
        self.imp().parts_stack.set_visible_child(&page.child());
    }

    fn reveal_button_clicked(&self) {
        let priv_ = self.imp();
        priv_.parts_stack.set_sensitive(true);
        priv_.parts_stack.remove_css_class("blurred");
        priv_.hide_button.set_visible(true);
        priv_.reveal_button.set_visible(false);
    }

    fn hide_button_clicked(&self) {
        let priv_ = self.imp();
        priv_.parts_stack.set_sensitive(false);
        priv_.parts_stack.add_css_class("blurred");
        priv_.hide_button.set_visible(false);
        priv_.reveal_button.set_visible(true);
    }

    /// `signals.confirm.connect`
    pub fn connect_confirm<F: Fn(&Self, bool) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::CONFIRM, false, move |values| {
            f(&values[0].get::<Self>().unwrap(), values[1].get().unwrap());
            None
        })
    }

    /// `signals.deny.connect`
    pub fn connect_deny<F: Fn(&Self, bool) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::DENY, false, move |values| {
            f(&values[0].get::<Self>().unwrap(), values[1].get().unwrap());
            None
        })
    }
}

/// The preview for one representation: the image if it decodes,
/// the text if it can be read as text, and otherwise a note that
/// it can't be previewed.
///
/// Every representation gets a page, an empty one included: an
/// empty representation is how a write clears the clipboard, and
/// its page is the empty text the request would send.
fn preview_part(mime: &[u8], data: &[u8]) -> Part {
    if mime.starts_with(b"image/") {
        if let Some(part) = image_part(data) {
            return part;
        }
    }

    if let Some(part) = text_part(data) {
        return part;
    }

    unpreviewable_part(data.len())
}

/// A picture of the representation, if the data decodes as an
/// image.
fn image_part(data: &[u8]) -> Option<Part> {
    let bytes = glib::Bytes::from(data);

    // TODO: use glycin directly here so untrusted image data
    // is decoded in its sandboxed decoder rather than by
    // GTK's in-process decoders.
    let texture = match gdk::Texture::from_bytes(&bytes) {
        Ok(t) => t,
        Err(err) => {
            log::debug!(
                target: LOG,
                "failed to decode clipboard image preview err={}",
                err.message()
            );
            return None;
        }
    };

    let picture = gtk4::Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk4::ContentFit::Contain);
    picture.add_css_class("clipboard-image");

    // A picture asks for the image's own size: `can-shrink` lowers
    // the minimum to nothing but leaves the natural size at the
    // full resolution, and the stack passes that on. The dialog
    // caps the width itself, so without this a tall image drags
    // the preview to whatever height the window allows -- measured
    // at 942px for an 800x6000 image in a 2400x1408 window, where
    // the same dialog showing text is 184px.
    let clamp = libadwaita::Clamp::new();
    clamp.set_orientation(gtk4::Orientation::Vertical);
    clamp.set_maximum_size(PREVIEW_HEIGHT);
    clamp.set_tightening_threshold(PREVIEW_HEIGHT);
    clamp.set_child(Some(&picture));
    Some(Part { widget: clamp.upcast(), kind: PartKind::Image, buffer: None })
}

/// A scrollable view of the representation, if it can be shown as
/// text at all. A MIME type we don't recognize still gets shown
/// when its data is readable, which is more use to someone
/// deciding whether to allow the request than the type name alone.
///
/// GTK text buffers hold UTF-8 and nothing else, so that is the
/// whole of the test.
fn text_part(data: &[u8]) -> Option<Part> {
    let text = std::str::from_utf8(data).ok()?;

    let text_view = gtk4::TextView::new();
    text_view.set_cursor_visible(false);
    text_view.set_editable(false);
    text_view.set_monospace(true);
    text_view.buffer().set_text(text);

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_child(Some(&text_view));
    Some(Part { widget: scroll.upcast(), kind: PartKind::Text, buffer: Some(text_view.buffer()) })
}

/// A note standing in for a representation we can't show, with its
/// size so the user knows how much data is involved.
fn unpreviewable_part(len: usize) -> Part {
    let bx = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    bx.set_valign(gtk4::Align::Center);

    // TODO: mark this string for translation with i18n._() and
    // regenerate the translation files.
    let label = gtk4::Label::new(Some("No preview available"));
    bx.append(&label);

    let size = glib::format_size(len as u64);
    let size_label = gtk4::Label::new(Some(&size));
    size_label.add_css_class("dim-label");
    bx.append(&size_label);

    Part { widget: bx.upcast(), kind: PartKind::None, buffer: None }
}

/// Copy `str` into a buffer of `buf_len` bytes as a sentinel-terminated
/// string, cutting it short if it doesn't fit. (GTK titles are UTF-8, so
/// the result is converted lossily.)
fn truncate_z(str: &[u8], buf_len: usize) -> String {
    let len = str.len().min(buf_len - 1);
    String::from_utf8_lossy(&str[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use crate::apprt::structs::Clipboard;
    use std::rc::Rc;

    #[test]
    fn blueprint_path() {
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.5/clipboard-confirmation-dialog.ui"
        );
    }

    #[test]
    fn truncate() {
        assert_eq!(truncate_z(b"text/plain", 256), "text/plain");
        assert_eq!(truncate_z(b"abcdef", 4), "abc");
    }

    #[test]
    fn instantiate_and_parts() {
        test_gtk::run(|| {
            let d = ClipboardConfirmationDialog::new();
            d.set_property(
                properties::REQUEST,
                boxed::ClipboardRequest(ClipboardRequest::Paste(Clipboard::Standard)),
            );
            assert_eq!(d.heading().as_deref(), Some("Warning: Potentially Unsafe Paste"));
            assert!(matches!(
                d.get_request().as_ref().map(|r| &r.0),
                Some(ClipboardRequest::Paste(_))
            ));

            // Blur
            d.set_property(properties::BLUR, true);
            assert!(!d.imp().parts_stack.is_sensitive());
            assert!(d.imp().reveal_button.is_visible());
            d.reveal_button_clicked();
            assert!(d.imp().parts_stack.is_sensitive());
            assert!(d.imp().hide_button.is_visible());
            d.hide_button_clicked();
            assert!(d.imp().parts_stack.has_css_class("blurred"));

            // Parts: one binary, one text, one bad image.
            let parts = [
                ClipboardContent { mime: b"application/octet-stream", data: &[0xff, 0xfe] },
                ClipboardContent { mime: b"text/plain", data: b"echo hi" },
                ClipboardContent { mime: b"image/png", data: b"not a png" },
            ];
            d.set_parts(&parts);
            assert!(d.imp().parts_dropdown.is_visible());
            let buf = d.get_clipboard_contents().unwrap();
            let (s, e) = buf.bounds();
            assert_eq!(buf.text(&s, &e, false).as_str(), "echo hi");
            assert_eq!(d.imp().parts_stack.pages().n_items(), 3);

            // Selecting a part shows it.
            d.imp().parts_dropdown.set_selected(1);
            assert_eq!(
                d.imp().parts_stack.visible_child_name().as_deref(),
                Some("part-1")
            );

            // Responses
            let got = Rc::new(std::cell::RefCell::new(Vec::new()));
            let g = got.clone();
            d.connect_confirm(move |_, r| g.borrow_mut().push(("confirm", r)));
            let g = got.clone();
            d.connect_deny(move |_, r| g.borrow_mut().push(("deny", r)));
            d.emit_by_name::<()>("response", &[&"ok"]);
            d.imp().remember_choice.set_active(true);
            d.emit_by_name::<()>("response", &[&"cancel"]);
            assert_eq!(*got.borrow(), vec![("confirm", false), ("deny", true)]);
        });
    }

    #[test]
    fn kitty_request_remember_text() {
        test_gtk::run(|| {
            let d = ClipboardConfirmationDialog::new();
            let req = crate::apprt::structs::KittyRead {
                location: Clipboard::Standard,
                mimes: vec![],
                list: false,
                id: vec![],
                pw: vec![],
                name: Default::default(),
                granted: false,
                terminator: Default::default(),
            };
            d.set_property(
                properties::REQUEST,
                boxed::ClipboardRequest(ClipboardRequest::KittyRead(Box::new(req))),
            );
            assert_eq!(
                d.imp().remember_choice.title().as_str(),
                "Remember choice for this terminal session"
            );
        });
    }
}
