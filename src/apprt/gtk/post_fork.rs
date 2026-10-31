//! Port of `src/apprt/gtk/post_fork.zig`.
//!
//! STATUS: ported
//!
//! [`post_fork`] reads `cmd.pid` and `cmd.rt_post_fork_info` like Zig's
//! `postFork`; the logic is in [`post_fork_info`] so tests can run it
//! without a `Command`.
//! `Application.default()` is the process's default `GApplication`, which is
//! what `gio::Application::default()` returns.
#![cfg(feature = "gtk")]

use std::time::{Duration, Instant};

use gio::prelude::*;

use super::cgroup;
use super::pre_exec::in_cgroup;
use crate::command::{Command, Pid, PostForkError};
use crate::config::config::{Config, GtkSingleInstance, LinuxCgroup};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostForkInfo {
    pub gtk_single_instance: GtkSingleInstance,
    pub linux_cgroup: LinuxCgroup,
    pub linux_cgroup_hard_fail: bool,
    pub linux_cgroup_memory_limit: Option<u64>,
    pub linux_cgroup_processes_limit: Option<u64>,
}

impl PostForkInfo {
    pub fn init(cfg: &Config) -> PostForkInfo {
        PostForkInfo {
            gtk_single_instance: cfg.gtk_single_instance,
            linux_cgroup: cfg.linux_cgroup,
            linux_cgroup_hard_fail: cfg.linux_cgroup_hard_fail,
            linux_cgroup_memory_limit: cfg.linux_cgroup_memory_limit,
            linux_cgroup_processes_limit: cfg.linux_cgroup_processes_limit,
        }
    }
}

/// If we are configured to do so, tell `systemd` to move the new child PID into
/// a transient `systemd` scope with the configured resource limits.
///
/// If we are configured to hard fail, log an error message and return an error
/// code if we don't detect the move in time.
pub fn post_fork(cmd: &mut Command) -> Result<(), PostForkError> {
    post_fork_info(cmd.pid, &cmd.rt_post_fork_info)
}

/// [`post_fork`] on the given child PID and info (Zig: `cmd.pid` and
/// `cmd.rt_post_fork_info`).
pub fn post_fork_info(pid: Option<Pid>, info: &PostForkInfo) -> Result<(), PostForkError> {
    match info.linux_cgroup {
        LinuxCgroup::Always => {}
        LinuxCgroup::Never => return Ok(()),
        LinuxCgroup::SingleInstance => match info.gtk_single_instance {
            GtkSingleInstance::True => {}
            GtkSingleInstance::False => return Ok(()),
            GtkSingleInstance::Detect => {
                log::error!(
                    target: "gtk_post_fork",
                    "gtk-single-instance is set to detect which should be impossible!"
                );
                return Err(PostForkError::PostForkError);
            }
        },
    }

    let Some(pid) = pid else {
        log::error!(target: "gtk_post_fork", "PID of child not known!");
        return Err(PostForkError::PostForkError);
    };
    let pid = pid as u32;

    let expected_cgroup = cgroup::fmt_scope(pid);

    log::debug!(
        target: "gtk_post_fork",
        "beginning transition to transient systemd scope {expected_cgroup}"
    );

    let dbus = gio::Application::default().and_then(|app| app.dbus_connection());
    let Some(dbus) = dbus else {
        if info.linux_cgroup_hard_fail {
            log::error!(
                target: "gtk_post_fork",
                "dbus connection required for cgroup isolation, exiting"
            );
            return Err(PostForkError::PostForkError);
        }
        return Ok(());
    };

    if let Err(err) = cgroup::create_scope(
        &dbus,
        pid,
        cgroup::Options {
            memory_high: info.linux_cgroup_memory_limit,
            tasks_max: info.linux_cgroup_processes_limit,
        },
    ) {
        if info.linux_cgroup_hard_fail {
            log::error!(
                target: "gtk_post_fork",
                "unable to create transient systemd scope {expected_cgroup}: {err}"
            );
            return Err(PostForkError::PostForkError);
        }
        log::warn!(
            target: "gtk_post_fork",
            "unable to create transient systemd scope {expected_cgroup}: {err}"
        );
        return Ok(());
    }

    let start = Instant::now();

    loop {
        if start.elapsed().as_millis() > 250 {
            // (Zig reads `cmd.rt_pre_exec_info.linux_cgroup_hard_fail` here,
            // which holds the same config value.)
            if info.linux_cgroup_hard_fail {
                log::error!(
                    target: "gtk_post_fork",
                    "transition to new transient systemd scope {expected_cgroup} took too long"
                );
                return Err(PostForkError::PostForkError);
            }
            log::warn!(
                target: "gtk_post_fork",
                "transition to transient systemd scope {expected_cgroup} took too long"
            );
            break;
        }

        if in_cgroup(pid, &expected_cgroup) {
            log::debug!(
                target: "gtk_post_fork",
                "transition to transient systemd scope {expected_cgroup} complete"
            );
            break;
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(linux_cgroup: LinuxCgroup, single: GtkSingleInstance, hard_fail: bool) -> PostForkInfo {
        PostForkInfo {
            gtk_single_instance: single,
            linux_cgroup,
            linux_cgroup_hard_fail: hard_fail,
            linux_cgroup_memory_limit: None,
            linux_cgroup_processes_limit: None,
        }
    }

    #[test]
    fn disabled_or_misconfigured() {
        assert!(post_fork_info(None, &info(LinuxCgroup::Never, GtkSingleInstance::True, true)).is_ok());
        assert!(post_fork_info(
            None,
            &info(LinuxCgroup::SingleInstance, GtkSingleInstance::False, true)
        )
        .is_ok());
        assert!(post_fork_info(
            Some(1),
            &info(LinuxCgroup::SingleInstance, GtkSingleInstance::Detect, false)
        )
        .is_err());
        // The PID must be known.
        assert!(post_fork_info(None, &info(LinuxCgroup::Always, GtkSingleInstance::True, false)).is_err());
    }

    #[test]
    fn no_application_dbus_connection() {
        // There is no default GApplication in the test process.
        let pid = std::process::id() as Pid;
        assert!(post_fork_info(Some(pid), &info(LinuxCgroup::Always, GtkSingleInstance::True, false)).is_ok());
        assert!(post_fork_info(Some(pid), &info(LinuxCgroup::Always, GtkSingleInstance::True, true)).is_err());
    }

    #[test]
    fn init_from_config() {
        let cfg = Config::default();
        let i = PostForkInfo::init(&cfg);
        assert_eq!(i.linux_cgroup, cfg.linux_cgroup);
        assert_eq!(i.linux_cgroup_memory_limit, cfg.linux_cgroup_memory_limit);
        assert_eq!(i.linux_cgroup_processes_limit, cfg.linux_cgroup_processes_limit);
    }
}
