//! Port of `src/benchmark/Benchmark.zig`.
//!
//! STATUS: ported
//!
//! A single benchmark case.
//!
//! The Zig `ptr: *anyopaque` is a `&mut dyn Any` here so the vtable
//! functions can recover their concrete type safely with [`cast`].
//! Deviation: the macOS os_signpost interval around the run is not
//! emitted (the `macos` package is not ported); it has no effect on
//! the measured results.

use std::any::Any;
use std::fmt;
use std::time::Instant;

pub struct Benchmark<'a> {
    pub ptr: &'a mut dyn Any,
    pub vtable: VTable,
}

impl<'a> Benchmark<'a> {
    /// Create a new benchmark from a pointer and a vtable.
    ///
    /// This usually is only called by benchmark implementations, not
    /// benchmark users.
    pub fn init<T: Any>(pointer: &'a mut T, vtable: VTable) -> Benchmark<'a> {
        Benchmark {
            ptr: pointer,
            vtable,
        }
    }

    /// Run the benchmark.
    pub fn run(&mut self, mode: RunMode) -> Result<RunResult, Error> {
        // Run our setup function if it exists. We do this first because
        // we don't want this part of our benchmark and we want to fail fast.
        if let Some(func) = self.vtable.setup_fn {
            func(self.ptr)?;
        }

        let result = self.run_steps(mode);

        if let Some(func) = self.vtable.teardown_fn {
            func(self.ptr);
        }

        result
    }

    fn run_steps(&mut self, mode: RunMode) -> Result<RunResult, Error> {
        // Our result accumulator. This will be returned at the end of the run.
        let mut result = RunResult::default();

        let start = Instant::now();
        loop {
            // Run our step function. If it fails, we return the error.
            (self.vtable.step_fn)(self.ptr)?;
            result.iterations += 1;

            // Get our current monotonic time and check our exit conditions.
            let elapsed = start.elapsed().as_nanos();
            let exit = match mode {
                RunMode::Once => true,
                RunMode::Duration(ns) => elapsed >= ns as u128,
            };

            if exit {
                result.duration = elapsed.min(u64::MAX as u128) as u64;
                return Ok(result);
            }
        }
    }
}

/// Recover the concrete implementation from the vtable pointer (Zig:
/// `@ptrCast(@alignCast(ptr))`).
#[inline(always)]
pub fn cast<T: Any>(ptr: &mut dyn Any) -> &mut T {
    ptr.downcast_mut::<T>()
        .expect("benchmark vtable called with a different implementation type")
}

/// The type of benchmark run. This is used to determine how the benchmark
/// is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Run the benchmark exactly once.
    Once,

    /// Run the benchmark for a fixed duration in nanoseconds. This
    /// will not interrupt a running step so if the granularity of the
    /// duration is too low, benchmark results may be inaccurate.
    Duration(u64),
}

/// The result of a benchmark run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunResult {
    /// The total iterations that step was executed. For "once" run
    /// modes this will always be 1.
    pub iterations: u32,

    /// The total time taken for the run. For "duration" run modes
    /// this will be relatively close to the requested duration.
    /// The units are nanoseconds.
    pub duration: u64,
}

/// The possible errors that can occur during various stages of the
/// benchmark. Right now its just "failure" which ends the benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BenchmarkFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BenchmarkFailed => f.write_str("BenchmarkFailed"),
        }
    }
}

impl std::error::Error for Error {}

/// A step/setup function (Zig: `*const fn (ptr: *anyopaque) Error!void`).
pub type StepFn = fn(&mut dyn Any) -> Result<(), Error>;

/// A teardown function (Zig: `*const fn (ptr: *anyopaque) void`).
pub type TeardownFn = fn(&mut dyn Any);

/// The vtable that must be provided to invoke the real implementation.
#[derive(Clone, Copy)]
pub struct VTable {
    /// A single step to execute the benchmark. This should do the work
    /// that is under test. This may be called multiple times if we're
    /// testing throughput.
    pub step_fn: StepFn,

    /// Setup and teardown functions. These are called once before
    /// the first step and once after the last step. They are not part
    /// of the benchmark results (unless you're benchmarking the full
    /// binary).
    pub setup_fn: Option<StepFn>,
    pub teardown_fn: Option<TeardownFn>,
}

impl VTable {
    /// A vtable with only a step function.
    pub fn new(step_fn: StepFn) -> VTable {
        VTable {
            step_fn,
            setup_fn: None,
            teardown_fn: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Simple {
        setup_i: usize,
        step_i: usize,
    }

    impl Simple {
        fn benchmark(&mut self) -> Benchmark<'_> {
            Benchmark::init(
                self,
                VTable {
                    step_fn: Simple::step,
                    setup_fn: Some(Simple::setup),
                    teardown_fn: None,
                },
            )
        }

        fn setup(ptr: &mut dyn Any) -> Result<(), Error> {
            let self_: &mut Simple = cast(ptr);
            self_.setup_i += 1;
            Ok(())
        }

        fn step(ptr: &mut dyn Any) -> Result<(), Error> {
            let self_: &mut Simple = cast(ptr);
            self_.step_i += 1;
            Ok(())
        }
    }

    #[test]
    fn benchmark() {
        let mut s = Simple::default();
        let mut b = s.benchmark();
        let result = b.run(RunMode::Once).unwrap();
        drop(b);
        assert_eq!(1, s.setup_i);
        assert_eq!(1, s.step_i);
        assert_eq!(1, result.iterations);
        assert!(result.duration > 0);
    }
}
