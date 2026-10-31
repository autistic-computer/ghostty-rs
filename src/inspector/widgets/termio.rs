//! Port of `src/inspector/widgets/termio.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * [`Ring`] stores `Option<VTEvent>` because the circular buffer needs a
//!   default value for its slots (Zig: `undefined`) and a cursor snapshot
//!   has no meaningful default.
//! * Events own their strings (Zig: a per-event arena).
//! * `encodeMetadata` uses Zig reflection over the OSC command payloads;
//!   the port spells out each payload's fields with the same rules: strings
//!   as-is, integers in decimal, booleans as `true`/`false`, enums by tag
//!   name, optionals as `(unset)` when null, nested structs by type name
//!   (`std::any::type_name`), and unions as `tag` or `tag=value`.
//! * The text filter (`ImGuiTextFilter`) only exists with the `inspector`
//!   feature; without it there is no UI to set a filter, so every event
//!   passes (as with an empty ImGui filter).
//! * Drawing is only compiled with the `inspector` feature.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::datastruct::circ_buf::{self, CircBuf};
use crate::terminal::parser::{self, ActionTag};
use crate::terminal::screen::Cursor;
use crate::terminal::stream::{self, VtRawError};
use crate::terminal::terminal::{ScrollingRegion, Terminal};
use crate::terminal::{osc, stream::Options};

#[cfg(feature = "inspector")]
use crate::inspector::widgets::{screen, ui};
#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::c;
#[cfg(feature = "inspector")]
use crate::surface::Surface;

#[allow(dead_code)]
const LOG: &str = "inspector_terminal_io";

/// VT event stream inspector widget.
pub struct Stream {
    pub events: Ring,
    pub parser_stream: stream::Stream<VTHandler>,

    /// The currently selected event sequence number for keyboard navigation
    pub selected_event_seq: Option<u32>,

    /// Flag indicating whether we need to scroll to the selected item
    pub need_scroll_to_selected: bool,

    /// Flag indicating whether the selection was made by keyboard
    pub is_keyboard_selection: bool,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("events", &self.events.len())
            .field("selected_event_seq", &self.selected_event_seq)
            .finish_non_exhaustive()
    }
}

impl Stream {
    pub fn init() -> Stream {
        let events = Ring::init(2, None);
        let handler = VTHandler::init();
        let mut options = Options::new(handler);
        options.allocator = Some(osc::Allocator::GLOBAL);
        Stream {
            events,
            parser_stream: stream::Stream::init(options),
            selected_event_seq: None,
            need_scroll_to_selected: false,
            is_keyboard_selection: false,
        }
    }

    pub fn record_pty_read(&mut self, t: &mut Terminal, data: &[u8]) -> Result<(), VtRawError> {
        self.parser_stream.handler.state = Some(State {
            terminal: t as *const Terminal,
            events: &mut self.events as *mut Ring,
        });
        self.parser_stream.next_slice(data);
        self.parser_stream.handler.state = None;
        Ok(())
    }

    /// Format all recorded events as tab-separated text. Events are exported
    /// oldest-first even though the inspector displays the newest event first.
    pub fn format_events(&self) -> String {
        let mut buf = String::new();
        buf.push_str("Seq\tKind\tDescription\n");
        let mut it = self.events.iterator(circ_buf::Direction::Forward);
        while let Some(ev) = it.next() {
            let Some(ev) = ev else { continue };
            let _ = writeln!(buf, "{}\t{}\t{}", ev.seq, ev.kind.as_str(), ev.raw_description);
        }
        buf
    }
}

#[cfg(feature = "inspector")]
impl Stream {
    pub fn draw(&mut self, surface: &mut Surface, palette: &crate::terminal::color::Palette) {
        let popup_filter = c"Filter";

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            // Controls
            {
                let handler = &mut self.parser_stream.handler;
                let pause_play = if !handler.paused {
                    c"Pause##pause_play"
                } else {
                    c"Resume##pause_play"
                };
                if c::ImGui_Button(pause_play.as_ptr()) {
                    handler.paused = !handler.paused;
                }

                c::ImGui_SameLineEx(0.0, (*c::ImGui_GetStyle()).ItemInnerSpacing.x);
                if c::ImGui_Button(c"Filter".as_ptr()) {
                    c::ImGui_OpenPopup(popup_filter.as_ptr(), c::ImGuiPopupFlags_None as _);
                }

                if !self.events.empty() {
                    c::ImGui_SameLineEx(0.0, (*c::ImGui_GetStyle()).ItemInnerSpacing.x);
                    if c::ImGui_Button(c"Clear".as_ptr()) {
                        self.events.clear();
                        self.parser_stream.handler.current_seq = 1;
                    }

                    c::ImGui_SameLineEx(0.0, (*c::ImGui_GetStyle()).ItemInnerSpacing.x);
                    if c::ImGui_Button(c"Copy".as_ptr()) {
                        self.copy_events(surface);
                    }

                    c::ImGui_SameLineEx(0.0, (*c::ImGui_GetStyle()).ItemInnerSpacing.x);
                    if c::ImGui_Button(c"Export to file".as_ptr()) {
                        self.export_events(surface);
                    }
                }
            }

            // Events Table
            if self.events.empty() {
                ui::text("Waiting for events...");
            } else {
                // TODO: Eventually
                // event_table(&self.events);
            }

            {
                c::ImGui_Separator();

                c::ImGui_BeginTable(
                    c"table_vt_events".as_ptr(),
                    3,
                    (c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_Borders) as _,
                );

                ui::setup_column(c"Seq", c::ImGuiTableColumnFlags_WidthFixed);
                ui::setup_column(c"Kind", c::ImGuiTableColumnFlags_WidthFixed);
                ui::setup_column(c"Description", c::ImGuiTableColumnFlags_WidthStretch);

                // Handle keyboard navigation when window is focused
                if c::ImGui_IsWindowFocused(c::ImGuiFocusedFlags_RootAndChildWindows as _) {
                    let key_pressed = get_key_action();

                    match key_pressed {
                        KeyAction::None => {}
                        KeyAction::Up | KeyAction::Down => {
                            // If no event is selected, select the first/last event based on direction
                            match self.selected_event_seq {
                                None => {
                                    if !self.events.empty() {
                                        let mut it = self.events.iterator(if key_pressed == KeyAction::Up {
                                            circ_buf::Direction::Forward
                                        } else {
                                            circ_buf::Direction::Reverse
                                        });
                                        if let Some(Some(ev)) = it.next() {
                                            self.selected_event_seq = Some(ev.seq as u32);
                                        }
                                    }
                                }
                                Some(selected) => {
                                    // Find next/previous event based on current selection
                                    let mut it = self.events.iterator(circ_buf::Direction::Reverse);
                                    match key_pressed {
                                        KeyAction::Down => {
                                            let mut found = false;
                                            while let Some(ev) = it.next() {
                                                let Some(ev) = ev else { continue };
                                                if found {
                                                    self.selected_event_seq = Some(ev.seq as u32);
                                                    break;
                                                }
                                                if ev.seq == selected as usize {
                                                    found = true;
                                                }
                                            }
                                        }
                                        KeyAction::Up => {
                                            let mut prev_seq: Option<usize> = None;
                                            while let Some(ev) = it.next() {
                                                let Some(ev) = ev else { continue };
                                                if ev.seq == selected as usize {
                                                    if let Some(prev) = prev_seq {
                                                        self.selected_event_seq = Some(prev as u32);
                                                        break;
                                                    }
                                                }
                                                prev_seq = Some(ev.seq);
                                            }
                                        }
                                        KeyAction::None => unreachable!(),
                                    }
                                }
                            }

                            // Mark that we need to scroll to the newly selected item
                            self.need_scroll_to_selected = true;
                            self.is_keyboard_selection = true;
                        }
                    }
                }

                let mut it = self.events.iterator_mut(circ_buf::Direction::Reverse);
                while let Some(ev) = it.next() {
                    let Some(ev) = ev.as_mut() else { continue };

                    // Need to push an ID so that our selectable is unique.
                    c::ImGui_PushIDPtr((ev as *const VTEvent).cast());

                    c::ImGui_TableNextRow();
                    c::ImGui_TableNextColumn();

                    // Store the previous selection state to detect changes
                    let was_selected = ev.imgui_selected;

                    // Update selection state based on keyboard navigation
                    if let Some(seq) = self.selected_event_seq {
                        ev.imgui_selected = ev.seq as u32 == seq;
                    }

                    // Handle selectable widget
                    if c::ImGui_SelectableBoolPtr(
                        c"##select".as_ptr(),
                        &mut ev.imgui_selected,
                        c::ImGuiSelectableFlags_SpanAllColumns as _,
                    ) {
                        // If selection state changed, update keyboard navigation state
                        if ev.imgui_selected != was_selected {
                            self.selected_event_seq = if ev.imgui_selected {
                                Some(ev.seq as u32)
                            } else {
                                None
                            };
                            self.is_keyboard_selection = false;
                        }
                    }

                    c::ImGui_SameLine();
                    crate::inspector::widgets::textf!("{}", ev.seq);
                    c::ImGui_TableNextColumn();
                    ui::text(ev.kind.as_str());
                    c::ImGui_TableNextColumn();
                    ui::text(&ev.raw_description);

                    // If the event is selected, we render info about it. For now
                    // we put this in the last column because that's the widest and
                    // imgui has no way to make a column span.
                    if ev.imgui_selected {
                        {
                            screen::cursor_table(&ev.cursor);
                            screen::cursor_style(&ev.cursor, Some(palette));

                            c::ImGui_BeginTable(c"details".as_ptr(), 2, c::ImGuiTableFlags_None as _);
                            {
                                ui::row();
                                ui::col(0);
                                ui::text("Scroll Region");
                                ui::col(1);
                                crate::inspector::widgets::textf!(
                                    "T={} B={} L={} R={}",
                                    ev.scrolling_region.top,
                                    ev.scrolling_region.bottom,
                                    ev.scrolling_region.left,
                                    ev.scrolling_region.right,
                                );
                            }

                            for (key, value) in &ev.metadata {
                                ui::row();
                                ui::next_col();
                                ui::text(key);
                                ui::next_col();
                                ui::text(value);
                            }
                            c::ImGui_EndTable();
                        }

                        // If this is the selected event and scrolling is needed, scroll to it
                        if self.need_scroll_to_selected && self.is_keyboard_selection {
                            c::ImGui_SetScrollHereY(0.5);
                            self.need_scroll_to_selected = false;
                        }
                    }

                    c::ImGui_PopID();
                }

                c::ImGui_EndTable();
            } // table

            if c::ImGui_BeginPopupModal(
                popup_filter.as_ptr(),
                std::ptr::null_mut(),
                c::ImGuiWindowFlags_AlwaysAutoResize as _,
            ) {
                let handler = &mut self.parser_stream.handler;

                ui::text("Changed filter settings will only affect future events.");

                c::ImGui_Separator();

                {
                    c::ImGui_BeginTable(c"table_filter_kind".as_ptr(), 3, c::ImGuiTableFlags_None as _);

                    for &tag in ACTION_TAGS {
                        if tag == ActionTag::ApcPut || tag == ActionTag::DcsPut {
                            continue;
                        }

                        c::ImGui_TableNextColumn();
                        let mut value = !handler.filter_exclude.contains(&tag);
                        if c::ImGui_Checkbox(ui::cstr(tag.as_str()).as_ptr(), &mut value) {
                            if value {
                                handler.filter_exclude.remove(&tag);
                            } else {
                                handler.filter_exclude.insert(tag);
                            }
                        }
                    }

                    c::ImGui_EndTable();
                } // Filter kind table

                c::ImGui_Separator();

                ui::text(
                    "Filter by string. Empty displays all, \"abc\" finds lines\n\
                     containing \"abc\", \"abc,xyz\" finds lines containing \"abc\"\n\
                     or \"xyz\", \"-abc\" excludes lines containing \"abc\".",
                );
                c::ImGuiTextFilter_Draw(&mut handler.filter_text.0, c"##filter_text".as_ptr(), 0.0);

                c::ImGui_Separator();
                if c::ImGui_Button(c"Close".as_ptr()) {
                    c::ImGui_CloseCurrentPopup();
                }

                c::ImGui_EndPopup();
            } // filter popup
        }
    }

    /// Copy all recorded events to the standard clipboard in chronological
    /// order.
    fn copy_events(&self, surface: &mut Surface) {
        let contents = self.format_events();

        let content = [crate::apprt::ClipboardContent {
            mime: b"text/plain",
            data: contents.as_bytes(),
        }];
        if let Err(err) = surface
            .rt_surface()
            .set_clipboard(crate::apprt::Clipboard::Standard, &content, false)
        {
            log::error!(target: LOG, "failed to copy terminal IO events err={err}");
        }
    }

    /// Ask the application runtime to save all recorded events to a file.
    fn export_events(&self, surface: &mut Surface) {
        let contents = self.format_events();

        let handled = match surface.rt_app().perform_action(
            surface.target(),
            crate::apprt::action::Action::ExportTerminalIo(crate::apprt::action::ExportTerminalIo {
                contents: contents.as_bytes(),
            }),
        ) {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "failed to export terminal IO events err={err}");
                return;
            }
        };
        if !handled {
            log::warn!(target: LOG, "application runtime does not support exporting terminal IO events");
        }
    }
}

/// All parser action tags in declaration order (Zig:
/// `@typeInfo(Action.Tag).enum.field_names`).
#[allow(dead_code)]
const ACTION_TAGS: &[ActionTag] = &[
    ActionTag::Print,
    ActionTag::Execute,
    ActionTag::CsiDispatch,
    ActionTag::EscDispatch,
    ActionTag::OscDispatch,
    ActionTag::DcsHook,
    ActionTag::DcsPut,
    ActionTag::DcsUnhook,
    ActionTag::ApcStart,
    ActionTag::ApcPut,
    ActionTag::ApcEnd,
];

/// Helper function to check keyboard state and determine navigation action.
#[cfg(feature = "inspector")]
fn get_key_action() -> KeyAction {
    let keys = [
        (c::ImGuiKey_J, KeyAction::Down),
        (c::ImGuiKey_DownArrow, KeyAction::Down),
        (c::ImGuiKey_K, KeyAction::Up),
        (c::ImGuiKey_UpArrow, KeyAction::Up),
    ];

    for (key, action) in keys {
        // SAFETY: requires the ImGui context the render callback set up.
        if unsafe { c::ImGui_IsKeyPressed(key as _) } {
            return action;
        }
    }
    KeyAction::None
}

#[cfg(feature = "inspector")]
pub fn event_table(events: &Ring) {
    if !ui::begin_table(c"events", 3, c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_Borders) {
        return;
    }

    ui::setup_column(c"Seq", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Kind", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Description", c::ImGuiTableColumnFlags_WidthStretch);

    let mut it = events.iterator(circ_buf::Direction::Reverse);
    while let Some(ev) = it.next() {
        let Some(ev) = ev else { continue };
        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            // Need to push an ID so that our selectable is unique.
            c::ImGui_PushIDPtr((ev as *const VTEvent).cast());

            c::ImGui_TableNextRow();
            c::ImGui_TableNextColumn();

            c::ImGui_SameLine();
            crate::inspector::widgets::textf!("{}", ev.seq);
            c::ImGui_TableNextColumn();
            ui::text(ev.kind.as_str());
            c::ImGui_TableNextColumn();
            ui::text(&ev.raw_description);

            c::ImGui_PopID();
        }
    }

    ui::end_table();
}

/// Kind of event, for filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Print,
    Execute,
    Csi,
    Esc,
    Osc,
    Dcs,
    Apc,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Print => "print",
            Kind::Execute => "execute",
            Kind::Csi => "csi",
            Kind::Esc => "esc",
            Kind::Osc => "osc",
            Kind::Dcs => "dcs",
            Kind::Apc => "apc",
        }
    }
}

/// Event metadata, keyed by field name.
pub type Metadata = HashMap<String, String>;

/// Circular buffer of VT events.
pub type Ring = CircBuf<Option<VTEvent>>;

/// VT event. This isn't public in Zig because this is just how we store
/// internal events.
#[derive(Debug, Clone)]
pub struct VTEvent {
    /// Sequence number, just monotonically increasing and wrapping if
    /// it ever overflows. It gives us a nice way to visualize progress.
    pub seq: usize,

    /// Kind of event, for filtering
    pub kind: Kind,

    /// The description of the raw event in a more human-friendly format.
    /// For example for control sequences this is the full sequence but
    /// control characters are replaced with human-readable names, e.g.
    /// 0x07 (bell) becomes BEL.
    pub raw_description: String,

    /// Various metadata at the time of the event (before processing).
    pub cursor: Cursor,
    pub scrolling_region: ScrollingRegion,
    pub metadata: Metadata,

    /// imgui selection state
    pub imgui_selected: bool,
}

impl VTEvent {
    /// Initialize the event information for the given parser action.
    pub fn init(t: &Terminal, action: &parser::Action<'_>) -> VTEvent {
        let mut md = Metadata::new();
        let mut desc = String::new();
        encode_action(&mut desc, &mut md, action);

        let kind = match action {
            parser::Action::Print(_) => Kind::Print,
            parser::Action::Execute(_) => Kind::Execute,
            parser::Action::CsiDispatch(_) => Kind::Csi,
            parser::Action::EscDispatch(_) => Kind::Esc,
            parser::Action::OscDispatch(_) => Kind::Osc,
            parser::Action::DcsHook(_) | parser::Action::DcsPut(_) | parser::Action::DcsUnhook => Kind::Dcs,
            parser::Action::ApcStart | parser::Action::ApcPut(_) | parser::Action::ApcEnd => Kind::Apc,
        };

        VTEvent {
            seq: 1,
            kind,
            raw_description: desc,
            cursor: t.screens.active.cursor.clone(),
            scrolling_region: t.scrolling_region,
            metadata: md,
            imgui_selected: false,
        }
    }

    /// Returns true if the event passes the given filter.
    pub fn pass_filter(&self, filter: &TextFilter) -> bool {
        // Check our main string
        if filter.pass_filter(&self.raw_description) {
            return true;
        }

        // We also check all metadata keys and values
        for (key, value) in &self.metadata {
            if filter.pass_filter(key) {
                return true;
            }
            if filter.pass_filter(value) {
                return true;
            }
        }

        false
    }
}

/// Encode a parser action as a string that we show in the logs.
fn encode_action(writer: &mut String, md: &mut Metadata, action: &parser::Action<'_>) {
    match action {
        parser::Action::Print(ch) => encode_print(writer, *ch),
        parser::Action::Execute(ch) => encode_execute(writer, *ch),
        parser::Action::CsiDispatch(v) => encode_csi(writer, v),
        parser::Action::EscDispatch(v) => encode_esc(writer, v),
        parser::Action::OscDispatch(v) => encode_osc(writer, md, v),
        _ => {
            let _ = write!(writer, "{action}");
        }
    }
}

fn encode_print(writer: &mut String, ch: u32) {
    let c = char::from_u32(ch).unwrap_or(char::REPLACEMENT_CHARACTER);
    let _ = write!(writer, "'{c}' (U+{ch:X})");
}

fn encode_execute(writer: &mut String, ch: u8) {
    writer.push_str(match ch {
        0x00 => "NUL",
        0x01 => "SOH",
        0x02 => "STX",
        0x03 => "ETX",
        0x04 => "EOT",
        0x05 => "ENQ",
        0x06 => "ACK",
        0x07 => "BEL",
        0x08 => "BS",
        0x09 => "HT",
        0x0A => "LF",
        0x0B => "VT",
        0x0C => "FF",
        0x0D => "CR",
        0x0E => "SO",
        0x0F => "SI",
        _ => "?",
    });
    let _ = write!(writer, " (0x{ch:X})");
}

fn encode_csi(writer: &mut String, csi: &parser::Csi<'_>) {
    for &v in csi.intermediates {
        let _ = write!(writer, "{} ", v as char);
    }
    for (i, v) in csi.params.iter().enumerate() {
        if i != 0 {
            writer.push(';');
        }
        let _ = write!(writer, "{v}");
    }
    if !csi.intermediates.is_empty() || !csi.params.is_empty() {
        writer.push(' ');
    }
    writer.push(csi.r#final as char);
}

fn encode_esc(writer: &mut String, esc: &parser::Esc<'_>) {
    for &v in esc.intermediates {
        let _ = write!(writer, "{} ", v as char);
    }
    writer.push(esc.r#final as char);
}

fn encode_osc(writer: &mut String, md: &mut Metadata, osc: &osc::Command<'_>) {
    // The description is just the tag
    let _ = write!(writer, "{} ", tag_name(&osc.key()));

    // Add additional fields to metadata
    encode_metadata(md, osc);
}

/// The Zig `@tagName` of a fieldless enum value, derived from its Debug
/// name (`FreshLine` -> `fresh_line`).
pub fn tag_name(v: &impl std::fmt::Debug) -> String {
    let name = format!("{v:?}");
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// A metadata value (Zig: `encodeMetadataSingle`'s per-type rules).
enum Value<'a> {
    Str(&'a [u8]),
    Int(i64),
    Bool(bool),
    /// An enum, by tag name.
    Enum(String),
    /// A nested struct, by type name.
    Struct(&'static str),
    /// An optional that is null.
    Unset,
}

fn put(md: &mut Metadata, key: &str, value: Value<'_>) {
    let s = match value {
        Value::Str(v) => String::from_utf8_lossy(v).into_owned(),
        Value::Int(v) => v.to_string(),
        Value::Bool(v) => if v { "true" } else { "false" }.to_string(),
        Value::Enum(v) => v,
        Value::Struct(name) => name.to_string(),
        Value::Unset => "(unset)".to_string(),
    };
    md.insert(key.to_string(), s);
}

fn opt_str<'a>(v: Option<&'a [u8]>) -> Value<'a> {
    v.map_or(Value::Unset, Value::Str)
}

fn opt_bool(v: Option<bool>) -> Value<'static> {
    v.map_or(Value::Unset, Value::Bool)
}

fn type_name_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

fn encode_metadata(md: &mut Metadata, osc: &osc::Command<'_>) {
    use osc::Command as C;
    match osc {
        // void
        C::Invalid | C::HyperlinkEnd | C::ConemuWaitInput => {}

        // strings
        C::ChangeWindowTitle(v)
        | C::ChangeWindowIcon(v)
        | C::ConemuShowMessageBox(v)
        | C::ConemuGuimacro(v)
        | C::ConemuRunProcess(v)
        | C::ConemuOutputEnvironmentVariable(v)
        | C::ConemuComment(v) => put(md, "data", Value::Str(v)),

        // structs
        C::SemanticPrompt(v) => {
            put(md, "action", Value::Enum(tag_name(&v.action)));
            put(md, "options_unvalidated", Value::Str(v.options_unvalidated));
        }
        C::ClipboardContents(v) => {
            put(md, "kind", Value::Int(v.kind as i64));
            put(md, "data", Value::Str(v.data));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::ReportPwd(v) => put(md, "value", Value::Str(v.value)),
        C::MouseShape(v) => put(md, "value", Value::Str(v.value)),
        C::ColorOperation(v) => {
            put(md, "op", Value::Enum(tag_name(&v.op)));
            put(md, "requests", Value::Struct(type_name_of(&v.requests)));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::KittyColorProtocol(v) => {
            put(md, "list", Value::Struct(type_name_of(&v.list)));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::ShowDesktopNotification(v) => {
            put(md, "title", Value::Str(v.title));
            put(md, "body", Value::Str(v.body));
        }
        C::HyperlinkStart(v) => {
            put(md, "id", opt_str(v.id));
            put(md, "uri", Value::Str(v.uri));
        }
        C::ConemuSleep(v) => put(md, "duration_ms", Value::Int(v.duration_ms as i64)),
        C::ConemuChangeTabTitle(v) => match v {
            osc::ConemuChangeTabTitle::Reset => put(md, "data", Value::Str(b"reset")),
            osc::ConemuChangeTabTitle::Value(s) => put(md, "value", Value::Str(s)),
        },
        C::ConemuProgressReport(v) => {
            put(md, "state", Value::Enum(tag_name(&v.state)));
            put(md, "progress", v.progress.map_or(Value::Unset, |p| Value::Int(p as i64)));
        }
        C::ConemuXtermEmulation(v) => {
            put(md, "keyboard", opt_bool(v.keyboard));
            put(md, "output", opt_bool(v.output));
        }
        C::KittyTextSizing(v) => {
            put(md, "scale", Value::Int(v.scale as i64));
            put(md, "width", Value::Int(v.width as i64));
            put(md, "numerator", Value::Int(v.numerator as i64));
            put(md, "denominator", Value::Int(v.denominator as i64));
            put(md, "valign", Value::Enum(tag_name(&v.valign)));
            put(md, "halign", Value::Enum(tag_name(&v.halign)));
            put(md, "text", Value::Str(v.text));
        }
        C::KittyClipboardProtocol(v) => {
            put(md, "metadata", Value::Str(v.metadata));
            put(md, "payload", opt_str(v.payload));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::KittyDndProtocol(v) => {
            put(md, "metadata", Value::Str(v.metadata));
            put(md, "payload", opt_str(v.payload));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::HierarchicalContext(v) => {
            put(md, "action", Value::Enum(tag_name(&v.action)));
            put(md, "id", Value::Str(v.id));
            put(md, "metadata", Value::Str(v.metadata));
        }
        C::KittyDesktopNotification(v) => {
            put(md, "metadata", Value::Str(v.metadata));
            put(md, "payload", Value::Str(v.payload));
            put(md, "terminator", Value::Enum(tag_name(&v.terminator)));
        }
        C::Unknown(v) => {
            put(md, "content", Value::Str(v.content));
            put(md, "truncated", Value::Bool(v.truncated));
        }

        // unions
        C::ProgramStatus(v) => match v {
            osc::parsers::program_status::Command::Query(t) => put(md, "query", Value::Enum(tag_name(t))),
            osc::parsers::program_status::Command::Report(r) => put(md, "report", Value::Struct(type_name_of(r))),
        },
    }
}

/// The ImGui text filter of the VT handler.
#[cfg(feature = "inspector")]
pub struct TextFilter(pub c::ImGuiTextFilter);

#[cfg(feature = "inspector")]
impl Default for TextFilter {
    fn default() -> Self {
        // SAFETY: an all-zero ImGuiTextFilter is the empty filter (empty
        // input buffer, empty filter vector, no grep count), the same as
        // Zig's `.{}`.
        TextFilter(unsafe { std::mem::zeroed() })
    }
}

#[cfg(feature = "inspector")]
impl Drop for TextFilter {
    fn drop(&mut self) {
        let data = self.0.Filters.Data;
        if !data.is_null() {
            // SAFETY: the vector storage was allocated by ImGui's allocator
            // in `ImGuiTextFilter_Build`.
            unsafe { c::ImGui_MemFree(data.cast()) };
        }
    }
}

#[cfg(feature = "inspector")]
impl TextFilter {
    fn pass_filter(&self, text: &str) -> bool {
        let r = text.as_bytes().as_ptr_range();
        // SAFETY: the range is valid for the call; the filter only reads.
        unsafe { c::ImGuiTextFilter_PassFilter(&self.0, r.start.cast(), r.end.cast()) }
    }
}

/// The text filter of the VT handler. Without the `inspector` feature
/// there is no UI to set it, so it is always empty and passes everything.
#[cfg(not(feature = "inspector"))]
#[derive(Debug, Default)]
pub struct TextFilter;

#[cfg(not(feature = "inspector"))]
impl TextFilter {
    fn pass_filter(&self, _text: &str) -> bool {
        true
    }
}

/// The capture state of a [`VTHandler`].
#[derive(Debug, Clone, Copy)]
pub struct State {
    /// The terminal state at the time of the event.
    pub terminal: *const Terminal,

    /// The event ring to write events to.
    pub events: *mut Ring,
}

/// Exclude certain actions by tag.
pub type ActionTagSet = HashSet<ActionTag>;

/// Our VT stream handler for the Stream widget. This isn't public in Zig
/// because there is no reason to use this directly.
pub struct VTHandler {
    /// The capture state, must be set before use. If None, then
    /// events are dropped.
    pub state: Option<State>,

    /// True to pause this artificially.
    pub paused: bool,

    /// Current sequence number
    pub current_seq: usize,

    /// Exclude certain actions by tag.
    pub filter_exclude: ActionTagSet,

    /// Boxed because a built ImGui filter points into its own buffer.
    pub filter_text: Box<TextFilter>,
}

impl VTHandler {
    pub fn init() -> VTHandler {
        VTHandler {
            state: None,
            paused: false,
            current_seq: 1,
            filter_exclude: [ActionTag::Print].into_iter().collect(),
            filter_text: Box::default(),
        }
    }
}

impl stream::Handler for VTHandler {
    const HAS_VT_RAW: bool = true;

    fn vt(&mut self, _action: stream::Action<'_>) {}

    /// This is called with every single terminal action.
    fn vt_raw(&mut self, action: &parser::Action<'_>) -> Result<bool, VtRawError> {
        let Some(state) = self.state else {
            return Ok(true);
        };

        // We always increment the sequence number, even if we're paused or
        // filter out the event. This helps show the user that there is a gap
        // between events and roughly how large that gap was.
        let seq = self.current_seq;
        self.current_seq = self.current_seq.wrapping_add(1);

        // If we're manually paused, we ignore all events.
        if self.paused {
            return Ok(true);
        }

        // We ignore certain action types that are too noisy.
        if matches!(action, parser::Action::DcsPut(_) | parser::Action::ApcPut(_)) {
            return Ok(true);
        }

        // If we requested a specific type to be ignored, ignore it.
        // We return true because we did "handle" it by ignoring it.
        if self.filter_exclude.contains(&action.tag()) {
            return Ok(true);
        }

        // Build our event
        // SAFETY: `record_pty_read` sets the state to the live terminal and
        // event ring for the duration of the parse.
        let (terminal, vt_events) = unsafe { (&*state.terminal, &mut *state.events) };
        let mut ev = VTEvent::init(terminal, action);
        ev.seq = seq;

        // Check if the event passes the filter
        if !ev.pass_filter(&self.filter_text) {
            return Ok(true);
        }

        const MAX_CAPACITY: usize = 100;
        append_capped(vt_events, Some(ev), MAX_CAPACITY);

        // Do NOT skip it, because we want to record more information
        // about this event.
        Ok(false)
    }
}

/// Append to a ring that grows (doubling) up to `max_capacity` and then
/// drops its oldest entry (the `append catch OutOfMemory` logic shared by
/// the key and VT event rings).
pub fn append_capped<T: Clone>(ring: &mut CircBuf<T>, v: T, max_capacity: usize) {
    if !ring.full {
        ring.append_assume_capacity(v);
    } else if ring.capacity() < max_capacity {
        // We're out of memory, but we can allocate to our capacity.
        let new_capacity = (ring.capacity() * 2).min(max_capacity);
        ring.resize(new_capacity);
        ring.append_assume_capacity(v);
    } else {
        ring.delete_oldest(1);
        ring.append_assume_capacity(v);
    }
}

/// Enum representing keyboard navigation actions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum KeyAction {
    Down,
    None,
    Up,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::terminal::Options as TerminalOptions;

    fn term() -> Terminal {
        Terminal::init(TerminalOptions::new(80, 24)).unwrap()
    }

    fn descriptions(s: &Stream) -> Vec<(usize, &'static str, String)> {
        let mut out = Vec::new();
        let mut it = s.events.iterator(circ_buf::Direction::Forward);
        while let Some(ev) = it.next() {
            let ev = ev.as_ref().unwrap();
            out.push((ev.seq, ev.kind.as_str(), ev.raw_description.clone()));
        }
        out
    }

    #[test]
    fn record_pty_read_events() {
        let mut t = term();
        let mut s = Stream::init();
        s.record_pty_read(&mut t, b"\x1b[1;2H\x1b(B\x1b]0;hi\x07").unwrap();

        assert_eq!(
            descriptions(&s),
            vec![
                (1, "csi", "1;2 H".to_string()),
                (2, "esc", "( B".to_string()),
                (3, "osc", "change_window_title ".to_string()),
            ]
        );

        let mut it = s.events.iterator(circ_buf::Direction::Reverse);
        let osc = it.next().unwrap().as_ref().unwrap();
        assert_eq!(osc.metadata.get("data").map(String::as_str), Some("hi"));
        // The cursor is a snapshot of the terminal, which only the termio
        // stream (not the inspector) updates.
        assert_eq!(osc.cursor.x, 0);

        // Without a state nothing is recorded.
        s.parser_stream.next_slice(b"\x1b[H");
        assert_eq!(s.events.len(), 3);
    }

    #[test]
    fn ground_text_bypasses_vt_raw() {
        // Like Zig, printable text and C0 controls in the ground state
        // are handled by the stream's UTF-8 path, not the parser, so
        // they never reach the raw hook. C0 controls the parser
        // executes (e.g. inside an escape sequence) are recorded.
        let mut t = term();
        let mut s = Stream::init();
        s.parser_stream.handler.filter_exclude.clear();
        s.record_pty_read(&mut t, b"A\x07").unwrap();
        assert!(s.events.empty());
        s.record_pty_read(&mut t, b"\x1b\x07").unwrap();
        assert_eq!(descriptions(&s), vec![(1, "execute", "BEL (0x7)".to_string())]);
    }

    #[test]
    fn paused_counts_seq() {
        let mut t = term();
        let mut s = Stream::init();
        s.parser_stream.handler.paused = true;
        s.record_pty_read(&mut t, b"\x1b[H\x1b[H").unwrap();
        assert!(s.events.empty());
        s.parser_stream.handler.paused = false;
        s.record_pty_read(&mut t, b"\x1b[H").unwrap();
        assert_eq!(descriptions(&s), vec![(3, "csi", "H".to_string())]);
    }

    #[test]
    fn excluded_kind_counts_seq() {
        let mut t = term();
        let mut s = Stream::init();
        s.parser_stream.handler.filter_exclude.insert(ActionTag::CsiDispatch);
        s.record_pty_read(&mut t, b"\x1b[H\x1b7").unwrap();
        assert_eq!(descriptions(&s), vec![(2, "esc", "7".to_string())]);
    }

    #[test]
    fn ring_capacity() {
        let mut t = term();
        let mut s = Stream::init();
        for _ in 0..150 {
            s.record_pty_read(&mut t, b"\x1b[H").unwrap();
        }
        assert_eq!(s.events.capacity(), 100);
        assert_eq!(s.events.len(), 100);
        assert_eq!(descriptions(&s)[0].0, 51);
        assert_eq!(descriptions(&s)[99].0, 150);
    }

    #[test]
    fn format_events_export() {
        let mut t = term();
        let mut s = Stream::init();
        s.record_pty_read(&mut t, b"\x1b[H\x1b7").unwrap();
        assert_eq!(s.format_events(), "Seq\tKind\tDescription\n1\tcsi\tH\n2\tesc\t7\n");
    }

    #[test]
    fn osc_metadata() {
        let mut t = term();
        let mut s = Stream::init();
        s.record_pty_read(&mut t, b"\x1b]8;id=x;http://a\x07\x1b]8;;\x07").unwrap();
        let mut it = s.events.iterator(circ_buf::Direction::Forward);
        let start = it.next().unwrap().as_ref().unwrap();
        assert_eq!(start.raw_description, "hyperlink_start ");
        assert_eq!(start.metadata.get("id").map(String::as_str), Some("x"));
        assert_eq!(start.metadata.get("uri").map(String::as_str), Some("http://a"));
        let end = it.next().unwrap().as_ref().unwrap();
        assert_eq!(end.raw_description, "hyperlink_end ");
        assert!(end.metadata.is_empty());
    }

    #[test]
    fn encoders() {
        let mut s = String::new();
        encode_print(&mut s, 'A' as u32);
        assert_eq!(s, "'A' (U+41)");
        s.clear();
        encode_execute(&mut s, 0x0A);
        assert_eq!(s, "LF (0xA)");
        s.clear();
        encode_execute(&mut s, 0x1C);
        assert_eq!(s, "? (0x1C)");
    }

    #[test]
    fn tag_names() {
        assert_eq!(tag_name(&osc::Key::ChangeWindowTitle), "change_window_title");
        assert_eq!(tag_name(&osc::Terminator::St), "st");
    }
}
