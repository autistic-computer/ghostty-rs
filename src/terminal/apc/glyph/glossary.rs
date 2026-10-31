//! Port of `src/terminal/apc/glyph/Glossary.zig`.
//!
//! STATUS: ported
//!
//! Glossary is the per-terminal storage for Glyph Protocol
//! codepoints. We use the word Glossary to match up with the spec which
//! also uses this word.
//!
//! Port notes / deviations:
//! - `std.AutoArrayHashMapUnmanaged` is an [`IndexMap`] (insertion ordered,
//!   O(1) lookup, `shift_remove` for ordered removal).
//! - Allocator parameters are dropped; entries free their outlines on drop.

use std::fmt;

use indexmap::IndexMap;

use super::request::{self, register_option as ro, Register as RegisterReq};
use crate::font::opentype::glyf::Outline;

use crate::font::glyph::{Constraint, ConstraintAlign, ConstraintSize, DesignMetrics};

/// Maximum entries allowed in the glossary before eviction.
/// Defined by the specification.
pub const MAX_ENTRIES: usize = 1024;

/// Maximum size of any allocation made while decoding a glyf registration.
/// This is limited by the dynamic allocations not the struct size so the
/// actual size allowed is slightly larger, but worst case only by 10KB.
pub const MAX_GLYF_ALLOCATION: usize = 64 * 1024; // 64 KiB

/// Errors that can occur while registering a glossary entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    OutOfMemory,
    OutOfNamespace,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for RegisterError {}

/// Errors that can occur while clearing glossary entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearError {
    OutOfNamespace,
}

impl fmt::Display for ClearError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ClearError {}

/// The per-terminal Glyph Protocol registration store.
#[derive(Debug, Default)]
pub struct Glossary {
    /// The set of entries in the glossary keyed by the codepoint.
    ///
    /// The array hash map preserves insertion order and has O(N)
    /// orderedRemove, so we use it as a FIFO too for eviction when
    /// the glossary is full. Since the specification limits the protocol
    /// to 1024 maximum entries, ordered removal should never be that
    /// expensive.
    ///
    /// I'm also operating under the assumption that full glossaries
    /// for a session will be rare, so the eviction cost shouldn't
    /// happen regularly.
    pub entries: IndexMap<u32, Entry>,
}

impl Glossary {
    /// An empty glossary with no registered glyphs.
    pub fn empty() -> Glossary {
        Glossary {
            entries: IndexMap::new(),
        }
    }

    /// Register the given glyph entry.
    ///
    /// This will act according to the glyph specification
    pub fn register(&mut self, cp: u32, entry: Entry) -> Result<(), RegisterError> {
        // Validate codepoint according to spec.
        if !is_private_use(cp) {
            return Err(RegisterError::OutOfNamespace);
        }

        if self.entries.contains_key(&cp) {
            // Found an existing entry, we need to shift the FIFO so
            // that this is now the most recent (at the end). This is
            // O(N) but N is usually small and max N is bounded by the spec.
            let removed = self.entries.shift_remove(&cp);
            debug_assert!(removed.is_some());

            // We already had enough capacity for this key before removing it, so
            // reinserting the replacement cannot require another allocation.
            self.entries.insert(cp, entry);
            return Ok(());
        }

        // Array hash maps preserve insertion order so always immediately insert.
        self.entries
            .try_reserve(1)
            .map_err(|_| RegisterError::OutOfMemory)?;
        self.entries.insert(cp, entry);

        // Fast, typical path: we fit within the glossary, just return.
        if self.entries.len() <= MAX_ENTRIES {
            return Ok(());
        }

        // Slow path: we need to evict.
        self.entries.shift_remove_index(0);
        Ok(())
    }

    /// Delete a single entry from the glossary. If the entry doesn't exist,
    /// then this does nothing and is safe.
    pub fn delete(&mut self, cp: u32) -> Result<(), ClearError> {
        if !is_private_use(cp) {
            return Err(ClearError::OutOfNamespace);
        }
        self.entries.shift_remove(&cp);
        Ok(())
    }

    /// Clear all entries from the glossary and free up any underlying
    /// storage.
    pub fn clear_and_free(&mut self) {
        self.entries = IndexMap::new();
    }

    /// Contains returns true if the codepoint is covered by the glossary.
    pub fn contains(&self, cp: u32) -> bool {
        self.entries.contains_key(&cp)
    }
}

/// Stored glyph payload variants (`Entry.Glyph`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Glyph {
    Glyf(Outline),
}

/// A single glyph registration entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The glyph itself. The tagged union only has glyf right now but
    /// will eventually expand to support COLR and maybe other formats.
    /// These are stored as raw outlines; rasterization is delayed to
    /// renderers. The outlines have been validated.
    pub glyph: Glyph,

    /// Authored metrics for the glyph's design coordinate space.
    pub design: DesignMetrics,

    /// Unicode cell width requested by the registration.
    pub width: request::Width,

    /// Normalized scale, alignment, and padding behavior for rasterization.
    pub constraint: Constraint,
}

/// Errors that can occur while constructing a glossary entry from a
/// register request (`Entry.InitError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    // RegisterReq.DecodeError
    OutOfMemory,
    PayloadTooLarge,
    MalformedPayload,
    CompositeUnsupported,
    HintingUnsupported,

    /// The register request is missing a required option or has an invalid
    /// explicitly-provided option value.
    InvalidOptions,

    /// The requested payload format is not supported by this glossary.
    UnsupportedFormat,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitError {}

impl From<request::DecodeError> for InitError {
    fn from(e: request::DecodeError) -> Self {
        match e {
            request::DecodeError::OutOfMemory => InitError::OutOfMemory,
            request::DecodeError::PayloadTooLarge => InitError::PayloadTooLarge,
            request::DecodeError::MalformedPayload => InitError::MalformedPayload,
            request::DecodeError::CompositeUnsupported => InitError::CompositeUnsupported,
            request::DecodeError::HintingUnsupported => InitError::HintingUnsupported,
        }
    }
}

impl Entry {
    /// Initialize a glossary entry from a register request.
    ///
    /// This validates the request fields needed to construct the entry,
    /// decodes the base64 glyph payload, and stores the decoded outline. The
    /// returned entry owns decoded glyph memory.
    pub fn init(req: &RegisterReq) -> Result<Entry, InitError> {
        // Validate format
        let fmt = req.get(ro::Fmt).ok_or(InitError::InvalidOptions)?;
        let design = DesignMetrics {
            units_per_em: req.get(ro::Upm).ok_or(InitError::InvalidOptions)?,
            advance_width: req.get(ro::Aw).ok_or(InitError::InvalidOptions)?,
            line_height: req.get(ro::Lh).ok_or(InitError::InvalidOptions)?,
        };
        if design.units_per_em == 0 || design.advance_width == 0 || design.line_height == 0 {
            return Err(InitError::InvalidOptions);
        }
        let width = req.get(ro::Width).ok_or(InitError::InvalidOptions)?;

        // Get our constraints
        let constraint = Self::constraint_from_register(req)?;

        // Decode the payload into some usable glyph format for
        // future rasterization.
        let glyph = match fmt {
            request::Format::Glyf => Glyph::Glyf(req.decode_glyf_payload(MAX_GLYF_ALLOCATION)?),
            request::Format::Colrv0 | request::Format::Colrv1 => {
                return Err(InitError::UnsupportedFormat)
            }
        };

        Ok(Entry {
            glyph,
            design,
            width,
            constraint,
        })
    }

    /// Return the renderer constraint for a register request.
    ///
    /// Glyph Protocol §8.5 defines sizing, alignment, and padding in terms of
    /// the authored extent and render span. This function is the single
    /// normalization point for how protocol sizing choices map to the
    /// renderer-neutral constraint stored here.
    fn constraint_from_register(req: &RegisterReq) -> Result<Constraint, InitError> {
        // Register.get applies the Glyph Protocol §6.1 defaults when options
        // are omitted: size=height, align=center,center, and pad=0,0,0,0.
        let size = req.get(ro::Size).ok_or(InitError::InvalidOptions)?;
        let alignment = req.get(ro::Align).ok_or(InitError::InvalidOptions)?;
        let pad = req.get(ro::Pad).ok_or(InitError::InvalidOptions)?;

        Ok(Constraint {
            size: match size {
                // The rasterizer's base transform already maps the design em
                // to the cell height. That is the closest existing behavior to
                // the protocol's default height-driven mode.
                request::Size::Height => ConstraintSize::None,
                // There is no width-driven, aspect-preserving constraint mode
                // today. Leave the base transform intact rather than forcing a
                // fit/contain policy that would unexpectedly prevent overflow.
                request::Size::Advance => ConstraintSize::None,
                // Constraint.cover currently scales preserving aspect ratio to
                // the available bounds, which is the best existing match for
                // the protocol's contain mode.
                request::Size::Contain => ConstraintSize::Cover,
                // There is no true protocol-cover equivalent that chooses the
                // larger axis scale, so use the nearest named renderer policy.
                request::Size::Cover => ConstraintSize::Cover,
                request::Size::Stretch => ConstraintSize::Stretch,
            },
            align_horizontal: match alignment.horizontal {
                request::Horizontal::Start => ConstraintAlign::Start,
                request::Horizontal::Center => ConstraintAlign::Center,
                request::Horizontal::End => ConstraintAlign::End,
            },
            align_vertical: match alignment.vertical {
                request::Vertical::Start => ConstraintAlign::Start,
                request::Vertical::Center => ConstraintAlign::Center,
                request::Vertical::End => ConstraintAlign::End,
                // The current constraint API has no baseline alignment mode.
                // Start is the closest stable default because the glyf
                // rasterizer's coordinate model already treats y=0 as the
                // baseline/bottom before constraints are applied.
                request::Vertical::Baseline => ConstraintAlign::Start,
            },
            pad_top: pad.top,
            pad_right: pad.right,
            pad_bottom: pad.bottom,
            pad_left: pad.left,
            ..Constraint::NONE
        })
    }
}

/// Return true if `cp` is in one of the Unicode Private Use Areas.
fn is_private_use(cp: u32) -> bool {
    (0xE000..=0xF8FF).contains(&cp)
        || (0xF0000..=0xFFFFD).contains(&cp)
        || (0x100000..=0x10FFFD).contains(&cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parse_register(data: &[u8]) -> RegisterReq {
        match request::Request::parse(data.to_vec()).unwrap() {
            request::Request::Register(reg) => reg,
            _ => unreachable!(),
        }
    }

    // Base64-encoded glyf payload from the "glyf: decode triangle" test in
    // font/opentype/glyf.zig. This is a real simple-glyph record with one contour
    // and three on-curve points.
    const TEST_TRIANGLE_GLYF_PAYLOAD: &str = "AAEAZABkA4QDhAACAAABAQEB9P5wAyADhPzgAAA=";

    fn base64_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn test_repeated_point_register_req(end_points: &[u16]) -> RegisterReq {
        assert!(!end_points.is_empty());

        let mut decoded: Vec<u8> = Vec::new();

        // Simple glyph header with zero bounds.
        let contour_count = end_points.len() as u16;
        decoded.extend_from_slice(&contour_count.to_be_bytes());
        decoded.extend_from_slice(&[0; 8]);
        for &end_point in end_points {
            decoded.extend_from_slice(&end_point.to_be_bytes());
        }
        decoded.extend_from_slice(&[0x00, 0x00]); // No instructions.

        // Maximum-length runs describe on-curve, zero-delta points compactly.
        let mut remaining: usize = end_points[end_points.len() - 1] as usize + 1;
        while remaining > 0 {
            let run = remaining.min(256);
            if run == 1 {
                decoded.push(0x31);
            } else {
                decoded.extend_from_slice(&[0x39, (run - 1) as u8]);
            }
            remaining -= run;
        }

        let encoded = base64_encode(&decoded);
        let command = format!("r;cp=e000;{}", encoded);
        test_parse_register(command.as_bytes())
    }

    fn test_register_req(cp: u32) -> RegisterReq {
        let data = format!(
            "r;cp={:x};upm=2048;aw=1024;lh=1536;width=2;size=stretch;align=end,start;pad=0.1,0.2,0.3,0.4;{}",
            cp, TEST_TRIANGLE_GLYF_PAYLOAD
        );
        test_parse_register(data.as_bytes())
    }

    fn test_register_entry(cp: u32) -> Entry {
        let req = test_register_req(cp);
        Entry::init(&req).unwrap()
    }

    fn outline(entry: &Entry) -> &Outline {
        match &entry.glyph {
            Glyph::Glyf(o) => o,
        }
    }

    #[test]
    fn entry_init_decodes_glyf_payload_and_applies_register_fields() {
        let req = test_register_req(0xE000);
        let entry = Entry::init(&req).unwrap();

        assert_eq!(2048u32, entry.design.units_per_em);
        assert_eq!(1024u32, entry.design.advance_width);
        assert_eq!(1536u32, entry.design.line_height);
        assert_eq!(request::Width::Wide, entry.width);
        assert_eq!(ConstraintSize::Stretch, entry.constraint.size);
        assert_eq!(ConstraintAlign::End, entry.constraint.align_horizontal);
        assert_eq!(ConstraintAlign::Start, entry.constraint.align_vertical);
        assert_eq!(0.1f64, entry.constraint.pad_top);
        assert_eq!(0.2f64, entry.constraint.pad_right);
        assert_eq!(0.3f64, entry.constraint.pad_bottom);
        assert_eq!(0.4f64, entry.constraint.pad_left);

        assert_eq!(3, outline(&entry).points.len());
        assert_eq!(1, outline(&entry).contours.len());
    }

    #[test]
    fn entry_init_rejects_invalid_register_payload() {
        let req = test_parse_register(b"r;cp=e000;%%%not-base64%%%");
        assert_eq!(Err(InitError::MalformedPayload), Entry::init(&req));
    }

    #[test]
    fn entry_init_rejects_glyf_allocation_over_limit() {
        // 5,462 points require a 65,544-byte points allocation.
        let req = test_repeated_point_register_req(&[5461]);
        assert_eq!(Err(InitError::PayloadTooLarge), Entry::init(&req));
    }

    #[test]
    fn entry_init_accepts_largest_glyf_points_allocation_under_limit() {
        // 5,461 points require a 65,532-byte points allocation.
        let req = test_repeated_point_register_req(&[0, 5460]);
        let entry = Entry::init(&req).unwrap();
        assert_eq!(5461, outline(&entry).points.len());
        assert_eq!(2, outline(&entry).contours.len());
    }

    #[test]
    fn glossary_register_overwrites_and_moves_entry_to_newest_position() {
        let mut glossary = Glossary::empty();

        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();
        glossary
            .register(0xE001, test_register_entry(0xE001))
            .unwrap();
        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();

        assert_eq!(2, glossary.entries.len());
        assert_eq!(0xE001u32, *glossary.entries.get_index(0).unwrap().0);
        assert_eq!(0xE000u32, *glossary.entries.get_index(1).unwrap().0);
    }

    #[test]
    fn glossary_register_evicts_oldest_entry() {
        let mut glossary = Glossary::empty();

        for i in 0..MAX_ENTRIES + 1 {
            let cp = (0xE000 + i) as u32;
            glossary.register(cp, test_register_entry(cp)).unwrap();
        }

        assert_eq!(MAX_ENTRIES, glossary.entries.len());
        assert!(!glossary.entries.contains_key(&0xE000));
        assert!(glossary.entries.contains_key(&0xE001));
        assert!(glossary
            .entries
            .contains_key(&(0xE000 + MAX_ENTRIES as u32)));
    }

    #[test]
    fn glossary_register_rejects_non_pua_codepoint() {
        let mut glossary = Glossary::empty();

        let entry = test_register_entry(0xE000);
        assert_eq!(
            Err(RegisterError::OutOfNamespace),
            glossary.register('A' as u32, entry)
        );
    }

    #[test]
    fn glossary_delete_removes_one_pua_slot_and_ignores_empty_pua_slot() {
        let mut glossary = Glossary::empty();

        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();
        glossary
            .register(0xE001, test_register_entry(0xE001))
            .unwrap();

        glossary.delete(0xE000).unwrap();
        assert_eq!(1, glossary.entries.len());
        assert!(!glossary.contains(0xE000));
        assert!(glossary.contains(0xE001));

        glossary.delete(0xE000).unwrap();
        assert_eq!(1, glossary.entries.len());
        assert!(glossary.contains(0xE001));
    }

    #[test]
    fn glossary_delete_rejects_non_pua_codepoint() {
        let mut glossary = Glossary::empty();

        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();
        assert_eq!(Err(ClearError::OutOfNamespace), glossary.delete('A' as u32));
        assert_eq!(1, glossary.entries.len());
        assert!(glossary.contains(0xE000));
    }

    #[test]
    fn glossary_clear_and_free_removes_all_slots_and_remains_reusable() {
        let mut glossary = Glossary::empty();

        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();
        glossary
            .register(0xE001, test_register_entry(0xE001))
            .unwrap();

        glossary.clear_and_free();
        assert_eq!(0, glossary.entries.len());
        assert!(!glossary.contains(0xE000));
        assert!(!glossary.contains(0xE001));

        glossary
            .register(0xE002, test_register_entry(0xE002))
            .unwrap();
        assert_eq!(1, glossary.entries.len());
        assert!(glossary.contains(0xE002));
    }

    #[test]
    fn glossary_contains_reports_registered_slots() {
        let mut glossary = Glossary::empty();

        assert!(!glossary.contains(0xE000));

        glossary
            .register(0xE000, test_register_entry(0xE000))
            .unwrap();
        assert!(glossary.contains(0xE000));
        assert!(!glossary.contains(0xE001));
    }
}
