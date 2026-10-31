//! Port of `src/apprt/none.zig`.
//!
//! STATUS: ported

use crate::apprt::ipc;

pub use crate::os::resources_dir;

#[derive(Debug, Default)]
pub struct App {}

impl App {
    /// Always return false as there is no apprt to communicate with.
    ///
    /// Zig takes the action key at comptime plus its value; in Rust the
    /// [`ipc::Action`] enum carries both.
    pub fn perform_ipc(
        _target: &ipc::Target<'_>,
        _action: &ipc::Action<'_>,
    ) -> Result<bool, ipc::Errors> {
        Ok(false)
    }
}

#[derive(Debug, Default)]
pub struct Surface {}
