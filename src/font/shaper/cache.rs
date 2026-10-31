//! Port of `src/font/shaper/Cache.zig`.
//!
//! STATUS: ported
//!
//! This structure caches the shaped cells for a given text run.
//!
//! At one point, shaping was the most expensive part of rendering text
//! (accounting for 96% of frame time on my machine). To speed it up, this
//! was introduced so that shaping results can be cached depending on the
//! run.
//!
//! The cache key is the text run. The text run builds its own hash value
//! based on the font, style, codepoint, etc. This just utilizes the hash that
//! the text run provides.

use crate::datastruct::cache_table::{CacheTable, CacheTableContext};
use crate::font::shape::{Cell, TextRun};

#[allow(dead_code)]
const LOG: &str = "font_shaper_cache";

/// Context for cache table.
#[derive(Debug, Default, Clone, Copy)]
pub struct CellCacheTableContext;

impl CacheTableContext<u64, Vec<Cell>> for CellCacheTableContext {
    fn hash(&self, key: &u64) -> u64 {
        *key
    }

    fn eql(&self, a: &u64, b: &u64) -> bool {
        a == b
    }
}

/// Cache table for run hash -> shaped cells.
pub type CellCacheTable = CacheTable<
    u64,
    Vec<Cell>,
    CellCacheTableContext,
    // Capacity is slightly arbitrary. These numbers are guesses.
    //
    // I'd expect then an average of 256 frequently cached runs is a
    // safe guess most terminal screens.
    256,
    // 8 items per bucket to give decent resiliency to important runs.
    8,
>;

pub struct Cache {
    /// The cache table of shaped cells.
    pub map: CellCacheTable,
}

impl Default for Cache {
    fn default() -> Cache {
        Cache::init()
    }
}

impl Cache {
    pub fn init() -> Cache {
        Cache {
            map: CellCacheTable::new(CellCacheTableContext),
        }
    }

    /// Get the shaped cells for the given text run,
    /// or null if they are not in the cache.
    pub fn get(&mut self, run: &TextRun<'_>) -> Option<&[Cell]> {
        self.map.get(&run.hash).map(|v| v.as_slice())
    }

    /// Insert the shaped cells for the given text run into the cache.
    ///
    /// The cells will be duplicated.
    pub fn put(&mut self, run: &TextRun<'_>, cells: &[Cell]) {
        let copy = cells.to_vec();
        // The evicted value (if any) is freed when dropped.
        let _evicted = self.map.put(run.hash, copy);
    }

    #[allow(dead_code)]
    fn clear(&mut self) {
        self.map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::codepoint_resolver::CodepointResolver;
    use crate::font::collection::{AddOptions, Collection, Index, LoadOptions, SizeAdjustment};
    use crate::font::embedded;
    use crate::font::face::{DesiredSize, Face};
    use crate::font::library::Library;
    use crate::font::shared_grid::SharedGrid;
    use crate::font::Style;

    #[test]
    fn cache() {
        let mut lib = Library::init().unwrap();
        {
            let mut c = Cache::init();

            // The Zig test uses an `undefined` run with only the hash set;
            // a Rust `TextRun` needs a real grid reference.
            let mut col = Collection::init();
            col.load_options = Some(LoadOptions::new(lib.clone()));
            col.add(
                Face::init(lib.clone(), embedded::REGULAR, DesiredSize::new(12.0).into()).unwrap(),
                AddOptions {
                    style: Style::Regular,
                    fallback: false,
                    size_adjustment: SizeAdjustment::None,
                },
            )
            .unwrap();
            let grid = SharedGrid::init(CodepointResolver::new(col)).unwrap();

            let run = TextRun {
                hash: 1,
                offset: 0,
                cells: 0,
                grid: &grid,
                font_index: Index::default(),
            };
            assert!(c.get(&run).is_none());
            c.put(
                &run,
                &[
                    Cell {
                        x: 0,
                        glyph_index: 0,
                        ..Default::default()
                    },
                    Cell {
                        x: 1,
                        glyph_index: 1,
                        ..Default::default()
                    },
                ],
            );

            let actual = c.get(&run).unwrap();
            assert!(actual.len() == 2);
        }
        lib.deinit();
    }
}
