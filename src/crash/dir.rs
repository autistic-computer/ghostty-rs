//! Port of `src/crash/dir.zig`.
//!
//! STATUS: ported

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::os::xdg;
use crate::os::EnvironMap;

/// Returns a Dir for the default directory.
pub fn default_dir(environ_map: &EnvironMap) -> Result<Dir, xdg::Error> {
    let crash_dir = xdg::state(
        environ_map,
        xdg::Options {
            subdir: Some("ghostty/crash"),
            ..Default::default()
        },
    )?;
    Ok(Dir {
        path: PathBuf::from(crash_dir),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    /// The directory where crash reports are stored.
    pub path: PathBuf,
}

impl Dir {
    /// Returns an iterator over the crash reports in this directory. The
    /// iterator may have no reports (e.g. if the directory can't be opened).
    pub fn iterator(&self) -> ReportIterator {
        match fs::read_dir(&self.path) {
            Ok(it) => ReportIterator {
                dir: Some(self.path.clone()),
                it: Some(it),
            },
            Err(_) => ReportIterator::default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct ReportIterator {
    dir: Option<PathBuf>,
    it: Option<fs::ReadDir>,
}

impl ReportIterator {
    /// Release the directory handle. Dropping the iterator does the same.
    pub fn deinit(&mut self) {
        self.it = None;
        self.dir = None;
    }

    pub fn next(&mut self) -> io::Result<Option<Report>> {
        // If we have no dir then we failed to open the directory.
        let (Some(dir), Some(it)) = (&self.dir, &mut self.it) else {
            return Ok(None);
        };

        // Get the next file entry, if any.
        let entry = loop {
            let Some(entry) = it.next() else {
                return Ok(None);
            };
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            break entry;
        };

        let stat = fs::metadata(dir.join(entry.file_name()))?;
        Ok(Some(Report {
            name: entry.file_name(),
            mtime: i128::from(stat.mtime()) * 1_000_000_000 + i128::from(stat.mtime_nsec()),
        }))
    }
}

impl Iterator for ReportIterator {
    type Item = io::Result<Report>;

    fn next(&mut self) -> Option<Self::Item> {
        ReportIterator::next(self).transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub name: OsString,
    /// Modification time in nanoseconds since the epoch.
    pub mtime: i128,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir {
        path: PathBuf,
    }

    impl TmpDir {
        fn new() -> TmpDir {
            let mut rand = [0u8; 8];
            getrandom::fill(&mut rand).unwrap();
            let name: String = rand.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("ghostty-crash-dir-test-{name}"));
            fs::create_dir_all(&path).unwrap();
            TmpDir { path }
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn default_dir_uses_xdg_state_home() {
        let mut env = EnvironMap::new();
        env.insert("XDG_STATE_HOME".into(), "/xdg/state".into());
        env.insert("HOME".into(), "/home/test".into());
        let dir = default_dir(&env).unwrap();
        assert_eq!(PathBuf::from("/xdg/state/ghostty/crash"), dir.path);
    }

    #[test]
    fn default_dir_falls_back_to_home() {
        let mut env = EnvironMap::new();
        env.insert("HOME".into(), "/home/test".into());
        let dir = default_dir(&env).unwrap();
        assert_eq!(
            PathBuf::from("/home/test/.local/state/ghostty/crash"),
            dir.path
        );

        // An empty XDG var is ignored.
        env.insert("XDG_STATE_HOME".into(), "".into());
        let dir = default_dir(&env).unwrap();
        assert_eq!(
            PathBuf::from("/home/test/.local/state/ghostty/crash"),
            dir.path
        );
    }

    #[test]
    fn iterator_missing_dir_is_empty() {
        let tmp = TmpDir::new();
        let dir = Dir {
            path: tmp.path.join("does-not-exist"),
        };
        let mut it = dir.iterator();
        assert!(it.next().unwrap().is_none());
        it.deinit();
    }

    #[test]
    fn iterator_lists_only_files() {
        let tmp = TmpDir::new();
        fs::write(tmp.path.join("a.ghosttycrash"), b"a").unwrap();
        fs::write(tmp.path.join("b.ghosttycrash"), b"b").unwrap();
        fs::create_dir(tmp.path.join("subdir")).unwrap();

        let dir = Dir {
            path: tmp.path.clone(),
        };
        let mut names: Vec<OsString> = Vec::new();
        let mut it = dir.iterator();
        while let Some(report) = it.next().unwrap() {
            let meta = fs::metadata(tmp.path.join(&report.name)).unwrap();
            let expected = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
            assert_eq!(expected, report.mtime);
            assert!(report.mtime > 0);
            names.push(report.name);
        }
        names.sort();
        assert_eq!(
            vec![
                OsString::from("a.ghosttycrash"),
                OsString::from("b.ghosttycrash")
            ],
            names
        );
    }
}
