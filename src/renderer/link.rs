//! Port of `src/renderer/link.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * `Set::render_cell_map` takes `&mut self` because searching with an
//!   oniguruma regex needs `&mut Regex` in the Rust wrapper.
//! * The allocator parameters are dropped. The Zig test "renderCellMap
//!   inactive links don't allocate" uses a failing allocator; here the
//!   same early return is checked by observing that nothing is matched
//!   (the Rust port cannot intercept the global allocator per call).

use crate::input::key_mods::Mods;
use crate::input::link::{Highlight, Link as InputLink};
use crate::pkg::oniguruma as oni;
use crate::terminal::point;
use crate::terminal::render::{CellSet, RenderState, StringError, StringMap};

const LOG: &str = "renderer_link";

/// The link configuration needed for renderers.
#[derive(Debug)]
pub struct Link {
    /// The regular expression to match the link against.
    pub regex: oni::Regex,

    /// The situations in which the link should be highlighted.
    pub highlight: Highlight,
}

impl Link {
    pub fn deinit(&mut self) {
        // The regex is freed on drop.
    }

    /// Returns true if this link's highlight condition matches the given mouse state.
    fn active(&self, mouse_viewport: Option<point::Coordinate>, mouse_mods: Mods) -> bool {
        match self.highlight {
            Highlight::Always => true,
            Highlight::AlwaysMods(v) => mouse_mods.equal(v),
            Highlight::Hover => mouse_viewport.is_some(),
            Highlight::HoverMods(v) => mouse_viewport.is_some() && mouse_mods.equal(v),
        }
    }
}

/// Errors from [`Set::render_cell_map`] (Zig: inferred error set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderCellMapError {
    /// Converting the render state to a string failed.
    WriteFailed,
    /// A regex search failed with something other than a mismatch.
    Oniguruma(oni::errors::Error),
}

impl std::fmt::Display for RenderCellMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderCellMapError::WriteFailed => f.write_str("WriteFailed"),
            RenderCellMapError::Oniguruma(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for RenderCellMapError {}

impl From<StringError> for RenderCellMapError {
    fn from(_: StringError) -> Self {
        RenderCellMapError::WriteFailed
    }
}

/// A set of links. This provides a higher level API for renderers
/// to match against a viewport and determine if cells are part of
/// a link.
#[derive(Debug, Default)]
pub struct Set {
    pub links: Vec<Link>,
}

impl Set {
    /// Returns the slice of links from the configuration.
    pub fn from_config(config: &[InputLink]) -> Result<Set, oni::errors::Error> {
        let mut links: Vec<Link> = Vec::with_capacity(config.len());

        for link in config {
            let regex = link.oni_regex()?;
            links.push(Link {
                regex,
                highlight: link.highlight,
            });
        }

        Ok(Set { links })
    }

    pub fn deinit(&mut self) {
        for link in &mut self.links {
            link.deinit();
        }
        self.links.clear();
    }

    /// Fills matches with the matches from regex link matches.
    pub fn render_cell_map(
        &mut self,
        result: &mut CellSet,
        render_state: &RenderState,
        mouse_viewport: Option<point::Coordinate>,
        mouse_mods: Mods,
    ) -> Result<(), RenderCellMapError> {
        // Fast path, not very likely since we have default links.
        if self.links.is_empty() {
            return Ok(());
        }

        // Determine if any links are active before building the string and
        // byte-to-cell map. Those buffers scale with viewport size and this
        // function runs during frame updates, so avoid allocating them when
        // the current mouse/modifier state can't highlight any regex links.
        if !self.links.iter().any(|link| link.active(mouse_viewport, mouse_mods)) {
            return Ok(());
        }

        // Convert our render state to a string + byte map.
        let mut builder: Vec<u8> = Vec::new();
        let mut map: StringMap = Vec::new();
        render_state.string(&mut builder, Some(&mut map))?;

        let str = &builder[..];

        // Go through each link and see if we have any matches.
        for link in &mut self.links {
            if !link.active(mouse_viewport, mouse_mods) {
                continue;
            }

            let mut offset: usize = 0;
            while offset < str.len() {
                let region = match link.regex.search(&str[offset..], oni::types::Option::default()) {
                    Ok(r) => r,
                    Err(oni::errors::Error::Mismatch) => break,
                    Err(e) => return Err(RenderCellMapError::Oniguruma(e)),
                };

                // We have a match!
                let offset_start: usize = region.starts()[0] as usize;
                let offset_end: usize = region.ends()[0] as usize;
                let start = offset + offset_start;
                let end = offset + offset_end;

                // Increment our offset by the number of bytes in the match.
                // (Zig defers this so that we can return the match before
                // modifying the offset.)
                offset = end;

                match link.highlight {
                    Highlight::Always | Highlight::AlwaysMods(_) => {}
                    Highlight::Hover | Highlight::HoverMods(_) => match mouse_viewport {
                        Some(vp) => {
                            if !map[start..end].iter().any(|pt| *pt == vp) {
                                continue;
                            }
                        }
                        None => continue,
                    },
                }

                // Record the match
                for pt in &map[start..end] {
                    result.insert(*pt);
                }
            }
        }

        let _ = LOG;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::link::Action as LinkAction;
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    fn coord(x: u16, y: u32) -> point::Coordinate {
        point::Coordinate { x, y }
    }

    fn input_link(regex: &'static str, highlight: Highlight) -> InputLink {
        InputLink {
            regex: std::borrow::Cow::Borrowed(regex.as_bytes()),
            action: LinkAction::Open,
            highlight,
        }
    }

    fn setup() -> (Terminal, RenderState) {
        oni::init(&[oni::Encoding::utf8()]).unwrap();
        let mut t = Terminal::init(TerminalOptions::new(5, 3)).unwrap();
        {
            let mut s = t.vt_stream();
            let str = "1ABCD2EFGH\r\n3IJKL";
            s.next_slice(str.as_bytes());
            s.deinit();
        }

        let mut state = RenderState::empty();
        state.update(&mut t);
        (t, state)
    }

    #[test]
    fn render_cell_map() {
        let (_t, state) = setup();

        // Get a set
        let mut set = Set::from_config(&[
            input_link("AB", Highlight::Always),
            input_link("EF", Highlight::Always),
        ])
        .unwrap();

        // Get our matches
        let mut result = CellSet::default();
        set.render_cell_map(&mut result, &state, None, Mods::default()).unwrap();
        assert!(!result.contains(&coord(0, 0)));
        assert!(result.contains(&coord(1, 0)));
        assert!(result.contains(&coord(2, 0)));
        assert!(!result.contains(&coord(3, 0)));
        assert!(result.contains(&coord(1, 1)));
        assert!(!result.contains(&coord(1, 2)));
    }

    #[test]
    fn render_cell_map_hover_links() {
        let (_t, state) = setup();

        // Get a set
        let mut set = Set::from_config(&[
            input_link("AB", Highlight::Hover),
            input_link("EF", Highlight::Always),
        ])
        .unwrap();

        // Not hovering over the first link
        {
            let mut result = CellSet::default();
            set.render_cell_map(&mut result, &state, None, Mods::default()).unwrap();

            // Test our matches
            assert!(!result.contains(&coord(0, 0)));
            assert!(!result.contains(&coord(1, 0)));
            assert!(!result.contains(&coord(2, 0)));
            assert!(!result.contains(&coord(3, 0)));
            assert!(result.contains(&coord(1, 1)));
            assert!(!result.contains(&coord(1, 2)));
        }

        // Hovering over the first link
        {
            let mut result = CellSet::default();
            set.render_cell_map(&mut result, &state, Some(coord(1, 0)), Mods::default())
                .unwrap();

            // Test our matches
            assert!(!result.contains(&coord(0, 0)));
            assert!(result.contains(&coord(1, 0)));
            assert!(result.contains(&coord(2, 0)));
            assert!(!result.contains(&coord(3, 0)));
            assert!(result.contains(&coord(1, 1)));
            assert!(!result.contains(&coord(1, 2)));
        }
    }

    #[test]
    fn render_cell_map_inactive_links_dont_allocate() {
        let (_t, state) = setup();

        let mut set = Set::from_config(&[
            input_link("AB", Highlight::Hover),
            input_link(
                "EF",
                Highlight::AlwaysMods(Mods {
                    ctrl: true,
                    ..Mods::default()
                }),
            ),
            input_link(
                "IJ",
                Highlight::HoverMods(Mods {
                    shift: true,
                    ..Mods::default()
                }),
            ),
        ])
        .unwrap();

        // No link is active, so we return before building the string
        // (Zig checks this with a failing allocator).
        assert!(!set.links.iter().any(|l| l.active(None, Mods::default())));

        let mut result = CellSet::default();
        set.render_cell_map(&mut result, &state, None, Mods::default()).unwrap();

        assert_eq!(0usize, result.len());
    }

    #[test]
    fn render_cell_map_mods_no_match() {
        let (_t, state) = setup();

        // Get a set
        let mut set = Set::from_config(&[
            input_link("AB", Highlight::Always),
            input_link(
                "EF",
                Highlight::AlwaysMods(Mods {
                    ctrl: true,
                    ..Mods::default()
                }),
            ),
        ])
        .unwrap();

        // Get our matches
        let mut result = CellSet::default();
        set.render_cell_map(&mut result, &state, None, Mods::default()).unwrap();

        // Test our matches
        assert!(!result.contains(&coord(0, 0)));
        assert!(result.contains(&coord(1, 0)));
        assert!(result.contains(&coord(2, 0)));
        assert!(!result.contains(&coord(3, 0)));
        assert!(!result.contains(&coord(1, 1)));
        assert!(!result.contains(&coord(1, 2)));
    }
}
