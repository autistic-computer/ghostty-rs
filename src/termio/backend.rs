//! Port of `src/termio/backend.zig`.
//!
//! STATUS: ported
//!
//! Dispatches to the backend implementations. Only `exec` exists.
//! Deviations: allocator parameters are dropped, `getProcessInfo`'s
//! comptime return type is [`ProcessInfoValue`], the termio is passed as a
//! raw pointer to `threadEnter` (it is shared with the backend's read
//! thread, like in Zig), and `childExitedAbnormally` is not ported: it is
//! dead code in Zig (lazily never analyzed) because `Exec` has no such
//! method at this revision (abnormal exits are handled by `Surface`).

use crate::pty::{ProcessInfo, ProcessInfoValue};
use crate::terminal::Terminal;
use crate::renderer::{GridSize, ScreenSize};
use crate::termio::exec::{
    Config as ExecConfig, Error, ThreadData as ExecThreadData,
};
use crate::termio::termio::{DerivedConfig as TermioDerivedConfig, Termio, ThreadData as TermioThreadData};
use crate::termio::Exec;

/// The preallocation size for the write request pool. This should be big
/// enough to satisfy most write requests. It must be a power of 2.
#[allow(dead_code)]
const WRITE_REQ_PREALLOC: usize = 2usize.pow(5);

/// The kinds of backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Exec,
}

/// Configuration for the various backend types.
#[derive(Debug, Clone)]
pub enum Config {
    /// Exec uses posix exec to run a command with a pty.
    Exec(ExecConfig),
}

/// Backend implementations. A backend is responsible for owning the pty
/// behavior and providing read/write capabilities.
#[derive(Debug)]
pub enum Backend {
    Exec(Exec),
}

impl Backend {
    pub fn kind(&self) -> Kind {
        match self {
            Backend::Exec(_) => Kind::Exec,
        }
    }

    pub fn deinit(&mut self) {
        match self {
            Backend::Exec(exec) => exec.deinit(),
        }
    }

    pub fn init_terminal(&mut self, t: &mut Terminal) {
        match self {
            Backend::Exec(exec) => exec.init_terminal(t),
        }
    }

    pub fn thread_enter(
        &mut self,
        io: *mut Termio,
        td: &mut TermioThreadData,
    ) -> Result<(), Error> {
        match self {
            Backend::Exec(exec) => exec.thread_enter(io, td),
        }
    }

    pub fn thread_exit(&mut self, td: &mut TermioThreadData) {
        match self {
            Backend::Exec(exec) => exec.thread_exit(td),
        }
    }

    pub fn focus_gained(&mut self, td: &mut TermioThreadData, focused: bool) -> Result<(), Error> {
        match self {
            Backend::Exec(exec) => exec.focus_gained(td, focused),
        }
    }

    pub fn resize(&mut self, grid_size: GridSize, screen_size: ScreenSize) -> Result<(), Error> {
        match self {
            Backend::Exec(exec) => exec.resize(grid_size, screen_size),
        }
    }

    pub fn queue_write(
        &mut self,
        td: &mut TermioThreadData,
        data: &[u8],
        linefeed: bool,
    ) -> Result<(), Error> {
        match self {
            Backend::Exec(exec) => exec.queue_write(td, data, linefeed),
        }
    }

    /// Get information about the process(es) attached to the backend. Returns
    /// `null` if there was an error getting the information or the information
    /// is not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        match self {
            Backend::Exec(exec) => exec.get_process_info(info),
        }
    }
}

/// Termio thread data. See termio.ThreadData for docs.
#[derive(Debug)]
pub enum ThreadData {
    Exec(ExecThreadData),
}

impl ThreadData {
    pub fn deinit(&mut self) {
        match self {
            ThreadData::Exec(exec) => exec.deinit(),
        }
    }

    pub fn change_config(&mut self, config: &mut TermioDerivedConfig) {
        let _ = self;
        let _ = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch() {
        let exec = Exec::init(ExecConfig {
            env: Default::default(),
            resources_dir: None,
            term: "xterm-ghostty".into(),
            ..ExecConfig::default()
        })
        .unwrap();
        let mut b = Backend::Exec(exec);
        assert_eq!(Kind::Exec, b.kind());
        // Not started: there is no pty to query.
        assert!(b.get_process_info(ProcessInfo::ForegroundPid).is_none());
        b.deinit();
        assert!(WRITE_REQ_PREALLOC.is_power_of_two());
    }
}
