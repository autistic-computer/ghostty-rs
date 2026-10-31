//! Port of `src/terminal/Tabstops.zig`.
//!
//! STATUS: ported
//!
//! Keep track of the location of tabstops.
//!
//! This is implemented as a bit set. There is a preallocation segment that
//! is used for almost all screen sizes. Then there is a dynamically allocated
//! segment if the screen is larger than the preallocation amount.
//!
//! In reality, tabstops don't need to be the most performant in any metric.
//! This implementation tries to balance denser memory usage (by using a bitset)
//! and minimizing unnecessary allocations.

use std::fmt;

use crate::tripwire;

/// Unit is the type we use per tabstop unit (see file docs).
type Unit = u8;
const UNIT_BITS: usize = Unit::BITS as usize;

/// The number of columns we preallocate for. This is kind of high which
/// costs us some memory, but this is more columns than my 6k monitor at
/// 12-point font size, so this should prevent allocation in almost all
/// real world scenarios for the price of wasting at most
/// (columns / sizeOf(Unit)) bytes.
const PREALLOC_COLUMNS: usize = 512;

/// The number of entries we need for our preallocation.
const PREALLOC_COUNT: usize = PREALLOC_COLUMNS / UNIT_BITS;

/// We precompute all the possible masks since we never use a huge bit size.
const MASKS: [Unit; UNIT_BITS] = {
    let mut res = [0 as Unit; UNIT_BITS];
    let mut i = 0;
    while i < UNIT_BITS {
        res[i] = 1 << i;
        i += 1;
    }
    res
};

/// Allocation failure (`Allocator.Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabstopsError {
    OutOfMemory,
}

impl fmt::Display for TabstopsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for TabstopsError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tabstops {
    /// The number of columns this tabstop is set to manage. Use resize()
    /// to change this number.
    pub cols: usize,

    /// Preallocated tab stops.
    pub prealloc_stops: [Unit; PREALLOC_COUNT],

    /// Dynamically expanded stops above prealloc stops.
    pub dynamic_stops: Vec<Unit>,
}

impl Default for Tabstops {
    fn default() -> Self {
        Self {
            cols: 0,
            prealloc_stops: [0; PREALLOC_COUNT],
            dynamic_stops: Vec::new(),
        }
    }
}

/// Returns the entry in the stops array that would contain this column.
#[inline]
fn entry(col: usize) -> usize {
    col / UNIT_BITS
}

#[inline]
fn index(col: usize) -> usize {
    col % UNIT_BITS
}

/// Failure points for `resize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeFailPoint {
    DynamicAlloc,
}

static RESIZE_TW: tripwire::Module<ResizeFailPoint, TabstopsError> = tripwire::Module::new();

impl Tabstops {
    pub fn init(cols: usize, interval: usize) -> Result<Tabstops, TabstopsError> {
        let mut res = Tabstops::default();
        res.resize(cols)?;
        res.reset(interval);
        Ok(res)
    }

    /// Set the tabstop at a certain column. The columns are 0-indexed.
    pub fn set(&mut self, col: usize) {
        let i = entry(col);
        let idx = index(col);
        if i < PREALLOC_COUNT {
            self.prealloc_stops[i] |= MASKS[idx];
            return;
        }

        let dynamic_i = i - PREALLOC_COUNT;
        debug_assert!(dynamic_i < self.dynamic_stops.len());
        self.dynamic_stops[dynamic_i] |= MASKS[idx];
    }

    /// Unset the tabstop at a certain column. The columns are 0-indexed.
    pub fn unset(&mut self, col: usize) {
        let i = entry(col);
        let idx = index(col);
        if i < PREALLOC_COUNT {
            self.prealloc_stops[i] &= !MASKS[idx];
            return;
        }

        let dynamic_i = i - PREALLOC_COUNT;
        debug_assert!(dynamic_i < self.dynamic_stops.len());
        self.dynamic_stops[dynamic_i] &= !MASKS[idx];
    }

    /// Get the value of a tabstop at a specific column. The columns are 0-indexed.
    pub fn get(&self, col: usize) -> bool {
        let i = entry(col);
        let idx = index(col);
        let mask = MASKS[idx];
        let unit = if i < PREALLOC_COUNT {
            self.prealloc_stops[i]
        } else {
            let dynamic_i = i - PREALLOC_COUNT;
            debug_assert!(dynamic_i < self.dynamic_stops.len());
            self.dynamic_stops[dynamic_i]
        };

        unit & mask == mask
    }

    /// Resize this to support up to cols columns.
    // TODO: needs interval to set new tabstops
    pub fn resize(&mut self, cols: usize) -> Result<(), TabstopsError> {
        // Do nothing if it fits.
        if cols <= PREALLOC_COLUMNS {
            self.cols = cols;
            return Ok(());
        }

        // Number of units needed beyond the preallocated columns.
        let dynamic_count = (cols - PREALLOC_COLUMNS).div_ceil(UNIT_BITS);
        if dynamic_count <= self.dynamic_stops.len() {
            self.cols = cols;
            return Ok(());
        }

        let tw = &RESIZE_TW;
        tw.check(ResizeFailPoint::DynamicAlloc)?;
        let additional = dynamic_count - self.dynamic_stops.len();
        self.dynamic_stops
            .try_reserve_exact(additional)
            .map_err(|_| TabstopsError::OutOfMemory)?;
        // New units are zeroed.
        self.dynamic_stops.resize(dynamic_count, 0);
        self.cols = cols;
        Ok(())
    }

    /// Return the maximum number of columns this can support currently.
    pub fn capacity(&self) -> usize {
        (PREALLOC_COUNT + self.dynamic_stops.len()) * UNIT_BITS
    }

    /// Unset all tabstops and then reset the initial tabstops to the given
    /// interval. An interval of 0 sets no tabstops.
    pub fn reset(&mut self, interval: usize) {
        self.prealloc_stops.fill(0);
        self.dynamic_stops.fill(0);

        if interval == 0 || self.cols <= 1 {
            return;
        }

        let mut i = interval;
        while i < self.cols - 1 {
            self.set(i);
            i += interval;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabstops_basic() {
        let mut t = Tabstops::default();
        assert_eq!(0usize, entry(4));
        assert_eq!(1usize, entry(8));
        assert_eq!(0usize, index(0));
        assert_eq!(1usize, index(1));
        assert_eq!(1usize, index(9));

        assert_eq!(0b00001000 as Unit, MASKS[3]);
        assert_eq!(0b00010000 as Unit, MASKS[4]);

        assert!(!t.get(4));
        t.set(4);
        assert!(t.get(4));
        assert!(!t.get(3));

        t.reset(0);
        assert!(!t.get(4));

        t.set(4);
        assert!(t.get(4));
        t.unset(4);
        assert!(!t.get(4));

        // Unsetting a column with no tabstop should be a no-op, not a toggle.
        t.unset(4);
        assert!(!t.get(4));
    }

    #[test]
    fn tabstops_dynamic_allocations() {
        let mut t = Tabstops::default();

        // Grow by less than one unit to verify the allocation rounds up.
        let cap = t.capacity();
        t.resize(cap + 5).unwrap();
        assert_eq!(cap + UNIT_BITS, t.capacity());

        // Set something that was out of range of the first
        t.set(cap + 4);
        assert!(t.get(cap + 4));
        assert!(!t.get(cap + 3));

        // Unsetting a column with no tabstop should be a no-op, not a toggle.
        t.unset(cap + 3);
        assert!(!t.get(cap + 3));

        // Growing again preserves existing stops and clears the new unit.
        t.resize(cap + UNIT_BITS + 1).unwrap();
        assert!(t.get(cap + 4));
        assert!(!t.get(cap + UNIT_BITS));

        // Prealloc still works
        assert!(!t.get(5));
    }

    #[test]
    fn tabstops_resize_to_existing_capacity_does_not_allocate() {
        // The Zig test uses a fixed buffer allocator sized for exactly one
        // allocation; here we check the second resize keeps the same buffer.
        let mut t = Tabstops::default();

        t.resize(PREALLOC_COLUMNS * 2).unwrap();
        let ptr = t.dynamic_stops.as_ptr();
        let cap = t.dynamic_stops.capacity();
        assert_eq!(cap, PREALLOC_COUNT);
        t.resize(PREALLOC_COLUMNS * 2).unwrap();
        assert_eq!(ptr, t.dynamic_stops.as_ptr());
        assert_eq!(cap, t.dynamic_stops.capacity());
    }

    #[test]
    fn tabstops_interval() {
        let t = Tabstops::init(80, 4).unwrap();
        assert!(!t.get(0));
        assert!(t.get(4));
        assert!(!t.get(5));
        assert!(t.get(8));
    }

    #[test]
    fn tabstops_interval_with_zero_columns() {
        let t = Tabstops::init(0, 8).unwrap();

        assert_eq!(0usize, t.cols);
    }

    #[test]
    fn tabstops_count_on_80() {
        // https://superuser.com/questions/710019/why-there-are-11-tabstops-on-a-80-column-console

        let t = Tabstops::init(80, 8).unwrap();

        // Count the tabstops
        let count = (0..80).filter(|&i| t.get(i)).count();

        assert_eq!(9usize, count);
    }

    #[test]
    fn tabstops_resize_alloc_failure_preserves_state() {
        // This test verifies that if resize() fails during allocation,
        // the original cols value is preserved (not corrupted).
        let mut t = Tabstops::init(80, 8).unwrap();

        let original_cols = t.cols;

        // Trigger allocation failure when resizing beyond prealloc
        RESIZE_TW.error_always(ResizeFailPoint::DynamicAlloc, TabstopsError::OutOfMemory);
        let result = t.resize(PREALLOC_COLUMNS * 2);
        assert_eq!(Err(TabstopsError::OutOfMemory), result);
        RESIZE_TW.end(tripwire::ResetMode::Reset).unwrap();

        // cols should be unchanged after failed resize
        assert_eq!(original_cols, t.cols);
    }
}
