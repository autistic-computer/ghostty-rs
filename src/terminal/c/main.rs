//! Port of `src/terminal/c/main.zig`.
//!
//! STATUS: ported
//!
//! The C API of libghostty-vt (`include/ghostty/vt.h`).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "allocator.rs"]
pub mod allocator;
#[path = "build_info.rs"]
pub mod build_info;
#[path = "cell.rs"]
pub mod cell;
#[path = "color.rs"]
pub mod color;
#[path = "color_scheme.rs"]
pub mod color_scheme;
#[path = "focus.rs"]
pub mod focus;
#[path = "formatter.rs"]
pub mod formatter;
#[path = "grid_ref.rs"]
pub mod grid_ref;
#[path = "grid_ref_tracked.rs"]
pub mod grid_ref_tracked;
#[path = "io.rs"]
pub mod io;
#[path = "key_encode.rs"]
pub mod key_encode;
#[path = "key_event.rs"]
pub mod key_event;
#[path = "kitty_graphics.rs"]
pub mod kitty_graphics;
#[path = "modes.rs"]
pub mod modes;
#[path = "mouse_encode.rs"]
pub mod mouse_encode;
#[path = "mouse_event.rs"]
pub mod mouse_event;
#[path = "osc.rs"]
pub mod osc;
#[path = "paste.rs"]
pub mod paste;
#[path = "render.rs"]
pub mod render;
#[path = "result.rs"]
pub mod result;
#[path = "row.rs"]
pub mod row;
#[path = "search.rs"]
pub mod search;
#[path = "selection.rs"]
pub mod selection;
#[path = "selection_gesture.rs"]
pub mod selection_gesture;
#[path = "sgr.rs"]
pub mod sgr;
#[path = "size_report.rs"]
pub mod size_report;
#[path = "snapshot.rs"]
pub mod snapshot;
#[path = "style.rs"]
pub mod style;
#[path = "sys.rs"]
pub mod sys;
#[path = "terminal.rs"]
pub mod terminal;
#[path = "types.rs"]
pub mod types;
#[path = "unicode.rs"]
pub mod unicode;
// @generated-mods-end

// The full C API. Every function is also exported under its C name
// (`ghostty_<name>`, see `include/ghostty/vt.h`) with `#[export_name]`
// on its definition; Rust always compiles and exports them (Zig only
// with `options.c_abi`), which costs nothing when unused.

pub use self::build_info::get as build_info;

pub use self::kitty_graphics::animation_tick as kitty_graphics_animation_tick;
pub use self::kitty_graphics::get as kitty_graphics_get;
pub use self::kitty_graphics::image_get as kitty_graphics_image_get;
pub use self::kitty_graphics::image_get_handle as kitty_graphics_image;
pub use self::kitty_graphics::image_get_multi as kitty_graphics_image_get_multi;
pub use self::kitty_graphics::placement_get as kitty_graphics_placement_get;
pub use self::kitty_graphics::placement_get_multi as kitty_graphics_placement_get_multi;
pub use self::kitty_graphics::placement_grid_size as kitty_graphics_placement_grid_size;
pub use self::kitty_graphics::placement_iterator_free as kitty_graphics_placement_iterator_free;
pub use self::kitty_graphics::placement_iterator_new as kitty_graphics_placement_iterator_new;
pub use self::kitty_graphics::placement_iterator_next as kitty_graphics_placement_next;
pub use self::kitty_graphics::placement_iterator_set as kitty_graphics_placement_iterator_set;
pub use self::kitty_graphics::placement_pixel_size as kitty_graphics_placement_pixel_size;
pub use self::kitty_graphics::placement_rect as kitty_graphics_placement_rect;
pub use self::kitty_graphics::placement_render_info as kitty_graphics_placement_render_info;
pub use self::kitty_graphics::placement_source_rect as kitty_graphics_placement_source_rect;
pub use self::kitty_graphics::placement_viewport_pos as kitty_graphics_placement_viewport_pos;

pub use self::osc::command_data as osc_command_data;
pub use self::osc::command_type as osc_command_type;
pub use self::osc::end as osc_end;
pub use self::osc::free as osc_free;
pub use self::osc::new as osc_new;
pub use self::osc::next as osc_next;
pub use self::osc::reset as osc_reset;
pub use self::osc::set as osc_set;

pub use self::color::contrast as color_contrast;
pub use self::color::luminance as color_luminance;
pub use self::color::palette_default as color_palette_default;
pub use self::color::palette_generate as color_palette_generate;
pub use self::color::parse as color_parse;
pub use self::color::parse_palette_entry as color_parse_palette_entry;
pub use self::color::parse_x11 as color_parse_x11;
pub use self::color::perceived_luminance as color_perceived_luminance;
pub use self::color::rgb_get as color_rgb_get;
pub use self::color::x11_name_count as color_x11_name_count;
pub use self::color::x11_names as color_x11_names;

pub use self::color_scheme::report_encode as color_scheme_report_encode;

pub use self::focus::encode as focus_encode;

pub use self::modes::report_encode as mode_report_encode;

pub use self::formatter::format as formatter_format;
pub use self::formatter::format_alloc as formatter_format_alloc;
pub use self::formatter::format_buf as formatter_format_buf;
pub use self::formatter::free as formatter_free;
pub use self::formatter::terminal_new as formatter_terminal_new;

pub use self::render::begin_update as render_state_begin_update;
pub use self::render::clean as render_state_clean;
pub use self::render::end_update as render_state_end_update;
pub use self::render::free as render_state_free;
pub use self::render::get as render_state_get;
pub use self::render::get_multi as render_state_get_multi;
pub use self::render::image_get as render_state_image_get;
pub use self::render::image_get_multi as render_state_image_get_multi;
pub use self::render::image_iterator_free as render_state_image_iterator_free;
pub use self::render::image_iterator_new as render_state_image_iterator_new;
pub use self::render::image_iterator_next as render_state_image_iterator_next;
pub use self::render::new as render_state_new;
pub use self::render::row_cells_free as render_state_row_cells_free;
pub use self::render::row_cells_get as render_state_row_cells_get;
pub use self::render::row_cells_get_multi as render_state_row_cells_get_multi;
pub use self::render::row_cells_new as render_state_row_cells_new;
pub use self::render::row_cells_next as render_state_row_cells_next;
pub use self::render::row_cells_select as render_state_row_cells_select;
pub use self::render::row_get as render_state_row_get;
pub use self::render::row_get_multi as render_state_row_get_multi;
pub use self::render::row_iterator_free as render_state_row_iterator_free;
pub use self::render::row_iterator_new as render_state_row_iterator_new;
pub use self::render::row_iterator_next as render_state_row_iterator_next;
pub use self::render::row_iterator_next_dirty as render_state_row_iterator_next_dirty;
pub use self::render::row_set as render_state_row_set;
pub use self::render::set as render_state_set;
pub use self::render::update as render_state_update;

pub use self::sgr::attribute_tag as sgr_attribute_tag;
pub use self::sgr::attribute_value as sgr_attribute_value;
pub use self::sgr::free as sgr_free;
pub use self::sgr::new as sgr_new;
pub use self::sgr::next as sgr_next;
pub use self::sgr::reset as sgr_reset;
pub use self::sgr::set_params as sgr_set_params;
pub use self::sgr::unknown_full as sgr_unknown_full;
pub use self::sgr::unknown_partial as sgr_unknown_partial;

pub use self::key_event::free as key_event_free;
pub use self::key_event::get_action as key_event_get_action;
pub use self::key_event::get_composing as key_event_get_composing;
pub use self::key_event::get_consumed_mods as key_event_get_consumed_mods;
pub use self::key_event::get_key as key_event_get_key;
pub use self::key_event::get_mods as key_event_get_mods;
pub use self::key_event::get_unshifted_codepoint as key_event_get_unshifted_codepoint;
pub use self::key_event::get_utf8 as key_event_get_utf8;
pub use self::key_event::new as key_event_new;
pub use self::key_event::set_action as key_event_set_action;
pub use self::key_event::set_composing as key_event_set_composing;
pub use self::key_event::set_consumed_mods as key_event_set_consumed_mods;
pub use self::key_event::set_key as key_event_set_key;
pub use self::key_event::set_mods as key_event_set_mods;
pub use self::key_event::set_unshifted_codepoint as key_event_set_unshifted_codepoint;
pub use self::key_event::set_utf8 as key_event_set_utf8;

pub use self::key_encode::encode as key_encoder_encode;
pub use self::key_encode::free as key_encoder_free;
pub use self::key_encode::new as key_encoder_new;
pub use self::key_encode::setopt as key_encoder_setopt;
pub use self::key_encode::setopt_from_terminal as key_encoder_setopt_from_terminal;

pub use self::mouse_event::clear_button as mouse_event_clear_button;
pub use self::mouse_event::free as mouse_event_free;
pub use self::mouse_event::get_action as mouse_event_get_action;
pub use self::mouse_event::get_button as mouse_event_get_button;
pub use self::mouse_event::get_mods as mouse_event_get_mods;
pub use self::mouse_event::get_position as mouse_event_get_position;
pub use self::mouse_event::new as mouse_event_new;
pub use self::mouse_event::set_action as mouse_event_set_action;
pub use self::mouse_event::set_button as mouse_event_set_button;
pub use self::mouse_event::set_mods as mouse_event_set_mods;
pub use self::mouse_event::set_position as mouse_event_set_position;

pub use self::mouse_encode::encode as mouse_encoder_encode;
pub use self::mouse_encode::free as mouse_encoder_free;
pub use self::mouse_encode::new as mouse_encoder_new;
pub use self::mouse_encode::reset as mouse_encoder_reset;
pub use self::mouse_encode::setopt as mouse_encoder_setopt;
pub use self::mouse_encode::setopt_from_terminal as mouse_encoder_setopt_from_terminal;

pub use self::paste::encode as paste_encode;
pub use self::paste::is_safe as paste_is_safe;
pub use self::paste::terminal_paste;

pub use self::allocator::alloc as alloc_alloc;
pub use self::allocator::free as alloc_free;

pub use self::size_report::encode as size_report_encode;

pub use self::cell::get as cell_get;
pub use self::cell::get_multi as cell_get_multi;

pub use self::row::get as row_get;
pub use self::row::get_multi as row_get_multi;

pub use self::style::default_style as style_default;
pub use self::style::style_is_default;

pub use self::sys::log_stderr as sys_log_stderr;
pub use self::sys::set as sys_set;

pub use self::terminal::compress as terminal_compress;
pub use self::terminal::compression_activity as terminal_compression_activity;
pub use self::terminal::continuation_alloc as terminal_continuation_alloc;
pub use self::terminal::continuation_buf as terminal_continuation_buf;
pub use self::terminal::continuation_write as terminal_continuation_write;
pub use self::terminal::free as terminal_free;
pub use self::terminal::get as terminal_get;
pub use self::terminal::get_multi as terminal_get_multi;
pub use self::terminal::new as terminal_new;
pub use self::terminal::reset as terminal_reset;
pub use self::terminal::resize as terminal_resize;
pub use self::terminal::scroll_viewport as terminal_scroll_viewport;
pub use self::terminal::set as terminal_set;
pub use self::terminal::vt_write as terminal_vt_write;
pub use self::terminal::vt_write_until as terminal_vt_write_until;
pub use self::selection::adjust as terminal_selection_adjust;
pub use self::selection::all as terminal_select_all;
pub use self::selection::contains as terminal_selection_contains;
pub use self::selection::equal as terminal_selection_equal;
pub use self::selection::format_alloc as terminal_selection_format_alloc;
pub use self::selection::format_buf as terminal_selection_format_buf;
pub use self::selection::line as terminal_select_line;
pub use self::selection::order as terminal_selection_order;
pub use self::selection::ordered as terminal_selection_ordered;
pub use self::selection::output as terminal_select_output;
pub use self::selection::word as terminal_select_word;
pub use self::selection::word_between as terminal_select_word_between;
pub use self::selection_gesture::event_free as selection_gesture_event_free;
pub use self::selection_gesture::event_new as selection_gesture_event_new;
pub use self::selection_gesture::event_set as selection_gesture_event_set;
pub use self::selection_gesture::free as selection_gesture_free;
pub use self::selection_gesture::get as selection_gesture_get;
pub use self::selection_gesture::get_multi as selection_gesture_get_multi;
pub use self::selection_gesture::handle_event as selection_gesture_event;
pub use self::selection_gesture::new as selection_gesture_new;
pub use self::selection_gesture::reset as selection_gesture_reset;
pub use self::search::feed as search_feed;
pub use self::search::free as search_free;
pub use self::search::get as search_get;
pub use self::search::get_multi as search_get_multi;
pub use self::search::new as search_new;
pub use self::search::run as search_run;
pub use self::search::set as search_set;
pub use self::search::tick as search_tick;
pub use self::terminal::grid_ref as terminal_grid_ref;
pub use self::terminal::grid_ref_track as terminal_grid_ref_track;
pub use self::terminal::point_from_grid_ref as terminal_point_from_grid_ref;

pub use self::grid_ref::grid_ref_cell;
pub use self::grid_ref::grid_ref_graphemes;
pub use self::grid_ref::grid_ref_hyperlink_uri;
pub use self::grid_ref::grid_ref_row;
pub use self::grid_ref::grid_ref_style;
pub use self::grid_ref_tracked::tracked_grid_ref_free;
pub use self::grid_ref_tracked::tracked_grid_ref_has_value;
pub use self::grid_ref_tracked::tracked_grid_ref_point;
pub use self::grid_ref_tracked::tracked_grid_ref_set;
pub use self::grid_ref_tracked::tracked_grid_ref_snapshot;

pub use self::snapshot::decoder_decode as snapshot_decoder_decode;
pub use self::snapshot::decoder_free as snapshot_decoder_free;
pub use self::snapshot::decoder_get as snapshot_decoder_get;
pub use self::snapshot::decoder_get_multi as snapshot_decoder_get_multi;
pub use self::snapshot::decoder_new as snapshot_decoder_new;
pub use self::snapshot::decoder_new_buf as snapshot_decoder_new_buf;
pub use self::snapshot::decoder_next as snapshot_decoder_next;
pub use self::snapshot::decoder_ready as snapshot_decoder_ready;
pub use self::snapshot::decoder_set as snapshot_decoder_set;
pub use self::snapshot::encode as snapshot_encode;
pub use self::snapshot::encode_alloc as snapshot_encode_alloc;
pub use self::snapshot::encode_buf as snapshot_encode_buf;

pub use self::types::get_json as type_json;

pub use self::unicode::codepoint_width as unicode_codepoint_width;
pub use self::unicode::grapheme_width as unicode_grapheme_width;

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::c_int;
    use std::mem::{align_of, offset_of, size_of};

    use crate::lib::check_ghostty_h_enum;

    /// Every libghostty-vt header, concatenated (the Rust counterpart of
    /// Zig's `@cImport` of `ghostty/vt.h`).
    pub(crate) fn all_headers() -> String {
        [
            include_str!("../../../include/ghostty/vt/types.h"),
            include_str!("../../../include/ghostty/vt/allocator.h"),
            include_str!("../../../include/ghostty/vt/build_info.h"),
            include_str!("../../../include/ghostty/vt/color.h"),
            include_str!("../../../include/ghostty/vt/color_scheme.h"),
            include_str!("../../../include/ghostty/vt/device.h"),
            include_str!("../../../include/ghostty/vt/focus.h"),
            include_str!("../../../include/ghostty/vt/formatter.h"),
            include_str!("../../../include/ghostty/vt/grid_ref.h"),
            include_str!("../../../include/ghostty/vt/grid_ref_tracked.h"),
            include_str!("../../../include/ghostty/vt/io.h"),
            include_str!("../../../include/ghostty/vt/key.h"),
            include_str!("../../../include/ghostty/vt/key/encoder.h"),
            include_str!("../../../include/ghostty/vt/key/event.h"),
            include_str!("../../../include/ghostty/vt/kitty_graphics.h"),
            include_str!("../../../include/ghostty/vt/modes.h"),
            include_str!("../../../include/ghostty/vt/mouse.h"),
            include_str!("../../../include/ghostty/vt/mouse/encoder.h"),
            include_str!("../../../include/ghostty/vt/mouse/event.h"),
            include_str!("../../../include/ghostty/vt/osc.h"),
            include_str!("../../../include/ghostty/vt/paste.h"),
            include_str!("../../../include/ghostty/vt/point.h"),
            include_str!("../../../include/ghostty/vt/render.h"),
            include_str!("../../../include/ghostty/vt/screen.h"),
            include_str!("../../../include/ghostty/vt/search.h"),
            include_str!("../../../include/ghostty/vt/selection.h"),
            include_str!("../../../include/ghostty/vt/sgr.h"),
            include_str!("../../../include/ghostty/vt/size_report.h"),
            include_str!("../../../include/ghostty/vt/snapshot.h"),
            include_str!("../../../include/ghostty/vt/style.h"),
            include_str!("../../../include/ghostty/vt/sys.h"),
            include_str!("../../../include/ghostty/vt/terminal.h"),
            include_str!("../../../include/ghostty/vt/unicode.h"),
            include_str!("../../../include/ghostty/vt/wasm.h"),
            include_str!("../../../include/ghostty/vt.h"),
        ]
        .join("\n")
    }

    /// Rust port: Zig compares the C API enums against `ghostty/vt.h` with
    /// `checkGhosttyHEnum` where they are declared; this checks the batch-1
    /// enums whose Rust variant names spell the header constants.
    #[test]
    fn enums_match_vt_headers() {
        use super::*;
        let h = all_headers();
        check_ghostty_h_enum::<result::Result>("GHOSTTY_", &h).unwrap();
        check_ghostty_h_enum::<build_info::BuildInfo>("GHOSTTY_BUILD_INFO_", &h).unwrap();
        check_ghostty_h_enum::<build_info::OptimizeMode>("GHOSTTY_OPTIMIZE_", &h).unwrap();
        check_ghostty_h_enum::<cell::CellData>("GHOSTTY_CELL_DATA_", &h).unwrap();
        check_ghostty_h_enum::<cell::ContentTag>("GHOSTTY_CELL_CONTENT_", &h).unwrap();
        check_ghostty_h_enum::<cell::Wide>("GHOSTTY_CELL_WIDE_", &h).unwrap();
        check_ghostty_h_enum::<cell::SemanticContent>("GHOSTTY_CELL_SEMANTIC_", &h).unwrap();
        check_ghostty_h_enum::<row::RowData>("GHOSTTY_ROW_DATA_", &h).unwrap();
        check_ghostty_h_enum::<row::SemanticPrompt>("GHOSTTY_ROW_SEMANTIC_", &h).unwrap();
        check_ghostty_h_enum::<osc::Option>("GHOSTTY_OSC_OPT_", &h).unwrap();
        check_ghostty_h_enum::<osc::CommandData>("GHOSTTY_OSC_DATA_", &h).unwrap();
        check_ghostty_h_enum::<sgr::Underline>("GHOSTTY_SGR_UNDERLINE_", &h).unwrap();
        check_ghostty_h_enum::<style::ColorTag>("GHOSTTY_STYLE_COLOR_", &h).unwrap();
        check_ghostty_h_enum::<sys::LogLevel>("GHOSTTY_SYS_LOG_LEVEL_", &h).unwrap();
        check_ghostty_h_enum::<sys::Option>("GHOSTTY_SYS_OPT_", &h).unwrap();
        check_ghostty_h_enum::<modes::ReportState>("GHOSTTY_MODE_REPORT_", &h).unwrap();
        check_ghostty_h_enum::<focus::Event>("GHOSTTY_FOCUS_", &h).unwrap();
        check_ghostty_h_enum::<color_scheme::ColorScheme>("GHOSTTY_COLOR_SCHEME_", &h).unwrap();
        check_ghostty_h_enum::<mouse_encode::Option>("GHOSTTY_MOUSE_ENCODER_OPT_", &h).unwrap();
        check_ghostty_h_enum::<mouse_encode::TrackingMode>("GHOSTTY_MOUSE_TRACKING_", &h).unwrap();
        check_ghostty_h_enum::<mouse_encode::Format>("GHOSTTY_MOUSE_FORMAT_", &h).unwrap();
        check_ghostty_h_enum::<paste::Source>("GHOSTTY_PASTE_SOURCE_", &h).unwrap();
        check_ghostty_h_enum::<terminal::Option>("GHOSTTY_TERMINAL_OPT_", &h).unwrap();
        check_ghostty_h_enum::<terminal::TerminalData>("GHOSTTY_TERMINAL_DATA_", &h).unwrap();
        check_ghostty_h_enum::<terminal::TerminalScreen>("GHOSTTY_TERMINAL_SCREEN_", &h).unwrap();
        check_ghostty_h_enum::<terminal::TerminalCursorStyle>("GHOSTTY_TERMINAL_CURSOR_STYLE_", &h).unwrap();
        check_ghostty_h_enum::<terminal::CompressionMode>("GHOSTTY_TERMINAL_COMPRESSION_MODE_", &h).unwrap();
        check_ghostty_h_enum::<terminal::CompressionResult>("GHOSTTY_TERMINAL_COMPRESSION_RESULT_", &h).unwrap();
        check_ghostty_h_enum::<terminal::Boundary>("GHOSTTY_TERMINAL_BOUNDARY_", &h).unwrap();
        check_ghostty_h_enum::<terminal::StringKind>("GHOSTTY_TERMINAL_STRING_KIND_", &h).unwrap();
        check_ghostty_h_enum::<terminal::StringIntroducer>("GHOSTTY_TERMINAL_STRING_INTRODUCER_", &h).unwrap();
        check_ghostty_h_enum::<terminal::StringEnd>("GHOSTTY_TERMINAL_STRING_END_", &h).unwrap();
        check_ghostty_h_enum::<terminal::ProgressState>("GHOSTTY_TERMINAL_PROGRESS_STATE_", &h).unwrap();
        check_ghostty_h_enum::<terminal::ProgramStatusState>("GHOSTTY_PROGRAM_STATUS_STATE_", &h).unwrap();
        check_ghostty_h_enum::<terminal::ProgramStatusKind>("GHOSTTY_PROGRAM_STATUS_KIND_", &h).unwrap();
        check_ghostty_h_enum::<terminal::SemanticPromptKind>("GHOSTTY_SEMANTIC_PROMPT_", &h).unwrap();
        check_ghostty_h_enum::<terminal::SemanticPromptPromptKind>("GHOSTTY_SEMANTIC_PROMPT_PROMPT_", &h).unwrap();
        check_ghostty_h_enum::<terminal::ResizeWindowTag>("GHOSTTY_TERMINAL_RESIZE_WINDOW_", &h).unwrap();
        check_ghostty_h_enum::<terminal::UnknownSequenceTag>("GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_", &h).unwrap();

        check_ghostty_h_enum::<render::Dirty>("GHOSTTY_RENDER_STATE_DIRTY_", &h).unwrap();
        check_ghostty_h_enum::<render::CursorVisualStyle>("GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_", &h).unwrap();
        check_ghostty_h_enum::<render::Data>("GHOSTTY_RENDER_STATE_DATA_", &h).unwrap();
        check_ghostty_h_enum::<render::SetOption>("GHOSTTY_RENDER_STATE_OPTION_", &h).unwrap();
        check_ghostty_h_enum::<render::RowCellsData>("GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_", &h).unwrap();
        check_ghostty_h_enum::<render::RowData>("GHOSTTY_RENDER_STATE_ROW_DATA_", &h).unwrap();
        check_ghostty_h_enum::<render::RowOption>("GHOSTTY_RENDER_STATE_ROW_OPTION_", &h).unwrap();
        check_ghostty_h_enum::<render::ImageProtocol>("GHOSTTY_IMAGE_PROTOCOL_", &h).unwrap();
        check_ghostty_h_enum::<render::ImageLayer>("GHOSTTY_RENDER_STATE_IMAGE_LAYER_", &h).unwrap();
        check_ghostty_h_enum::<render::KittyPlacementKind>("GHOSTTY_RENDER_STATE_KITTY_PLACEMENT_KIND_", &h).unwrap();
        check_ghostty_h_enum::<render::ImageData>("GHOSTTY_RENDER_STATE_IMAGE_DATA_", &h).unwrap();
        check_ghostty_h_enum::<search::Status>("GHOSTTY_SEARCH_STATUS_", &h).unwrap();
        check_ghostty_h_enum::<search::Scroll>("GHOSTTY_SEARCH_SCROLL_", &h).unwrap();
        check_ghostty_h_enum::<search::Data>("GHOSTTY_SEARCH_DATA_", &h).unwrap();
        check_ghostty_h_enum::<search::Option>("GHOSTTY_SEARCH_OPT_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::Data>("GHOSTTY_KITTY_GRAPHICS_DATA_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::PlacementData>("GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::PlacementLayer>("GHOSTTY_KITTY_PLACEMENT_LAYER_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::PlacementIteratorOption>(
            "GHOSTTY_KITTY_GRAPHICS_PLACEMENT_ITERATOR_OPTION_",
            &h,
        )
        .unwrap();
        check_ghostty_h_enum::<kitty_graphics::ImageFormat>("GHOSTTY_KITTY_IMAGE_FORMAT_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::ImageCompression>("GHOSTTY_KITTY_IMAGE_COMPRESSION_", &h).unwrap();
        check_ghostty_h_enum::<kitty_graphics::ImageData>("GHOSTTY_KITTY_IMAGE_DATA_", &h).unwrap();
        check_ghostty_h_enum::<selection_gesture::Behavior>("GHOSTTY_SELECTION_GESTURE_BEHAVIOR_", &h).unwrap();
        check_ghostty_h_enum::<selection_gesture::Autoscroll>("GHOSTTY_SELECTION_GESTURE_AUTOSCROLL_", &h).unwrap();
        check_ghostty_h_enum::<selection_gesture::Data>("GHOSTTY_SELECTION_GESTURE_DATA_", &h).unwrap();
        check_ghostty_h_enum::<selection_gesture::EventType>("GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_", &h).unwrap();
        check_ghostty_h_enum::<selection_gesture::EventOption>("GHOSTTY_SELECTION_GESTURE_EVENT_OPT_", &h).unwrap();
        check_ghostty_h_enum::<snapshot::DecoderOption>("GHOSTTY_SNAPSHOT_DECODER_OPT_", &h).unwrap();
        check_ghostty_h_enum::<snapshot::DecoderProgressKind>("GHOSTTY_SNAPSHOT_DECODER_PROGRESS_KIND_", &h).unwrap();
        check_ghostty_h_enum::<snapshot::DecoderData>("GHOSTTY_SNAPSHOT_DECODER_DATA_", &h).unwrap();
    }

    /// Rust port: struct layouts against the C declarations in the
    /// headers.
    #[test]
    fn struct_layouts_match_vt_headers() {
        use super::*;
        let ptr = size_of::<usize>();

        // GhosttyString / GhosttyBuffer
        assert_eq!(2 * ptr, size_of::<crate::lib::String>());
        assert_eq!(3 * ptr, size_of::<crate::lib::Buffer>());
        // GhosttyColorRgb
        assert_eq!(3, size_of::<crate::terminal::color::RgbC>());
        assert_eq!(1, align_of::<crate::terminal::color::RgbC>());
        // GhosttyColorPaletteMask
        assert_eq!(32, size_of::<color::PaletteMask>());
        // GhosttyStyleColor: { int tag; union { uint8_t; rgb; uint64_t } }
        assert_eq!(16, size_of::<style::Color>());
        assert_eq!(8, offset_of!(style::Color, value));
        // GhosttyStyle
        assert_eq!(0, offset_of!(style::Style, size));
        assert_eq!(ptr, offset_of!(style::Style, fg_color));
        assert_eq!(ptr + 48, offset_of!(style::Style, bold));
        assert_eq!(ptr + 56, offset_of!(style::Style, underline));
        // GhosttySgrAttribute: { int tag; union { ...; uint64_t _padding[8]; } }
        assert_eq!(64, size_of::<sgr::AttributeCValue>());
        assert_eq!(72, size_of::<sgr::AttributeC>());
        assert_eq!(4 * ptr, size_of::<crate::terminal::sgr::UnknownC>());
        // GhosttyMousePosition / GhosttyMouseEncoderSize
        assert_eq!(8, size_of::<mouse_event::Position>());
        assert_eq!((ptr + 8 * 4).next_multiple_of(ptr), size_of::<mouse_encode::Size>());
        // GhosttySizeReportSize
        assert_eq!(12, size_of::<size_report::Size>());
        // GhosttySysImage
        assert_eq!(8 + 2 * ptr, size_of::<sys::Image>());
        // GhosttyReader / GhosttyWriter / GhosttyMimeReader
        assert_eq!(2 * ptr, size_of::<io::Reader>());
        assert_eq!(2 * ptr, size_of::<io::Writer>());
        assert_eq!(2 * ptr, size_of::<io::MimeReader>());
        // GhosttyColorX11Entry
        assert_eq!(2 * ptr, size_of::<color::X11Entry>());
        // GhosttyCellsView
        assert_eq!(2 * ptr, size_of::<cell::CellsView>());
        // GhosttySurfacePosition / GhosttyCodepoints
        assert_eq!(16, size_of::<types::SurfacePosition>());
        assert_eq!(2 * ptr, size_of::<types::Codepoints>());
        // GhosttyAllocator
        assert_eq!(2 * ptr, size_of::<crate::lib::allocator::Allocator>());
        assert_eq!(4 * ptr, size_of::<crate::lib::allocator::VTable>());
        // GhosttyPaste
        assert_eq!(0, offset_of!(paste::Request, size));
        assert_eq!(ptr, offset_of!(paste::Request, location));
        assert_eq!(ptr + 4, offset_of!(paste::Request, source));
        assert_eq!(ptr + 8, offset_of!(paste::Request, mimes));
        // GhosttyFormatter{Screen,Terminal}Extra / GhosttyFormatterTerminalOptions
        assert_eq!(2 * ptr, size_of::<formatter::ScreenOptionsExtra>());
        assert_eq!(ptr, offset_of!(formatter::TerminalOptionsExtra, palette));
        assert_eq!(2 * ptr, offset_of!(formatter::TerminalOptionsExtra, screen));
        assert_eq!(4 * ptr, size_of::<formatter::TerminalOptionsExtra>());
        assert_eq!(ptr, offset_of!(formatter::TerminalOptions, emit));
        assert_eq!(ptr + 4, offset_of!(formatter::TerminalOptions, unwrap));
        assert_eq!(ptr + 8, offset_of!(formatter::TerminalOptions, extra));
        assert_eq!(ptr + 8 + 4 * ptr, offset_of!(formatter::TerminalOptions, selection));
        // GhosttyKittyGraphicsPlacementRenderInfo
        assert_eq!(ptr, offset_of!(kitty_graphics::PlacementRenderInfo, pixel_width));
        assert_eq!(ptr + 24, offset_of!(kitty_graphics::PlacementRenderInfo, viewport_visible));
        assert_eq!(ptr + 28, offset_of!(kitty_graphics::PlacementRenderInfo, source_x));
        assert_eq!((ptr + 44).next_multiple_of(ptr), size_of::<kitty_graphics::PlacementRenderInfo>());
        // GhosttyRenderState{RowSelection,Overscan,RowId,Cursor,Colors}
        assert_eq!(2 * ptr, size_of::<render::RowSelection>());
        assert_eq!(4, size_of::<render::Overscan>());
        assert_eq!(16, size_of::<render::RowId>());
        assert_eq!(ptr, offset_of!(render::Cursor, viewport_has_value));
        assert_eq!(ptr + 2, offset_of!(render::Cursor, viewport_x));
        assert_eq!(ptr + 4, offset_of!(render::Cursor, viewport_y));
        assert_eq!(ptr + 6, offset_of!(render::Cursor, wide_tail));
        assert_eq!(ptr + 12, offset_of!(render::Cursor, visual_style));
        assert_eq!(ptr, offset_of!(render::Colors, background));
        assert_eq!(ptr + 9, offset_of!(render::Colors, cursor_has_value));
        assert_eq!(ptr + 10, offset_of!(render::Colors, palette));
        assert_eq!((ptr + 10 + 3 * 256).next_multiple_of(ptr), size_of::<render::Colors>());
        // GhosttySelectionGesture{Behaviors,Geometry}
        assert_eq!(12, size_of::<selection_gesture::Behaviors>());
        assert_eq!(16, size_of::<selection_gesture::Geometry>());
        // Enums are C ints.
        assert_eq!(size_of::<c_int>(), size_of::<result::Result>());
        assert_eq!(size_of::<c_int>(), size_of::<sgr::AttributeTag>());
        assert_eq!(size_of::<c_int>(), size_of::<paste::Source>());
    }
}
