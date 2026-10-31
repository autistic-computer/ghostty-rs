//! Port of `src/terminal/compress.zig`.
//!
//! STATUS: ported
//!
//! Compression primitives used by the terminal.
//!
//! This namespace contains only the representation and codecs. Policy about
//! which pages to compress, when to decommit their resident memory, and when
//! to restore them belongs to `PageList`.
//!
//! Zig re-exports `lz4` (the raw LZ4 block codec used for terminal page
//! memory) and `Page` (a compressed terminal page which retains its
//! original virtual mapping); in Rust those are the `lz4` module and
//! [`Page`] below.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "compress/lz4.rs"]
pub mod lz4;
#[path = "compress/lz4_differential.rs"]
pub mod lz4_differential;
#[path = "compress/page.rs"]
pub mod page;
// @generated-mods-end

/// A compressed terminal page which retains its original virtual mapping.
pub use self::page::Page;
