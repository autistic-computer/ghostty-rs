//! Port of `src/benchmark/HyperlinkMap.zig`.
//!
//! STATUS: ported
//!
//! Benchmark hyperlink cell-map lookups and remove/insert churn.
//!
//! Hyperlink cells are stored in a fixed-capacity, open-addressed hash map.
//! The `churn` mode models terminal output that repeatedly replaces cells in
//! a page whose hyperlink map is already close to full. This is particularly
//! useful for catching probe-length cliffs at high load factors.

use std::any::Any;
use std::borrow::Cow;
use std::fmt;
use std::hint::black_box;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use crate::cli::args::{cli_enum, cli_struct};
use crate::terminal::hyperlink;
use crate::terminal::page::{self, Capacity, Page};
use crate::terminal::size::AllocError;

const LOG: &str = "hyperlink-map-bench";

pub struct HyperlinkMap {
    pub opts: Options,
    pub page: Page,
    pub link_id: hyperlink::Id,
    pub entry_count: usize,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Requested hyperlink working-set size. Must be a power of two and at
        /// least 16. The map may reserve additional probe headroom.
        pub entries: u16 = 4096,

        /// Percentage of the map populated before the timed operation.
        /// Values above 100 are treated as 100.
        pub load_percent as "load-percent": u8 = 100,

        /// Number of complete passes over the populated cells per step.
        pub loops: u16 = 1,

        /// Operation to perform in the timed region.
        pub mode: Mode = Mode::Churn,
    }
}

cli_enum! {
    pub enum Mode {
        /// Look up every populated hyperlink cell.
        Lookup => "lookup",

        /// Remove and reinsert every populated hyperlink cell.
        Churn => "churn",
    }
}

/// Errors from [`HyperlinkMap::create`] (Zig's inferred error set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateError {
    InvalidEntries,
    UnexpectedCapacity,
    OutOfMemory,
    InsertHyperlink(page::InsertHyperlinkError),
    Hyperlink(page::HyperlinkError),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateError::InvalidEntries => f.write_str("InvalidEntries"),
            CreateError::UnexpectedCapacity => f.write_str("UnexpectedCapacity"),
            CreateError::OutOfMemory => f.write_str("OutOfMemory"),
            CreateError::InsertHyperlink(e) => write!(f, "{e:?}"),
            CreateError::Hyperlink(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for CreateError {}

impl From<AllocError> for CreateError {
    fn from(_: AllocError) -> Self {
        CreateError::OutOfMemory
    }
}

impl BenchmarkImpl for HyperlinkMap {
    type Options = Options;
    type CreateError = CreateError;

    fn create(opts: Options) -> Result<Box<HyperlinkMap>, CreateError> {
        if opts.entries < 16 || !opts.entries.is_power_of_two() {
            log::error!(target: LOG, "entries must be a power of two greater than or equal to 16");
            return Err(CreateError::InvalidEntries);
        }

        // The page requests one map slot per `hyperlink_cell_multiplier` set
        // entries. Keep this relationship explicit so `entries` is the working
        // set size under test regardless of the map's reserved probe headroom.
        let set_entries = opts.entries / 16;
        let mut page = Page::init(Capacity {
            hyperlink_bytes: (set_entries as usize * std::mem::size_of::<hyperlink::SetItem>())
                as _,
            ..Capacity::new(opts.entries, 1)
        })?;

        // From here on the page must be deinitialized on error.
        let result = (|| -> Result<(hyperlink::Id, usize), CreateError> {
            if page.hyperlink_capacity() < opts.entries as usize {
                log::error!(
                    target: LOG,
                    "insufficient map capacity expected_at_least={} actual={}",
                    opts.entries,
                    page.hyperlink_capacity(),
                );
                return Err(CreateError::UnexpectedCapacity);
            }

            let link_id = page
                .insert_hyperlink(&hyperlink::Hyperlink {
                    id: hyperlink::HyperlinkId::Implicit(1),
                    uri: Cow::Borrowed(b"https://example.com/benchmark"),
                })
                .map_err(CreateError::InsertHyperlink)?;

            let load = opts.load_percent.min(100) as usize;
            let entry_count = 1.max((opts.entries as usize * load) / 100);
            for x in 0..entry_count {
                let rac = page.get_row_and_cell(x, 0);
                // SAFETY: the page memory is live and owns the set.
                unsafe { page.hyperlink_set.use_(page.memory, link_id) };
                // SAFETY: `rac` points into this page's memory.
                unsafe { page.set_hyperlink(&mut *rac.row, &mut *rac.cell, link_id) }
                    .map_err(CreateError::Hyperlink)?;
            }
            Ok((link_id, entry_count))
        })();

        match result {
            Ok((link_id, entry_count)) => Ok(Box::new(HyperlinkMap {
                opts,
                page,
                link_id,
                entry_count,
            })),
            Err(err) => {
                page.deinit();
                Err(err)
            }
        }
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Lookup => step_lookup,
            Mode::Churn => step_churn,
        };
        Benchmark::init(self, VTable::new(step_fn))
    }
}

impl Drop for HyperlinkMap {
    fn drop(&mut self) {
        self.page.deinit();
    }
}

fn step_lookup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut HyperlinkMap = cast(ptr);
    for _ in 0..self_.opts.loops {
        for x in 0..self_.entry_count {
            let cell = self_.page.get_row_and_cell(x, 0).cell;
            // SAFETY: `cell` points into this page's memory.
            let id = self_
                .page
                .lookup_hyperlink(unsafe { &*cell })
                .ok_or(benchmark::Error::BenchmarkFailed)?;
            black_box(id);
        }
    }
    Ok(())
}

fn step_churn(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut HyperlinkMap = cast(ptr);
    for _ in 0..self_.opts.loops {
        for x in 0..self_.entry_count {
            let rac = self_.page.get_row_and_cell(x, 0);
            // SAFETY: `rac` points into this page's memory, which stays
            // live and is not otherwise borrowed during these calls.
            unsafe {
                self_.page.clear_hyperlink(&mut *rac.cell);
                self_.page.hyperlink_set.use_(self_.page.memory, self_.link_id);
                self_
                    .page
                    .set_hyperlink(&mut *rac.row, &mut *rac.cell, self_.link_id)
                    .map_err(|_| benchmark::Error::BenchmarkFailed)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn hyperlink_map() {
        for mode in [Mode::Lookup, Mode::Churn] {
            let mut impl_ = HyperlinkMap::create(Options {
                entries: 64,
                mode,
                ..Options::default()
            })
            .unwrap();

            let mut bench = impl_.benchmark();
            let _ = bench.run(RunMode::Once).unwrap();
        }
    }
}
