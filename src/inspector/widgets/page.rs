//! Port of `src/inspector/widgets/page.zig`.
//!
//! STATUS: ported (only compiled with the `inspector` feature)
#![cfg(feature = "inspector")]

use crate::inspector::units;
use crate::inspector::widgets::termio::tag_name;
use crate::inspector::widgets::{self, help_marker, textf, ui};
use crate::pkg::dcimgui::c;
use crate::terminal::hyperlink::PageEntryId;
use crate::terminal::page::{Cell, Page, Row};
use crate::terminal::size::CellCountInt;

pub fn inspector(page: &Page) {
    ui::separator_text(c"Managed Memory");
    managed_memory(page);

    ui::separator_text(c"Styles");
    styles_list(page);

    ui::separator_text(c"Hyperlinks");
    hyperlinks_list(page);

    ui::separator_text(c"Rows");
    rows_table(page);
}

/// The header state of [`tree_node`].
pub struct TreeNodeState {
    /// Page dimensions available without reading its backing memory.
    pub cols: CellCountInt,
    pub rows: CellCountInt,
    /// The index of the page in a page list, used for headers.
    pub index: usize,
    /// The range of rows this page covers, inclusive.
    pub row_range: [usize; 2],
    /// Whether this page is the active or viewport node.
    pub active: bool,
    pub viewport: bool,
    /// Whether the page backing memory is currently compressed.
    pub compressed: bool,
    /// Dirty state is unavailable without restoring a compressed page.
    pub dirty: Option<bool>,
}

/// Draw a tree node header with metadata about this page. Returns if
/// the tree node is open or not. If it is open you must close it with
/// TreePop.
pub fn tree_node(state: TreeNodeState) -> bool {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        // Setup our node.
        let open = {
            let label = ui::cstr(&format!("Page {}", state.index));
            let flags = c::ImGuiTreeNodeFlags_AllowOverlap
                | c::ImGuiTreeNodeFlags_SpanFullWidth
                | c::ImGuiTreeNodeFlags_FramePadding;
            c::ImGui_TreeNodeEx(label.as_ptr(), flags as _)
        };

        // Move our cursor into the tree header so we can add extra info.
        let header_min = c::ImGui_GetItemRectMin();
        let header_max = c::ImGui_GetItemRectMax();
        let header_height = header_max.y - header_min.y;
        let text_line = c::ImGui_GetTextLineHeight();
        let y_center = header_min.y + (header_height - text_line) * 0.5;
        c::ImGui_SetCursorScreenPos(ui::vec2(header_min.x + 170.0, y_center));

        // Metadata
        ui::text_disabled(&format!("{}c x {}r", state.cols, state.rows));
        ui::same_line();
        textf!("rows {}..{}", state.row_range[0], state.row_range[1]);

        // Labels
        if state.active {
            ui::same_line();
            ui::text_colored(ui::vec4(0.4, 0.9, 0.4, 1.0), "active");
        }
        if state.viewport {
            ui::same_line();
            ui::text_colored(ui::vec4(0.4, 0.8, 1.0, 1.0), "viewport");
        }
        if state.compressed {
            ui::same_line();
            ui::text_colored(ui::vec4(0.8, 0.6, 1.0, 1.0), "compressed");
        }
        if state.dirty.unwrap_or(false) {
            ui::same_line();
            ui::text_colored(ui::vec4(1.0, 0.4, 0.4, 1.0), "dirty");
        }

        open
    }
}

pub fn managed_memory(page: &Page) {
    let flags = c::ImGuiTableFlags_BordersInnerV | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit;
    if ui::begin_table(c"##overview", 3, flags) {
        ui::row();
        ui::col(0);
        ui::text("Memory Size");
        ui::col(1);
        help_marker(
            "Memory allocated for this page. Note the backing memory \
             may be a larger allocation from which this page \
             uses a portion.",
        );
        ui::col(2);
        textf!("{} KiB", units::to_kibi_bytes(page.memory.len()));

        ui::end_table();
    }

    if ui::begin_table(c"##managed", 4, flags) {
        ui::setup_column(c"Resource", c::ImGuiTableColumnFlags_WidthFixed);
        ui::setup_column(c"", c::ImGuiTableColumnFlags_WidthFixed);
        ui::setup_column(c"Used", c::ImGuiTableColumnFlags_WidthFixed);
        ui::setup_column(c"Capacity", c::ImGuiTableColumnFlags_WidthFixed);
        // SAFETY: requires an open table.
        unsafe { c::ImGui_TableHeadersRow() };

        let size = page.size;
        let cap = page.capacity;
        let resource_row = |name: &str, help: &str, used: usize, capacity: usize| {
            ui::row();
            ui::col(0);
            ui::text(name);
            ui::col(1);
            help_marker(help);
            ui::col(2);
            textf!("{used}");
            ui::col(3);
            textf!("{capacity}");
        };

        resource_row(
            "Columns",
            "Number of columns in the terminal grid.",
            size.cols as usize,
            cap.cols as usize,
        );
        resource_row(
            "Rows",
            "Number of rows in this page.",
            size.rows as usize,
            cap.rows as usize,
        );
        resource_row(
            "Styles",
            "Unique text styles (colors, attributes) currently in use.",
            page.styles.count(),
            page.styles.layout.cap,
        );
        resource_row(
            "Graphemes",
            "Extended grapheme clusters for multi-codepoint characters.",
            page.grapheme_count(),
            page.grapheme_capacity(),
        );
        resource_row(
            "Strings (bytes)",
            "String storage for hyperlink URIs and other text data.",
            // SAFETY: the string allocator lives in this page's memory.
            unsafe { page.string_alloc.used_bytes(page.memory) },
            page.string_alloc.capacity_bytes(),
        );

        // SAFETY: the hyperlink map lives in this page's memory.
        let hyperlink_map = unsafe { page.hyperlink_map.map(page.memory) };
        resource_row(
            "Hyperlink Map",
            "Maps cell positions to hyperlink IDs.",
            hyperlink_map.count() as usize,
            hyperlink_map.capacity() as usize,
        );
        resource_row(
            "Hyperlink IDs",
            "Unique hyperlink definitions (URI + optional ID).",
            page.hyperlink_set.count(),
            page.hyperlink_set.layout.cap,
        );

        ui::end_table();
    }
}

/// Begin a bordered child window of `visible_rows` text rows (plus a
/// header). Like Zig, `EndChild` must always be called afterwards.
fn begin_child(id: &std::ffi::CStr, visible_rows: usize) -> bool {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        let row_height: f32 = c::ImGui_GetTextLineHeightWithSpacing();
        let child_height: f32 = row_height * (visible_rows as f32 + 2.0);
        c::ImGui_BeginChild(
            id.as_ptr(),
            ui::vec2(0.0, child_height),
            c::ImGuiChildFlags_Borders as _,
            c::ImGuiWindowFlags_None as _,
        )
    }
}

fn end_child() {
    // SAFETY: paired with `begin_child`.
    unsafe { c::ImGui_EndChild() };
}

fn rows_table(page: &Page) {
    let visible_rows: usize = (page.size.rows as usize).min(12);

    // Child window so scrolling is separate.
    // EndChild always needs to be called.
    if begin_child(c"##page_rows", visible_rows) {
        rows_table_contents(page);
    }
    end_child();
}

fn rows_table_contents(page: &Page) {
    if !ui::begin_table(
        c"##page_rows_table",
        10,
        c::ImGuiTableFlags_BordersInnerV | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit,
    ) {
        return;
    }

    // SAFETY: requires an open table.
    unsafe { c::ImGui_TableSetupScrollFreeze(0, 1) };
    for name in [
        c"Row", c"Text", c"Dirty", c"Wrap", c"Cont", c"Styled", c"Grapheme", c"Link", c"Prompt", c"Kitty",
    ] {
        ui::setup_column(name, c::ImGuiTableColumnFlags_WidthFixed);
    }
    // SAFETY: requires an open table.
    unsafe { c::ImGui_TableHeadersRow() };

    // SAFETY: the rows live in this page's memory.
    let rows: &[Row] =
        unsafe { std::slice::from_raw_parts(page.rows.ptr(page.memory), page.size.rows as usize) };
    for (row_index, row) in rows.iter().enumerate() {
        // SAFETY: the cells of a row live in this page's memory.
        let cells: &[Cell] = unsafe { &*page.get_cells(row) };
        let text_cells = cells.iter().filter(|cell| cell.has_text()).count();

        ui::row();

        ui::col(0);
        textf!("{row_index}");

        ui::col(1);
        if text_cells == 0 {
            ui::text_disabled("0");
        } else {
            textf!("{text_cells}");
        }

        ui::col(2);
        flag_cell(row.dirty());

        ui::col(3);
        flag_cell(row.wrap());

        ui::col(4);
        flag_cell(row.wrap_continuation());

        ui::col(5);
        flag_cell(row.styled());

        ui::col(6);
        flag_cell(row.grapheme());

        ui::col(7);
        flag_cell(row.hyperlink());

        ui::col(8);
        ui::text(&tag_name(&row.semantic_prompt()));

        ui::col(9);
        flag_cell(row.kitty_virtual_placeholder());
    }

    ui::end_table();
}

fn styles_list(page: &Page) {
    // SAFETY: the style set lives in this page's memory.
    let items = unsafe { std::slice::from_raw_parts(page.styles.items.ptr(page.memory), page.styles.layout.cap) };

    let count = items
        .iter()
        .enumerate()
        .filter(|(index, item)| *index != 0 && item.meta.r#ref != 0)
        .count();

    if count == 0 {
        ui::text_disabled("(no styles in use)");
        return;
    }

    let visible_rows: usize = count.min(8);

    if begin_child(c"##page_styles", visible_rows) {
        if ui::begin_table(
            c"##page_styles_table",
            3,
            c::ImGuiTableFlags_BordersInnerV | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit,
        ) {
            // SAFETY: requires an open table.
            unsafe { c::ImGui_TableSetupScrollFreeze(0, 1) };
            ui::setup_column(c"ID", c::ImGuiTableColumnFlags_WidthFixed);
            ui::setup_column(c"Refs", c::ImGuiTableColumnFlags_WidthFixed);
            ui::setup_column(c"Style", c::ImGuiTableColumnFlags_WidthStretch);
            // SAFETY: requires an open table.
            unsafe { c::ImGui_TableHeadersRow() };

            for (index, item) in items.iter().enumerate() {
                if index == 0 {
                    continue;
                }
                if item.meta.r#ref == 0 {
                    continue;
                }

                ui::row();
                // SAFETY: requires the ImGui context.
                unsafe { c::ImGui_PushIDInt(index as i32) };

                ui::col(0);
                textf!("{index}");

                ui::col(1);
                textf!("{}", item.meta.r#ref);

                ui::col(2);
                // SAFETY: requires the ImGui context.
                if unsafe { c::ImGui_TreeNodeEx(c"Details".as_ptr(), c::ImGuiTreeNodeFlags_None as _) } {
                    // SAFETY: items with a nonzero ref count are initialized.
                    widgets::style::table(unsafe { item.value.assume_init() }, None);
                    // SAFETY: paired with TreeNodeEx.
                    unsafe { c::ImGui_TreePop() };
                }

                // SAFETY: paired with PushIDInt.
                unsafe { c::ImGui_PopID() };
            }

            ui::end_table();
        }
    }
    end_child();
}

fn hyperlinks_list(page: &Page) {
    // SAFETY: the hyperlink set lives in this page's memory.
    let items = unsafe {
        std::slice::from_raw_parts(page.hyperlink_set.items.ptr(page.memory), page.hyperlink_set.layout.cap)
    };

    let count = items
        .iter()
        .enumerate()
        .filter(|(index, item)| *index != 0 && item.meta.r#ref != 0)
        .count();

    if count == 0 {
        ui::text_disabled("(no hyperlinks in use)");
        return;
    }

    let visible_rows: usize = count.min(8);

    if begin_child(c"##page_hyperlinks", visible_rows) {
        if ui::begin_table(
            c"##page_hyperlinks_table",
            4,
            c::ImGuiTableFlags_BordersInnerV | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit,
        ) {
            // SAFETY: requires an open table.
            unsafe { c::ImGui_TableSetupScrollFreeze(0, 1) };
            ui::setup_column(c"ID", c::ImGuiTableColumnFlags_WidthFixed);
            ui::setup_column(c"Refs", c::ImGuiTableColumnFlags_WidthFixed);
            ui::setup_column(c"Explicit ID", c::ImGuiTableColumnFlags_WidthFixed);
            ui::setup_column(c"URI", c::ImGuiTableColumnFlags_WidthStretch);
            // SAFETY: requires an open table.
            unsafe { c::ImGui_TableHeadersRow() };

            for (index, item) in items.iter().enumerate() {
                if index == 0 {
                    continue;
                }
                if item.meta.r#ref == 0 {
                    continue;
                }
                // SAFETY: items with a nonzero ref count are initialized.
                let value = unsafe { item.value.assume_init_ref() };

                ui::row();

                ui::col(0);
                textf!("{index}");

                ui::col(1);
                textf!("{}", item.meta.r#ref);

                ui::col(2);
                match value.id {
                    PageEntryId::Explicit(slice) => {
                        // SAFETY: the string lives in this page's memory.
                        let explicit_id = unsafe { slice.slice(page.memory) };
                        ui::text(&String::from_utf8_lossy(explicit_id));
                    }
                    PageEntryId::Implicit(_) => ui::text_disabled("-"),
                }

                ui::col(3);
                // SAFETY: the string lives in this page's memory.
                let uri = unsafe { value.uri.slice(page.memory) };
                ui::text(&String::from_utf8_lossy(uri));
            }

            ui::end_table();
        }
    }
    end_child();
}

fn flag_cell(value: bool) {
    if value {
        ui::text_colored(ui::vec4(0.4, 0.9, 0.4, 1.0), "yes");
    } else {
        ui::text_disabled("-");
    }
}
