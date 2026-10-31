//! Port of `src/inspector/widgets/renderer.zig`.
//!
//! STATUS: ported
//!
//! Deviation: `features` is a `Vec` used as an insertion-ordered set with
//! swap-remove semantics (Zig: an `AutoArrayHashMap` keyed by the feature
//! tag; overlay features carry no payload so key and value coincide).
//! Drawing is only compiled with the `inspector` feature.

use crate::renderer::overlay::Feature;

#[cfg(feature = "inspector")]
use crate::inspector::widgets::{help_marker, ui};
#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::c;

#[allow(dead_code)]
const LOG: &str = "inspector_renderer";

/// Renderer information inspector widget.
#[derive(Debug, Default, Clone)]
pub struct Info {
    pub features: Vec<Feature>,
}

impl Info {
    pub const EMPTY: Info = Info { features: Vec::new() };

    /// Grab the features into a new allocated slice. This is used by
    /// the renderer to know which overlays to draw.
    pub fn overlay_features(&self) -> Vec<Feature> {
        // For now we do a dumb copy since the features have no managed
        // memory.
        self.features.clone()
    }

    /// Whether the feature is enabled (Zig: `features.contains`).
    pub fn contains(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Enable a feature (Zig: `features.put`).
    pub fn put(&mut self, feature: Feature) {
        if !self.contains(feature) {
            self.features.push(feature);
        }
    }

    /// Disable a feature (Zig: `features.swapRemove`). Returns whether it
    /// was enabled.
    pub fn swap_remove(&mut self, feature: Feature) -> bool {
        match self.features.iter().position(|f| *f == feature) {
            Some(i) => {
                self.features.swap_remove(i);
                true
            }
            None => false,
        }
    }
}

#[cfg(feature = "inspector")]
impl Info {
    /// Draw the renderer info window.
    pub fn draw(&mut self, open: bool) {
        if !open {
            return;
        }

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            c::ImGui_SetNextItemOpen(true, c::ImGuiCond_Once as _);
        }
        if !ui::collapsing_header(c"Overlays", c::ImGuiTreeNodeFlags_None) {
            return;
        }

        ui::separator_text(c"Hyperlinks");
        self.overlay_hyperlinks();
        ui::separator_text(c"Semantic Prompts");
        self.overlay_semantic_prompts();
    }

    fn overlay_hyperlinks(&mut self) {
        let mut hyperlinks: bool = self.contains(Feature::HighlightHyperlinks);
        // SAFETY: see `draw`.
        unsafe { c::ImGui_Checkbox(c"Overlay Hyperlinks".as_ptr(), &mut hyperlinks) };
        ui::same_line();
        help_marker("When enabled, highlights OSC8 hyperlinks.");

        if !hyperlinks {
            self.swap_remove(Feature::HighlightHyperlinks);
        } else {
            self.put(Feature::HighlightHyperlinks);
        }
    }

    fn overlay_semantic_prompts(&mut self) {
        let mut semantic_prompts: bool = self.contains(Feature::SemanticPrompts);
        // SAFETY: see `draw`.
        unsafe { c::ImGui_Checkbox(c"Overlay Semantic Prompts".as_ptr(), &mut semantic_prompts) };
        ui::same_line();
        help_marker("When enabled, highlights OSC 133 semantic prompts.");

        // Handle the checkbox results
        if !semantic_prompts {
            self.swap_remove(Feature::SemanticPrompts);
        } else {
            self.put(Feature::SemanticPrompts);
        }

        // Help
        // SAFETY: see `draw`.
        unsafe {
            c::ImGui_Indent();

            ui::text_disabled("Colors:");

            use crate::renderer::overlay::Color;
            let prompt_rgb = Color::SemanticPrompt.rgb();
            let input_rgb = Color::SemanticInput.rgb();
            let prompt_col = ui::vec4(
                prompt_rgb.r as f32 / 255.0,
                prompt_rgb.g as f32 / 255.0,
                prompt_rgb.b as f32 / 255.0,
                1.0,
            );
            let input_col = ui::vec4(
                input_rgb.r as f32 / 255.0,
                input_rgb.g as f32 / 255.0,
                input_rgb.b as f32 / 255.0,
                1.0,
            );

            c::ImGui_ColorButton(
                c"##prompt_color".as_ptr(),
                prompt_col,
                c::ImGuiColorEditFlags_NoTooltip as _,
            );
            ui::same_line();
            ui::text("Prompt");

            c::ImGui_ColorButton(
                c"##input_color".as_ptr(),
                input_col,
                c::ImGuiColorEditFlags_NoTooltip as _,
            );
            ui::same_line();
            ui::text("Input");

            c::ImGui_Unindent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_set() {
        let mut info = Info::EMPTY;
        assert!(info.overlay_features().is_empty());
        info.put(Feature::HighlightHyperlinks);
        info.put(Feature::SemanticPrompts);
        info.put(Feature::HighlightHyperlinks);
        assert_eq!(
            info.overlay_features(),
            vec![Feature::HighlightHyperlinks, Feature::SemanticPrompts]
        );
        assert!(info.swap_remove(Feature::HighlightHyperlinks));
        assert!(!info.swap_remove(Feature::HighlightHyperlinks));
        assert_eq!(info.overlay_features(), vec![Feature::SemanticPrompts]);
    }
}
