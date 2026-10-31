//! Port of `src/cli/ssh_cache.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "ssh_cache/disk_cache.rs"]
pub mod disk_cache;
#[path = "ssh_cache/entry.rs"]
pub mod entry;
// @generated-mods-end

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, ArgIterator, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use crate::config::config::Duration;
use crate::terminfo as terminfopkg;

pub use self::disk_cache::DiskCache;
pub use self::entry::Entry;

const NS_PER_S: u64 = 1_000_000_000;
const S_PER_MIN: u64 = 60;
const S_PER_HOUR: u64 = 60 * S_PER_MIN;
const S_PER_DAY: u64 = 24 * S_PER_HOUR;
const S_PER_WEEK: u64 = 7 * S_PER_DAY;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        pub clear: bool = false,
        pub add: Option<String> = None,
        pub remove: Option<String> = None,
        pub prune: Option<Duration> = None,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"ssh-cache"`).
pub const HELP: &str = "Manage the SSH terminfo cache for automatic remote host setup.

The `+ssh` action installs Ghostty's terminfo on remote hosts and records
each success in this cache so it doesn't re-upload on later connections.
(`+ssh` runs automatically from the shell integration when
`shell-integration-features` includes `ssh-terminfo`.) This command
inspects and maintains that cache.

The cache stores destinations (a hostname or user@hostname) along with
timestamps. A destination on a port other than 22 carries it as a
`:port` suffix (`user@hostname:2222`, or `user@[2001:db8::1]:2222` for an
IPv6 address), since each port can be a different machine.

A positional destination queries the cache: `user@hostname` shows that
exact entry, while a bare `hostname` shows every cached entry for that
host regardless of user and port, and a bare `hostname:port` every entry
for that port. With no destination and no action, the entire cache is
listed. A query that matches nothing exits 1.

At most one action (`--clear`, `--add`, `--remove`, or `--prune`) may be
specified, and not together with a positional destination; combining them
is an error.

`--prune` takes a duration with unit suffixes (`s`, `m`, `h`, `d`, `w`,
`y`) and removes every entry older than it, e.g. `--prune=30d`,
`--prune=6h`, `--prune=1y`.

Examples:

    ghostty +ssh-cache                           # List all cached destinations
    ghostty +ssh-cache user@example.com          # Show that destination
    ghostty +ssh-cache example.com               # Show all users on that host
    ghostty +ssh-cache user@example.com:2222     # Show that destination on port 2222
    ghostty +ssh-cache --add=user@example.com    # Manually add a destination
    ghostty +ssh-cache --remove=user@example.com # Remove a destination
    ghostty +ssh-cache --prune=30d               # Remove entries older than 30 days
    ghostty +ssh-cache --clear                   # Clear entire cache";

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Manage the SSH terminfo cache for automatic remote host setup. See
/// [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    let mut stdout = BufWriter::with_capacity(1024, io::stdout().lock());
    let mut stderr = BufWriter::with_capacity(1024, io::stderr().lock());

    // The cache is queried by a positional destination (`user@host` or a
    // bare `host`). `args.parse` rejects non-`--` tokens, so we lift the
    // positional out here and parse only the remaining flags. `--host=X`
    // is accepted as a deprecated spelling of the positional (it was the
    // original shipped flag name).
    let mut query: Option<String> = None;
    let mut flags: Vec<String> = Vec::new();
    {
        let mut iter = args::args_iterator(global_args());
        while let Some(arg) = iter.next() {
            let host_flag = arg.strip_prefix("--host=");
            if let Some(h) = host_flag {
                write!(
                    stderr,
                    "Warning: --host is deprecated; pass the destination \
                     directly, e.g. `ghostty +ssh-cache {}`.\n",
                    h
                )?;
            }
            let dest: Option<&str> = if let Some(h) = host_flag {
                Some(h)
            } else if !arg.starts_with('-') {
                Some(&arg)
            } else {
                None
            };

            if let Some(d) = dest {
                if query.is_some() {
                    write!(stderr, "Error: only one destination may be specified.\n")?;
                    let _ = stderr.flush();
                    return Ok(2);
                }
                query = Some(d.to_owned());
            } else {
                flags.push(arg.clone());
            }
        }
    }

    {
        let flag_refs: Vec<&str> = flags.iter().map(String::as_str).collect();
        let mut iter = args::slice_iterator(&flag_refs);
        match args::parse(&mut opts, &mut iter) {
            Ok(()) => {}
            Err(Error::InvalidField) => {
                write!(stderr, "Error: unknown flag.\n")?;
                let _ = stderr.flush();
                return Ok(2);
            }
            Err(Error::InvalidValue) | Err(Error::ValueRequired) => {
                write!(stderr, "Error: invalid flag value.\n")?;
                let _ = stderr.flush();
                return Ok(2);
            }
            Err(err) => return Err(Box::new(err)),
        }
    }

    // Setup our disk cache to the standard location
    let cache_path = match DiskCache::default_path("ghostty") {
        Ok(path) => path,
        Err(err) => {
            write!(stderr, "Error: unable to determine the cache path: {}\n", err)?;
            let _ = stderr.flush();
            return Ok(1);
        }
    };

    let result = run_inner(
        &opts,
        query.as_deref(),
        &DiskCache { path: cache_path },
        &mut stdout,
        &mut stderr,
    );

    // Flushing *shouldn't* fail but...
    let _ = stdout.flush();
    let _ = stderr.flush();
    result
}

pub fn run_inner<O: Write + ?Sized, E: Write + ?Sized>(
    opts: &Options,
    query: Option<&str>,
    cache: &DiskCache,
    stdout: &mut O,
    stderr: &mut E,
) -> RunResult {
    // At most one action may be specified, and a query (positional
    // destination) is itself an action.
    let action_count = opts.clear as usize
        + opts.add.is_some() as usize
        + opts.remove.is_some() as usize
        + opts.prune.is_some() as usize
        + query.is_some() as usize;
    if action_count > 1 {
        write!(
            stderr,
            "Error: only one of a destination, --clear, --add, --remove, \
             or --prune may be specified.\n"
        )?;
        return Ok(2);
    }

    let path = cache.path.display();

    if opts.clear {
        if let Err(err) = cache.clear() {
            write!(stderr, "Error: unable to clear cache '{}': {}\n", path, err)?;
            return Ok(1);
        }
        return Ok(0);
    }

    if let Some(dest) = &opts.add {
        match cache.add(dest, terminfopkg::version(), now_seconds()) {
            Ok(()) => {}
            Err(disk_cache::Error::InvalidCacheKey) => {
                write!(
                    stderr,
                    "Error: invalid destination '{}' (expected [user@]hostname[:port])\n",
                    dest
                )?;
                return Ok(2);
            }
            Err(err) => {
                write!(
                    stderr,
                    "Error: unable to add '{}' to cache '{}': {}\n",
                    dest, path, err
                )?;
                return Ok(1);
            }
        }
        return Ok(0);
    }

    if let Some(dest) = &opts.remove {
        let removed = match cache.remove(dest) {
            Ok(removed) => removed,
            Err(disk_cache::Error::InvalidCacheKey) => {
                write!(
                    stderr,
                    "Error: invalid destination '{}' (expected [user@]hostname[:port])\n",
                    dest
                )?;
                return Ok(2);
            }
            Err(err) => {
                write!(
                    stderr,
                    "Error: unable to remove '{}' from cache '{}': {}\n",
                    dest, path, err
                )?;
                return Ok(1);
            }
        };
        // Silence on success; a no-op removal is an error (exit 1).
        if !removed {
            write!(stderr, "Error: '{}' is not in the cache.\n", dest)?;
            return Ok(1);
        }
        return Ok(0);
    }

    if let Some(max_age) = opts.prune {
        let max_age_s = max_age.duration / NS_PER_S;
        if max_age_s == 0 {
            write!(
                stderr,
                "Error: --prune requires a duration of at least one second.\n"
            )?;
            return Ok(2);
        }
        let pruned = match cache.prune(max_age_s) {
            Ok(pruned) => pruned,
            Err(err) => {
                write!(stderr, "Error: unable to prune cache '{}': {}\n", path, err)?;
                return Ok(1);
            }
        };
        write!(stdout, "Pruned cache entries: {}\n", pruned)?;
        return Ok(0);
    }

    let entries = match cache.list() {
        Ok(entries) => entries,
        Err(err) => {
            write!(stderr, "Error: unable to read cache '{}': {}\n", path, err)?;
            return Ok(1);
        }
    };

    // A positional query filters the listing: an exact `user@host` match,
    // or every entry on a bare `host`.
    if let Some(q) = query {
        if !disk_cache::is_valid_cache_key(q) {
            write!(
                stderr,
                "Error: invalid destination '{}' (expected [user@]hostname[:port])\n",
                q
            )?;
            return Ok(2);
        }

        let matches: HashMap<String, Entry> = entries
            .iter()
            .filter(|(key, _)| matches_query(key, q))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        if matches.is_empty() {
            return Ok(1);
        }
        list_entries(&matches, stdout)?;
        return Ok(0);
    }

    // List all destinations by default.
    list_entries(&entries, stdout)?;
    Ok(0)
}

fn list_entries<W: Write + ?Sized>(entries: &HashMap<String, Entry>, writer: &mut W) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    // Sort entries by hostname for consistent output
    let mut items: Vec<&Entry> = entries.values().collect();
    items.sort_by(|a, b| a.hostname.as_bytes().cmp(b.hostname.as_bytes()));

    // Align the timestamp column by padding destinations to the widest.
    let widest = items.iter().map(|e| e.hostname.len()).max().unwrap_or(0);

    let now = now_seconds();
    for entry in items {
        write!(writer, "{}", entry.hostname)?;
        for _ in 0..widest - entry.hostname.len() + 2 {
            writer.write_all(b" ")?;
        }
        write!(
            writer,
            "{} ({})\n",
            format_timestamp(entry.timestamp),
            relative_age(now, entry.timestamp)
        )?;
    }
    Ok(())
}

/// Whether a cache `key` matches a positional `query`. A `user@host` query
/// (containing `@`) matches one exact destination; a bare `host` query
/// matches every key on that host regardless of user and port, and a bare
/// `host:port` query every key on that host and port regardless of user.
/// A missing port is the same as port 22.
fn matches_query(key: &str, query: &str) -> bool {
    let key_at = key.find('@');
    let query_at = query.find('@');
    if let Some(i) = query_at {
        let Some(j) = key_at else {
            return false;
        };
        if key[..j] != query[..i] {
            return false;
        }
    }

    let key_hp = disk_cache::split_host_port(match key_at {
        Some(i) => &key[i + 1..],
        None => key,
    });
    let query_hp = disk_cache::split_host_port(match query_at {
        Some(i) => &query[i + 1..],
        None => query,
    });
    if key_hp.host != query_hp.host {
        return false;
    }

    // Only a bare host query matches every port.
    if query_at.is_none() && query_hp.port.is_none() {
        return true;
    }
    key_hp.port.unwrap_or("22") == query_hp.port.unwrap_or("22")
}

/// Format a Unix timestamp as an ISO-8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`). Out-of-range input is clamped so this can't
/// crash on a garbage cache line.
fn format_timestamp(timestamp: i64) -> String {
    // Clamp to [epoch, last second of 9999-12-31Z]: the output only fits
    // a 4-digit year.
    let secs: u64 = timestamp.clamp(0, 253402300799) as u64;

    let days = secs / S_PER_DAY;
    let day_secs = secs % S_PER_DAY;

    // Zig `std.time.epoch`: walk years, then months.
    let mut year: u64 = 1970;
    let mut day = days;
    loop {
        let len = if is_leap_year(year) { 366 } else { 365 };
        if day < len {
            break;
        }
        day -= len;
        year += 1;
    }
    let mut month: u64 = 1;
    loop {
        let len = days_in_month(year, month);
        if day < len {
            break;
        }
        day -= len;
        month += 1;
    }

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day + 1,
        day_secs / S_PER_HOUR,
        (day_secs % S_PER_HOUR) / S_PER_MIN,
        day_secs % S_PER_MIN,
    )
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Format the age of `timestamp` (relative to `now`, both Unix seconds)
/// as a coarse relative time, e.g. "2w ago". Uses `Duration`'s unit
/// vocabulary but keeps only the single largest unit for scannability.
/// A non-positive age (timestamp at or after `now`) is "now".
fn relative_age(now: i64, timestamp: i64) -> String {
    // Saturating so a garbage timestamp can't overflow; clamp at 0 so a
    // future timestamp becomes a zero age rather than going negative.
    let age: u64 = now.saturating_sub(timestamp).max(0) as u64;
    if age == 0 {
        return "now".to_owned();
    }

    // Round down to the largest unit that fits, so Duration.format emits
    // only that unit (e.g. 19d -> 2w, 90m -> 1h).
    let units: [u64; 6] = [
        365 * S_PER_DAY, // y
        S_PER_WEEK,      // w
        S_PER_DAY,       // d
        S_PER_HOUR,      // h
        S_PER_MIN,       // m
        1,               // s
    ];
    let unit = units.iter().copied().find(|&u| age >= u).unwrap_or(1);

    // Cap the age so `age * ns_per_s` can't overflow u64 (a garbage, e.g.
    // hugely negative, timestamp otherwise yields an age near i64-max).
    let max_age = u64::MAX / NS_PER_S;
    let rounded = age.min(max_age) / unit * unit;
    let d = Duration {
        duration: rounded * NS_PER_S,
    };
    format!("{} ago", d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_query() {
        // Exact user@host: only the identical key.
        assert!(matches_query("user@example.com", "user@example.com"));
        assert!(!matches_query("root@example.com", "user@example.com"));
        assert!(!matches_query("example.com", "user@example.com"));

        // Bare host: every key on that host, plus a keyless entry for it.
        assert!(matches_query("user@example.com", "example.com"));
        assert!(matches_query("root@example.com", "example.com"));
        assert!(matches_query("example.com", "example.com"));
        assert!(!matches_query("user@other.com", "example.com"));

        // Bare host: every port on that host too.
        assert!(matches_query("user@example.com:2222", "example.com"));
        assert!(matches_query("user@[::1]:2222", "::1"));
        assert!(matches_query("user@::1", "::1"));

        // Bare host:port: that port only, any user.
        assert!(matches_query("user@example.com:2222", "example.com:2222"));
        assert!(matches_query("root@example.com:2222", "example.com:2222"));
        assert!(!matches_query("user@example.com", "example.com:2222"));
        assert!(!matches_query("user@example.com:2223", "example.com:2222"));
        assert!(matches_query("user@[::1]:2222", "[::1]:2222"));

        // Exact user@host:port.
        assert!(matches_query("user@example.com:2222", "user@example.com:2222"));
        assert!(!matches_query("user@example.com", "user@example.com:2222"));
        assert!(!matches_query("user@example.com:2222", "user@example.com"));

        // A missing port is port 22.
        assert!(matches_query("user@example.com", "example.com:22"));
        assert!(matches_query("user@example.com", "user@example.com:22"));
        assert!(matches_query("user@example.com:22", "user@example.com"));
        assert!(matches_query("user@[::1]:22", "user@::1"));
        assert!(!matches_query("user@example.com:2222", "example.com:22"));
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!("2026-05-05T22:49:33Z", format_timestamp(1778021373));

        // Epoch.
        assert_eq!("1970-01-01T00:00:00Z", format_timestamp(0));

        // Out-of-range inputs clamp instead of overflowing: negatives floor
        // at the epoch, huge values cap at the last second of year 9999.
        assert_eq!("1970-01-01T00:00:00Z", format_timestamp(-5));
        assert_eq!("9999-12-31T23:59:59Z", format_timestamp(i64::MAX));
    }

    #[test]
    fn test_relative_age() {
        let now: i64 = 2_000_000_000; // fixed reference
        let min = S_PER_MIN as i64;
        let hour = S_PER_HOUR as i64;
        let day = S_PER_DAY as i64;

        // Out-of-range timestamps don't crash: a huge future one saturates to
        // a non-positive age ("now"); a negative one is a large but real age.
        assert_eq!("now", relative_age(now, i64::MAX));
        assert_eq!("63y ago", relative_age(now, -100));

        // A huge age (garbage timestamp) saturates the ns conversion instead of
        // overflowing; it must not crash.
        assert!(relative_age(i64::MAX, 0).ends_with(" ago"));

        // Future timestamp (clock skew) and same-instant read "now".
        assert_eq!("now", relative_age(now, now + 100));
        assert_eq!("now", relative_age(now, now));

        // Only the single largest unit is kept (smaller units rounded away).
        assert_eq!("30s ago", relative_age(now, now - 30));
        assert_eq!("1m ago", relative_age(now, now - min));
        assert_eq!("1m ago", relative_age(now, now - 90)); // 90s -> 1m
        assert_eq!("1h ago", relative_age(now, now - hour));
        assert_eq!("1h ago", relative_age(now, now - (hour + 30 * min))); // 1h30m -> 1h
        assert_eq!("1d ago", relative_age(now, now - day));
        assert_eq!("2w ago", relative_age(now, now - 19 * day)); // 19d -> 2w
    }

    #[test]
    fn run_inner_rejects_multiple_actions() {
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();

        // The check runs before any cache access, so it never touches disk.
        let cache = DiskCache {
            path: "/nonexistent/ssh_cache".into(),
        };
        let code = run_inner(
            &Options {
                add: Some("example.com".into()),
                remove: Some("other.com".into()),
                ..Options::default()
            },
            None,
            &cache,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();

        assert_eq!(2, code);
        assert!(stdout.is_empty());
        assert!(String::from_utf8_lossy(&stderr).contains("only one"));

        // A positional query is itself an action: query + a flag conflicts.
        stderr.clear();
        let code2 = run_inner(
            &Options {
                clear: true,
                ..Options::default()
            },
            Some("example.com"),
            &cache,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(2, code2);
        assert!(String::from_utf8_lossy(&stderr).contains("only one"));
    }

    #[test]
    fn run_inner_fails_cleanly_when_the_cache_is_unusable() {
        // A directory where the cache file belongs fails every operation.
        let tmp = std::env::temp_dir().join(format!("ghostty-ssh-cache-cli-{}", std::process::id()));
        let cache_path = tmp.join("ssh_cache");
        std::fs::create_dir_all(&cache_path).unwrap();
        let cache = DiskCache { path: cache_path };

        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();

        let opts = [
            Options::default(), // the default listing
            Options {
                add: Some("example.com".into()),
                ..Options::default()
            },
            Options {
                remove: Some("example.com".into()),
                ..Options::default()
            },
            Options {
                prune: Some(Duration { duration: NS_PER_S }),
                ..Options::default()
            },
        ];
        for o in &opts {
            let code = run_inner(o, None, &cache, &mut stdout, &mut stderr).unwrap();
            assert_eq!(1, code);
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
