//! Port of `src/inspector/widgets/pagelist.zig`.
//!
//! STATUS: ported (only compiled with the `inspector` feature)
#![cfg(feature = "inspector")]

use crate::inspector::widgets::termio::tag_name;
use crate::inspector::widgets::{self, help_marker, textf, ui};
use crate::pkg::dcimgui::c;
use crate::terminal::page::{Cell, Page, Row};
use crate::terminal::page_list::{self, LimitKey, PageList, Scroll, Scrollbar, Storage};
use crate::terminal::point::{Coordinate, Point, Tag};
use crate::terminal::size::CellCountInt;
use crate::terminal::style as stylepkg;

/// All point tags in declaration order (Zig: `std.meta.tags(point.Tag)`).
const POINT_TAGS: [Tag; 4] = [Tag::Active, Tag::Viewport, Tag::Screen, Tag::History];

/// Cell pointers resolved against a Node.PreservedPage rather than the
/// PageList node.
/// Omitting the node makes it impossible for inspector helpers to
/// accidentally restore the original compressed page.
#[derive(Clone, Copy)]
struct InspectedCell<'a> {
    row: &'a Row,
    cell: &'a Cell,
    row_idx: CellCountInt,
    col_idx: CellCountInt,
}

/// PageList inspector widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inspector;

impl Inspector {
    pub const EMPTY: Inspector = Inspector;

    pub fn draw(&self, pages: &mut PageList) {
        ui::text_wrapped(
            "PageList manages the backing pages that hold scrollback and the active \
             terminal grid. Each page is a contiguous memory buffer with its \
             own rows, cells, style set, grapheme map, and hyperlink storage.",
        );

        if ui::collapsing_header(c"Overview", c::ImGuiTreeNodeFlags_DefaultOpen) {
            summary_table(pages);
        }

        if ui::collapsing_header(c"Page Compression", c::ImGuiTreeNodeFlags_DefaultOpen) {
            compression_table(pages);
        }

        if ui::collapsing_header(c"Scrollbar & Regions", c::ImGuiTreeNodeFlags_DefaultOpen) {
            ui::separator_text(c"Scrollbar");
            scrollbar_info(pages);
            ui::separator_text(c"Regions");
            regions_table(pages);
        }

        if ui::collapsing_header(c"Tracked Pins", c::ImGuiTreeNodeFlags_DefaultOpen) {
            tracked_pins_table(pages);
        }

        if ui::collapsing_header(c"Pages", c::ImGuiTreeNodeFlags_DefaultOpen) {
            help_marker(
                "Pages are shown most-recent first. Each page holds a grid of rows/cells \
                 plus metadata tables for styles, graphemes, strings, and hyperlinks.",
            );

            let active_pin = pages.get_top_left(Tag::Active);
            let viewport_pin = pages.get_top_left(Tag::Viewport);

            let mut row_offset = pages.total_rows;
            let mut index: usize = pages.total_pages();
            let mut node = pages.pages.last;
            while let Some(node_ptr) = node {
                // SAFETY: nodes of the list are live while we hold the
                // terminal lock.
                let page_node = unsafe { &mut *node_ptr.as_ptr() };
                node = page_node.prev;

                let rows = page_node.rows();
                let compressed = page_node.storage() == Storage::Compressed;
                let dirty = page_node.page_if_resident().map(|page| page.is_dirty());
                row_offset -= rows as usize;
                index -= 1;

                // We use our location as the ID so that even if reallocations
                // happen we remain open if we're open already.
                // SAFETY: requires the ImGui context the render callback set up.
                unsafe { c::ImGui_PushIDInt(index as i32) };

                // Open up the tree node.
                if widgets::page::tree_node(widgets::page::TreeNodeState {
                    cols: page_node.cols(),
                    rows,
                    index,
                    row_range: [row_offset, row_offset + rows as usize - 1],
                    active: node_ptr == active_pin.node,
                    viewport: node_ptr == viewport_pin.node,
                    compressed,
                    dirty,
                }) {
                    // Decode compressed contents into a temporary page. The
                    // original node stays compressed while its entry is open.
                    match page_node.page_preserving_state(page_list::Allocator::page()) {
                        Ok(mut preserved) => {
                            widgets::page::inspector(preserved.page());
                            preserved.deinit();
                        }
                        Err(_) => ui::text_disabled("(unable to copy compressed page)"),
                    }

                    // SAFETY: paired with the open tree node.
                    unsafe { c::ImGui_TreePop() };
                }

                // SAFETY: paired with PushIDInt.
                unsafe { c::ImGui_PopID() };
            }
        }
    }
}

const TABLE_FLAGS: c::ImGuiTableFlags_ =
    c::ImGuiTableFlags_BordersInnerV | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit;

/// A label / help marker / value row of a three column table.
fn info_row(label: &str, help: &str, value: &str) {
    ui::row();
    ui::col(0);
    ui::text(label);
    ui::col(1);
    help_marker(help);
    ui::col(2);
    ui::text(value);
}

fn summary_table(pages: &PageList) {
    if !ui::begin_table(c"pagelist_summary", 3, TABLE_FLAGS) {
        return;
    }

    info_row(
        "Active Grid",
        "Active viewport size in columns x rows.",
        &format!("{}c x {}r", pages.cols, pages.rows),
    );
    info_row(
        "Pages",
        "Total number of pages in the linked list.",
        &format!("{}", pages.total_pages()),
    );
    info_row(
        "Total Rows",
        "Total rows represented by scrollback + active area.",
        &format!("{}", pages.total_rows),
    );
    info_row(
        "Scrollback Limit",
        "Maximum uncompressed logical page memory before the oldest\n\
         history is evicted. Minimum page allocation sizes can make\n\
         current usage temporarily exceed this value.",
        &format_bytes(pages.limits.max(LimitKey::Bytes)),
    );
    info_row(
        "Viewport",
        "Current viewport anchoring mode.",
        &tag_name(&pages.viewport),
    );

    ui::end_table();
}

fn compression_table(pages: &PageList) {
    let memory = pages.memory_stats();

    if !ui::begin_table(c"pagelist_compression", 3, TABLE_FLAGS) {
        return;
    }

    compression_text_row(
        "Platform Support",
        "Whether this target can discard physical page memory while retaining \
         its virtual address range. The scrollback-compression setting may \
         still disable automatic compression on a supported platform.",
        if crate::terminal::COMPRESSION_ENABLED {
            "supported"
        } else {
            "unsupported"
        },
    );

    let state = format!(
        "{} compressed, {} resident",
        memory.compressed_pages, memory.resident_pages
    );
    compression_text_row(
        "Page States",
        "Compressed pages retain an encoded allocation while their raw mapping \
         is decommitted. Resident pages still have physical raw backing. \
         Active, visible, and recently changed pages are expected to be resident.",
        &state,
    );

    compression_text_row(
        "Uncompressed Size",
        "Total size of all raw page mappings. This is the approximate page \
         backing memory required if no pages were compressed and is also \
         the virtual address space retained across compression.",
        &format_bytes(memory.raw_bytes),
    );

    let storage = if memory.decommitted_raw_bytes > 0 {
        let ratio = percentage(memory.encoded_bytes, memory.decommitted_raw_bytes);
        format!(
            "{} encoded / {} raw ({:.1}%)",
            format_bytes(memory.encoded_bytes),
            format_bytes(memory.decommitted_raw_bytes),
            ratio,
        )
    } else {
        "none".to_string()
    };
    compression_text_row(
        "Compressed Storage",
        "Encoded allocation size compared with the original raw size of only \
         the compressed pages. The percentage is the compression ratio, \
         so smaller is better. Raw physical pages have been discarded.",
        &storage,
    );

    compression_text_row(
        "Estimated Resident",
        "Physical page backing estimated to remain after compression: resident \
         raw allocations, including unused pool tails, plus encoded storage. \
         Node and allocator metadata and unrelated terminal memory are excluded.",
        &format_bytes(memory.estimated_resident_bytes()),
    );

    let savings = memory.estimated_savings();
    let savings_text = format!(
        "{} ({:.1}% of raw page memory)",
        format_bytes(savings),
        percentage(savings, memory.raw_bytes),
    );
    compression_text_row(
        "Estimated Savings",
        "Physical page backing avoided across the complete PageList. This is \
         decommitted raw memory minus replacement encoded storage and is \
         not a measurement of total process RSS.",
        &savings_text,
    );

    ui::end_table();
}

fn compression_text_row(label: &str, help: &str, value: &str) {
    info_row(label, help, value);
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        let value: f64 = bytes as f64 / (1024.0 * 1024.0);
        return format!("{value:.2} MiB");
    }

    if bytes >= 1024 {
        let value: f64 = bytes as f64 / 1024.0;
        return format!("{value:.1} KiB");
    }

    format!("{bytes} B")
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    100.0 * numerator as f64 / denominator as f64
}

fn scrollbar_info(pages: &mut PageList) {
    let scrollbar = pages.scrollbar();

    // If we have a scrollbar, show it.
    if scrollbar.total > 0 {
        let mut delta_row: isize = 0;
        scrollbar_widget(&scrollbar, &mut delta_row);
        if delta_row != 0 {
            pages.scroll(Scroll::DeltaRow(delta_row));
        }
    }

    if !ui::begin_table(c"scrollbar_info", 3, TABLE_FLAGS) {
        return;
    }

    info_row(
        "Total",
        "Total number of scrollable rows including scrollback and active area.",
        &format!("{}", scrollbar.total),
    );
    info_row(
        "Offset",
        "Current scroll position as row offset from the top of scrollback.",
        &format!("{}", scrollbar.offset),
    );
    info_row(
        "Length",
        "Number of rows visible in the viewport.",
        &format!("{}", scrollbar.len),
    );

    ui::end_table();
}

fn regions_table(pages: &PageList) {
    if !ui::begin_table(c"pagelist_regions", 4, TABLE_FLAGS) {
        return;
    }

    ui::setup_column(c"Region", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Top-Left", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Bottom-Right", c::ImGuiTableColumnFlags_WidthStretch);
    // SAFETY: requires an open table.
    unsafe { c::ImGui_TableHeadersRow() };

    for tag in POINT_TAGS {
        region_row(pages, tag);
    }

    ui::end_table();
}

fn region_row(pages: &PageList, tag: Tag) {
    let tl_pin = pages.get_top_left(tag);
    let br_pin = pages.get_bottom_right(tag);

    ui::row();
    ui::col(0);
    ui::text(tag.as_str());

    ui::col(1);
    help_marker(region_help_text(tag));

    ui::col(2);
    if let Some(pt) = pages.point_from_pin(tag, tl_pin) {
        let coord = pt.coord();
        textf!("({}, {})", coord.x, coord.y);
    } else {
        ui::text_disabled("(n/a)");
    }

    ui::col(3);
    if let Some(br) = br_pin {
        if let Some(pt) = pages.point_from_pin(tag, br) {
            let coord = pt.coord();
            textf!("({}, {})", coord.x, coord.y);
        } else {
            ui::text_disabled("(n/a)");
        }
    } else {
        ui::text_disabled("(empty)");
    }
}

fn region_help_text(tag: Tag) -> &'static str {
    match tag {
        Tag::Active => {
            "The active area where a running program can jump the cursor \
             and make changes. This is the 'editable' part of the screen. \
             Bottom-right includes the full height of the screen, including \
             rows that may not be written yet."
        }
        Tag::Viewport => {
            "The visible viewport. If the user has scrolled, top-left changes. \
             Bottom-right is the last written row from the top-left."
        }
        Tag::Screen => {
            "Top-left is the furthest back in scrollback history. Bottom-right \
             is the last written row. Unlike 'active', this only contains \
             written rows."
        }
        Tag::History => {
            "Same top-left as 'screen' but bottom-right is the line just before \
             the top of 'active'. Contains only the scrollback history."
        }
    }
}

fn tracked_pins_table(pages: &PageList) {
    if !ui::begin_table(
        c"tracked_pins",
        5,
        c::ImGuiTableFlags_Borders | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit,
    ) {
        return;
    }

    ui::setup_column(c"Index", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Pin", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Context", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Dirty", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"State", c::ImGuiTableColumnFlags_WidthFixed);
    // SAFETY: requires an open table.
    unsafe { c::ImGui_TableHeadersRow() };

    let active_pin = pages.get_top_left(Tag::Active);
    let viewport_pin = pages.get_top_left(Tag::Viewport);

    for (idx, tracked) in pages.tracked_pins().iter().enumerate() {
        // SAFETY: tracked pins are live while tracked.
        let pin = unsafe { *tracked.as_ptr() };
        ui::row();
        ui::col(0);
        textf!("{idx}");

        ui::col(1);
        let pin_text = format!("({}, {})", pin.x, pin.y);
        if pin.garbage {
            ui::text_colored(ui::vec4(1.0, 0.5, 0.3, 1.0), &pin_text);
        } else {
            ui::text(&pin_text);
        }

        ui::col(2);
        if let Some(pt) = pages.point_from_pin(Tag::Screen, pin) {
            let coord = pt.coord();
            textf!("screen ({}, {})", coord.x, coord.y);
        } else {
            ui::text_disabled("screen (out of range)");
        }

        ui::col(3);
        // SAFETY: the pin's node is live while we hold the terminal lock.
        if let Some(page) = unsafe { (*pin.node.as_ptr()).page_if_resident() } {
            let rac = page.get_row_and_cell(pin.x as usize, pin.y as usize);
            // SAFETY: the row pointer points into the resident page.
            let dirty = page.dirty || unsafe { (*rac.row).dirty() };
            if dirty {
                ui::text_colored(ui::vec4(1.0, 0.4, 0.4, 1.0), "dirty");
            } else {
                ui::text_disabled("clean");
            }
        } else {
            // Dirty state lives in the discarded mapping. Keep inspector
            // traversal metadata-only rather than restoring this page.
            ui::text_disabled("compressed");
        }

        ui::col(4);
        if pin.eql(active_pin) {
            ui::text_colored(ui::vec4(0.4, 0.9, 0.4, 1.0), "active top");
        } else if pin.eql(viewport_pin) {
            ui::text_colored(ui::vec4(0.4, 0.8, 1.0, 1.0), "viewport top");
        } else if pin.garbage {
            ui::text_colored(ui::vec4(1.0, 0.5, 0.3, 1.0), "garbage");
        } else if *tracked == pages.viewport_pin {
            ui::text("viewport pin");
        } else {
            ui::text_disabled("tracked");
        }
    }

    ui::end_table();
}

fn scrollbar_widget(scrollbar: &Scrollbar, delta_row: &mut isize) {
    *delta_row = 0;

    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        let avail_width = c::ImGui_GetContentRegionAvail().x;
        let bar_height: f32 = c::ImGui_GetFrameHeight();
        let cursor_pos = c::ImGui_GetCursorScreenPos();

        let total_f: f32 = scrollbar.total as f32;
        let offset_f: f32 = scrollbar.offset as f32;
        let len_f: f32 = scrollbar.len as f32;

        let grab_start = (offset_f / total_f) * avail_width;
        let grab_width = ((len_f / total_f) * avail_width).max(4.0);

        let draw_list = c::ImGui_GetWindowDrawList();
        let bg_color = c::ImGui_GetColorU32(c::ImGuiCol_ScrollbarBg as _);
        let grab_color = c::ImGui_GetColorU32(c::ImGuiCol_ScrollbarGrab as _);

        let bg_min: c::ImVec2 = cursor_pos;
        let bg_max = ui::vec2(cursor_pos.x + avail_width, cursor_pos.y + bar_height);
        c::ImDrawList_AddRectFilledEx(draw_list, bg_min, bg_max, bg_color, 0.0, 0);

        let grab_min = ui::vec2(cursor_pos.x + grab_start, cursor_pos.y);
        let grab_max = ui::vec2(cursor_pos.x + grab_start + grab_width, cursor_pos.y + bar_height);
        c::ImDrawList_AddRectFilledEx(draw_list, grab_min, grab_max, grab_color, 0.0, 0);
        c::ImGui_InvisibleButton(c"scrollbar_drag".as_ptr(), ui::vec2(avail_width, bar_height), 0);
        if c::ImGui_IsItemActive() {
            let drag_delta = c::ImGui_GetMouseDragDelta(c::ImGuiMouseButton_Left as _, 0.0);
            if drag_delta.x != 0.0 {
                let row_delta = (drag_delta.x / avail_width) * total_f;
                *delta_row = row_delta as isize;
                c::ImGui_ResetMouseDragDelta();
            }
        }

        if c::ImGui_IsItemHovered(c::ImGuiHoveredFlags_DelayShort as _) {
            ui::set_tooltip(&format!(
                "offset={} len={} total={}",
                scrollbar.offset, scrollbar.len, scrollbar.total
            ));
        }
    }
}

/// Grid inspector widget for choosing and inspecting a specific cell.
#[derive(Debug, Clone, Copy)]
pub struct CellChooser {
    pub lookup_region: Tag,
    pub lookup_coord: Coordinate,
    pub cell_info: CellInfo,
}

impl CellChooser {
    pub const EMPTY: CellChooser = CellChooser {
        lookup_region: Tag::Viewport,
        lookup_coord: Coordinate { x: 0, y: 0 },
        cell_info: CellInfo::EMPTY,
    };

    pub fn draw(&mut self, pages: &PageList) {
        ui::text_wrapped(
            "Inspect a cell by choosing a coordinate space and entering the X/Y position. \
             The inspector resolves the point into the page list and displays the cell contents.",
        );

        ui::separator_text(c"Cell Inspector");

        let region_max = max_coord(pages, self.lookup_region);
        if let Some(coord) = region_max {
            self.lookup_coord.x = self.lookup_coord.x.min(coord.x);
            self.lookup_coord.y = self.lookup_coord.y.min(coord.y);
        } else {
            self.lookup_coord = Coordinate { x: 0, y: 0 };
        }

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            let disabled = region_max.is_none();
            c::ImGui_BeginDisabled(disabled);

            let preview = ui::cstr(self.lookup_region.as_str());
            let combo_width = POINT_TAGS.iter().map(|t| t.as_str().len()).max().unwrap_or(0) + 4;
            let xs = ui::cstr(&"X".repeat(combo_width));
            c::ImGui_SetNextItemWidth(c::ImGui_CalcTextSize(xs.as_ptr()).x);
            if c::ImGui_BeginCombo(
                c"##grid_region".as_ptr(),
                preview.as_ptr(),
                c::ImGuiComboFlags_HeightSmall as _,
            ) {
                for tag in POINT_TAGS {
                    let selected = tag == self.lookup_region;
                    if c::ImGui_SelectableEx(
                        ui::cstr(tag.as_str()).as_ptr(),
                        selected,
                        c::ImGuiSelectableFlags_None as _,
                        ui::vec2(0.0, 0.0),
                    ) {
                        self.lookup_region = tag;
                    }
                    if selected {
                        c::ImGui_SetItemDefaultFocus();
                    }
                }
                c::ImGui_EndCombo();
            }

            c::ImGui_SameLine();

            let width = c::ImGui_CalcTextSize(c"00000".as_ptr()).x;
            let mut x_value: CellCountInt = self.lookup_coord.x;
            let mut y_value: u32 = self.lookup_coord.y;
            let mut changed = false;

            c::ImGui_AlignTextToFramePadding();
            ui::text("x:");
            c::ImGui_SameLine();
            c::ImGui_SetNextItemWidth(width);
            if c::ImGui_InputScalar(
                c"##grid_x".as_ptr(),
                c::ImGuiDataType_U16 as _,
                (&mut x_value as *mut CellCountInt).cast(),
            ) {
                changed = true;
            }

            c::ImGui_SameLine();
            c::ImGui_AlignTextToFramePadding();
            ui::text("y:");
            c::ImGui_SameLine();
            c::ImGui_SetNextItemWidth(width);
            if c::ImGui_InputScalar(
                c"##grid_y".as_ptr(),
                c::ImGuiDataType_U32 as _,
                (&mut y_value as *mut u32).cast(),
            ) {
                changed = true;
            }

            c::ImGui_SameLine();
            help_marker("Choose the coordinate space and X/Y position (0-indexed).");

            if changed {
                if let Some(coord) = region_max {
                    self.lookup_coord.x = x_value.min(coord.x);
                    self.lookup_coord.y = y_value.min(coord.y);
                }
            }

            c::ImGui_EndDisabled();
        }

        if let Some(coord) = region_max {
            ui::text_disabled(&format!("Range: x 0..{}, y 0..{}", coord.x, coord.y));
        } else {
            ui::text_disabled("(region has no rows)");
            return;
        }

        let pt = match self.lookup_region {
            Tag::Active => Point::Active(self.lookup_coord),
            Tag::Viewport => Point::Viewport(self.lookup_coord),
            Tag::Screen => Point::Screen(self.lookup_coord),
            Tag::History => Point::History(self.lookup_coord),
        };

        let Some(pin) = pages.pin(pt) else {
            ui::text_disabled("(cell out of range)");
            return;
        };

        // Cell pointers must come from the same page whose auxiliary tables
        // we inspect. For compressed nodes this is an independent preserved
        // page, leaving the PageList node compressed across inspector frames.
        // SAFETY: the pin's node is live while we hold the terminal lock.
        let preserved = unsafe { pin.node.as_ref() }.page_preserving_state(page_list::Allocator::page());
        let mut preserved = match preserved {
            Ok(v) => v,
            Err(_) => {
                ui::text_disabled("(unable to copy compressed page)");
                return;
            }
        };

        {
            let page = preserved.page();
            let rac = page.get_row_and_cell(pin.x as usize, pin.y as usize);
            // SAFETY: the pointers point into `page`, which outlives this
            // block.
            let cell = InspectedCell {
                row: unsafe { &*rac.row },
                cell: unsafe { &*rac.cell },
                row_idx: pin.y,
                col_idx: pin.x,
            };

            self.cell_info.draw(cell, pt, page);

            if cell.cell.style_id() != stylepkg::DEFAULT_ID {
                ui::separator_text(c"Style");
                // SAFETY: the style ID is in use by this cell of the page.
                let style = unsafe { *page.styles.get(page.memory, cell.cell.style_id()) };
                widgets::style::table(style, None);
            }

            if cell.cell.hyperlink() {
                ui::separator_text(c"Hyperlink");
                hyperlink_table(cell, page);
            }

            if cell.cell.has_grapheme() {
                ui::separator_text(c"Grapheme");
                grapheme_table(cell, page);
            }
        }

        preserved.deinit();
    }
}

fn max_coord(pages: &PageList, tag: Tag) -> Option<Coordinate> {
    let br_pin = pages.get_bottom_right(tag)?;
    let br_point = pages.point_from_pin(tag, br_pin)?;
    Some(br_point.coord())
}

fn hyperlink_table(cell: InspectedCell<'_>, page: &Page) {
    if !ui::begin_table(c"cell_hyperlink", 2, c::ImGuiTableFlags_None) {
        return;
    }

    let Some(link_id) = page.lookup_hyperlink(cell.cell) else {
        ui::row();
        ui::col(0);
        ui::text("Status");
        ui::col(1);
        ui::text_disabled("(missing link data)");
        ui::end_table();
        return;
    };

    // SAFETY: the link ID is in use by this cell of the page.
    let entry = unsafe { &*page.hyperlink_set.get(page.memory, link_id) };

    ui::row();
    ui::col(0);
    ui::text("ID");
    ui::col(1);
    match entry.id {
        crate::terminal::hyperlink::PageEntryId::Implicit(value) => textf!("implicit {value}"),
        crate::terminal::hyperlink::PageEntryId::Explicit(slice) => {
            // SAFETY: the string lives in this page's memory.
            let id = unsafe { slice.slice(page.memory) };
            if id.is_empty() {
                ui::text_disabled("(empty)");
            } else {
                ui::text(&String::from_utf8_lossy(id));
            }
        }
    }

    ui::row();
    ui::col(0);
    ui::text("URI");
    ui::col(1);
    // SAFETY: the string lives in this page's memory.
    let uri = unsafe { entry.uri.slice(page.memory) };
    if uri.is_empty() {
        ui::text_disabled("(empty)");
    } else {
        ui::text(&String::from_utf8_lossy(uri));
    }

    ui::row();
    ui::col(0);
    ui::text("Ref Count");
    ui::col(1);
    // SAFETY: the link ID is in use by this cell of the page.
    let refs = unsafe { page.hyperlink_set.ref_count(page.memory, link_id) };
    textf!("{refs}");

    ui::end_table();
}

/// A list box of `U+XXXX` codepoint labels.
fn codepoint_list_box(id: &std::ffi::CStr, cps: Option<&[u32]>) {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        if c::ImGui_BeginListBox(id.as_ptr(), ui::vec2(0.0, 0.0)) {
            match cps {
                Some(cps) => {
                    for &cp in cps {
                        ui::selectable(&ui::cstr(&format!("U+{cp:X}")));
                    }
                }
                None => {
                    ui::selectable(c"(missing)");
                }
            }
            c::ImGui_EndListBox();
        }
    }
}

fn grapheme_table(cell: InspectedCell<'_>, page: &Page) {
    if !ui::begin_table(c"cell_grapheme", 2, c::ImGuiTableFlags_None) {
        return;
    }

    let Some(cps) = page.lookup_grapheme(cell.cell) else {
        ui::row();
        ui::col(0);
        ui::text("Status");
        ui::col(1);
        ui::text_disabled("(missing grapheme data)");
        ui::end_table();
        return;
    };

    ui::row();
    ui::col(0);
    ui::text("Extra Codepoints");
    ui::col(1);
    if cps.is_empty() {
        ui::text_disabled("(none)");
    } else {
        codepoint_list_box(c"##grapheme_list", Some(cps));
    }

    ui::end_table();
}

/// Cell inspector widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellInfo;

impl CellInfo {
    pub const EMPTY: CellInfo = CellInfo;

    fn draw(&self, cell: InspectedCell<'_>, point: Point, page: &Page) {
        if !ui::begin_table(c"cell_info", 3, TABLE_FLAGS) {
            return;
        }

        info_row(
            "Grid Position",
            "The cell's X/Y coordinates in the selected region.",
            &{
                let coord = point.coord();
                format!("({}, {})", coord.x, coord.y)
            },
        );

        info_row(
            "Page Location",
            "Row and column indices within the backing page.",
            &format!("row={} col={}", cell.row_idx, cell.col_idx),
        );

        info_row(
            "Content",
            "Content tag describing how the cell data is stored.",
            &tag_name(&cell.cell.content_tag()),
        );

        {
            ui::row();
            ui::col(0);
            ui::text("Codepoint");
            ui::col(1);
            help_marker("Primary Unicode codepoint for the cell.");
            ui::col(2);
            let cp = cell.cell.codepoint();
            if cp == 0 {
                ui::text_disabled("(empty)");
            } else {
                textf!("U+{cp:04X}");
            }
        }

        if cell.cell.has_grapheme() {
            ui::row();
            ui::col(0);
            ui::text("Grapheme");
            ui::col(1);
            help_marker(
                "Extra codepoints that combine with the primary codepoint to form the grapheme cluster.",
            );
            ui::col(2);
            codepoint_list_box(c"##cell_grapheme", page.lookup_grapheme(cell.cell));
        }

        info_row(
            "Width Property",
            "Character width property (narrow, wide, spacer, etc.).",
            &tag_name(&cell.cell.wide()),
        );

        {
            ui::row();
            ui::col(0);
            ui::text("Row Flags");
            ui::col(1);
            help_marker("Flags set on the row containing this cell.");
            ui::col(2);
            let row = cell.row;
            if row.wrap() || row.wrap_continuation() || row.grapheme() || row.styled() || row.hyperlink() {
                let flags = [
                    (row.wrap(), ui::vec4(0.4, 0.8, 1.0, 1.0), "wrap"),
                    (row.wrap_continuation(), ui::vec4(0.4, 0.8, 1.0, 1.0), "cont"),
                    (row.grapheme(), ui::vec4(0.9, 0.7, 0.3, 1.0), "grapheme"),
                    (row.styled(), ui::vec4(0.7, 0.9, 0.5, 1.0), "styled"),
                    (row.hyperlink(), ui::vec4(0.8, 0.6, 1.0, 1.0), "link"),
                ];
                for (set, color, label) in flags {
                    if set {
                        ui::text_colored(color, label);
                        ui::same_line();
                    }
                }
            } else {
                ui::text_disabled("(none)");
            }
        }

        info_row(
            "Style ID",
            "Internal style reference ID for this cell.",
            &format!("{}", cell.cell.style_id()),
        );

        {
            ui::row();
            ui::col(0);
            ui::text("Style");
            ui::col(1);
            help_marker("Resolved style for the cell (colors, attributes, etc.).");
            ui::col(2);
            if cell.cell.style_id() == stylepkg::DEFAULT_ID {
                ui::text_disabled("(default)");
            } else {
                ui::text_disabled("(see below)");
            }
        }

        if cell.cell.hyperlink() {
            let link_id = page.lookup_hyperlink(cell.cell).unwrap_or(0);
            info_row(
                "Hyperlink",
                "OSC8 hyperlink ID associated with this cell.",
                &format!("id={link_id}"),
            );
        }

        ui::end_table();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.00 MiB");
        assert_eq!(percentage(1, 0), 0.0);
        assert_eq!(percentage(1, 4), 25.0);
    }
}
