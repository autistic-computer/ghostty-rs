//! Port of `src/apprt/gtk/pre_exec.zig`.
//!
//! STATUS: ported
//!
//! [`pre_exec`] reads `cmd.rt_pre_exec_info` like Zig's `preExec`; the
//! logic is in [`pre_exec_info`] so tests can run it without a `Command`.
#![cfg(feature = "gtk")]

use std::time::{Duration, Instant};

use super::cgroup;
use crate::command::Command;
use crate::config::config::{Config, GtkSingleInstance, LinuxCgroup};
use crate::os::cgroup as internal_cgroup;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreExecInfo {
    pub gtk_single_instance: GtkSingleInstance,
    pub linux_cgroup: LinuxCgroup,
    pub linux_cgroup_hard_fail: bool,
}

impl PreExecInfo {
    pub fn init(cfg: &Config) -> PreExecInfo {
        PreExecInfo {
            gtk_single_instance: cfg.gtk_single_instance,
            linux_cgroup: cfg.linux_cgroup,
            linux_cgroup_hard_fail: cfg.linux_cgroup_hard_fail,
        }
    }
}

/// Whether the current cgroup of `pid` is `expected` (the last path
/// component of `/proc/<pid>/cgroup`). Shared with `post_fork`.
pub(crate) fn in_cgroup(pid: u32, expected: &str) -> bool {
    let mut current_cgroup_buf = [0u8; 4096];

    let Some(current_cgroup_raw) = internal_cgroup::current(&mut current_cgroup_buf, pid) else {
        return false;
    };

    let Some(index) = current_cgroup_raw.iter().rposition(|&c| c == b'/') else {
        return false;
    };
    let current_cgroup = &current_cgroup_raw[index + 1..];

    current_cgroup == expected.as_bytes()
}

/// If we are expecting to be moved to a transient systemd scope, wait to see if
/// that happens by checking for the correct name of the current cgroup. Wait at
/// most 250ms so that we don't overly delay the soft-fail scenario.
///
/// If we are configured to hard fail, log an error message and return an error
/// code if we don't detect the move in time.
pub fn pre_exec(cmd: &mut Command) -> Option<u8> {
    pre_exec_info(&cmd.rt_pre_exec_info)
}

/// [`pre_exec`] on the given info (Zig: `cmd.rt_pre_exec_info`).
pub fn pre_exec_info(info: &PreExecInfo) -> Option<u8> {
    match info.linux_cgroup {
        LinuxCgroup::Always => {}
        LinuxCgroup::Never => return None,
        LinuxCgroup::SingleInstance => match info.gtk_single_instance {
            GtkSingleInstance::True => {}
            GtkSingleInstance::False => return None,
            GtkSingleInstance::Detect => {
                log::error!(target: "gtk_pre_exec", "gtk-single-instance is set to detect");
                return Some(127);
            }
        },
    }

    let pid = std::process::id();

    let expected_cgroup = cgroup::fmt_scope(pid);

    let start = Instant::now();

    loop {
        if start.elapsed().as_millis() > 250 {
            if info.linux_cgroup_hard_fail {
                log::error!(
                    target: "gtk_pre_exec",
                    "transition to new transient systemd scope took too long"
                );
                return Some(127);
            }
            break;
        }

        if in_cgroup(pid, &expected_cgroup) {
            return None;
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(linux_cgroup: LinuxCgroup, single: GtkSingleInstance, hard_fail: bool) -> PreExecInfo {
        PreExecInfo {
            gtk_single_instance: single,
            linux_cgroup,
            linux_cgroup_hard_fail: hard_fail,
        }
    }

    #[test]
    fn disabled_returns_immediately() {
        assert_eq!(pre_exec_info(&info(LinuxCgroup::Never, GtkSingleInstance::True, true)), None);
        assert_eq!(
            pre_exec_info(&info(LinuxCgroup::SingleInstance, GtkSingleInstance::False, true)),
            None
        );
        assert_eq!(
            pre_exec_info(&info(LinuxCgroup::SingleInstance, GtkSingleInstance::Detect, false)),
            Some(127)
        );
    }

    #[test]
    fn times_out_when_not_moved() {
        // Nothing moves the test process into the transient scope.
        let start = Instant::now();
        assert_eq!(pre_exec_info(&info(LinuxCgroup::Always, GtkSingleInstance::True, false)), None);
        assert_eq!(pre_exec_info(&info(LinuxCgroup::Always, GtkSingleInstance::True, true)), Some(127));
        assert!(start.elapsed() >= Duration::from_millis(500));
    }

    #[test]
    fn init_from_config() {
        let cfg = Config::default();
        let i = PreExecInfo::init(&cfg);
        assert_eq!(i.linux_cgroup, cfg.linux_cgroup);
        assert_eq!(i.gtk_single_instance, cfg.gtk_single_instance);
        assert_eq!(i.linux_cgroup_hard_fail, cfg.linux_cgroup_hard_fail);
    }
}
