//! Port of libxev `src/watcher/*.zig`: the high-level watcher types.

#[path = "async.rs"]
pub mod r#async;
#[path = "process.rs"]
pub mod process;
#[path = "stream.rs"]
pub mod stream;
#[path = "timer.rs"]
pub mod timer;
