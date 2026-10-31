//! Port of `src/cli/ssh-cache/DiskCache.zig`.
//!
//! STATUS: ported
//!
//! An SSH terminfo entry cache that stores its cache data on
//! disk. The cache only stores metadata (hostname, terminfo value,
//! etc.) and does not store any sensitive data.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Ipv6Addr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use super::entry::Entry;

// 512KB - sufficient for approximately 10k entries
const MAX_CACHE_SIZE: usize = 512 * 1024;

/// An SSH terminfo entry cache that stores its cache data on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCache {
    /// Path to a file where the cache is stored.
    pub path: PathBuf,
}

/// Errors returned by cache operations (the Zig code uses inferred error
/// sets; the named errors are kept as variants, everything else is I/O).
#[derive(Debug)]
pub enum Error {
    InvalidCacheKey,
    CacheLocked,
    InvalidCachePath,
    XdgLookupFailed,
    /// `error.StreamTooLong`: the cache file exceeds `MAX_CACHE_SIZE`.
    StreamTooLong,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "{err}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

/// The entries of a cache, keyed by hostname (`std.StringHashMap(Entry)`).
pub type Entries = HashMap<String, Entry>;

impl DiskCache {
    /// Returns the default path for the cache for a given program.
    ///
    /// On all platforms, this is `${XDG_STATE_HOME}/ghostty/ssh_cache`.
    pub fn default_path(program: &str) -> Result<PathBuf, Error> {
        let environ_map = crate::global::environ_map();
        let state_dir = crate::os::xdg::state(
            &environ_map,
            crate::os::xdg::Options {
                subdir: Some(program),
                ..Default::default()
            },
        )
        .map_err(|_| Error::XdgLookupFailed)?;
        Ok(PathBuf::from(state_dir).join("ssh_cache"))
    }

    /// Clear all cache data stored in the disk cache.
    /// This removes the cache file from disk, effectively clearing all cached
    /// SSH terminfo entries.
    pub fn clear(&self) -> Result<(), Error> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Add or update an entry in the cache, recording `timestamp` (Unix seconds).
    /// The cache file is created if it doesn't exist with secure permissions (0600).
    pub fn add(&self, key: &str, version: &str, timestamp: i64) -> Result<(), Error> {
        if !is_valid_cache_key(key) {
            return Err(Error::InvalidCacheKey);
        }

        // Create cache directory if needed
        if let Some(dir) = dirname(&self.path) {
            match fs::create_dir_all(dir) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err.into()),
            }
        }

        // Open or create cache file with secure permissions
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let existing_file = OpenOptions::new().read(true).write(true).open(&self.path)?;
                fixup_permissions(&existing_file)?;
                existing_file
            }
            Err(err) => return Err(err.into()),
        };

        // Lock
        let _lock = FileLock::try_lock(&file)?;

        let mut entries = read_entries(&file)?;

        // Update the timestamp of an existing entry, or insert a new one.
        if let Some(existing) = entries.get_mut(key) {
            existing.timestamp = timestamp;
            existing.terminfo_version = version.to_owned();
        } else {
            entries.insert(
                key.to_owned(),
                Entry {
                    hostname: key.to_owned(),
                    timestamp,
                    terminfo_version: version.to_owned(),
                },
            );
        }

        self.write_cache_file(&entries)
    }

    /// Remove an entry from the cache. Returns true if an entry was removed,
    /// false if the key wasn't present (or the cache file is missing).
    pub fn remove(&self, key: &str) -> Result<bool, Error> {
        if !is_valid_cache_key(key) {
            return Err(Error::InvalidCacheKey);
        }

        // Open our file
        let file = match OpenOptions::new().read(true).write(true).open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        fixup_permissions(&file)?;

        // Lock
        let _lock = FileLock::try_lock(&file)?;

        // Read existing entries
        let mut entries = read_entries(&file)?;

        // Remove the entry if it exists
        let removed = match entries.remove(key) {
            Some(kv) => {
                debug_assert_eq!(kv.hostname, key);
                true
            }
            None => false,
        };

        self.write_cache_file(&entries)?;
        Ok(removed)
    }

    /// Remove all entries older than `max_age_s` seconds and return how many
    /// were pruned. Returns zero (and nothing written) if the cache file is
    /// missing.
    pub fn prune(&self, max_age_s: u64) -> Result<usize, Error> {
        let file = match OpenOptions::new().read(true).write(true).open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(err.into()),
        };
        fixup_permissions(&file)?;

        // Lock
        let _lock = FileLock::try_lock(&file)?;

        // Read existing entries
        let mut entries = read_entries(&file)?;

        // Drop expired entries from the map, then persist what remains.
        let now = now_seconds();
        let expired: Vec<String> = entries
            .iter()
            .filter(|(_, v)| {
                let age_s = now.saturating_sub(v.timestamp);
                i128::from(age_s) > i128::from(max_age_s)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in &expired {
            let kv = entries.remove(key).unwrap();
            debug_assert_eq!(&kv.hostname, key);
        }

        self.write_cache_file(&entries)?;
        Ok(expired.len())
    }

    /// Check if a key with `version` exists in the cache.
    /// Returns false if the cache file doesn't exist or the version doesn't match.
    pub fn contains(&self, key: &str, version: &str) -> Result<bool, Error> {
        if !is_valid_cache_key(key) {
            return Err(Error::InvalidCacheKey);
        }

        // Open our file
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };

        // Read existing entries
        let entries = read_entries(&file)?;

        let Some(entry) = entries.get(key) else {
            return Ok(false);
        };
        Ok(entry.terminfo_version == version)
    }

    fn write_cache_file(&self, entries: &Entries) -> Result<(), Error> {
        let cache_dir = dirname(&self.path).ok_or(Error::InvalidCachePath)?;
        let cache_basename = self.path.file_name().ok_or(Error::InvalidCachePath)?;

        // Atomic file: write to a temporary file in the same directory and
        // rename it over the destination.
        let mut atomic_file = AtomicFile::create(cache_dir, Path::new(cache_basename))?;
        {
            let mut file_writer = io::BufWriter::with_capacity(1024, &mut atomic_file.file);
            for entry in entries.values() {
                entry.format(&mut file_writer)?;
            }
            file_writer.flush()?;
        }
        atomic_file.replace()?;
        Ok(())
    }

    /// List all entries in the cache.
    /// Returns an empty map if the cache file doesn't exist.
    pub fn list(&self) -> Result<Entries, Error> {
        // Open our file
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(err) => return Err(err.into()),
        };
        read_entries(&file)
    }
}

/// Free memory allocated by the `list` function. In Rust the map owns its
/// data, so this just empties it.
pub fn deinit_entries(entries: &mut Entries) {
    entries.clear();
}

fn fixup_permissions(file: &File) -> Result<(), Error> {
    // Ensure file has correct permissions (readable/writable by
    // owner only)
    let stat = file.metadata()?;
    if stat.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn read_entries(file: &File) -> Result<Entries, Error> {
    // allocRemaining(.limited(MAX_CACHE_SIZE))
    let mut content = Vec::new();
    let reader = file;
    reader
        .take(MAX_CACHE_SIZE as u64 + 1)
        .read_to_end(&mut content)?;
    if content.len() > MAX_CACHE_SIZE {
        return Err(Error::StreamTooLong);
    }
    // NOTE: the Zig code works on raw bytes; invalid UTF-8 is replaced here.
    let content = String::from_utf8_lossy(&content);

    let mut entries: Entries = HashMap::new();
    for line in content.split('\n').filter(|l| !l.is_empty()) {
        let trimmed = line.trim_matches(|c| matches!(c, ' ' | '\t' | '\r'));
        let Some(entry) = Entry::parse(trimmed) else {
            continue;
        };

        match entries.get_mut(&entry.hostname) {
            None => {
                // New entry: transfer to the map.
                entries.insert(entry.hostname.clone(), entry);
            }
            Some(existing) => {
                // Handle duplicate entries - keep newer timestamp
                if entry.timestamp > existing.timestamp {
                    existing.timestamp = entry.timestamp;
                    if existing.terminfo_version != entry.terminfo_version {
                        existing.terminfo_version = entry.terminfo_version;
                    }
                }
            }
        }
    }

    Ok(entries)
}

/// Whether an error from a cache operation reflects a problem with the cache
/// itself. A destination we can't key on and a lock held by another process
/// both leave the cache healthy, so neither is worth reporting.
pub fn is_failure(err: &Error) -> bool {
    !matches!(err, Error::InvalidCacheKey | Error::CacheLocked)
}

// Supports both standalone hostnames and user@hostname format, each with an
// optional `:port` suffix (`[addr]:port` for IPv6 addresses).
pub fn is_valid_cache_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }

    // Check for user@hostname format
    if let Some(at_pos) = key.find('@') {
        let user = &key[..at_pos];
        let hostname = &key[at_pos + 1..];
        return is_valid_user(user) && is_valid_host_port(hostname);
    }

    is_valid_host_port(key)
}

/// A host with an optional port, split apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPort<'a> {
    pub host: &'a str,
    pub port: Option<&'a str>,
}

/// Split `host`, `host:port` or `[ipv6]:port` into its host and port. A
/// bare IPv6 address (more than one colon, no brackets) has no port. The
/// parts are not validated.
pub fn split_host_port(s: &str) -> HostPort<'_> {
    if s.starts_with('[') {
        if let Some(close) = s.find("]:") {
            return HostPort {
                host: &s[1..close],
                port: Some(&s[close + 2..]),
            };
        }
        return HostPort {
            host: s,
            port: None,
        };
    }

    if let Some(colon) = s.find(':') {
        if !s[colon + 1..].contains(':') {
            return HostPort {
                host: &s[..colon],
                port: Some(&s[colon + 1..]),
            };
        }
    }

    HostPort {
        host: s,
        port: None,
    }
}

fn is_valid_host_port(s: &str) -> bool {
    let hp = split_host_port(s);
    let Some(port) = hp.port else {
        return is_valid_host(hp.host);
    };

    // Only IPv6 addresses are bracketed, and only when they carry a port.
    let bracketed = s.as_bytes()[0] == b'[';
    let ip6 = hp.host.contains(':');
    if bracketed != ip6 {
        return false;
    }

    is_valid_host(hp.host) && is_valid_port(port)
}

fn is_valid_port(port: &str) -> bool {
    // Digits only and no leading zero (which also rules out port 0), so
    // `host:22` and `host:022` can't become distinct keys.
    let bytes = port.as_bytes();
    if bytes.is_empty() || bytes[0] == b'0' {
        return false;
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    port.parse::<u16>().is_ok()
}

// Checks if a host is a valid hostname or IP address
fn is_valid_host(host: &str) -> bool {
    // First check for valid hostnames because this is assumed to be the more
    // likely ssh host format.
    if host_name_validate(host.as_bytes()).is_ok() {
        return true;
    }

    // We also accept valid IP addresses. In practice, IPv4 addresses are also
    // considered valid hostnames due to their overlapping syntax, so we can
    // simplify this check to be IPv6-specific.
    host.parse::<Ipv6Addr>().is_ok()
}

fn is_valid_user(user: &str) -> bool {
    if user.is_empty() || user.len() > 64 {
        return false;
    }
    user.bytes()
        .all(|c| matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

/// The errors of [`host_name_validate`] (Zig: `HostName.ValidateError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostNameValidateError {
    NameTooLong,
    InvalidHostName,
}

/// Port of `std.Io.net.HostName.validate` (Zig 0.17): validates a hostname
/// according to RFC 1123.
///
/// NOTE(port): Zig standard library function with no Rust std counterpart.
/// Also used by `termio::stream_handler` (`HostName.fromUri`).
pub(crate) fn host_name_validate(bytes: &[u8]) -> Result<(), HostNameValidateError> {
    use HostNameValidateError::*;

    /// `HostName.max_len - 1`
    const MAX_LEN_WITHOUT_ROOT: usize = 254 - 1;

    if bytes.is_empty() {
        return Err(InvalidHostName);
    }

    // Ignore trailing dot (FQDN).
    let end = if bytes[bytes.len() - 1] == b'.' { bytes.len() - 1 } else { bytes.len() };

    // The accepted maximum length of a hostname, including labels and dots.
    if end > MAX_LEN_WITHOUT_ROOT {
        return Err(NameTooLong);
    }

    // Hostnames are divided into dot-separated "labels", which:
    //
    // - Start with a letter or digit
    // - Can contain letters, digits, or hyphens
    // - Must end with a letter or digit
    // - Have a minimum of 1 character and a maximum of 63
    let mut label_len: usize = 0;
    for (i, &c) in bytes[..end].iter().enumerate() {
        match c {
            b'.' => {
                if label_len == 0 || label_len > 63 {
                    return Err(InvalidHostName);
                }
                if !bytes[i - 1].is_ascii_alphanumeric() {
                    return Err(InvalidHostName);
                }
                label_len = 0;
            }
            b'-' => {
                if label_len == 0 {
                    return Err(InvalidHostName);
                }
                label_len += 1;
            }
            _ => {
                if !c.is_ascii_alphanumeric() {
                    return Err(InvalidHostName);
                }
                label_len += 1;
            }
        }
    }

    // Validate the final label
    if label_len == 0 || label_len > 63 {
        return Err(InvalidHostName);
    }
    if !bytes[end - 1].is_ascii_alphanumeric() {
        return Err(InvalidHostName);
    }
    Ok(())
}

/// `std.fs.path.dirname`: `None` for a path without a directory component.
fn dirname(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

fn now_seconds() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// An exclusive advisory lock (`File.tryLock(.exclusive)`), released on drop.
///
/// Like the Zig code, failing to acquire the lock because another process
/// holds it is ignored (the `tryLock` result is discarded); any other
/// locking error is reported as `CacheLocked`.
struct FileLock<'a> {
    file: &'a File,
}

impl<'a> FileLock<'a> {
    fn try_lock(file: &'a File) -> Result<FileLock<'a>, Error> {
        // SAFETY: the fd is valid for the lifetime of `file`.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(Error::CacheLocked);
            }
        }
        Ok(FileLock { file })
    }
}

impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the fd is valid for the lifetime of `file`.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// `Dir.createFileAtomic(.., .{ .replace = true })`: a temporary file in the
/// destination directory that is renamed over the destination by
/// [`AtomicFile::replace`] and deleted on drop otherwise.
struct AtomicFile {
    file: File,
    tmp_path: PathBuf,
    dest_path: PathBuf,
    replaced: bool,
}

impl AtomicFile {
    fn create(dir: &Path, basename: &Path) -> Result<AtomicFile, Error> {
        let dest_path = dir.join(basename);
        loop {
            let mut rand = [0u8; 8];
            getrandom::fill(&mut rand).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
            let suffix: String = rand.iter().map(|b| format!("{b:02x}")).collect();
            let tmp_path = dir.join(format!(".{}.{suffix}.tmp", basename.display()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)
            {
                Ok(file) => {
                    return Ok(AtomicFile {
                        file,
                        tmp_path,
                        dest_path,
                        replaced: false,
                    })
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn replace(&mut self) -> Result<(), Error> {
        fs::rename(&self.tmp_path, &self.dest_path)?;
        self.replaced = true;
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.replaced {
            let _ = fs::remove_file(&self.tmp_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `std.testing.tmpDir`: a fresh directory removed on drop.
    struct TmpDir {
        path: PathBuf,
    }

    impl TmpDir {
        fn new() -> TmpDir {
            let mut rand = [0u8; 8];
            getrandom::fill(&mut rand).unwrap();
            let name: String = rand.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("ghostty-ssh-cache-test-{name}"));
            fs::create_dir_all(&path).unwrap();
            TmpDir {
                path: path.canonicalize().unwrap(),
            }
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    const S_PER_MIN: i64 = 60;
    const S_PER_HOUR: i64 = 60 * S_PER_MIN;
    const S_PER_DAY: i64 = 24 * S_PER_HOUR;

    #[test]
    fn disk_cache_default_path() {
        let path = DiskCache::default_path("ghostty").unwrap();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn disk_cache_clear() {
        // Create our path
        let tmp = TmpDir::new();
        fs::write(tmp.path.join("cache"), b"HELLO!").unwrap();
        let path = tmp.path.join("cache").canonicalize().unwrap();

        // Setup our cache
        let cache = DiskCache { path };
        cache.clear().unwrap();

        // Verify the file is gone
        let err = File::open(tmp.path.join("cache")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn disk_cache_operations() {
        // Create our path
        let tmp = TmpDir::new();
        fs::write(tmp.path.join("cache"), b"HELLO!").unwrap();
        let path = tmp.path.join("cache").canonicalize().unwrap();

        // Setup our cache. Adding the same key twice exercises both the new
        // and existing-entry paths.
        let cache = DiskCache { path };
        cache.add("example.com", "v1", now_seconds()).unwrap();
        assert!(!cache.contains("example.com", "v2").unwrap());
        cache.add("example.com", "v2", now_seconds()).unwrap();
        assert!(cache.contains("example.com", "v2").unwrap());

        // List
        let mut entries = cache.list().unwrap();
        assert_eq!("v2", entries.get("example.com").unwrap().terminfo_version);
        deinit_entries(&mut entries);

        // Remove reports that it removed the entry, and a second remove of the
        // same key reports nothing to remove.
        assert!(cache.remove("example.com").unwrap());
        assert!(!cache.remove("example.com").unwrap());
        assert!(!cache.contains("example.com", "v2").unwrap());
        cache.add("example.com", "v2", now_seconds()).unwrap();
    }

    #[test]
    fn disk_cache_cleans_up_temp_files() {
        let tmp = TmpDir::new();
        let cache_path = tmp.path.join("cache");

        let cache = DiskCache { path: cache_path };
        cache.add("example.com", "v1", now_seconds()).unwrap();
        cache.add("example.org", "v1", now_seconds()).unwrap();

        // Verify only the cache file exists and no temp files left behind
        let mut count = 0usize;
        for entry in fs::read_dir(&tmp.path).unwrap() {
            let entry = entry.unwrap();
            count += 1;
            assert_eq!("cache", entry.file_name());
        }
        assert_eq!(1, count);
    }

    #[test]
    fn disk_cache_prune() {
        let tmp = TmpDir::new();
        let cache_path = tmp.path.join("cache");

        let cache = DiskCache { path: cache_path };

        // Back-date one entry an hour old and one 100 days old.
        let day = S_PER_DAY;
        let hour = S_PER_HOUR;
        let now = now_seconds();
        cache.add("recent.com", "v1", now - hour).unwrap();
        cache.add("old.com", "v1", now - 100 * day).unwrap();

        // Prune entries older than 90 days: only old.com goes.
        assert_eq!(1usize, cache.prune((90 * day) as u64).unwrap());
        assert!(cache.contains("recent.com", "v1").unwrap());
        assert!(!cache.contains("old.com", "v1").unwrap());

        // Pruning again removes nothing.
        assert_eq!(0usize, cache.prune((90 * day) as u64).unwrap());

        // Sub-day granularity: a 30-minute max age prunes the hour-old entry.
        assert_eq!(1usize, cache.prune((30 * S_PER_MIN) as u64).unwrap());
        assert!(!cache.contains("recent.com", "v1").unwrap());
    }

    #[test]
    fn disk_cache_prune_missing_file() {
        let tmp = TmpDir::new();
        let cache_path = tmp.path.join("cache");

        let cache = DiskCache { path: cache_path };
        assert_eq!(0usize, cache.prune(30).unwrap());
    }

    #[test]
    fn disk_cache_reads_duplicate_keys() {
        let tmp = TmpDir::new();

        // Exercise read_entries' found_existing branch: replace the existing
        // key with the updated entry.
        fs::write(
            tmp.path.join("cache"),
            b"example.com|100|xterm-ghostty\nexample.com|200|xterm-newer\n",
        )
        .unwrap();
        let path = tmp.path.join("cache").canonicalize().unwrap();

        let cache = DiskCache { path };
        let entries = cache.list().unwrap();

        assert_eq!(1, entries.len());
        let entry = entries.get("example.com").unwrap();
        assert_eq!(200i64, entry.timestamp);
        assert_eq!("xterm-newer", entry.terminfo_version);
    }

    // The Zig test drives `list` with a FailingAllocator to cover every
    // allocation failure. Rust aborts on allocation failure, so only the
    // final (no induced failure) iteration is meaningful: the read must
    // complete successfully.
    #[test]
    fn disk_cache_reads_survive_allocation_failure() {
        let tmp = TmpDir::new();

        // Exercise a populated cache containing a duplicate key.
        fs::write(
            tmp.path.join("cache"),
            concat!(
                "a.com|100|xterm-ghostty\n",
                "b.com|100|xterm-ghostty\n",
                "c.com|100|xterm-ghostty\n",
                "a.com|200|xterm-newer\n",
            ),
        )
        .unwrap();
        let path = tmp.path.join("cache").canonicalize().unwrap();

        let cache = DiskCache { path };
        let mut entries = cache.list().unwrap();
        assert_eq!(3, entries.len());
        assert_eq!(
            "xterm-newer",
            entries.get("a.com").unwrap().terminfo_version
        );
        deinit_entries(&mut entries);
    }

    // See `disk_cache_reads_survive_allocation_failure`: only the
    // no-failure path can be exercised.
    #[test]
    fn disk_cache_add_survives_allocation_failure() {
        let tmp = TmpDir::new();
        let path = tmp.path.join("cache");

        let cache = DiskCache { path: path.clone() };
        let _ = fs::remove_file(&path);
        cache.add("user@example.com", "v1", 100).unwrap();
        assert!(cache.contains("user@example.com", "v1").unwrap());
    }

    #[test]
    fn test_is_failure() {
        assert!(!is_failure(&Error::InvalidCacheKey));
        assert!(!is_failure(&Error::CacheLocked));
        assert!(is_failure(&Error::Io(io::Error::from(
            io::ErrorKind::PermissionDenied
        ))));
        assert!(is_failure(&Error::Io(io::Error::from(
            io::ErrorKind::IsADirectory
        ))));
    }

    #[test]
    fn test_is_valid_host() {
        // Valid hostnames
        assert!(is_valid_host("localhost"));
        assert!(is_valid_host("example.com"));
        assert!(is_valid_host("sub.example.com"));

        // IPv4 addresses
        assert!(is_valid_host("127.0.0.1"));
        assert!(is_valid_host("192.168.1.1"));

        // IPv6 addresses
        assert!(is_valid_host("::1"));
        assert!(is_valid_host("2001:db8::1"));
        assert!(is_valid_host("2001:db8:0:1:1:1:1:1"));
        assert!(!is_valid_host("fe80::1%eth0")); // scopes not supported

        // Invalid hosts
        assert!(!is_valid_host(""));
        assert!(!is_valid_host("host\nname"));
        assert!(!is_valid_host(".example.com"));
        assert!(!is_valid_host("host..domain"));
        assert!(!is_valid_host("-hostname"));
        assert!(!is_valid_host("hostname-"));
        assert!(!is_valid_host("host name"));
        assert!(!is_valid_host("host_name"));
        assert!(!is_valid_host("host@domain"));
        assert!(!is_valid_host("host:port"));
    }

    #[test]
    fn test_is_valid_user() {
        // Valid
        assert!(is_valid_user("user"));
        assert!(is_valid_user("user-user"));
        assert!(is_valid_user("user_name"));
        assert!(is_valid_user("user.name"));
        assert!(is_valid_user("user123"));

        // Invalid
        assert!(!is_valid_user(""));
        assert!(!is_valid_user("user name"));
        assert!(!is_valid_user("user@example"));
        assert!(!is_valid_user("user:group"));
        assert!(!is_valid_user("user\nname"));
        let loooong = "a".repeat(65);
        assert!(!is_valid_user(&loooong)); // too long
    }

    #[test]
    fn test_split_host_port() {
        let plain = split_host_port("example.com");
        assert_eq!("example.com", plain.host);
        assert!(plain.port.is_none());

        let with_port = split_host_port("example.com:2222");
        assert_eq!("example.com", with_port.host);
        assert_eq!("2222", with_port.port.unwrap());

        let ip6 = split_host_port("2001:db8::1");
        assert_eq!("2001:db8::1", ip6.host);
        assert!(ip6.port.is_none());

        let ip6_port = split_host_port("[2001:db8::1]:2222");
        assert_eq!("2001:db8::1", ip6_port.host);
        assert_eq!("2222", ip6_port.port.unwrap());
    }

    #[test]
    fn test_is_valid_cache_key() {
        // Valid
        assert!(is_valid_cache_key("example.com"));
        assert!(is_valid_cache_key("sub.example.com"));
        assert!(is_valid_cache_key("192.168.1.1"));
        assert!(is_valid_cache_key("::1"));
        assert!(is_valid_cache_key("user@example.com"));
        assert!(is_valid_cache_key("user@192.168.1.1"));
        assert!(is_valid_cache_key("user@::1"));
        assert!(is_valid_cache_key("example.com:2222"));
        assert!(is_valid_cache_key("user@example.com:2222"));
        assert!(is_valid_cache_key("user@192.168.1.1:65535"));
        assert!(is_valid_cache_key("user@[::1]:2222"));
        assert!(is_valid_cache_key("[2001:db8::1]:22"));

        // Invalid ports
        assert!(!is_valid_cache_key("user@example.com:"));
        assert!(!is_valid_cache_key("user@example.com:0"));
        assert!(!is_valid_cache_key("user@example.com:022"));
        assert!(!is_valid_cache_key("user@example.com:00"));
        assert!(!is_valid_cache_key("user@example.com:65536"));
        assert!(!is_valid_cache_key("user@example.com:+22"));
        assert!(!is_valid_cache_key("user@example.com:ssh"));
        assert!(!is_valid_cache_key("user@[::1]"));
        assert!(!is_valid_cache_key("user@[::1]:"));
        assert!(!is_valid_cache_key("user@[example.com]:22"));

        // Invalid
        assert!(!is_valid_cache_key(""));
        assert!(!is_valid_cache_key(".example.com"));
        assert!(!is_valid_cache_key("@example.com"));
        assert!(!is_valid_cache_key("user@"));
        assert!(!is_valid_cache_key("user@@example"));
        assert!(!is_valid_cache_key("user@.example.com"));
    }
}
