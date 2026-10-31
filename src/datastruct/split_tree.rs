//! Port of `src/datastruct/split_tree.zig`.
//!
//! STATUS: ported
//!
//! Rust notes:
//!
//! * The Zig tree stores `*View` pointers and calls `View.ref`/`View.unref`
//!   explicitly. In Rust the view type is a reference-counted handle
//!   (e.g. a GObject wrapper or `Rc`): `Clone` is `ref`, `Drop` is `unref`.
//!   Identity comparison (Zig: pointer equality) is [`View::eql`].
//! * Each tree owns its node array (Zig: an arena per tree); dropping the
//!   tree unrefs its views, cloning it refs them.
//! * Ratios and spatial coordinates are `f16` exactly like Zig (via the
//!   `half` crate) so the layout math rounds identically.
//! * Allocator parameters are dropped, so only `split` (which can exceed
//!   the 16-bit handle space) still returns an error.
//! * `getGObjectType` (a GObject boxed type for the GTK apprt) is not
//!   defined here: a generic Rust type can't register itself as a boxed
//!   type, so the GTK apprt registers its concrete `SplitTree<Surface>`
//!   wrapper itself.

use std::fmt;

pub use half::f16;

use crate::quirks::inline_assert as assert;

/// The view type stored in a [`SplitTree`].
///
/// The View type will be stored within the tree and must implement
/// reference counting (`Clone` takes a new reference, `Drop` releases it).
///
///   - `fn eql(&self, &Self) -> bool` - Check if two views are the same
///     view (identity).
///
/// Optionally the following functions can also be implemented:
///
///   - `fn split_tree_label(&self) -> Option<&str>` - Return a label that
///     is used for the debug view. If this isn't specified then the node
///     handle will be used.
pub trait View: Clone {
    fn eql(&self, other: &Self) -> bool;

    fn split_tree_label(&self) -> Option<&str> {
        None
    }
}

/// A handle into the nodes array. This lets us keep track of
/// nodes with 16-bit handles rather than full pointer-width
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Handle(pub HandleBacking);

pub type HandleBacking = u16;

impl Handle {
    pub const ROOT: Handle = Handle(0);

    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }

    /// Offset the handle by a given amount.
    pub fn offset(self, v: usize) -> Handle {
        let final_ = self.0 as usize + v;
        assert(final_ < HandleBacking::MAX as usize);
        Handle(final_ as HandleBacking)
    }

    fn from_usize(v: usize) -> Handle {
        Handle(v as HandleBacking)
    }
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A node in the tree.
#[derive(Debug, Clone)]
pub enum Node<V> {
    Leaf(V),
    Split(Split),
}

/// A split node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    pub layout: Layout,
    pub ratio: f16,
    pub left: Handle,
    pub right: Handle,
}

/// Split layout (Zig: `Split.Layout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    Horizontal,
    Vertical,
}

impl Layout {
    pub fn as_str(self) -> &'static str {
        match self {
            Layout::Horizontal => "horizontal",
            Layout::Vertical => "vertical",
        }
    }
}

/// Split direction (Zig: `Split.Direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Down,
    Up,
}

/// Spatial direction (Zig: `Spatial.Direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpatialDirection {
    Left,
    Right,
    Down,
    Up,
}

/// A view and its handle, returned by the iterator.
#[derive(Debug)]
pub struct ViewEntry<'a, V> {
    pub handle: Handle,
    pub view: &'a V,
}

/// An iterator over all the views in the tree.
pub struct Iterator<'a, V> {
    i: Handle,
    nodes: &'a [Node<V>],
}

impl<'a, V> std::iter::Iterator for Iterator<'a, V> {
    type Item = ViewEntry<'a, V>;

    fn next(&mut self) -> Option<ViewEntry<'a, V>> {
        loop {
            // If we have no nodes, return null.
            if self.i.idx() >= self.nodes.len() {
                return None;
            }

            // Get the current node and increment the index.
            let handle = self.i;
            self.i = Handle::from_usize(handle.idx() + 1);

            match &self.nodes[handle.idx()] {
                Node::Leaf(v) => return Some(ViewEntry { handle, view: v }),
                Node::Split(_) => continue,
            }
        }
    }
}

/// Where to go from a view (see [`SplitTree::goto`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goto {
    /// Previous view, null if we're the first view.
    Previous,

    /// Next view, null if we're the last view.
    Next,

    /// Previous view, but wrapped around to the last view. May
    /// return the same view if this is the first view.
    PreviousWrapped,

    /// Next view, but wrapped around to the first view. May return
    /// the same view if this is the last view.
    NextWrapped,

    /// A spatial direction. "Spatial" means that the direction is
    /// based on the nearest surface in the given direction visually
    /// as the surfaces are laid out on a 2D grid.
    Spatial(SpatialDirection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

// Design note: we use a recursive backtracking search because
// split trees are never that deep, so we can abuse the stack as
// a safe allocator (stack overflow unlikely unless the kernel is
// tuned in some really weird way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backtrack {
    Deadend,
    Backtrack,
    Result(Handle),
}

/// Spatial representation of the split tree. See [`SplitTree::spatial`].
#[derive(Debug, Clone, Default)]
pub struct Spatial {
    /// The slots of the spatial representation in the same order
    /// as the tree it was created from.
    pub slots: Vec<Slot>,
}

impl Spatial {
    pub const EMPTY: Spatial = Spatial { slots: Vec::new() };
}

/// One slot of a [`Spatial`] representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub x: f16,
    pub y: f16,
    pub width: f16,
    pub height: f16,
}

impl Slot {
    fn max_x(&self) -> f16 {
        self.x + self.width
    }

    fn max_y(&self) -> f16 {
        self.y + self.height
    }
}

/// Relative dimensions of a subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dimensions {
    width: u16,
    height: u16,
}

/// Returned by [`SplitTree::split`] when the result would not fit in the
/// 16-bit handle space (Zig: `error.OutOfMemory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for Error {}

/// SplitTree represents a tree of view types that can be divided.
///
/// Concretely for Ghostty, it represents a tree of terminal views. In
/// its basic state, there are no splits and it is a single full-sized
/// terminal. However, it can be split arbitrarily many times among two
/// axes (horizontal and vertical) to create a tree of terminal views.
///
/// This is an immutable tree structure, meaning all operations on it
/// will return a new tree with the operation applied. This allows us to
/// store versions of the tree in a history for easy undo/redo. To facilitate
/// this, the stored View type must implement reference counting; this is left
/// as an implementation detail of the View type.
#[derive(Debug, Clone)]
pub struct SplitTree<V> {
    /// All the nodes in the tree. Node at index 0 is always the root.
    nodes: Vec<Node<V>>,

    /// The handle of the zoomed node. A "zoomed" node is one that is
    /// expected to be made the full size of the split tree. Various
    /// operations may unzoom (e.g. resize).
    pub zoomed: Option<Handle>,
}

impl<V> Default for SplitTree<V> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<V> SplitTree<V> {
    /// An empty tree.
    pub const EMPTY: Self = Self {
        nodes: Vec::new(),
        zoomed: None,
    };

    /// All the nodes in the tree. Node at index 0 is always the root.
    pub fn nodes(&self) -> &[Node<V>] {
        &self.nodes
    }
}

impl<V: View> SplitTree<V> {
    /// Initialize a new tree with a single view.
    pub fn init(view: &V) -> Self {
        Self {
            nodes: vec![Node::Leaf(view.clone())],
            zoomed: None,
        }
    }

    /// Release the tree (and the references to its views). Equivalent to
    /// dropping it.
    pub fn deinit(self) {
        drop(self);
    }

    /// Clone this tree, returning a new tree with the same nodes. This
    /// takes a new reference on every view.
    pub fn clone_tree(&self) -> Self {
        // If we're empty then return an empty tree.
        if self.is_empty() {
            return Self::EMPTY;
        }
        self.clone()
    }

    /// Returns true if this is an empty tree.
    pub fn is_empty(&self) -> bool {
        // An empty tree has no nodes.
        self.nodes.is_empty()
    }

    /// Returns true if this tree has more than one split (i.e., the root
    /// is a split node). This is useful for determining if actions like
    /// resize_split or toggle_split_zoom are performable.
    pub fn is_split(&self) -> bool {
        // An empty tree is not split.
        if self.is_empty() {
            return false;
        }
        // The root node is at index 0. If it's a split, we have multiple splits.
        matches!(self.nodes[0], Node::Split(_))
    }

    /// Find the handle within this tree that corresponds
    /// to the given view. Uses view identity.
    pub fn locate(&self, view: &V) -> Option<Handle> {
        self.iterator()
            .find(|entry| entry.view.eql(view))
            .map(|entry| entry.handle)
    }

    /// An iterator over all the views in the tree.
    pub fn iterator(&self) -> Iterator<'_, V> {
        Iterator {
            i: Handle::ROOT,
            nodes: &self.nodes,
        }
    }

    /// Change the zoomed state to the given node. Assumes the handle
    /// is valid.
    pub fn zoom(&mut self, handle: Option<Handle>) {
        if let Some(v) = handle {
            assert(v.idx() < self.nodes.len());
        }
        self.zoomed = handle;
    }

    /// Goto a view from a certain point in the split tree. Returns None
    /// if the direction results in no visitable view.
    ///
    /// `from` must be a valid handle in this tree, so the tree must
    /// not be empty.
    pub fn goto(&self, from: Handle, to: Goto) -> Option<Handle> {
        assert(from.idx() < self.nodes.len());

        match to {
            Goto::Previous => self.previous(from),
            Goto::Next => self.next(from),
            Goto::PreviousWrapped => self
                .previous(from)
                .or_else(|| Some(self.deepest(Side::Right, Handle::ROOT))),
            Goto::NextWrapped => self
                .next(from)
                .or_else(|| Some(self.deepest(Side::Left, Handle::ROOT))),
            Goto::Spatial(d) => {
                // Get our spatial representation.
                let sp = self.spatial();
                self.nearest_wrapped(&sp, from, d)
            }
        }
    }

    /// Returns the deepest view in the tree in the given direction.
    /// This can be used to find the leftmost/rightmost surface within
    /// a given split structure.
    pub fn deepest(&self, side: Side, from: Handle) -> Handle {
        let mut current: Handle = from;
        loop {
            match &self.nodes[current.idx()] {
                Node::Leaf(_) => return current,
                Node::Split(s) => {
                    current = match side {
                        Side::Left => s.left,
                        Side::Right => s.right,
                    }
                }
            }
        }
    }

    /// Returns the previous view from the given node handle (which itself
    /// doesn't need to be a view). If there is no previous (this is the
    /// most previous view) then this will return None.
    ///
    /// "Previous" is defined as the previous node in an in-order
    /// traversal of the tree. This isn't a perfect definition and we
    /// may want to change this to something that better matches a
    /// spatial view of the tree later.
    fn previous(&self, from: Handle) -> Option<Handle> {
        match self.previous_backtrack(from, Handle::ROOT) {
            Backtrack::Result(v) => Some(v),
            Backtrack::Backtrack | Backtrack::Deadend => None,
        }
    }

    /// Same as `previous`, but returns the next view instead.
    fn next(&self, from: Handle) -> Option<Handle> {
        match self.next_backtrack(from, Handle::ROOT) {
            Backtrack::Result(v) => Some(v),
            Backtrack::Backtrack | Backtrack::Deadend => None,
        }
    }

    fn previous_backtrack(&self, from: Handle, current: Handle) -> Backtrack {
        // If we reached the point that we're trying to find the previous
        // value of, then we need to backtrack from here.
        if from == current {
            return Backtrack::Backtrack;
        }

        match &self.nodes[current.idx()] {
            // If we hit a leaf that isn't our target, then deadend.
            Node::Leaf(_) => Backtrack::Deadend,

            Node::Split(s) => match self.previous_backtrack(from, s.left) {
                Backtrack::Result(v) => Backtrack::Result(v),

                // Backtrack from the left means we have to continue
                // backtracking because we can't see what's before the left.
                Backtrack::Backtrack => Backtrack::Backtrack,

                // If we hit a deadend on the left then let's move right.
                Backtrack::Deadend => match self.previous_backtrack(from, s.right) {
                    Backtrack::Result(v) => Backtrack::Result(v),

                    // Deadend means its not in this split at all since
                    // we already tracked the left.
                    Backtrack::Deadend => Backtrack::Deadend,

                    // Backtrack means that its in our left view because
                    // we can see the immediate previous and there MUST
                    // be leaves (we can't have split-only leaves).
                    Backtrack::Backtrack => Backtrack::Result(self.deepest(Side::Right, s.left)),
                },
            },
        }
    }

    // See previousBacktrack for detailed comments. This is a mirror
    // of that.
    fn next_backtrack(&self, from: Handle, current: Handle) -> Backtrack {
        if from == current {
            return Backtrack::Backtrack;
        }
        match &self.nodes[current.idx()] {
            Node::Leaf(_) => Backtrack::Deadend,
            Node::Split(s) => match self.next_backtrack(from, s.right) {
                Backtrack::Result(v) => Backtrack::Result(v),
                Backtrack::Backtrack => Backtrack::Backtrack,
                Backtrack::Deadend => match self.next_backtrack(from, s.left) {
                    Backtrack::Result(v) => Backtrack::Result(v),
                    Backtrack::Deadend => Backtrack::Deadend,
                    Backtrack::Backtrack => Backtrack::Result(self.deepest(Side::Left, s.right)),
                },
            },
        }
    }

    /// Returns the nearest leaf node (view) in the given direction.
    /// This does not handle wrapping and will return None if there
    /// is no node in that direction.
    fn nearest(
        &self,
        sp: &Spatial,
        from: Handle,
        direction: SpatialDirection,
        target: Slot,
    ) -> Option<Handle> {
        let mut result: Option<(Handle, f16)> = None;
        for (handle, slot) in sp.slots.iter().enumerate() {
            // Never match ourself
            if handle == from.idx() {
                continue;
            }

            // Only match leaves
            match &self.nodes[handle] {
                Node::Leaf(_) => {}
                Node::Split(_) => continue,
            }

            // Ensure it is in the proper direction
            if !match direction {
                SpatialDirection::Left => slot.max_x() <= target.x,
                SpatialDirection::Right => slot.x >= target.max_x(),
                SpatialDirection::Up => slot.max_y() <= target.y,
                SpatialDirection::Down => slot.y >= target.max_y(),
            } {
                continue;
            }

            // Track our distance
            let dx = slot.x - target.x;
            let dy = slot.y - target.y;
            let distance = f16::from_f32((dx * dx + dy * dy).to_f32().sqrt());

            // If we have a nearest it must be closer.
            if let Some((_, n_distance)) = result {
                if distance >= n_distance {
                    continue;
                }
            }
            result = Some((Handle::from_usize(handle), distance));
        }

        result.map(|(h, _)| h)
    }

    /// Same as nearest but supports wrapping.
    fn nearest_wrapped(
        &self,
        sp: &Spatial,
        from: Handle,
        direction: SpatialDirection,
    ) -> Option<Handle> {
        // If we can find a nearest value without wrapping, then
        // use that.
        let mut target = sp.slots[from.idx()];
        if let Some(v) = self.nearest(sp, from, direction, target) {
            return Some(v);
        }

        // The spatial grid is normalized to 1x1, so wrapping is modeled
        // by shifting the target slot by one full grid in the opposite
        // direction and reusing the same nearest distance logic.
        // We don't actually modify the grid or spatial representation,
        // this just fakes it.
        assert(target.x >= f16::ZERO && target.y >= f16::ZERO);
        assert(target.max_x() <= f16::ONE && target.max_y() <= f16::ONE);
        match direction {
            SpatialDirection::Left => target.x += f16::ONE,
            SpatialDirection::Right => target.x -= f16::ONE,
            SpatialDirection::Up => target.y += f16::ONE,
            SpatialDirection::Down => target.y -= f16::ONE,
        }

        self.nearest(sp, from, direction, target)
    }

    /// Resize the given node in place. The node MUST be a split (asserted).
    ///
    /// In general, this is an immutable data structure so this is
    /// heavily discouraged. However, this is provided for convenience
    /// and performance reasons where its very important for GUIs to
    /// update the ratio during a live resize than to redraw the entire
    /// widget tree.
    pub fn resize_in_place(&mut self, at: Handle, ratio: f16) {
        match &mut self.nodes[at.idx()] {
            Node::Split(s) => s.ratio = ratio,
            Node::Leaf(_) => panic!("resize_in_place on a leaf"),
        }
    }

    /// Insert another tree into this tree at the given node in the
    /// specified direction. The other tree will be inserted in the
    /// new direction. For example, if the direction is "right" then
    /// `insert` is inserted right of the existing node.
    ///
    /// The previous trees are not modified, but reference counts
    /// for the views will be increased accordingly for the new tree.
    pub fn split(
        &self,
        at: Handle,
        direction: Direction,
        ratio: f16,
        insert: &Self,
    ) -> Result<Self, Error> {
        // We know we're going to need the sum total of the nodes
        // between the two trees plus one for the new split node.
        let total = self.nodes.len() + insert.nodes.len() + 1;
        if total > HandleBacking::MAX as usize {
            return Err(Error::OutOfMemory);
        }
        let mut nodes: Vec<Node<V>> = Vec::with_capacity(total);

        // We can copy our nodes exactly as they are, since they're
        // mostly not changing (only `at` is changing).
        nodes.extend(self.nodes.iter().cloned());

        // We can copy the destination nodes as well directly next to
        // the source nodes. We just have to go through and offset
        // all the handles in the destination tree to account for
        // the shift.
        for node in insert.nodes.iter() {
            nodes.push(match node {
                Node::Leaf(v) => Node::Leaf(v.clone()),
                Node::Split(s) => {
                    // We need to offset the handles in the split
                    let mut s = *s;
                    s.left = s.left.offset(self.nodes.len());
                    s.right = s.right.offset(self.nodes.len());
                    Node::Split(s)
                }
            });
        }

        // Determine our split layout and if we're on the left
        let (layout, left): (Layout, bool) = match direction {
            Direction::Left => (Layout::Horizontal, true),
            Direction::Right => (Layout::Horizontal, false),
            Direction::Up => (Layout::Vertical, true),
            Direction::Down => (Layout::Vertical, false),
        };

        // Copy our previous value to the end of the nodes list and
        // create our new split node.
        let last = total - 1;
        let new_split = Node::Split(Split {
            layout,
            ratio,
            left: Handle::from_usize(if left { self.nodes.len() } else { last }),
            right: Handle::from_usize(if left { last } else { self.nodes.len() }),
        });
        let prev = std::mem::replace(&mut nodes[at.idx()], new_split);
        nodes.push(prev);
        debug_assert_eq!(nodes.len(), total);

        Ok(Self {
            nodes,
            // Splitting always resets zoom state.
            zoomed: None,
        })
    }

    /// Remove a node from the tree.
    pub fn remove(&self, at: Handle) -> Self {
        assert(at.idx() < self.nodes.len());

        // If we're removing node zero then we're clearing the tree.
        if at == Handle::ROOT {
            return Self::EMPTY;
        }

        // Allocate our new nodes list with the number of nodes we'll
        // need after the removal.
        let count = self.count_after_removal(Handle::ROOT, at, 0);
        let mut new_nodes: Vec<Option<Node<V>>> = (0..count).map(|_| None).collect();
        let mut new_zoomed: Option<Handle> = None;

        // Traverse the tree and copy all our nodes into place.
        let n = self.remove_node(&mut new_nodes, &mut new_zoomed, 0, Handle::ROOT, at);
        assert(n != 0);

        Self {
            nodes: new_nodes
                .into_iter()
                .map(|n| n.expect("remove filled every node"))
                .collect(),
            zoomed: new_zoomed,
        }
    }

    fn remove_node(
        &self,
        new_nodes: &mut [Option<Node<V>>],
        new_zoomed: &mut Option<Handle>,
        new_offset: usize,
        current: Handle,
        target: Handle,
    ) -> usize {
        assert(current != target);

        // If we have a zoomed node and this is it then we migrate it.
        if let Some(v) = self.zoomed {
            if v == current {
                assert(new_zoomed.is_none());
                *new_zoomed = Some(Handle::from_usize(new_offset));
            }
        }

        match &self.nodes[current.idx()] {
            // Leaf is simple, just copy it over (taking a reference).
            Node::Leaf(view) => {
                new_nodes[new_offset] = Some(Node::Leaf(view.clone()));
                1
            }

            Node::Split(s) => {
                // If we're removing one of the split node sides then
                // we remove the split node itself as well and only add
                // the other (non-removed) side.
                if s.left == target {
                    return self.remove_node(new_nodes, new_zoomed, new_offset, s.right, target);
                }
                if s.right == target {
                    return self.remove_node(new_nodes, new_zoomed, new_offset, s.left, target);
                }

                // Neither side is being directly removed, so we traverse.
                let left = self.remove_node(new_nodes, new_zoomed, new_offset + 1, s.left, target);
                assert(left != 0);
                let right = self.remove_node(
                    new_nodes,
                    new_zoomed,
                    new_offset + left + 1,
                    s.right,
                    target,
                );
                assert(right != 0);
                new_nodes[new_offset] = Some(Node::Split(Split {
                    layout: s.layout,
                    ratio: s.ratio,
                    left: Handle::from_usize(new_offset + 1),
                    right: Handle::from_usize(new_offset + 1 + left),
                }));

                left + right + 1
            }
        }
    }

    /// Returns the number of nodes that would be needed to store
    /// the tree if the target node is removed.
    fn count_after_removal(&self, current: Handle, target: Handle, acc: usize) -> usize {
        assert(current != target);

        match &self.nodes[current.idx()] {
            // Leaf is simple, always takes one node.
            Node::Leaf(_) => acc + 1,

            // Split is slightly more complicated. If either side is the
            // target to remove, then we remove the split node as well
            // so our count is just the count of the other side.
            //
            // If neither side is the target, then we count both sides
            // and add one to account for the split node itself.
            Node::Split(s) => {
                if s.left == target {
                    self.count_after_removal(s.right, target, acc)
                } else if s.right == target {
                    self.count_after_removal(s.left, target, acc)
                } else {
                    self.count_after_removal(s.left, target, acc)
                        + self.count_after_removal(s.right, target, acc)
                        + 1
                }
            }
        }
    }

    /// Equalize this node and all its children, returning a new node with splits
    /// adjusted so that each split's ratio is based on the relative weight
    /// (number of leaves) of its children.
    pub fn equalize(&self) -> Self {
        if self.is_empty() {
            return Self::EMPTY;
        }

        // Copy the existing nodes (taking new view references).
        let mut nodes = self.nodes.clone();

        // Go through and equalize our ratios based on weights.
        for node in nodes.iter_mut() {
            match node {
                Node::Leaf(_) => {}
                Node::Split(s) => {
                    let weight_left = self.weight(s.left, s.layout, 0);
                    let weight_right = self.weight(s.right, s.layout, 0);
                    assert(weight_left > 0);
                    assert(weight_right > 0);
                    let total_f16 = f16::from_f32((weight_left + weight_right) as f32);
                    let weight_left_f16 = f16::from_f32(weight_left as f32);
                    s.ratio = weight_left_f16 / total_f16;
                }
            }
        }

        Self {
            nodes,
            zoomed: self.zoomed,
        }
    }

    fn weight(&self, from: Handle, layout: Layout, acc: usize) -> usize {
        match &self.nodes[from.idx()] {
            Node::Leaf(_) => acc + 1,
            Node::Split(s) => {
                if s.layout == layout {
                    self.weight(s.left, layout, acc) + self.weight(s.right, layout, acc)
                } else {
                    1
                }
            }
        }
    }

    /// Resize the nearest split matching the layout by the given ratio.
    /// Positive is right and down.
    ///
    /// The ratio is a signed delta representing the percentage to move
    /// the divider. The percentage is of the entire grid size, not just
    /// the specific split size.
    /// We use the entire grid size because that's what Ghostty's
    /// `resize_split` keybind does, because it maps to a general human
    /// understanding of moving a split relative to the entire window
    /// (generally).
    ///
    /// For example, a ratio of 0.1 and a layout of `vertical` will find
    /// the nearest vertical split and move the divider down by 10% of
    /// the total grid height.
    ///
    /// If no matching split is found, this does nothing, but will always
    /// still return a cloned tree.
    pub fn resize(&self, from: Handle, layout: Layout, ratio: f16) -> Self {
        assert(ratio >= -f16::ONE && ratio <= f16::ONE);
        assert(!ratio.is_nan());
        assert(!ratio.is_infinite());

        // Fast path empty trees.
        if self.is_empty() {
            return Self::EMPTY;
        }

        // From this point forward worst case we return a clone.
        let mut result = self.clone_tree();

        // Find our nearest parent split node matching the layout.
        let parent_handle = match self.find_parent_split(layout, from, Handle::ROOT) {
            Backtrack::Deadend | Backtrack::Backtrack => return result,
            Backtrack::Result(v) => v,
        };

        // Get our spatial layout, because we need the dimensions of this
        // split with regards to the entire grid.
        let sp = result.spatial();

        // Our scale is the amount we need to divide our ratio delta by to
        // get a delta relative to the split, not the entire grid.
        // Its actually a ratio on its own but I'm trying to avoid that word:
        // its the ratio of our spatial width/height to the total.
        let scale = match layout {
            Layout::Horizontal => sp.slots[parent_handle.idx()].width / sp.slots[0].width,
            Layout::Vertical => sp.slots[parent_handle.idx()].height / sp.slots[0].height,
        };

        // If the split has spatial width/height 0, resizing by a percentage
        // of the total grid size doesn't make sense.
        if scale == f16::ZERO {
            return result;
        }

        // Adjust the old split ratio by the scaled ratio delta.
        let old_ratio = match &result.nodes[parent_handle.idx()] {
            Node::Split(s) => s.ratio,
            Node::Leaf(_) => unreachable!(),
        };
        let new_ratio = old_ratio + (ratio / scale);

        // Set the new ratio, clamping it to [0, 1]
        result.resize_in_place(parent_handle, new_ratio.max(f16::ZERO).min(f16::ONE));
        result
    }

    fn find_parent_split(&self, layout: Layout, from: Handle, current: Handle) -> Backtrack {
        if from == current {
            return Backtrack::Backtrack;
        }
        match &self.nodes[current.idx()] {
            Node::Leaf(_) => Backtrack::Deadend,
            Node::Split(s) => match self.find_parent_split(layout, from, s.left) {
                Backtrack::Result(v) => Backtrack::Result(v),
                Backtrack::Backtrack => {
                    if s.layout == layout {
                        Backtrack::Result(current)
                    } else {
                        Backtrack::Backtrack
                    }
                }
                Backtrack::Deadend => match self.find_parent_split(layout, from, s.right) {
                    Backtrack::Deadend => Backtrack::Deadend,
                    Backtrack::Result(v) => Backtrack::Result(v),
                    Backtrack::Backtrack => {
                        if s.layout == layout {
                            Backtrack::Result(current)
                        } else {
                            Backtrack::Backtrack
                        }
                    }
                },
            },
        }
    }

    /// Spatial representation of the split tree. This can be used to
    /// better understand the layout of the tree in a 2D space.
    ///
    /// The bounds of the representation are always based on the total
    /// 2D space being 1x1. The x/y coordinates and width/height dimensions
    /// of each individual split and leaf are relative to this.
    /// This means that the spatial representation is a normalized
    /// representation of the actual space.
    ///
    /// The top-left corner of the tree is always (0, 0).
    ///
    /// We use a normalized form because we can calculate it without
    /// accessing to the actual rendered view sizes. These actual sizes
    /// may not be available at various times because GUI toolkits often
    /// only make them available once they're part of a widget tree and
    /// a SplitTree can represent views that aren't currently visible.
    pub fn spatial(&self) -> Spatial {
        // No nodes, empty spatial representation.
        if self.nodes.is_empty() {
            return Spatial::EMPTY;
        }

        // Get our total dimensions.
        let dim = self.dimensions(Handle::ROOT);
        let dim_w = f16::from_f32(dim.width as f32);
        let dim_h = f16::from_f32(dim.height as f32);

        // Create our slots which will match our nodes exactly.
        let zero = Slot {
            x: f16::ZERO,
            y: f16::ZERO,
            width: f16::ZERO,
            height: f16::ZERO,
        };
        let mut slots = vec![zero; self.nodes.len()];
        slots[0] = Slot {
            x: f16::ZERO,
            y: f16::ZERO,
            width: dim_w,
            height: dim_h,
        };
        self.fill_spatial_slots(&mut slots, Handle::ROOT);

        // Normalize the dimensions to 1x1 grid.
        for slot in slots.iter_mut() {
            slot.x /= dim_w;
            slot.y /= dim_h;
            slot.width /= dim_w;
            slot.height /= dim_h;
        }

        Spatial { slots }
    }

    fn fill_spatial_slots(&self, slots: &mut [Slot], current_: Handle) {
        let current = current_.idx();
        assert(slots[current].width >= f16::ZERO && slots[current].height >= f16::ZERO);
        match &self.nodes[current] {
            // Leaf node, current slot is already filled by caller.
            Node::Leaf(_) => {}

            Node::Split(s) => {
                let cur = slots[current];
                match s.layout {
                    Layout::Horizontal => {
                        slots[s.left.idx()] = Slot {
                            x: cur.x,
                            y: cur.y,
                            width: cur.width * s.ratio,
                            height: cur.height,
                        };
                        slots[s.right.idx()] = Slot {
                            x: cur.x + cur.width * s.ratio,
                            y: cur.y,
                            width: cur.width * (f16::ONE - s.ratio),
                            height: cur.height,
                        };
                    }

                    Layout::Vertical => {
                        slots[s.left.idx()] = Slot {
                            x: cur.x,
                            y: cur.y,
                            width: cur.width,
                            height: cur.height * s.ratio,
                        };
                        slots[s.right.idx()] = Slot {
                            x: cur.x,
                            y: cur.y + cur.height * s.ratio,
                            width: cur.width,
                            height: cur.height * (f16::ONE - s.ratio),
                        };
                    }
                }

                self.fill_spatial_slots(slots, s.left);
                self.fill_spatial_slots(slots, s.right);
            }
        }
    }

    /// Get the dimensions of the tree starting from the given node.
    ///
    /// This creates relative dimensions (see Spatial) by assuming each
    /// leaf is exactly 1x1 unit in size.
    fn dimensions(&self, current: Handle) -> Dimensions {
        match &self.nodes[current.idx()] {
            Node::Leaf(_) => Dimensions {
                width: 1,
                height: 1,
            },
            Node::Split(s) => {
                let left = self.dimensions(s.left);
                let right = self.dimensions(s.right);
                match s.layout {
                    Layout::Horizontal => Dimensions {
                        width: left.width + right.width,
                        height: left.height.max(right.height),
                    },

                    Layout::Vertical => Dimensions {
                        width: left.width.max(right.width),
                        height: left.height + right.height,
                    },
                }
            }
        }
    }

    /// Format the tree as text (Zig `formatText`).
    pub fn format_text(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        if self.nodes.is_empty() {
            return writer.write_str("empty");
        }
        self.format_text_inner(writer, Handle::ROOT, 0)
    }

    fn format_text_inner(
        &self,
        writer: &mut dyn fmt::Write,
        current: Handle,
        depth: usize,
    ) -> fmt::Result {
        for _ in 0..depth {
            writer.write_str("  ")?;
        }

        if self.zoomed == Some(current) {
            writer.write_str("(zoomed) ")?;
        }

        match &self.nodes[current.idx()] {
            Node::Leaf(v) => match v.split_tree_label() {
                Some(label) => writeln!(writer, "leaf: {}", label),
                None => writeln!(writer, "leaf: {}", current),
            },

            Node::Split(s) => {
                writeln!(
                    writer,
                    "split (layout: {}, ratio: {:.2})",
                    s.layout.as_str(),
                    s.ratio.to_f32()
                )?;
                self.format_text_inner(writer, s.left, depth + 1)?;
                self.format_text_inner(writer, s.right, depth + 1)
            }
        }
    }

    /// Format the tree as an ASCII diagram (Zig `formatDiagram`).
    pub fn format_diagram(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        if self.nodes.is_empty() {
            return writer.write_str("empty");
        }

        // Get our spatial representation.
        let sp = {
            let sp = self.spatial();

            // Scale our spatial representation to have minimum width/height 1.
            let mut min_w = f16::ONE;
            let mut min_h = f16::ONE;
            for slot in sp.slots.iter() {
                if slot.width > f16::ZERO {
                    min_w = min_w.min(slot.width);
                }
                if slot.height > f16::ZERO {
                    min_h = min_h.min(slot.height);
                }
            }

            let ratio_w = f16::ONE / min_w;
            let ratio_h = f16::ONE / min_h;
            let mut slots = sp.slots.clone();
            for slot in slots.iter_mut() {
                slot.x *= ratio_w;
                slot.y *= ratio_h;
                slot.width *= ratio_w;
                slot.height *= ratio_h;
            }

            Spatial { slots }
        };

        // Labels: the view's label if it has one, else the node handle.
        let has_labels = self.nodes.iter().any(|node| match node {
            Node::Leaf(v) => v.split_tree_label().is_some(),
            Node::Split(_) => false,
        });

        // The width we need for the largest label.
        let max_label_width: usize = if !has_labels {
            sp.slots.len().ilog10() as usize + 1
        } else {
            let mut max: usize = 0;
            for node in self.nodes.iter() {
                if let Node::Leaf(view) = node {
                    let label = view.split_tree_label().unwrap_or("");
                    max = max.max(label.len());
                }
            }
            max
        };

        // We need space for whitespace and ASCII art so add that.
        // We need to accommodate the leaf handle, whitespace, and
        // then the border.
        //
        // Border + whitespace + label + whitespace + border.
        let cell_width = 2 + max_label_width + 2;
        // Border + label + border. No whitespace needed on the
        // vertical axis.
        let cell_height = 1 + 1 + 1;

        // Make a grid that can fit our entire ASCII diagram. We know
        // the width/height based on node 0.
        let mut grid: Vec<Vec<u8>> = {
            // Get our initial width/height. Each leaf is 1x1 in this.
            // We round up for this because partial widths/heights should
            // take up an extra cell.
            let mut width = sp.slots[0].width.to_f32().ceil() as usize;
            let mut height = sp.slots[0].height.to_f32().ceil() as usize;

            // We need space for whitespace and ASCII art so add that.
            // We need to accommodate the leaf handle, whitespace, and
            // then the border.
            width *= cell_width;
            height *= cell_height;

            (0..height)
                .map(|_| {
                    let mut row = vec![b' '; width + 1];
                    row[width] = b'\n';
                    row
                })
                .collect()
        };

        // Draw each node
        for (handle, slot) in sp.slots.iter().enumerate() {
            // We only draw leaf nodes. Splits are only used for layout.
            let node = &self.nodes[handle];
            let view = match node {
                Node::Leaf(v) => v,
                Node::Split(_) => continue,
            };

            // If our width/height is zero then we skip this.
            if slot.width == f16::ZERO || slot.height == f16::ZERO {
                continue;
            }

            let mut x = slot.x.to_f32().floor() as usize;
            let mut y = slot.y.to_f32().floor() as usize;
            let mut width = slot.width.to_f32().floor().max(1.0) as usize;
            let mut height = slot.height.to_f32().floor().max(1.0) as usize;
            x *= cell_width;
            y *= cell_height;
            width *= cell_width;
            height *= cell_height;

            // Top border
            {
                let top = &mut grid[y][x..][..width];
                top[0] = b'+';
                for c in top[1..width - 1].iter_mut() {
                    *c = b'-';
                }
                top[width - 1] = b'+';
            }

            // Bottom border
            {
                let bottom = &mut grid[y + height - 1][x..][..width];
                bottom[0] = b'+';
                for c in bottom[1..width - 1].iter_mut() {
                    *c = b'-';
                }
                bottom[width - 1] = b'+';
            }

            // Left border
            for row in grid[y + 1..y + height - 1].iter_mut() {
                row[x] = b'|';
            }
            for row in grid[y + 1..y + height - 1].iter_mut() {
                row[x + width - 1] = b'|';
            }

            // Get our label text
            let handle_label;
            let label: &str = match view.split_tree_label() {
                Some(l) if has_labels => l,
                _ => {
                    handle_label = handle.to_string();
                    &handle_label
                }
            };

            // Draw the handle in the center
            let x_mid = width / 2 + x;
            let y_mid = height / 2 + y;
            let label_width = label.len();
            let label_start = x_mid - label_width / 2;
            let row = &mut grid[y_mid][label_start..];
            if row.len() < label.len() {
                return Err(fmt::Error);
            }
            row[..label.len()].copy_from_slice(label.as_bytes());
        }

        // Output every row
        for row in grid.iter() {
            // We currently have a bug in our height calculation that
            // results in trailing blank lines. Ignore those. We should
            // really fix our height calculation instead. If someone wants
            // to do that just remove this line and see the tests that fail
            // and go from there.
            if row[0] == b' ' {
                break;
            }
            writer.write_str(&String::from_utf8_lossy(row))?;
        }

        Ok(())
    }

    /// Display adapter for [`SplitTree::format_text`]
    /// (Zig: `std.fmt.alt(t, .formatText)`).
    pub fn text(&self) -> TextFormatter<'_, V> {
        TextFormatter(self)
    }

    /// Display adapter for [`SplitTree::format_diagram`]
    /// (Zig: `std.fmt.alt(t, .formatDiagram)`).
    pub fn diagram(&self) -> DiagramFormatter<'_, V> {
        DiagramFormatter(self)
    }
}

/// Format the tree in a human-readable format. By default this will
/// output a diagram followed by a textual representation.
impl<V: View> fmt::Display for SplitTree<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.nodes.is_empty() {
            return f.write_str("empty");
        }
        // Diagram errors are ignored like the Zig version.
        let mut diagram = String::new();
        if self.format_diagram(&mut diagram).is_ok() {
            f.write_str(&diagram)?;
        }
        self.format_text(f)
    }
}

/// See [`SplitTree::text`].
pub struct TextFormatter<'a, V>(&'a SplitTree<V>);

impl<V: View> fmt::Display for TextFormatter<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.format_text(f)
    }
}

/// See [`SplitTree::diagram`].
pub struct DiagramFormatter<'a, V>(&'a SplitTree<V>);

impl<V: View> fmt::Display for DiagramFormatter<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.format_diagram(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Zig test view copies itself on `ref`; here `Clone` does the same.
    #[derive(Debug, Clone)]
    struct TestView {
        label: &'static str,
    }

    impl View for TestView {
        fn eql(&self, other: &Self) -> bool {
            self.label == other.label
        }

        fn split_tree_label(&self) -> Option<&str> {
            Some(self.label)
        }
    }

    type TestTree = SplitTree<TestView>;

    fn h(v: f64) -> f16 {
        f16::from_f64(v)
    }

    fn view(label: &'static str) -> TestView {
        TestView { label }
    }

    fn find(t: &TestTree, label: &str) -> Handle {
        t.iterator()
            .find(|entry| entry.view.label == label)
            .map(|entry| entry.handle)
            .expect("NotFound")
    }

    fn leaf(t: &TestTree, handle: Handle) -> &TestView {
        match &t.nodes()[handle.idx()] {
            Node::Leaf(v) => v,
            Node::Split(_) => panic!("not a leaf"),
        }
    }

    #[test]
    fn split_tree_is_split() {
        // Empty tree should not be split
        let empty: TestTree = TestTree::EMPTY;
        assert!(!empty.is_split());

        // Single node tree should not be split
        let single = TestTree::init(&view("A"));
        assert!(!single.is_split());

        // Split tree should be split
        let tree2 = TestTree::init(&view("B"));
        let split = single
            .split(Handle::ROOT, Direction::Right, h(0.5), &tree2)
            .unwrap();
        assert!(split.is_split());
    }

    #[test]
    fn split_tree_empty_tree() {
        let t: TestTree = TestTree::EMPTY;
        let s = format!("{}", t);
        assert_eq!(s, "empty");
    }

    #[test]
    fn split_tree_single_node() {
        let t = TestTree::init(&view("A"));

        let s = format!("{}", t.diagram());
        assert_eq!(
            s,
            "+---+\n\
             | A |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_horizontal() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));
        let t3 = t1
            .split(
                Handle::ROOT,     // at root
                Direction::Right, // split right
                h(0.5),
                &t2, // insert t2
            )
            .unwrap();

        assert_eq!(
            format!("{}", t3),
            "+---++---+\n\
             | A || B |\n\
             +---++---+\n\
             split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               leaf: B\n"
        );

        // Split right at B
        let t_c = TestTree::init(&view("C"));
        let t4 = t3
            .split(find(&t3, "B"), Direction::Right, h(0.5), &t_c)
            .unwrap();

        assert_eq!(
            format!("{}", t4),
            "+--------++---++---+\n\
             |    A   || B || C |\n\
             +--------++---++---+\n\
             split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               split (layout: horizontal, ratio: 0.50)\n    \
                 leaf: B\n    \
                 leaf: C\n"
        );

        // Split right at C
        let t_d = TestTree::init(&view("D"));
        let t5 = t4
            .split(find(&t4, "C"), Direction::Right, h(0.5), &t_d)
            .unwrap();

        assert_eq!(
            "+------------------++--------++---++---+\n\
             |         A        ||    B   || C || D |\n\
             +------------------++--------++---++---+\n\
             split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               split (layout: horizontal, ratio: 0.50)\n    \
                 leaf: B\n    \
                 split (layout: horizontal, ratio: 0.50)\n      \
                   leaf: C\n      \
                   leaf: D\n",
            format!("{}", t5)
        );

        // Find "previous" from D back.
        {
            let mut current: u8 = b'D';
            while current != b'A' {
                let label = (current as char).to_string();
                let handle = t5.previous(find(&t5, &label)).unwrap();
                let entry = leaf(&t5, handle);
                assert_eq!(entry.label, ((current - 1) as char).to_string());
                current -= 1;
            }

            let label = (current as char).to_string();
            assert!(t5.previous(find(&t5, &label)).is_none());
        }

        // Find "next" from A forward.
        {
            let mut current: u8 = b'A';
            while current != b'D' {
                let label = (current as char).to_string();
                let handle = t5.next(find(&t5, &label)).unwrap();
                let entry = leaf(&t5, handle);
                assert_eq!(entry.label, ((current + 1) as char).to_string());
                current += 1;
            }

            let label = (current as char).to_string();
            assert!(t5.next(find(&t5, &label)).is_none());
        }
    }

    #[test]
    fn split_tree_split_vertical() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        let t3 = t1
            .split(
                Handle::ROOT,    // at root
                Direction::Down, // split down
                h(0.5),
                &t2, // insert t2
            )
            .unwrap();

        assert_eq!(
            format!("{}", t3.diagram()),
            "+---+\n\
             | A |\n\
             +---+\n\
             +---+\n\
             | B |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_horizontal_with_zero_ratio() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Right, h(0.0), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.diagram()),
            "+---+\n\
             | B |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_vertical_with_zero_ratio() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Down, h(0.0), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.diagram()),
            "+---+\n\
             | B |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_horizontal_with_full_width() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Right, h(1.0), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.diagram()),
            "+---+\n\
             | A |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_vertical_with_full_width() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Down, h(1.0), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.diagram()),
            "+---+\n\
             | A |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_remove_leaf() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));
        let t3 = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();

        // Remove "A"
        let t4 = t3.remove(find(&t3, "A"));

        assert_eq!(
            format!("{}", t4.diagram()),
            "+---+\n\
             | B |\n\
             +---+\n"
        );
    }

    #[test]
    fn split_tree_split_twice_remove_intermediary() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));
        let t3 = TestTree::init(&view("C"));

        // A | B horizontal.
        let split1 = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();

        // Insert C below that.
        let split2 = split1
            .split(Handle::ROOT, Direction::Down, h(0.5), &t3)
            .unwrap();

        assert_eq!(
            format!("{}", split2.diagram()),
            "+---++---+\n\
             | A || B |\n\
             +---++---+\n\
             +--------+\n\
             |    C   |\n\
             +--------+\n"
        );

        // Remove "B"
        let split3 = split2.remove(find(&split2, "B"));

        assert_eq!(
            format!("{}", split3.diagram()),
            "+---+\n\
             | A |\n\
             +---+\n\
             +---+\n\
             | C |\n\
             +---+\n"
        );

        // Remove every node from split2 (our most complex one), which should
        // never crash. We don't test the result is correct, this just verifies
        // we don't hit any assertion failures.
        for i in 0..split2.nodes().len() {
            let t = split2.remove(Handle::from_usize(i));
            t.deinit();
        }
    }

    #[test]
    fn split_tree_spatial_goto() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));
        let t3 = TestTree::init(&view("C"));
        let t4 = TestTree::init(&view("D"));

        // A | B horizontal
        let split_ab = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();

        // A | C vertical
        let split_ac = split_ab
            .split(find(&split_ab, "A"), Direction::Down, h(0.8), &t3)
            .unwrap();

        // B | D vertical
        let split_bd = split_ac
            .split(find(&split_ab, "B"), Direction::Down, h(0.3), &t4)
            .unwrap();
        let split = &split_bd;

        assert_eq!(
            format!("{}", split.diagram()),
            "+---++---+\n\
             |   || B |\n\
             |   |+---+\n\
             |   |+---+\n\
             | A ||   |\n\
             |   ||   |\n\
             |   ||   |\n\
             |   || D |\n\
             +---+|   |\n\
             +---+|   |\n\
             | C ||   |\n\
             +---++---+\n"
        );

        // Spatial C => right
        {
            let target = split
                .goto(find(split, "C"), Goto::Spatial(SpatialDirection::Right))
                .unwrap();
            assert_eq!(leaf(split, target).label, "D");
        }

        // Spatial D => left
        {
            let target = split
                .goto(find(split, "D"), Goto::Spatial(SpatialDirection::Left))
                .unwrap();
            assert_eq!("A", leaf(split, target).label);
        }

        // Spatial A => left (wrapped)
        {
            let target = split
                .goto(find(split, "A"), Goto::Spatial(SpatialDirection::Left))
                .unwrap();
            assert_eq!("B", leaf(split, target).label);
        }

        // Spatial B => right (wrapped)
        {
            let target = split
                .goto(find(split, "B"), Goto::Spatial(SpatialDirection::Right))
                .unwrap();
            assert_eq!("A", leaf(split, target).label);
        }

        // Spatial C => down (wrapped)
        {
            let target = split
                .goto(find(split, "C"), Goto::Spatial(SpatialDirection::Down))
                .unwrap();
            assert_eq!("A", leaf(split, target).label);
        }

        // Equalize
        let equal = split.equalize();

        assert_eq!(
            format!("{}", equal.diagram()),
            "+---++---+\n\
             | A || B |\n\
             +---++---+\n\
             +---++---+\n\
             | C || D |\n\
             +---++---+\n"
        );
    }

    #[test]
    fn split_tree_resize() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.diagram()),
            "+---++---+\n\
             | A || B |\n\
             +---++---+\n"
        );

        // Resize
        {
            let resized = split.resize(
                find(&split, "B"),
                Layout::Horizontal, // resize right
                h(0.25),
            );
            assert_eq!(
                format!("{}", resized.diagram()),
                "+-------------++---+\n\
                 |      A      || B |\n\
                 +-------------++---+\n"
            );
        }

        // Resize the other direction (negative ratio)
        {
            let resized = split.resize(
                find(&split, "B"),
                Layout::Horizontal, // resize left
                h(-0.25),
            );
            assert_eq!(
                format!("{}", resized.diagram()),
                "+---++-------------+\n\
                 | A ||      B      |\n\
                 +---++-------------+\n"
            );
        }
    }

    #[test]
    fn split_tree_resize_nested_split() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));
        let t3 = TestTree::init(&view("C"));

        // A | B vertical
        let split_ab = t1
            .split(Handle::ROOT, Direction::Down, h(0.5), &t2)
            .unwrap();

        let split_bc = split_ab
            .split(find(&split_ab, "B"), Direction::Down, h(0.5), &t3)
            .unwrap();

        assert_eq!(
            format!("{}", split_bc.diagram()),
            "+---+\n\
             |   |\n\
             |   |\n\
             | A |\n\
             |   |\n\
             +---+\n\
             +---+\n\
             | B |\n\
             +---+\n\
             +---+\n\
             | C |\n\
             +---+\n"
        );

        // Resize
        {
            let resized = split_bc.resize(
                find(&split_bc, "B"),
                Layout::Vertical, // resize down
                h(0.125),
            );
            assert_eq!(
                format!("{}", resized.diagram()),
                "+---+\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 | A |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 +---+\n\
                 +---+\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 | B |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 +---+\n\
                 +---+\n\
                 | C |\n\
                 +---+\n"
            );
        }

        // Resize the other direction (negative ratio)
        {
            let resized = split_bc.resize(
                find(&split_bc, "B"),
                Layout::Vertical, // resize up
                h(-0.0833),
            );
            assert_eq!(
                format!("{}", resized.diagram()),
                "+---+\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 | A |\n\
                 |   |\n\
                 |   |\n\
                 |   |\n\
                 +---+\n\
                 +---+\n\
                 | B |\n\
                 +---+\n\
                 +---+\n\
                 |   |\n\
                 |   |\n\
                 | C |\n\
                 |   |\n\
                 +---+\n"
            );
        }
    }

    #[test]
    fn split_tree_clone_empty_tree() {
        let t: TestTree = TestTree::EMPTY;

        let t2 = t.clone_tree();

        assert_eq!(format!("{}", t2), "empty");
    }

    #[test]
    fn split_tree_zoom() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let mut split = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();
        let b = find(&split, "B");
        split.zoom(Some(b));

        assert_eq!(
            format!("{}", split.text()),
            "split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               (zoomed) leaf: B\n"
        );

        // Clone preserves zoom
        let clone = split.clone_tree();

        assert_eq!(
            format!("{}", clone.text()),
            "split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               (zoomed) leaf: B\n"
        );
    }

    #[test]
    fn split_tree_split_resets_zoom() {
        let mut t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // Zoom A
        let a = find(&t1, "A");
        t1.zoom(Some(a));

        // A | B horizontal
        let split = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();

        assert_eq!(
            format!("{}", split.text()),
            "split (layout: horizontal, ratio: 0.50)\n  \
               leaf: A\n  \
               leaf: B\n"
        );
    }

    #[test]
    fn split_tree_remove_and_zoom() {
        let t1 = TestTree::init(&view("A"));
        let t2 = TestTree::init(&view("B"));

        // A | B horizontal
        let mut split = t1
            .split(Handle::ROOT, Direction::Right, h(0.5), &t2)
            .unwrap();
        let a = find(&split, "A");
        split.zoom(Some(a));

        // Remove A, should unzoom
        {
            let removed = split.remove(find(&split, "A"));
            assert!(removed.zoomed.is_none());

            assert_eq!(format!("{}", removed.text()), "leaf: B\n");
        }

        // Remove B, should keep zoom
        {
            let removed = split.remove(find(&split, "B"));

            assert_eq!(format!("{}", removed.text()), "(zoomed) leaf: A\n");
        }
    }

    #[test]
    fn split_tree_views_are_reference_counted() {
        // Not in the Zig file: cloning/dropping trees refs/unrefs views.
        use std::rc::Rc;

        #[derive(Clone)]
        struct RcView(Rc<&'static str>);
        impl View for RcView {
            fn eql(&self, other: &Self) -> bool {
                Rc::ptr_eq(&self.0, &other.0)
            }
        }

        let a = RcView(Rc::new("A"));
        let b = RcView(Rc::new("B"));
        let ta = SplitTree::init(&a);
        let tb = SplitTree::init(&b);
        let split = ta
            .split(Handle::ROOT, Direction::Right, h(0.5), &tb)
            .unwrap();
        assert_eq!(Rc::strong_count(&a.0), 3);
        assert_eq!(split.locate(&b), Some(Handle(1)));
        let removed = split.remove(Handle(1));
        assert_eq!(Rc::strong_count(&a.0), 4);
        drop(removed);
        drop(split);
        drop(ta);
        assert_eq!(Rc::strong_count(&a.0), 1);
        // Handles are used as labels when views have none.
        let tb2 = tb
            .split(Handle::ROOT, Direction::Down, h(0.5), &tb)
            .unwrap();
        assert_eq!(
            format!("{}", tb2.text()),
            "split (layout: vertical, ratio: 0.50)\n  leaf: 2\n  leaf: 1\n"
        );
    }
}
