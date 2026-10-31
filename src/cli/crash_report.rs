//! Port of `src/cli/crash_report.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use crate::config::config::Duration;
use crate::crash;
use crate::global;
use crate::os::EnvironMap;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {}
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"crash-report"`).
pub const HELP: &str = "The `crash-report` command is used to inspect and send crash reports.

When executed without any arguments, this will list existing crash reports.

This command currently only supports listing crash reports. Viewing
and sending crash reports is unimplemented and will be added in the future.";

/// The `crash-report` command is used to inspect and send crash reports.
/// See [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let stdout_file = io::stdout();
    let tty = stdout_file.is_terminal();
    let mut stdout = BufWriter::with_capacity(1024, stdout_file.lock());

    let environ_map = global::environ_map();
    let result = run_inner(&environ_map, tty, &mut stdout);
    let _ = stdout.flush();
    result
}

fn run_inner<W: Write + ?Sized>(environ_map: &EnvironMap, tty: bool, stdout: &mut W) -> RunResult {
    let crash_dir = crash::default_dir(environ_map)?;
    let mut reports: Vec<crash::Report> = Vec::new();

    let mut it = crash_dir.iterator();
    while let Some(report) = it.next()? {
        reports.push(report);
    }

    // If we have no reports, then we're done. If we have a tty then we
    // print a message, otherwise we do nothing.
    if reports.is_empty() {
        if tty {
            stdout.write_all("No crash reports! 👻\n".as_bytes())?;
        }
        return Ok(0);
    }

    reports.sort_by(|lhs, rhs| rhs.mtime.cmp(&lhs.mtime));

    for report in &reports {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        let diff = now - report.mtime;
        let since = if diff <= 0 {
            "now".to_owned()
        } else {
            let d = Duration {
                duration: u64::try_from(diff).unwrap_or(u64::MAX),
            };
            format!("{} ago", d.round(NS_PER_S))
        };
        writeln!(stdout, "{} ({})", report.name.to_string_lossy(), since)?;
    }

    Ok(0)
}

const NS_PER_S: u64 = 1_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_reports_newest_first() {
        let base = std::env::temp_dir().join(format!("ghostty-crash-report-{}", std::process::id()));
        let dir = base.join("ghostty/crash");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.ghosttycrash"), b"").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("new.ghosttycrash"), b"").unwrap();

        let mut env = EnvironMap::new();
        env.insert("XDG_STATE_HOME".to_owned(), base.to_string_lossy().into_owned());
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(0, run_inner(&env, false, &mut out).unwrap());
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(2, lines.len());
        assert!(lines[0].starts_with("new.ghosttycrash ("), "{}", out);
        assert!(lines[1].starts_with("old.ghosttycrash ("), "{}", out);

        let _ = std::fs::remove_dir_all(&base);
    }
}
