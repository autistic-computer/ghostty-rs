//! Port of `src/font/Atlas.zig`.
//!
//! STATUS: ported
//!
//! Implements a texture atlas (https://en.wikipedia.org/wiki/Texture_atlas).
//!
//! The implementation is based on "A Thousand Ways to Pack the Bin - A
//! Practical Approach to Two-Dimensional Rectangle Bin Packing" by Jukka
//! Jylänki. This specific implementation is based heavily on
//! Nicolas P. Rougier's freetype-gl project as well as Jukka's C++
//! implementation: https://github.com/juj/RectangleBinPack
//!
//! Limitations that are easy to fix, but I didn't need them:
//!
//!   * Written data must be packed, no support for custom strides.
//!   * Texture is always a square, no ability to set width != height. Note
//!     that regions written INTO the atlas do not have to be square, only
//!     the full atlas texture itself.
//!
//! The wasm-only `Wasm` export API is not ported (no wasm target here).

use std::sync::atomic::{AtomicUsize, Ordering};
use crate::tripwire;

#[derive(Debug)]
pub struct Atlas {
    /// Data is the raw texture data.
    pub data: Vec<u8>,

    /// Width and height of the atlas texture. The current implementation is
    /// always square so this is both the width and the height.
    pub size: u32,

    /// The nodes (rectangles) of available space.
    pub nodes: Vec<Node>,

    /// The format of the texture data being written into the Atlas. This must be
    /// uniform for all textures in the Atlas. If you have some textures with
    /// different formats, you must use multiple atlases or convert the textures.
    pub format: Format,

    /// This will be incremented every time the atlas is modified. This is useful
    /// for knowing if the texture data has changed since the last time it was
    /// sent to the GPU. It is up the user of the atlas to read this value atomically
    /// to observe it.
    pub modified: AtomicUsize,

    /// This will be incremented every time the atlas is resized. This is useful
    /// for knowing if a GPU texture can be updated in-place or if it requires
    /// a resize operation.
    pub resized: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Format {
    /// 1 byte per pixel grayscale.
    #[default]
    Grayscale = 0,
    /// 3 bytes per pixel BGR.
    Bgr = 1,
    /// 4 bytes per pixel BGRA.
    Bgra = 2,
}

impl Format {
    pub fn depth(self) -> u8 {
        match self {
            Format::Grayscale => 1,
            Format::Bgr => 3,
            Format::Bgra => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub x: u32,
    pub y: u32,
    pub width: u32,
}

/// Zig `Allocator.Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for AllocError {}

/// Errors returned by [`Atlas::reserve`] (Zig `Allocator.Error || Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Out of memory.
    OutOfMemory,
    /// Atlas cannot fit the desired region. You must enlarge the atlas.
    AtlasFull,
}

impl From<AllocError> for Error {
    fn from(_: AllocError) -> Error {
        Error::OutOfMemory
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::OutOfMemory => f.write_str("OutOfMemory"),
            Error::AtlasFull => f.write_str("AtlasFull"),
        }
    }
}

impl std::error::Error for Error {}

/// A region within the texture atlas. These can be acquired using the
/// "reserve" function. A region reservation is required to write data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Number of nodes to preallocate in the list on init.
///
/// TODO: figure out optimal prealloc based on real world usage
const NODE_PREALLOC: usize = 64;

/// Fail points of [`Atlas::init`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitFailPoint {
    AllocData,
    AllocNodes,
}

/// Fail points of [`Atlas::reserve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveFailPoint {
    InsertNode,
}

/// Fail points of [`Atlas::grow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowFailPoint {
    EnsureNodeCapacity,
    AllocData,
}

pub static INIT_TW: tripwire::Module<InitFailPoint, AllocError> = tripwire::Module::new();
pub static RESERVE_TW: tripwire::Module<ReserveFailPoint, AllocError> = tripwire::Module::new();
pub static GROW_TW: tripwire::Module<GrowFailPoint, AllocError> = tripwire::Module::new();

#[cfg(test)]
thread_local! {
    /// Test-only allocation budget in bytes, standing in for the Zig
    /// tests' `FixedBufferAllocator`. `None` means unlimited.
    static ALLOC_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Account for an allocation of `bytes` bytes.
#[inline]
fn alloc_check(bytes: usize) -> Result<(), AllocError> {
    #[cfg(test)]
    {
        let ok = ALLOC_BUDGET.with(|b| match b.get() {
            None => true,
            Some(left) if left >= bytes => {
                b.set(Some(left - bytes));
                true
            }
            Some(_) => false,
        });
        if !ok {
            return Err(AllocError::OutOfMemory);
        }
    }
    let _ = bytes;
    Ok(())
}

/// Allocate a zeroed byte buffer, reporting allocation failure.
fn alloc_bytes(len: usize) -> Result<Vec<u8>, AllocError> {
    alloc_check(len)?;
    let mut v: Vec<u8> = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| AllocError::OutOfMemory)?;
    v.resize(len, 0);
    Ok(v)
}

/// Ensure room for `additional` more nodes, reporting allocation failure.
fn reserve_nodes(nodes: &mut Vec<Node>, additional: usize) -> Result<(), AllocError> {
    if nodes.capacity() - nodes.len() >= additional {
        return Ok(());
    }
    let new_cap = (nodes.len() + additional).max(nodes.capacity() * 2);
    alloc_check((new_cap - nodes.capacity()) * std::mem::size_of::<Node>())?;
    nodes
        .try_reserve_exact(new_cap - nodes.len())
        .map_err(|_| AllocError::OutOfMemory)
}

impl Atlas {
    pub fn init(size: u32, format: Format) -> Result<Atlas, AllocError> {
        let tw = &INIT_TW;

        tw.check(InitFailPoint::AllocData)?;
        let mut result = Atlas {
            data: alloc_bytes((size * size * format.depth() as u32) as usize)?,
            size,
            nodes: Vec::new(),
            format,
            modified: AtomicUsize::new(0),
            resized: AtomicUsize::new(0),
        };

        // Prealloc some nodes.
        tw.check(InitFailPoint::AllocNodes)?;
        reserve_nodes(&mut result.nodes, NODE_PREALLOC)?;

        // This sets up our initial state
        result.clear();

        Ok(result)
    }

    /// Reserve a region within the atlas with the given width and height.
    ///
    /// May allocate to add a new rectangle into the internal list of rectangles.
    /// This will not automatically enlarge the texture if it is full.
    pub fn reserve(&mut self, width: u32, height: u32) -> Result<Region, Error> {
        let tw = &RESERVE_TW;

        // x, y are populated within :best_idx below
        let mut region = Region {
            x: 0,
            y: 0,
            width,
            height,
        };

        // If our width/height are 0, then we return the region as-is. This
        // may seem like an error case but it simplifies downstream callers who
        // might be trying to write empty data.
        if width == 0 && height == 0 {
            return Ok(region);
        }

        // Find the location in our nodes list to insert the new node for this region.
        let best_idx: usize = {
            let mut best_height: u32 = u32::MAX;
            let mut best_width: u32 = best_height;
            let mut chosen: Option<usize> = None;

            let mut i: usize = 0;
            while i < self.nodes.len() {
                // Check if our region fits within this node.
                if let Some(y) = self.fit(i, width, height) {
                    let node = self.nodes[i];
                    if (y + height) < best_height
                        || ((y + height) == best_height && (node.width > 0 && node.width < best_width))
                    {
                        chosen = Some(i);
                        best_width = node.width;
                        best_height = y + height;
                        region.x = node.x;
                        region.y = y;
                    }
                }
                i += 1;
            }

            // If we never found a chosen index, the atlas cannot fit our region.
            chosen.ok_or(Error::AtlasFull)?
        };

        // Insert our new node for this rectangle at the exact best index
        tw.check(ReserveFailPoint::InsertNode)?;
        reserve_nodes(&mut self.nodes, 1)?;
        self.nodes.insert(
            best_idx,
            Node {
                x: region.x,
                y: region.y + height,
                width,
            },
        );

        // Optimize our rectangles
        let i: usize = best_idx + 1;
        while i < self.nodes.len() {
            let prev = self.nodes[i - 1];
            let node = &mut self.nodes[i];
            if node.x < (prev.x + prev.width) {
                let shrink = prev.x + prev.width - node.x;
                node.x += shrink;
                node.width = node.width.saturating_sub(shrink);
                if node.width == 0 {
                    self.nodes.remove(i);
                    // Zig: `i -= 1; continue;` followed by the loop's `i += 1`.
                    continue;
                }
            }

            break;
        }
        self.merge();

        Ok(region)
    }

    /// Attempts to fit a rectangle of width x height into the node at idx.
    /// The return value is the y within the texture where the rectangle can be
    /// placed. The x is the same as the node.
    fn fit(&self, idx: usize, width: u32, height: u32) -> Option<u32> {
        // If the added width exceeds our texture size, it doesn't fit.
        let node = self.nodes[idx];
        if (node.x + width) > (self.size - 1) {
            return None;
        }

        // Go node by node looking for space that can fit our width.
        let mut y = node.y;
        let mut i = idx;
        let mut width_left = width;
        while width_left > 0 {
            let n = self.nodes[i];
            if n.y > y {
                y = n.y;
            }

            // If the added height exceeds our texture size, it doesn't fit.
            if (y + height) > (self.size - 1) {
                return None;
            }

            width_left = width_left.saturating_sub(n.width);
            i += 1;
        }

        Some(y)
    }

    /// Merge adjacent nodes with the same y value.
    fn merge(&mut self) {
        let mut i: usize = 0;
        while i < self.nodes.len() - 1 {
            let next = self.nodes[i + 1];
            let node = &mut self.nodes[i];
            if node.y == next.y {
                node.width += next.width;
                self.nodes.remove(i + 1);
                continue;
            }

            i += 1;
        }
    }

    /// Set the data associated with a reserved region. The data is expected
    /// to fit exactly within the region. The data must be formatted with the
    /// proper bpp configured on init.
    pub fn set(&mut self, reg: Region, data: &[u8]) {
        debug_assert!(reg.x < (self.size - 1));
        debug_assert!((reg.x + reg.width) <= (self.size - 1));
        debug_assert!(reg.y < (self.size - 1));
        debug_assert!((reg.y + reg.height) <= (self.size - 1));

        let depth = self.format.depth() as u32;
        let mut i: u32 = 0;
        while i < reg.height {
            let tex_offset = ((((reg.y + i) * self.size) + reg.x) * depth) as usize;
            let data_offset = (i * reg.width * depth) as usize;
            let len = (reg.width * depth) as usize;
            self.data[tex_offset..tex_offset + len]
                .copy_from_slice(&data[data_offset..data_offset + len]);
            i += 1;
        }

        self.modified.fetch_add(1, Ordering::Relaxed);
    }

    /// Like `set` but allows specifying a width for the source data and an
    /// offset x and y, so that a section of a larger buffer may be copied
    /// in to the atlas.
    pub fn set_from_larger(
        &mut self,
        reg: Region,
        src: &[u8],
        src_width: u32,
        src_x: u32,
        src_y: u32,
    ) {
        debug_assert!(reg.x < (self.size - 1));
        debug_assert!((reg.x + reg.width) <= (self.size - 1));
        debug_assert!(reg.y < (self.size - 1));
        debug_assert!((reg.y + reg.height) <= (self.size - 1));

        let depth = self.format.depth() as u32;
        let mut i: u32 = 0;
        while i < reg.height {
            let tex_offset = ((((reg.y + i) * self.size) + reg.x) * depth) as usize;
            let src_offset = ((((src_y + i) * src_width) + src_x) * depth) as usize;
            let len = (reg.width * depth) as usize;
            self.data[tex_offset..tex_offset + len]
                .copy_from_slice(&src[src_offset..src_offset + len]);
            i += 1;
        }

        self.modified.fetch_add(1, Ordering::Relaxed);
    }

    /// Grow the texture to the new size, preserving all previously written data.
    pub fn grow(&mut self, size_new: u32) -> Result<(), AllocError> {
        let tw = &GROW_TW;

        debug_assert!(size_new >= self.size);
        if size_new == self.size {
            return Ok(());
        }

        // We reserve space ahead of time for the new node, so that we
        // won't have to handle any errors after allocating our new data.
        tw.check(GrowFailPoint::EnsureNodeCapacity)?;
        reserve_nodes(&mut self.nodes, 1)?;

        tw.check(GrowFailPoint::AllocData)?;
        let data_new = alloc_bytes((size_new * size_new * self.format.depth() as u32) as usize)?;

        // Function is infallible from this point.

        // Keep track of our old data so that we can copy it.
        let data_old = std::mem::replace(&mut self.data, data_new);
        let size_old = self.size;

        // Update our data and size to our new ones.
        self.size = size_new;

        // Zero the new data out and copy the old data over.
        // (`alloc_bytes` already zeroed the new data.)
        let depth = self.format.depth() as usize;
        self.set(
            Region {
                x: 0, // don't bother skipping border so we can avoid strides
                y: 1, // skip the first border row
                width: size_old,
                height: size_old - 2, // skip the last border row
            },
            &data_old[size_old as usize * depth..],
        );

        // Add the new rectangle for our added righthand space.
        self.nodes.push(Node {
            x: size_old - 1,
            y: 1,
            width: size_new - size_old,
        });

        // We are both modified and resized
        self.modified.fetch_add(1, Ordering::Relaxed);
        self.resized.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Empty the atlas. This doesn't reclaim any previously allocated memory.
    pub fn clear(&mut self) {
        self.modified.fetch_add(1, Ordering::Relaxed);
        self.data.fill(0);
        self.nodes.clear();

        // Add our initial rectangle. This is the size of the full texture
        // and is the initial rectangle we fit our regions in. We keep a 1px border
        // to avoid artifacting when sampling the texture.
        self.nodes.push(Node {
            x: 1,
            y: 1,
            width: self.size - 2,
        });
    }

    /// Dump the atlas as a PPM to a writer, for debug purposes.
    /// Only supports grayscale and bgr atlases.
    ///
    /// NOTE: BGR atlases will have the red and blue channels
    ///       swapped because PPM expects RGB. This would be
    ///       easy enough to fix so next time someone needs
    ///       to debug a color atlas they should fix it.
    pub fn dump<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let c = match self.format {
            Format::Grayscale => '5',
            Format::Bgr => '6',
            _ => {
                log::error!(target: "atlas", "Unsupported format for dump: {:?}", self.format);
                panic!("Cannot dump this atlas format.");
            }
        };
        write!(writer, "P{}\n{} {}\n255\n", c, self.size, self.size)?;
        writer.write_all(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(v: &AtomicUsize) -> usize {
        v.load(Ordering::Relaxed)
    }

    #[test]
    fn exact_fit() {
        let mut atlas = Atlas::init(34, Format::Grayscale).unwrap(); // +2 for 1px border

        let modified = load(&atlas.modified);
        atlas.reserve(32, 32).unwrap();
        assert_eq!(modified, load(&atlas.modified));
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));
    }

    #[test]
    fn doesnt_fit() {
        let mut atlas = Atlas::init(32, Format::Grayscale).unwrap();

        // doesn't fit due to border
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(32, 32));
    }

    #[test]
    fn fit_multiple() {
        let mut atlas = Atlas::init(32, Format::Grayscale).unwrap();

        atlas.reserve(15, 30).unwrap();
        atlas.reserve(15, 30).unwrap();
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));
    }

    #[test]
    fn writing_data() {
        let mut atlas = Atlas::init(32, Format::Grayscale).unwrap();

        let reg = atlas.reserve(2, 2).unwrap();
        let old = load(&atlas.modified);
        atlas.set(reg, &[1, 2, 3, 4]);
        let new = load(&atlas.modified);
        assert!(new > old);

        // 33 because of the 1px border and so on
        assert_eq!(1u8, atlas.data[33]);
        assert_eq!(2u8, atlas.data[34]);
        assert_eq!(3u8, atlas.data[65]);
        assert_eq!(4u8, atlas.data[66]);
    }

    #[test]
    fn writing_data_from_a_larger_source() {
        let mut atlas = Atlas::init(32, Format::Grayscale).unwrap();

        let reg = atlas.reserve(2, 2).unwrap();
        let old = load(&atlas.modified);
        #[rustfmt::skip]
        atlas.set_from_larger(reg, &[
            8, 8, 8, 8, 8,
            8, 8, 1, 2, 8,
            8, 8, 3, 4, 8,
            8, 8, 8, 8, 8,
        ], 5, 2, 1);
        let new = load(&atlas.modified);
        assert!(new > old);

        // 33 because of the 1px border and so on
        assert_eq!(1u8, atlas.data[33]);
        assert_eq!(2u8, atlas.data[34]);
        assert_eq!(3u8, atlas.data[65]);
        assert_eq!(4u8, atlas.data[66]);

        // None of the `8`s from the source data outside of the
        // specified region should have made it on to the atlas.
        assert_eq!(None, atlas.data.iter().position(|&b| b == 8));
    }

    #[test]
    fn grow() {
        let mut atlas = Atlas::init(4, Format::Grayscale).unwrap(); // +2 for 1px border

        let reg = atlas.reserve(2, 2).unwrap();
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));

        // Write some data so we can verify that growing doesn't mess it up
        atlas.set(reg, &[1, 2, 3, 4]);
        assert_eq!(1u8, atlas.data[5]);
        assert_eq!(2u8, atlas.data[6]);
        assert_eq!(3u8, atlas.data[9]);
        assert_eq!(4u8, atlas.data[10]);

        // Expand by exactly 1 should fit our new 1x1 block.
        let old_modified = load(&atlas.modified);
        let old_resized = load(&atlas.resized);
        atlas.grow(atlas.size + 1).unwrap();
        let new_modified = load(&atlas.modified);
        let new_resized = load(&atlas.resized);
        assert!(new_modified > old_modified);
        assert!(new_resized > old_resized);
        atlas.reserve(1, 1).unwrap();

        // Ensure our data is still set. Not the offsets change due to size.
        let size = atlas.size as usize;
        assert_eq!(1u8, atlas.data[size + 1]);
        assert_eq!(2u8, atlas.data[size + 2]);
        assert_eq!(3u8, atlas.data[size * 2 + 1]);
        assert_eq!(4u8, atlas.data[size * 2 + 2]);
    }

    #[test]
    fn writing_bgr_data() {
        let mut atlas = Atlas::init(32, Format::Bgr).unwrap();

        // This is BGR so its 3 bpp
        let reg = atlas.reserve(1, 2).unwrap();
        atlas.set(reg, &[1, 2, 3, 4, 5, 6]);

        // 33 because of the 1px border and so on
        let depth = atlas.format.depth() as usize;
        assert_eq!(1u8, atlas.data[33 * depth]);
        assert_eq!(2u8, atlas.data[33 * depth + 1]);
        assert_eq!(3u8, atlas.data[33 * depth + 2]);
        assert_eq!(4u8, atlas.data[65 * depth]);
        assert_eq!(5u8, atlas.data[65 * depth + 1]);
        assert_eq!(6u8, atlas.data[65 * depth + 2]);
    }

    #[test]
    fn grow_bgr() {
        // Atlas is 4x4 so its a 1px border meaning we only have 2x2 available
        let mut atlas = Atlas::init(4, Format::Bgr).unwrap();

        // Get our 2x2, which should be ALL our usable space
        let reg = atlas.reserve(2, 2).unwrap();
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));

        // This is BGR so its 3 bpp
        #[rustfmt::skip]
        atlas.set(reg, &[
            10, 11, 12, // (0, 0) (x, y) from top-left
            13, 14, 15, // (1, 0)
            20, 21, 22, // (0, 1)
            23, 24, 25, // (1, 1)
        ]);

        // Our top left skips the first row (size * depth) and the first
        // column (depth) for the 1px border.
        let depth = atlas.format.depth() as usize;
        let mut tl = (atlas.size as usize * depth) + depth;
        assert_eq!(10u8, atlas.data[tl]);
        assert_eq!(11u8, atlas.data[tl + 1]);
        assert_eq!(12u8, atlas.data[tl + 2]);
        assert_eq!(13u8, atlas.data[tl + 3]);
        assert_eq!(14u8, atlas.data[tl + 4]);
        assert_eq!(15u8, atlas.data[tl + 5]);
        assert_eq!(0u8, atlas.data[tl + 6]); // border

        tl += atlas.size as usize * depth; // next row
        assert_eq!(20u8, atlas.data[tl]);
        assert_eq!(21u8, atlas.data[tl + 1]);
        assert_eq!(22u8, atlas.data[tl + 2]);
        assert_eq!(23u8, atlas.data[tl + 3]);
        assert_eq!(24u8, atlas.data[tl + 4]);
        assert_eq!(25u8, atlas.data[tl + 5]);
        assert_eq!(0u8, atlas.data[tl + 6]); // border

        // Expand by exactly 1 should fit our new 1x1 block.
        atlas.grow(atlas.size + 1).unwrap();

        // Data should be in same place accounting for the new size
        tl = (atlas.size as usize * depth) + depth;
        assert_eq!(10u8, atlas.data[tl]);
        assert_eq!(11u8, atlas.data[tl + 1]);
        assert_eq!(12u8, atlas.data[tl + 2]);
        assert_eq!(13u8, atlas.data[tl + 3]);
        assert_eq!(14u8, atlas.data[tl + 4]);
        assert_eq!(15u8, atlas.data[tl + 5]);
        assert_eq!(0u8, atlas.data[tl + 6]); // border

        tl += atlas.size as usize * depth; // next row
        assert_eq!(20u8, atlas.data[tl]);
        assert_eq!(21u8, atlas.data[tl + 1]);
        assert_eq!(22u8, atlas.data[tl + 2]);
        assert_eq!(23u8, atlas.data[tl + 3]);
        assert_eq!(24u8, atlas.data[tl + 4]);
        assert_eq!(25u8, atlas.data[tl + 5]);
        assert_eq!(0u8, atlas.data[tl + 6]); // border

        // Should fit the new blocks around the edges
        atlas.reserve(1, 3).unwrap();
        atlas.reserve(2, 1).unwrap();
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));
    }

    #[test]
    fn grow_oom() {
        // We use a fixed allocation budget (Zig: FixedBufferAllocator) so
        // that we can consistently hit OOM.
        //
        // We calculate the size to exactly fit the 4x4 pixels and node list.
        ALLOC_BUDGET.with(|b| {
            b.set(Some(
                4 * 4 * 1 // 4x4 pixels, each 1 byte.
                + NODE_PREALLOC * std::mem::size_of::<Node>(), // preallocated nodes.
            ))
        });
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                ALLOC_BUDGET.with(|b| b.set(None));
            }
        }
        let _reset = Reset;

        let mut atlas = Atlas::init(4, Format::Grayscale).unwrap(); // +2 for 1px border

        let reg = atlas.reserve(2, 2).unwrap();
        assert_eq!(Err(Error::AtlasFull), atlas.reserve(1, 1));

        // Write some data so we can verify that attempted growing doesn't mess it up.
        atlas.set(reg, &[1, 2, 3, 4]);
        assert_eq!(1u8, atlas.data[5]);
        assert_eq!(2u8, atlas.data[6]);
        assert_eq!(3u8, atlas.data[9]);
        assert_eq!(4u8, atlas.data[10]);

        // Expand by 1, should give OOM, modified and resized should be unchanged.
        let old_modified = load(&atlas.modified);
        let old_resized = load(&atlas.resized);
        assert_eq!(Err(AllocError::OutOfMemory), atlas.grow(atlas.size + 1));
        let new_modified = load(&atlas.modified);
        let new_resized = load(&atlas.resized);
        assert_eq!(old_modified, new_modified);
        assert_eq!(old_resized, new_resized);

        // Ensure our data is still set.
        assert_eq!(1u8, atlas.data[5]);
        assert_eq!(2u8, atlas.data[6]);
        assert_eq!(3u8, atlas.data[9]);
        assert_eq!(4u8, atlas.data[10]);
    }

    #[test]
    fn init_error() {
        // Test every failure point in `init` and ensure that we exit early.
        for tag in [InitFailPoint::AllocData, InitFailPoint::AllocNodes] {
            let tw = &INIT_TW;
            tw.error_always(tag, AllocError::OutOfMemory);
            assert_eq!(
                AllocError::OutOfMemory,
                Atlas::init(32, Format::Grayscale).unwrap_err()
            );
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }
    }

    #[test]
    fn reserve_error() {
        // Test every failure point in `reserve` and ensure that we exit early.
        for tag in [ReserveFailPoint::InsertNode] {
            let tw = &RESERVE_TW;

            let mut atlas = Atlas::init(32, Format::Grayscale).unwrap();

            tw.error_always(tag, AllocError::OutOfMemory);
            assert_eq!(Err(Error::OutOfMemory), atlas.reserve(2, 2));
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }
    }

    #[test]
    fn grow_error() {
        // Test every failure point in `grow` and ensure that we exit early.
        for tag in [GrowFailPoint::EnsureNodeCapacity, GrowFailPoint::AllocData] {
            let tw = &GROW_TW;

            let mut atlas = Atlas::init(4, Format::Grayscale).unwrap();

            // Write some data to verify it's preserved after failed grow
            let reg = atlas.reserve(2, 2).unwrap();
            atlas.set(reg, &[1, 2, 3, 4]);

            let old_modified = load(&atlas.modified);
            let old_resized = load(&atlas.resized);

            tw.error_always(tag, AllocError::OutOfMemory);
            assert_eq!(Err(AllocError::OutOfMemory), atlas.grow(atlas.size + 1));
            tw.end(tripwire::ResetMode::Reset).unwrap();

            // Verify atlas state is unchanged after failed grow
            assert_eq!(old_modified, load(&atlas.modified));
            assert_eq!(old_resized, load(&atlas.resized));
            assert_eq!(1u8, atlas.data[5]);
            assert_eq!(2u8, atlas.data[6]);
            assert_eq!(3u8, atlas.data[9]);
            assert_eq!(4u8, atlas.data[10]);
        }
    }

    #[test]
    fn dump_ppm() {
        let atlas = Atlas::init(4, Format::Grayscale).unwrap();
        let mut out = Vec::new();
        atlas.dump(&mut out).unwrap();
        assert!(out.starts_with(b"P5\n4 4\n255\n"));
        assert_eq!(out.len(), b"P5\n4 4\n255\n".len() + 16);
    }
}
