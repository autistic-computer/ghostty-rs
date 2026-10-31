//! Port of `src/config/ErrorList.zig`.
//!
//! STATUS: ported

use crate::cli::args::StringZ;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: StringZ,
}

/// The list of errors. (Zig allocates these with the arena associated with
/// the config structure; here the list owns them.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorList {
    /// The list of errors.
    pub list: Vec<Error>,
}

impl ErrorList {
    /// True if there are no errors.
    pub fn empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Add a new error to the list.
    pub fn add(&mut self, err: Error) {
        self.list.push(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_list() {
        let mut list = ErrorList::default();
        assert!(list.empty());
        list.add(Error {
            message: "bad".into(),
        });
        assert!(!list.empty());
        assert_eq!("bad", list.list[0].message.as_str());
    }
}
