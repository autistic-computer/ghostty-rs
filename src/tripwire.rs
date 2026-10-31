//! Port of `src/tripwire.zig`.
//!
//! STATUS: ported
//!
//! A library for injecting failures into code for the express
//! purpose of testing error handling paths.
//!
//! Improper `errdefer` is one of the highest sources of bugs in Zig code
//! (in Rust: improper cleanup/rollback on the `?` paths). Many error
//! points are hard to exercise in unit tests and rare to encounter in
//! production, so they often hide bugs. Worse, error scenarios are most
//! likely to put your code in an unexpected state that can result in
//! future assertion failures or memory safety issues.
//!
//! This module aims to solve this problem by providing a way to inject
//! errors at specific points in your code during unit tests, allowing you
//! to test every possible error path.
//!
//! # Usage
//!
//! To use this package, create a [`Module`] for each failable
//! function you want to test. The fail point enum must be hand-curated to
//! be the set of fail points, and the error type comes directly from the
//! function itself.
//!
//! Pepper your function with `tw.check(..)?` calls wherever you want to
//! have a testable failure point. You don't need every `?` to have
//! an associated tripwire check, only the ones you care about testing.
//! Usually, this is going to be the points where you want to test
//! cleanup logic above it.
//!
//! In unit tests, add `tw.error_always(..)` or related calls to
//! configure expected failures. Then, call your function. Finally, always
//! call `tw.end(ResetMode::Reset)` to verify your expectations were met
//! and to reset the tripwire module for future tests.
//!
//! ```ignore
//! #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//! enum FailPoint { AllocBuf, OpenFile }
//! static TW: tripwire::Module<FailPoint, MyError> = tripwire::Module::new();
//!
//! fn my_function() -> Result<(), MyError> {
//!     TW.check(FailPoint::AllocBuf)?;
//!     let buf = vec![0u8; 1024];
//!
//!     TW.check(FailPoint::OpenFile)?;
//!     let file = std::fs::File::open("foo.txt")?;
//!     // ...
//! }
//!
//! #[test]
//! fn my_function_fails_on_alloc() {
//!     TW.error_always(FailPoint::AllocBuf, MyError::OutOfMemory);
//!     assert_eq!(my_function(), Err(MyError::OutOfMemory));
//!     TW.end(tripwire::ResetMode::Reset).unwrap();
//! }
//! ```
//!
//! ## Transitive Function Calls
//!
//! To test transitive calls, there are two schools of thought:
//!
//!   1. Put a failure point above the transitive call in the caller
//!      and assume the child function error handling works correctly.
//!
//!   2. Create another tripwire module for the child function and
//!      trigger failures there. This is recommended if the child function
//!      can't really be called in isolation (e.g. its an auxiliary function
//!      to a public API).
//!
//! Either works, its situationally dependent which is better.
//!
//! ## Differences from the Zig version
//!
//! * Zig gives each `tripwire.module(P, E)` instantiation its own global
//!   (container-level) state. Rust has no generic statics, so the state
//!   lives in a thread-local map keyed by the `(P, E)` type pair. Each
//!   module should therefore use its own fail point enum (which is the
//!   natural thing to do anyway).
//! * The state is **thread-local** rather than process-global because
//!   Rust's test harness runs tests in parallel threads; a tripwire armed
//!   in one test must not trip a different test that happens to run the
//!   same code at the same time. The failable function must be called on
//!   the same thread that armed the tripwire (which is always the case for
//!   ordinary unit tests).
//! * The error type `E` is a regular Rust type (usually an error enum).
//!   Zig's "error set derived from a function type" maps to simply naming
//!   that function's error type.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

// Future ideas:
//
//   - Assert that the errors are actually tripped. e.g. you set a
//     errorAlways on a point, and want to verify it was tripped.
//   - Assert that every point is covered by at least one test. We
//     can probably use global state for this.
//   - Error only after a certain number of calls to a point.
//   - Error only on a range of calls (first 5 times, 2-7th time, etc.)
//
// I don't want to implement these until they're actually needed by
// some part of our codebase, but I want to list them here in case they
// do become useful.

/// Whether tripwire modules are enabled. Like Zig's `builtin.is_test`,
/// this is only true when compiling this crate's unit tests; otherwise
/// every check compiles to nothing.
pub const ENABLED: bool = cfg!(test);

/// How `end` treats the configured expectations afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Reset,
    Retain,
}

/// Returned by [`Module::end`] when a configured tripwire was never tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndError {
    UntrippedError,
}

impl fmt::Display for EndError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndError::UntrippedError => f.write_str("UntrippedError"),
        }
    }
}

impl std::error::Error for EndError {}

/// A configured tripwire.
#[derive(Debug, Clone)]
struct Tripwire<E> {
    /// Error to return when tripped
    err: E,

    /// The amount of times this tripwire has been reached. This
    /// is NOT the number of times it has tripped, since we may
    /// have mins for that.
    reached: usize,

    /// The minimum number of times this must be reached before
    /// tripping. After this point, it trips every time. This is
    /// a "before" check so if this is "1" then it'll trip the
    /// second time it's reached.
    min: usize,

    /// True if this has been tripped at least once.
    tripped: bool,
}

/// The configured tripwires for one module (Zig: `std.EnumMap`).
struct TripwireMap<P, E> {
    entries: Vec<(P, Tripwire<E>)>,
}

impl<P: PartialEq, E> TripwireMap<P, E> {
    fn put(&mut self, point: P, tw: Tripwire<E>) {
        if let Some(entry) = self.entries.iter_mut().find(|(p, _)| *p == point) {
            entry.1 = tw;
        } else {
            self.entries.push((point, tw));
        }
    }

    fn get_mut(&mut self, point: &P) -> Option<&mut Tripwire<E>> {
        self.entries
            .iter_mut()
            .find(|(p, _)| p == point)
            .map(|(_, tw)| tw)
    }
}

thread_local! {
    /// Per-thread storage for every module's tripwire map, keyed by the
    /// module's `(P, E)` type pair.
    static STATE: RefCell<HashMap<TypeId, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

/// A tripwire module that can be used to inject failures at specific points.
///
/// Outside of unit tests, this module is free and completely optimized away.
/// It takes up zero binary or runtime space and all function calls are
/// optimized out.
///
/// To use this module, add `check` (or related) calls prior to every
/// `?` operation that you want to be able to fail arbitrarily. Then,
/// in your unit tests, call the `error_*` family of functions to configure
/// when errors should be injected.
///
/// `P` is an enum type representing the failure points in the module.
/// `E` is the error type of possible errors that can be returned from the
/// failure points. Use `check_constrained` to narrow it down to a
/// different (convertible) error type.
pub struct Module<P, E> {
    _marker: PhantomData<fn() -> (P, E)>,
}

impl<P, E> Module<P, E> {
    /// Create a module handle. Every handle with the same `P`/`E` shares
    /// the same (thread-local) tripwire state, so this is usually stored in
    /// a `static`.
    pub const fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<P, E> Default for Module<P, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P, E> Module<P, E>
where
    P: Copy + PartialEq + fmt::Debug + 'static,
    E: Clone + 'static,
{
    /// Whether our module is enabled or not. In the future we may
    /// want to make this a comptime parameter to the module.
    pub const ENABLED: bool = ENABLED;

    fn with_map<R>(f: impl FnOnce(&mut TripwireMap<P, E>) -> R) -> R {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = state.entry(TypeId::of::<(P, E)>()).or_insert_with(|| {
                Box::new(TripwireMap::<P, E> {
                    entries: Vec::new(),
                })
            });
            let map = entry
                .downcast_mut::<TripwireMap<P, E>>()
                .expect("tripwire state type mismatch");
            f(map)
        })
    }

    /// Check for a failure at the given failure point. These should
    /// be placed directly before the `?` operation that may fail.
    #[inline(always)]
    pub fn check(&self, point: P) -> Result<(), E> {
        if !ENABLED {
            return Ok(());
        }
        self.check_constrained::<E>(point)
    }

    /// Same as check but allows specifying a custom error type for the
    /// return value. The configured error is converted with `TryFrom` and
    /// this panics at runtime if the configured tripwire error can't be
    /// converted to the `C` type.
    #[inline(always)]
    pub fn check_constrained<C>(&self, point: P) -> Result<(), C>
    where
        C: TryFrom<E>,
    {
        if !ENABLED {
            return Ok(());
        }
        let err = Self::with_map(|map| {
            let tripwire = map.get_mut(&point)?;
            tripwire.reached += 1;
            if tripwire.reached <= tripwire.min {
                return None;
            }
            tripwire.tripped = true;
            Some(tripwire.err.clone())
        });
        match err {
            None => Ok(()),
            Some(err) => match C::try_from(err) {
                Ok(c) => Err(c),
                Err(_) => panic!("tripwire error not in constrained error set"),
            },
        }
    }

    /// Mark a failure point to always trip with the given error.
    pub fn error_always(&self, point: P, err: E) {
        self.error_after(point, err, 0);
    }

    /// Mark a failure point to trip with the given error after
    /// the failure point is reached at least `min` times. A value of
    /// zero is equivalent to `error_always`.
    pub fn error_after(&self, point: P, err: E, min: usize) {
        Self::with_map(|map| {
            map.put(
                point,
                Tripwire {
                    err,
                    reached: 0,
                    min,
                    tripped: false,
                },
            )
        });
    }

    /// Ends the tripwire session. This will raise an error if there
    /// were untripped error expectations. The reset mode specifies
    /// whether expectations are reset too. Expectations are always reset,
    /// even if this returns an error.
    pub fn end(&self, reset_mode: ResetMode) -> Result<(), EndError> {
        let mut untripped = false;
        Self::with_map(|map| {
            for (point, tw) in &map.entries {
                if !tw.tripped {
                    log::warn!(target: "tripwire", "untripped point={:?}", point);
                    untripped = true;
                }
            }
        });

        match reset_mode {
            ResetMode::Reset => self.reset(),
            ResetMode::Retain => {}
        }

        if untripped {
            return Err(EndError::UntrippedError);
        }
        Ok(())
    }

    /// Unset all the tripwires. You should usually call `end` instead.
    pub fn reset(&self) {
        Self::with_map(|map| map.entries.clear());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for Zig's `anyerror` in these tests.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum AnyError {
        OutOfMemory,
    }

    #[test]
    fn basic() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum P {
            Read,
            #[allow(dead_code)]
            Write,
        }
        let io: Module<P, AnyError> = Module::new();

        // Reset should work
        io.end(ResetMode::Reset).unwrap();

        // By default, its pass-through
        io.check(P::Read).unwrap();

        // Always trip
        io.error_always(P::Read, AnyError::OutOfMemory);
        assert_eq!(Err(AnyError::OutOfMemory), io.check(P::Read));
        // Happens again
        assert_eq!(Err(AnyError::OutOfMemory), io.check(P::Read));
        io.end(ResetMode::Reset).unwrap();
    }

    #[test]
    fn module_as_error_set() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[allow(dead_code)]
        enum P {
            Read,
            Write,
        }
        // The error type of a function `fn func() -> Result<(), FooBar>`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[allow(dead_code)]
        enum FooBar {
            Foo,
            Bar,
        }
        #[allow(dead_code)]
        fn func() -> Result<(), FooBar> {
            Err(FooBar::Foo)
        }
        let io: Module<P, FooBar> = Module::new();
        io.end(ResetMode::Reset).unwrap();
    }

    #[test]
    fn error_after() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[allow(dead_code)]
        enum P {
            Read,
            Write,
        }
        let io: Module<P, AnyError> = Module::new();
        // Trip after 2 calls (on the 3rd call)
        io.error_after(P::Read, AnyError::OutOfMemory, 2);

        // First two calls succeed
        io.check(P::Read).unwrap();
        io.check(P::Read).unwrap();

        // Third call and on trips
        assert_eq!(Err(AnyError::OutOfMemory), io.check(P::Read));
        assert_eq!(Err(AnyError::OutOfMemory), io.check(P::Read));

        io.end(ResetMode::Reset).unwrap();
    }

    #[test]
    fn error_after_untripped_error_if_min_not_reached() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum P {
            Read,
        }
        let io: Module<P, AnyError> = Module::new();
        io.error_after(P::Read, AnyError::OutOfMemory, 2);
        // Only call once, not enough to trip
        io.check(P::Read).unwrap();
        // end should fail because tripwire was set but never tripped
        assert_eq!(Err(EndError::UntrippedError), io.end(ResetMode::Reset));
    }

    #[test]
    fn check_constrained_converts() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum P {
            A,
        }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Narrow;
        impl TryFrom<AnyError> for Narrow {
            type Error = ();
            fn try_from(_: AnyError) -> Result<Self, ()> {
                Ok(Narrow)
            }
        }
        static IO: Module<P, AnyError> = Module::new();
        IO.error_always(P::A, AnyError::OutOfMemory);
        assert_eq!(Err(Narrow), IO.check_constrained::<Narrow>(P::A));
        IO.end(ResetMode::Reset).unwrap();
    }
}
