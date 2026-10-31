//! Port of `src/terminal/ScreenSet.zig`.
//!
//! STATUS: ported
//!
//! A ScreenSet holds multiple terminal screens. This is initially created
//! to handle simple primary vs alternate screens, but could be extended
//! in the future to handle N screens.
//!
//! One of the goals of this is to allow lazy initialization of screens
//! as needed. The primary screen is always initialized, but the alternate
//! screen may not be until first used.
//!
//! Notes on the port:
//! - Zig stores `*Screen` heap pointers. The port keeps that design: each
//!   screen is a leaked `Box<Screen>` addressed by a [`ScreenPtr`] (a thin,
//!   `Copy` non-null pointer that dereferences to the screen, like a Zig
//!   `*Screen`). The set owns every initialized screen and frees it in
//!   `remove` or on drop.
//! - `io`/`alloc` parameters are dropped (global allocator).

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

use crate::terminal::screen::{Options as ScreenOptions, Screen};
use crate::terminal::size::AllocError;

/// The possible keys for screens in the screen set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Key {
    Primary = 0,
    Alternate = 1,
}

impl Key {
    /// Number of keys.
    pub const COUNT: usize = 2;

    /// All keys in declaration order.
    pub const ALL: [Key; Key::COUNT] = [Key::Primary, Key::Alternate];

    /// The Zig tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            Key::Primary => "primary",
            Key::Alternate => "alternate",
        }
    }
}

/// A pointer to a screen owned by a [`ScreenSet`] (Zig: `*Screen`).
///
/// This is a raw pointer with dereference sugar: it is only valid while
/// the owning set keeps the screen alive (until `remove` of its key or the
/// set is dropped). Comparing two `ScreenPtr`s compares identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenPtr(NonNull<Screen>);

impl ScreenPtr {
    /// The raw pointer.
    #[inline]
    pub fn as_ptr(self) -> *mut Screen {
        self.0.as_ptr()
    }

    /// Get a mutable reference with an unbounded lifetime.
    ///
    /// # Safety
    /// The screen must remain alive and unaliased for `'a`.
    #[inline]
    pub unsafe fn as_mut<'a>(self) -> &'a mut Screen {
        // SAFETY: caller contract.
        unsafe { &mut *self.0.as_ptr() }
    }
}

impl fmt::Debug for ScreenPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScreenPtr({:p})", self.0.as_ptr())
    }
}

impl Deref for ScreenPtr {
    type Target = Screen;

    #[inline(always)]
    fn deref(&self) -> &Screen {
        // SAFETY: a ScreenPtr always points at a live screen owned by a
        // ScreenSet (type contract, see above).
        unsafe { &*self.0.as_ptr() }
    }
}

impl DerefMut for ScreenPtr {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Screen {
        // SAFETY: see Deref.
        unsafe { &mut *self.0.as_ptr() }
    }
}

/// See the module documentation.
pub struct ScreenSet {
    /// The key value of the currently active screen. Useful for simple
    /// comparisons, e.g. "is this screen the primary screen".
    pub active_key: Key,

    /// The active screen pointer.
    pub active: ScreenPtr,

    /// All screens that are initialized (Zig: `std.EnumMap(Key, *Screen)`).
    pub all: [Option<ScreenPtr>; Key::COUNT],

    /// Monotonic generation counter for each screen key. This changes whenever
    /// screen storage is removed or replaced so external handles can distinguish a
    /// newly initialized screen from stale references into destroyed storage.
    pub generations: [usize; Key::COUNT],
}

// SAFETY: the set exclusively owns every screen it points to.
unsafe impl Send for ScreenSet {}

fn create(opts: ScreenOptions) -> Result<ScreenPtr, AllocError> {
    let screen = Box::new(Screen::init(opts)?);
    Ok(ScreenPtr(NonNull::from(Box::leak(screen))))
}

fn destroy(screen: ScreenPtr) {
    // SAFETY: every ScreenPtr in a set was created by `create` and is
    // destroyed exactly once.
    drop(unsafe { Box::from_raw(screen.as_ptr()) });
}

impl ScreenSet {
    pub fn init(opts: ScreenOptions) -> Result<ScreenSet, AllocError> {
        // We need to initialize our initial primary screen
        let screen = create(opts)?;
        Ok(ScreenSet {
            active_key: Key::Primary,
            active: screen,
            all: [Some(screen), None],
            generations: [0; Key::COUNT],
        })
    }

    /// Get the screen for the given key, if it is initialized.
    #[inline]
    pub fn get(&self, key: Key) -> Option<ScreenPtr> {
        self.all[key as usize]
    }

    /// Get the current generation for the given screen key.
    #[inline]
    pub fn generation(&self, key: Key) -> usize {
        self.generations[key as usize]
    }

    /// Get the screen for the given key, initializing it if necessary.
    pub fn get_init(&mut self, key: Key, opts: ScreenOptions) -> Result<ScreenPtr, AllocError> {
        if let Some(screen) = self.get(key) {
            return Ok(screen);
        }
        let screen = create(opts)?;
        self.all[key as usize] = Some(screen);
        Ok(screen)
    }

    /// Remove a key from the set. The primary screen cannot be removed (asserted).
    pub fn remove(&mut self, key: Key) {
        assert!(key != Key::Primary);
        if let Some(screen) = self.all[key as usize].take() {
            self.generations[key as usize] = self.generation(key).wrapping_add(1);
            destroy(screen);
        }
    }

    /// Switch the active screen to the given key. Requires that the
    /// screen is initialized.
    pub fn switch_to(&mut self, key: Key) {
        self.active_key = key;
        self.active = self.all[key as usize].unwrap();
    }

    /// Iterate over all initialized screens (Zig: `all.iterator()`).
    pub fn iter(&self) -> impl Iterator<Item = (Key, ScreenPtr)> + '_ {
        Key::ALL
            .iter()
            .filter_map(move |&k| self.all[k as usize].map(|s| (k, s)))
    }
}

impl Drop for ScreenSet {
    fn drop(&mut self) {
        // Destroy all initialized screens
        for slot in self.all.iter_mut() {
            if let Some(screen) = slot.take() {
                destroy(screen);
            }
        }
    }
}

impl fmt::Debug for ScreenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScreenSet")
            .field("active_key", &self.active_key)
            .field("active", &self.active)
            .field("all", &self.all)
            .field("generations", &self.generations)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_set() {
        let mut set = ScreenSet::init(ScreenOptions::default()).unwrap();
        assert_eq!(Key::Primary, set.active_key);
        assert_eq!(0usize, set.generation(Key::Primary));
        assert_eq!(0usize, set.generation(Key::Alternate));

        // Initialize a secondary screen
        set.get_init(Key::Alternate, ScreenOptions::default()).unwrap();
        assert_eq!(0usize, set.generation(Key::Alternate));

        set.switch_to(Key::Alternate);
        assert_eq!(Key::Alternate, set.active_key);
    }

    #[test]
    fn screen_set_generations() {
        let mut set = ScreenSet::init(ScreenOptions::default()).unwrap();

        assert_eq!(0usize, set.generation(Key::Primary));
        assert_eq!(0usize, set.generation(Key::Alternate));

        // A no-op removal doesn't change the generation.
        set.remove(Key::Alternate);
        assert_eq!(0usize, set.generation(Key::Alternate));

        // Initializing a screen doesn't change the generation.
        set.get_init(Key::Alternate, ScreenOptions::default()).unwrap();
        assert_eq!(0usize, set.generation(Key::Alternate));

        let alternate_generation = set.generation(Key::Alternate);
        set.remove(Key::Alternate);
        assert_eq!(alternate_generation.wrapping_add(1), set.generation(Key::Alternate));

        // Reinitializing keeps the generation from the last removal, so stale
        // handles can distinguish the new screen from the destroyed screen.
        set.get_init(Key::Alternate, ScreenOptions::default()).unwrap();
        assert_eq!(alternate_generation.wrapping_add(1), set.generation(Key::Alternate));
        assert_eq!(0usize, set.generation(Key::Primary));
    }
}
