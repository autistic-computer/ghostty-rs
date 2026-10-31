//! Port of `src/os/TempDir.zig`.
//!
//! STATUS: ported
//!
//! Creates a temporary directory at runtime that can be safely used to
//! store temporary data and is destroyed on deinit.
//!
//! Zig holds `std.Io.Dir` handles for the directory and its parent; the
//! Rust port holds open `std::fs::File` handles (directory fds) plus the
//! parent path, since `std::fs` operates on paths. Dropping a `TempDir`
//! is equivalent to `deinit` (close with [`CloseMode::Delete`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::file;

/// A temporary directory.
#[derive(Debug)]
pub struct TempDir {
    /// Dir is the directory handle
    pub dir: Option<fs::File>,

    /// Parent directory
    pub parent: Option<fs::File>,

    /// Path of the parent directory.
    parent_path: PathBuf,

    /// Name buffer that name points into. Generally do not use. To get the
    /// name call the name() function.
    name_buf: [u8; file::RANDOM_BASENAME_LEN],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseMode {
    Delete,
    Retain,
}

impl TempDir {
    /// Create the temporary directory.
    pub fn init() -> io::Result<TempDir> {
        let mut tmp_path_buf = [0u8; file::RANDOM_BASENAME_LEN];

        let parent_path = PathBuf::from(file::alloc_tmp_dir(&super::environ_map()));
        let parent = fs::File::open(&parent_path)?;

        // We now loop forever until we can find a directory that we can
        // create.
        loop {
            let tmp_path = file::random_basename(&mut tmp_path_buf)
                .map_err(|e| io::Error::other(e))?
                .to_string();

            match fs::create_dir(parent_path.join(&tmp_path)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }

            return Ok(TempDir {
                dir: Some(fs::File::open(parent_path.join(&tmp_path))?),
                parent: Some(parent),
                parent_path,
                name_buf: tmp_path_buf,
            });
        }
    }

    /// Name returns the name of the directory. This is just the basename
    /// and is not the full absolute path.
    pub fn name(&self) -> &str {
        // The name is always base64 (ASCII).
        std::str::from_utf8(&self.name_buf).unwrap()
    }

    /// The full path of the temporary directory (parent path + name).
    pub fn path(&self) -> PathBuf {
        self.parent_path.join(self.name())
    }

    /// The path of the parent directory.
    pub fn parent_path(&self) -> &Path {
        &self.parent_path
    }

    /// Finish with the temporary directory. This deletes all contents in
    /// the directory.
    pub fn deinit(mut self) {
        self.close_inner(CloseMode::Delete);
    }

    /// Close the directory handles, optionally retaining the temporary
    /// directory and its contents on disk.
    pub fn close(mut self, mode: CloseMode) {
        self.close_inner(mode);
    }

    fn close_inner(&mut self, mode: CloseMode) {
        // Already closed.
        let Some(parent) = self.parent.take() else {
            return;
        };
        drop(self.dir.take());
        match mode {
            CloseMode::Delete => {
                if let Err(err) = fs::remove_dir_all(self.path()) {
                    log::error!(target: "tempdir", "error deleting temp dir err={err}");
                }
            }
            CloseMode::Retain => {}
        }
        drop(parent);
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        self.close_inner(CloseMode::Delete);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir() {
        let path;
        {
            let td = TempDir::init().unwrap();

            let nameval = td.name();
            assert!(!nameval.is_empty());

            // Can open a new handle to it proves it exists.
            let dir = fs::File::open(td.parent_path().join(nameval)).unwrap();
            drop(dir);

            path = fs::canonicalize(td.path()).unwrap();

            // Don't check the raw handles after close: descriptor numbers
            // can be reused immediately, so a valid number does not imply
            // that we leaked it.
            td.deinit();
        }

        assert_eq!(
            io::ErrorKind::NotFound,
            fs::File::open(&path).unwrap_err().kind()
        );
    }

    #[test]
    fn close_retains_temporary_directory() {
        let path;
        {
            let td = TempDir::init().unwrap();
            path = fs::canonicalize(td.path()).unwrap();
            td.close(CloseMode::Retain);
        }

        let dir = fs::File::open(&path).unwrap();
        drop(dir);
        let _ = fs::remove_dir(&path);
    }
}
