//! Port of `src/cli/ssh.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{self, BufWriter, Write};
use std::process::{Command, ExitStatus, Stdio};

use super::args::{self as cli_args, cli_struct, ArgIterator, Error, ParseHooks};
use super::diagnostics::DiagnosticList;
use super::ghostty::{global_args, Action, RunResult};
use super::ssh_cache::disk_cache;
use super::ssh_cache::DiskCache;
use crate::global;
use crate::os as internal_os;
use crate::terminfo as terminfopkg;

const LOG: &str = "ssh";

const USAGE: &str = "Usage: ghostty +ssh [flags] [--] <ssh args...>

Flags:
  --forward-env[=bool]  Send Ghostty env vars to the remote. Default: true.
  --terminfo[=bool]     Install Ghostty terminfo on first connect. Default: true.
  --cache[=bool]        Use the terminfo install cache. Default: true.
  --ssh=<path>          Path to the ssh binary. Default: first `ssh` on PATH.
  --verbose             Print +ssh status lines to stderr.
  --help                Show full help.

ssh flags and the destination go after +ssh's own flags (or after `--`).
";

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Send `COLORTERM`, `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION` to
        /// the remote host. Maps to the `ssh-env` shell integration feature.
        pub forward_env as "forward-env": bool = true,

        /// Install Ghostty's terminfo on the remote host and use
        /// `xterm-ghostty` as `TERM` when it's available. Maps to the
        /// `ssh-terminfo` shell integration feature.
        pub terminfo: bool = true,

        /// When false, both cache read and write are bypassed.
        pub cache: bool = true,

        /// The wrapped `ssh` binary.
        /// `/`-containing values are treated as paths; otherwise resolved via PATH.
        pub ssh: String = "ssh".to_owned(),

        /// When true, print verbose output to stderr.
        pub verbose: bool = false,
    }
    private {
        /// Arguments passed through to `ssh` verbatim. Populated by
        /// `parse_manually_hook` when we reach the first non-flag argument (or
        /// an explicit `--`).
        pub _ssh_args: Vec<String> = Vec::new(),

        /// Enables arg parsing diagnostics so unknown flags become
        /// diagnostics rather than fatal errors.
        pub _diagnostics: DiagnosticList = DiagnosticList::default(),
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticList> {
        Some(&mut self._diagnostics)
    }

    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }

    /// Manual parse hook. For each argument:
    ///   - If it's a literal `--`, consume everything after it as ssh
    ///     args and stop parsing.
    ///   - If it doesn't start with `--`, this is the start of the ssh
    ///     argv. Consume this arg and everything after as ssh args and
    ///     stop parsing.
    ///   - Otherwise (a `--foo` arg), return true so the generic parser
    ///     handles it as one of our own flags.
    fn parse_manually_hook<I: ArgIterator + ?Sized>(&mut self, arg: &str, iter: &mut I) -> Result<bool, Error> {
        if arg == "--" {
            while let Some(rest) = iter.next() {
                self._ssh_args.push(rest);
            }
            return Ok(false);
        }

        if !arg.starts_with("--") {
            self._ssh_args.push(arg.to_owned());
            while let Some(rest) = iter.next() {
                self._ssh_args.push(rest);
            }
            return Ok(false);
        }

        Ok(true)
    }
}

/// The help text of this action (Zig `help_strings.Action.ssh`).
pub const HELP: &str = include_str!("ssh_help.txt");

/// Wrap `ssh` to automatically configure Ghostty terminal integration on
/// remote hosts. See [`HELP`] for the full documentation.
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = cli_args::args_iterator(global_args());
        cli_args::parse(&mut opts, &mut iter)?;
    }

    let mut stderr = BufWriter::with_capacity(1024, io::stderr().lock());

    // Any diagnostic from the arg parser is an unknown flag or bad
    // value. Reject loudly — silently forwarding `--typo` to ssh would
    // produce confusing downstream errors.
    if !opts._diagnostics.empty() {
        for diag in opts._diagnostics.items() {
            if !diag.key.is_empty() {
                let _ = write!(stderr, "Error: unknown flag `--{}`.\n", diag.key);
            } else {
                let _ = write!(stderr, "Error: {}\n", diag.message);
            }
        }
        let _ = write!(stderr, "\n{}", USAGE);
        let _ = stderr.flush();
        return Ok(2);
    }

    let result = run_inner(&opts, &mut stderr);

    let _ = stderr.flush();
    result
}

struct ToCache {
    cache: DiskCache,
    dest: String,
}

struct Session {
    term: &'static str,
    to_cache: Option<ToCache>,
}

fn run_inner<W: Write + ?Sized>(opts: &Options, stderr: &mut W) -> RunResult {
    if opts._ssh_args.is_empty() {
        write!(stderr, "Error: no ssh arguments provided.\n\n{}", USAGE)?;
        return Ok(2);
    }

    let session: Session = 'session: {
        if !opts.terminfo {
            break 'session Session {
                term: "xterm-256color",
                to_cache: None,
            };
        }

        let Some(dest) = resolve_destination(&opts.ssh, &opts._ssh_args) else {
            warn_print(
                stderr,
                format_args!("could not resolve ssh destination; skipping terminfo install"),
            );
            break 'session Session {
                term: "xterm-256color",
                to_cache: None,
            };
        };

        let cache: Option<DiskCache> = if opts.cache {
            match DiskCache::default_path("ghostty") {
                Ok(path) => Some(DiskCache { path }),
                Err(err) => {
                    warn_print(
                        stderr,
                        format_args!("ghostty terminfo cache unavailable: {}", err),
                    );
                    break 'session Session {
                        term: "xterm-256color",
                        to_cache: None,
                    };
                }
            }
        } else {
            None
        };

        if let Some(c) = &cache {
            let cached = match c.contains(&dest, terminfopkg::version()) {
                Ok(cached) => cached,
                Err(err) => {
                    if disk_cache::is_failure(&err) {
                        warn_print(
                            stderr,
                            format_args!("unable to read the cache '{}': {}", c.path.display(), err),
                        );
                    }
                    false
                }
            };

            if cached {
                verbose_print(
                    opts,
                    stderr,
                    format_args!("dest: {} (cached, skipping install)", dest),
                );
                break 'session Session {
                    term: "xterm-ghostty",
                    to_cache: None,
                };
            } else {
                verbose_print(
                    opts,
                    stderr,
                    format_args!("dest: {} (not cached, will install)", dest),
                );
            }
        } else {
            verbose_print(
                opts,
                stderr,
                format_args!("dest: {} (cache disabled, will install)", dest),
            );
        }

        let _ = write!(stderr, "Setting up xterm-ghostty terminfo on {}...\n", dest);
        let _ = stderr.flush();

        if let Err(err) = install_remote_terminfo(opts, stderr) {
            warn_print(stderr, format_args!("failed to install terminfo: {}", err));
            break 'session Session {
                term: "xterm-256color",
                to_cache: None,
            };
        }
        Session {
            term: "xterm-ghostty",
            to_cache: cache.map(|cache| ToCache { cache, dest }),
        }
    };

    // Set TERM in ssh's own environment instead of passing
    // `-o SetEnv=TERM=...`. ssh only uses the first SetEnv it finds, so a
    // SetEnv on the command line would replace every SetEnv in the user's
    // ssh config. ssh sends TERM on its own when it opens a remote
    // terminal, so the server doesn't need `AcceptEnv TERM` either.
    //
    // We do this whenever either feature is on, not only --forward-env.
    // Otherwise a failed install with --forward-env=false would leave the
    // inherited TERM=xterm-ghostty on a host that can't use it.
    let mut environ_map = if opts.forward_env || opts.terminfo {
        Some(global::environ_map())
    } else {
        None
    };
    if let Some(m) = environ_map.as_mut() {
        m.insert("TERM".to_owned(), session.term.to_owned());
    }

    // Build the full argv: [ssh, ...our opts, ...user args]
    let env_opts: &[&str] = if opts.forward_env {
        &[
            "-o",
            "SendEnv=COLORTERM",
            "-o",
            "SendEnv=TERM_PROGRAM",
            "-o",
            "SendEnv=TERM_PROGRAM_VERSION",
        ]
    } else {
        &[]
    };
    let mut argv: Vec<String> = vec![opts.ssh.clone()];
    argv.extend(env_opts.iter().map(|s| s.to_string()));
    argv.extend(opts._ssh_args.iter().cloned());
    verbose_print(opts, stderr, format_args!("exec: {}", Joined { items: &argv }));

    let exit_code = match child_exec(&argv, environ_map.as_ref()) {
        Ok(code) => code,
        Err(err) => {
            write!(stderr, "Error: failed to run {}: {}\n", argv[0], err)?;
            return Ok(1);
        }
    };
    verbose_print(opts, stderr, format_args!("exit: {}", exit_code));

    // Attempt to cache (if needed) on a successful ssh execution.
    if exit_code == 0 {
        if let Some(entry) = &session.to_cache {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            match entry.cache.add(&entry.dest, terminfopkg::version(), now) {
                Ok(()) => {
                    verbose_print(opts, stderr, format_args!("cache: wrote {}", entry.dest));
                }
                Err(err) => {
                    if disk_cache::is_failure(&err) {
                        warn_print(
                            stderr,
                            format_args!(
                                "unable to add '{}' to the cache '{}': {}",
                                entry.dest,
                                entry.cache.path.display(),
                                err
                            ),
                        );
                    } else {
                        verbose_print(
                            opts,
                            stderr,
                            format_args!("cache: skipped {}: {}", entry.dest, err),
                        );
                    }
                }
            }
        }
    }

    Ok(exit_code)
}

/// Log to `ssh` and, if `--verbose`, also print to stderr.
fn verbose_print<W: Write + ?Sized>(opts: &Options, stderr: &mut W, args: fmt::Arguments<'_>) {
    log::debug!(target: LOG, "{}", args);
    if !opts.verbose {
        return;
    }
    if write!(stderr, "+ssh: {}\n", args).is_err() {
        return;
    }
    let _ = stderr.flush();
}

/// Log a warning and also print a `Warning: <msg>` line to stderr.
fn warn_print<W: Write + ?Sized>(stderr: &mut W, args: fmt::Arguments<'_>) {
    log::warn!(target: LOG, "{}", args);
    if write!(stderr, "Warning: {}\n", args).is_err() {
        return;
    }
    let _ = stderr.flush();
}

/// Space-joined items, formattable with `{}`.
struct Joined<'a, S: AsRef<str>> {
    items: &'a [S],
}

impl<S: AsRef<str>> fmt::Display for Joined<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, a) in self.items.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            f.write_str(a.as_ref())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChildFailed;

impl fmt::Display for ChildFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChildFailed")
    }
}

fn check_exit(term: ExitStatus, label: &str) -> Result<(), ChildFailed> {
    match term.code() {
        Some(rc) => {
            if rc != 0 {
                log::warn!(target: LOG, "{} exited with non-zero status: {}", label, rc);
                return Err(ChildFailed);
            }
            Ok(())
        }
        None => {
            log::warn!(target: LOG, "{} terminated abnormally: {:?}", label, term);
            Err(ChildFailed)
        }
    }
}

/// Run `ssh -G <args>` and parse the output for `user` and `hostname`.
/// Returns the resolved `user@hostname`, or None if the destination
/// could not be resolved.
fn resolve_destination(ssh: &str, args: &[String]) -> Option<String> {
    let result = match Command::new(ssh)
        .arg("-G")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(result) => result,
        Err(err) => {
            log::warn!(target: LOG, "ssh -G spawn failed: {}", err);
            return None;
        }
    };
    check_exit(result.status, "ssh -G").ok()?;
    parse_destination(&String::from_utf8_lossy(&result.stdout))
}

/// Parse `ssh -G` output for `user`, `hostname` and `port` and return the
/// formatted `user@hostname`, or `user@hostname:port` when the port isn't
/// the default 22 (`user@[hostname]:port` for an IPv6 address), so that
/// different sshd instances behind one address are cached separately.
/// Returns None if user or hostname is missing.
fn parse_destination(stdout: &str) -> Option<String> {
    let mut user = "";
    let mut host = "";
    let mut port = "";
    for line in stdout.split('\n').filter(|l| !l.is_empty()) {
        let Some(space) = line.find(' ') else {
            continue;
        };
        let key = &line[..space];
        let value = &line[space + 1..];
        match key {
            "user" => user = value,
            "hostname" => host = value,
            "port" => port = value,
            _ => {}
        }
        if !user.is_empty() && !host.is_empty() && !port.is_empty() {
            break;
        }
    }

    if user.is_empty() {
        log::warn!(target: LOG, "ssh -G output missing user");
        return None;
    }
    if host.is_empty() {
        log::warn!(target: LOG, "ssh -G output missing hostname");
        return None;
    }

    if port.is_empty() || port == "22" {
        return Some(format!("{}@{}", user, host));
    }

    if host.contains(':') {
        return Some(format!("{}@[{}]:{}", user, host, port));
    }
    Some(format!("{}@{}:{}", user, host, port))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallError {
    InstallFailed,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InstallFailed")
    }
}

/// Install Ghostty's terminfo on the remote host over a short-lived SSH
/// ControlMaster connection. The master tears down with the client
/// (`ControlPersist=no`) so no socket lingers.
fn install_remote_terminfo<W: Write + ?Sized>(opts: &Options, stderr: &mut W) -> Result<(), InstallError> {
    let mut terminfo: Vec<u8> = Vec::new();
    terminfopkg::GHOSTTY
        .encode(&mut terminfo)
        .map_err(|_| InstallError::InstallFailed)?;

    // ControlPath is in TMPDIR with a short, random basename. ssh uses
    // ControlPath as the bind address for a Unix domain socket; macOS
    // limits sockaddr_un.sun_path to ~104 bytes, so keeping the path
    // short leaves margin.
    let control_path = internal_os::random_tmp_path("ghostty-ssh-");
    let control_path_opt = format!("ControlPath={}", control_path);

    // Under --verbose, let remote stderr through (the `tic` step is
    // the most common failure source) and inherit ssh's stderr so it
    // reaches the user's terminal. Other steps stay quiet either way.
    let remote_script = if opts.verbose {
        "command -v tic >/dev/null 2>&1 || exit 1
mkdir -p ~/.terminfo 2>/dev/null && tic -x - && exit 0
exit 1"
    } else {
        "command -v tic >/dev/null 2>&1 || exit 1
mkdir -p ~/.terminfo 2>/dev/null && tic -x - 2>/dev/null && exit 0
exit 1"
    };

    // Set up an SSH ControlMaster scoped to this single install:
    //   - ControlMaster=yes makes our client also act as the master.
    //   - ControlPersist=no tears the master down when our client
    //     exits; no socket lingers on the remote side.
    let mut argv: Vec<String> = vec![opts.ssh.clone()];
    argv.extend(
        ["-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-o"]
            .iter()
            .map(|s| s.to_string()),
    );
    argv.push(control_path_opt);
    argv.extend(opts._ssh_args.iter().cloned());
    argv.push(remote_script.to_owned());
    verbose_print(opts, stderr, format_args!("exec: {}", Joined { items: &argv }));

    let mut child = match Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(if opts.verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::warn!(target: LOG, "terminfo install spawn failed: {}", err);
            return Err(InstallError::InstallFailed);
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&terminfo);
        drop(stdin);
    }

    let term = match child.wait() {
        Ok(term) => term,
        Err(err) => {
            log::warn!(target: LOG, "terminfo install wait failed: {}", err);
            return Err(InstallError::InstallFailed);
        }
    };
    check_exit(term, "terminfo install").map_err(|_| InstallError::InstallFailed)
}

/// Run `argv` with the terminal's stdin, stdout, and stderr, and wait for
/// it to exit. When `environ_map` is None, the child inherits our
/// environment. Otherwise, it replaces the environment entirely.
///
/// Returns the child's exit code, or `128 + signum` if a signal killed it,
/// matching shell convention.
fn child_exec(argv: &[String], environ_map: Option<&internal_os::EnvironMap>) -> io::Result<u8> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(env) = environ_map {
        cmd.env_clear().envs(env);
    }

    let term = cmd.spawn()?.wait()?;
    if let Some(rc) = term.code() {
        return Ok(rc as u8);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = term.signal() {
            return Ok(128u8 + sig.clamp(0, 127) as u8);
        }
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ArgIteratorGeneral;

    fn parse_test_args(opts: &mut Options, line: &str) {
        let mut iter = ArgIteratorGeneral::init(line);
        cli_args::parse(opts, &mut iter).unwrap();
    }

    #[test]
    fn joined() {
        let empty: [&str; 0] = [];
        assert_eq!("", Joined { items: &empty }.to_string());
        assert_eq!("only", Joined { items: &["only"] }.to_string());
        assert_eq!("a b c", Joined { items: &["a", "b", "c"] }.to_string());
    }

    #[test]
    fn parse_manually_hook_bare_destination_starts_ssh_args() {
        let mut opts = Options::default();
        parse_test_args(&mut opts, "--terminfo=false user@example.com");
        assert!(!opts.terminfo);
        assert!(opts.forward_env);
        assert_eq!(1, opts._ssh_args.len());
        assert_eq!("user@example.com", opts._ssh_args[0]);
    }

    #[test]
    fn parse_manually_hook_short_ssh_flags_pass_through_verbatim() {
        let mut opts = Options::default();
        parse_test_args(&mut opts, "-p 2222 user@example.com");
        assert_eq!(3, opts._ssh_args.len());
        assert_eq!("-p", opts._ssh_args[0]);
        assert_eq!("2222", opts._ssh_args[1]);
        assert_eq!("user@example.com", opts._ssh_args[2]);
    }

    #[test]
    fn parse_manually_hook_explicit_separator() {
        let mut opts = Options::default();
        parse_test_args(&mut opts, "--verbose -- --some-rare-ssh-arg user@example.com");
        assert!(opts.verbose);
        assert_eq!(2, opts._ssh_args.len());
        assert_eq!("--some-rare-ssh-arg", opts._ssh_args[0]);
        assert_eq!("user@example.com", opts._ssh_args[1]);
    }

    #[test]
    fn parse_destination_typical_ssh_g_output() {
        let stdout = "user alice\nhostname example.com\nport 22\nidentityfile ~/.ssh/id_ed25519\n";
        assert_eq!(Some("alice@example.com".to_owned()), parse_destination(stdout));
    }

    #[test]
    fn parse_destination_hostname_before_user() {
        let stdout = "hostname example.com\nport 22\nuser alice\n";
        assert_eq!(Some("alice@example.com".to_owned()), parse_destination(stdout));
    }

    #[test]
    fn parse_destination_missing_hostname_returns_null() {
        assert_eq!(None, parse_destination("user alice\nport 22\n"));
    }

    #[test]
    fn parse_destination_missing_user_returns_null() {
        assert_eq!(None, parse_destination("hostname example.com\nport 22\n"));
    }

    #[test]
    fn parse_destination_empty_input_returns_null() {
        assert_eq!(None, parse_destination(""));
    }

    #[test]
    fn parse_destination_ipv6_hostname() {
        assert_eq!(
            Some("alice@::1".to_owned()),
            parse_destination("user alice\nhostname ::1\n")
        );
    }

    #[test]
    fn parse_destination_non_default_port() {
        let stdout = "user alice\nhostname example.com\nport 2222\n";
        assert_eq!(Some("alice@example.com:2222".to_owned()), parse_destination(stdout));
    }

    #[test]
    fn parse_destination_port_before_hostname() {
        let stdout = "port 2222\nuser alice\nhostname example.com\n";
        assert_eq!(Some("alice@example.com:2222".to_owned()), parse_destination(stdout));
    }

    #[test]
    fn parse_destination_ipv6_hostname_with_non_default_port() {
        let stdout = "user alice\nhostname ::1\nport 2222\n";
        assert_eq!(Some("alice@[::1]:2222".to_owned()), parse_destination(stdout));
    }

    #[test]
    fn parse_destination_result_is_a_valid_cache_key() {
        let outputs = [
            "user alice\nhostname example.com\nport 22\n",
            "user alice\nhostname example.com\nport 2222\n",
            "user alice\nhostname 2001:db8::1\nport 22\n",
            "user alice\nhostname 2001:db8::1\nport 2222\n",
        ];
        for stdout in outputs {
            let result = parse_destination(stdout).unwrap();
            assert!(disk_cache::is_valid_cache_key(&result), "{}", result);
        }
    }
}
