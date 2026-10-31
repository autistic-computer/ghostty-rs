//! Port of `src/terminal/kitty/clipboard_write.zig`.
//!
//! STATUS: ported
//!
//! Kitty clipboard protocol (OSC 5522) write transactions: the
//! stateful accumulation of wdata chunks and walias aliases until the
//! commit packet arrives. See clipboard.zig for the protocol overview.
//!
//! Deviation: the transaction arena is replaced by owned `Vec`s; the
//! "repeated alias updates do not grow memory" test checks the alias
//! list's capacity instead of the arena's.

use std::fmt;

use crate::simd::base64 as simd_base64;
use crate::terminal::clipboard;
use crate::terminal::osc::parsers::kitty_clipboard_protocol::Operation;

use super::clipboard_command::{Metadata, Payload, MAX_MIME_LEN};

/// Default maximum total decoded bytes accumulated by one write
/// transaction. Embedders can override this per-transaction via
/// WriteState.Options (Ghostty exposes it as the
/// `clipboard-write-limit-bytes` configuration).
pub const MAX_WRITE_SIZE: usize = 64 * 1024 * 1024;

/// Maximum MIME types and aliases per write transaction.
pub const MAX_WRITE_MIMES: usize = 64;
pub const MAX_WRITE_ALIASES: usize = 64;

/// One MIME representation of committed clipboard data. This is the
/// same type the clipboard write effect consumes so committed contents
/// can be passed through directly.
pub use clipboard::Content;

/// Error from [`WriteState::data`] (`error{TooLarge, Invalid}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataError {
    TooLarge,
    Invalid,
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DataError::TooLarge => "TooLarge",
            DataError::Invalid => "Invalid",
        })
    }
}

impl std::error::Error for DataError {}

/// Error from [`WriteState::alias`] and [`WriteState::commit`]
/// (`error{Invalid}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidError {
    Invalid,
}

impl fmt::Display for InvalidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid")
    }
}

impl std::error::Error for InvalidError {}

impl From<InvalidError> for DataError {
    fn from(_: InvalidError) -> DataError {
        DataError::Invalid
    }
}

/// The state of one in-flight write transaction: a single `type=write`
/// plus all the `wdata` chunks and `walias` aliases until completion
/// or error.
#[derive(Debug, Clone)]
pub struct WriteState {
    pub loc: clipboard::Location,
    pub id: Vec<u8>,
    pub pw: Vec<u8>,
    pub name: Vec<u8>,
    spool: Vec<u8>,
    entries: Vec<Entry>,
    aliases: Vec<Alias>,

    /// Maximum total decoded bytes this transaction will accumulate.
    /// Captured at init so a change to the configured limit doesn't
    /// apply to a transaction already in flight.
    pub max_size: usize,

    /// Index into entries currently receiving data.
    current: Option<usize>,

    /// Decodes the concatenated payload stream of the entry currently
    /// receiving data. Per the spec's "Encoding of payloads" section,
    /// individual wdata packet payloads split one base64 stream at
    /// arbitrary boundaries; only the concatenation per MIME type must
    /// be valid, correctly padded base64.
    decoder: simd_base64::Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Maximum total decoded bytes accumulated by the transaction.
    pub max_size: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            max_size: MAX_WRITE_SIZE,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    mime: Vec<u8>,
    start: usize,
    len: usize,
}

#[derive(Debug, Clone)]
struct Alias {
    alias: Vec<u8>,

    /// The target is stored inline because it can be overwritten by
    /// any number of walias packets, and the arena can't reclaim
    /// superseded copies.
    target_buf: [u8; MAX_MIME_LEN],
    target_len: usize,
}

impl Alias {
    fn target(&self) -> &[u8] {
        &self.target_buf[..self.target_len]
    }

    fn set_target(&mut self, v: &[u8]) {
        debug_assert!(v.len() <= MAX_MIME_LEN);
        self.target_buf[..v.len()].copy_from_slice(v);
        self.target_len = v.len();
    }
}

/// The result of a committed transaction. All slices borrow the
/// WriteState's memory and are valid until it is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed<'a> {
    pub loc: clipboard::Location,
    pub id: &'a [u8],
    pub pw: &'a [u8],
    pub name: &'a [u8],
    pub contents: Vec<Content<'a>>,
}

impl WriteState {
    /// Begin a transaction from a type=write packet.
    pub fn init(meta: &Metadata, opts: Options) -> WriteState {
        debug_assert!(meta.op == Operation::Write);
        WriteState {
            loc: meta.loc,
            id: meta.id.clone(),
            pw: meta.pw.clone(),
            name: meta.name.clone(),
            spool: Vec::new(),
            entries: Vec::new(),
            aliases: Vec::new(),
            max_size: opts.max_size,
            current: None,
            decoder: simd_base64::Streaming::default(),
        }
    }

    /// Accumulate one wdata chunk carrying data for meta.mime (which
    /// must be non-empty; an empty mime is a commit, not data).
    ///
    /// Returns DataError::TooLarge when the transaction exceeds max_size
    /// and DataError::Invalid when the payload stream is not valid base64.
    /// The caller must fail the whole transaction with EFBIG or EINVAL
    /// respectively and abort it, as required by the protocol.
    pub fn data(&mut self, meta: &Metadata, payload: &[u8]) -> Result<(), DataError> {
        debug_assert!(meta.op == Operation::Wdata);
        debug_assert!(!meta.mime.is_empty());
        // Switch the receiving entry if this chunk is for a different
        // MIME type than the last one.
        'entry: {
            if let Some(idx) = self.current {
                if self.entries[idx].mime == meta.mime {
                    break 'entry;
                }

                // Finalize the previous region. Its concatenated
                // stream must have ended on a complete base64 group,
                // otherwise the data was not correctly padded.
                self.finish_current()?;
                let entry = &mut self.entries[idx];
                entry.len = self.spool.len() - entry.start;
            }

            // Re-using an earlier MIME type starts a fresh region,
            // overwriting the previous mapping.
            for (idx, entry) in self.entries.iter_mut().enumerate() {
                if entry.mime == meta.mime {
                    entry.start = self.spool.len();
                    entry.len = 0;
                    self.current = Some(idx);
                    break 'entry;
                }
            }

            if self.entries.len() >= MAX_WRITE_MIMES {
                log::warn!(
                    target: "kitty_clipboard",
                    "clipboard write has too many MIME types, ignoring mime={}",
                    String::from_utf8_lossy(&meta.mime)
                );
                self.current = None;
                return Ok(());
            }

            self.entries.push(Entry {
                mime: meta.mime.clone(),
                start: self.spool.len(),
                len: 0,
            });
            self.current = Some(self.entries.len() - 1);
        }

        // The payloads for one MIME region concatenate into a single
        // strict base64 stream, decoded directly into the spool's
        // unused capacity. Invalid data aborts the transaction; per
        // the spec it must not be silently discarded "since that
        // turns corrupted data into apparently valid data".
        let old_len = self.spool.len();
        self.spool.resize(old_len + self.decoder.max_len(payload), 0);
        let decoded_len = match self.decoder.feed(payload, &mut self.spool[old_len..]) {
            Ok(decoded) => decoded.len(),
            Err(_) => {
                self.spool.truncate(old_len);
                return Err(DataError::Invalid);
            }
        };

        // The limit covers all decoded data in the transaction. Going
        // over it aborts the entire write; partial clipboard contents
        // must never reach the embedder.
        let remaining = self.max_size.saturating_sub(old_len);
        if decoded_len > remaining {
            self.spool.truncate(old_len);
            return Err(DataError::TooLarge);
        }
        self.spool.truncate(old_len + decoded_len);
        Ok(())
    }

    /// Finish the decode stream of the entry currently receiving
    /// data. Returns InvalidError::Invalid when the stream ends mid-group,
    /// i.e. the concatenated payload was not correctly padded.
    fn finish_current(&mut self) -> Result<(), InvalidError> {
        self.decoder.finish().map_err(|_| InvalidError::Invalid)
    }

    /// Register aliases from a walias packet: meta.mime is the target
    /// (the type that carries data) and the payload is a base64-encoded,
    /// whitespace-separated list of aliases. Returns InvalidError::Invalid
    /// for an undecodable payload, which aborts the transaction with EINVAL.
    pub fn alias(&mut self, meta: &Metadata, payload: &[u8]) -> Result<(), InvalidError> {
        debug_assert!(meta.op == Operation::Walias);
        debug_assert!(!meta.mime.is_empty());
        let decoded = Payload::init(payload).map_err(|_| InvalidError::Invalid)?;
        if !decoded.is_valid_utf8() {
            return Err(InvalidError::Invalid);
        }

        // Associate the aliases
        for name in decoded.mime_iterator() {
            if name.len() > MAX_MIME_LEN {
                continue;
            }

            // A repeated alias overwrites its previous target.
            if let Some(a) = self.aliases.iter_mut().find(|a| a.alias == name) {
                a.set_target(&meta.mime);
            } else {
                if self.aliases.len() >= MAX_WRITE_ALIASES {
                    log::warn!(
                        target: "kitty_clipboard",
                        "clipboard write has too many aliases, ignoring"
                    );
                    return Ok(());
                }

                let mut a = Alias {
                    alias: name.to_vec(),
                    target_buf: [0; MAX_MIME_LEN],
                    target_len: 0,
                };
                a.set_target(&meta.mime);
                self.aliases.push(a);
            }
        }
        Ok(())
    }

    /// Commit the transaction (a wdata packet without a MIME type).
    /// The caller must use the result and then drop this state. Returns
    /// InvalidError::Invalid when the last region's concatenated payload
    /// was not correctly padded; the caller must fail the transaction
    /// with EINVAL and abort it.
    pub fn commit(&mut self) -> Result<Committed<'_>, InvalidError> {
        // Finalize the region receiving data.
        if let Some(idx) = self.current {
            self.finish_current()?;
            let entry = &mut self.entries[idx];
            entry.len = self.spool.len() - entry.start;
            self.current = None;
        }

        let this: &WriteState = self;

        // Resolve the final MIME map: entries in arrival order, then
        // aliases applied sequentially against the evolving map so
        // chained aliases work like kitty's dict iteration. An alias
        // whose target has no mapping is dropped; an alias colliding
        // with an existing name overwrites it.
        let mut contents: Vec<Content<'_>> =
            Vec::with_capacity(this.entries.len() + this.aliases.len());

        for entry in &this.entries {
            contents.push(Content {
                mime: &entry.mime,
                data: &this.spool[entry.start..][..entry.len],
            });
        }

        for a in &this.aliases {
            let Some(target) = contents.iter().find(|c| c.mime == a.target()).copied() else {
                continue;
            };

            if let Some(c) = contents.iter_mut().find(|c| c.mime == a.alias.as_slice()) {
                c.data = target.data;
            } else {
                contents.push(Content {
                    mime: &a.alias,
                    data: target.data,
                });
            }
        }

        Ok(Committed {
            loc: this.loc,
            id: &this.id,
            pw: &this.pw,
            name: &this.name,
            contents,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(op: Operation) -> Metadata {
        Metadata::new(op)
    }

    fn wdata(mime: &[u8]) -> Metadata {
        Metadata {
            mime: mime.to_vec(),
            ..Metadata::new(Operation::Wdata)
        }
    }

    fn walias(mime: &[u8]) -> Metadata {
        Metadata {
            mime: mime.to_vec(),
            ..Metadata::new(Operation::Walias)
        }
    }

    fn begin(max_size: Option<usize>) -> WriteState {
        let opts = match max_size {
            Some(max_size) => Options { max_size },
            None => Options::default(),
        };
        WriteState::init(&meta(Operation::Write), opts)
    }

    #[test]
    fn write_basic_transaction() {
        let begin_meta = Metadata {
            id: b"42".to_vec(),
            ..Metadata::new(Operation::Write)
        };
        let mut state = WriteState::init(&begin_meta, Options::default());

        state.data(&wdata(b"text/plain"), b"R2hvc3R0eQ==").unwrap(); // "Ghostty"

        let committed = state.commit().unwrap();
        assert_eq!(b"42".as_slice(), committed.id);
        assert!(committed.loc == clipboard::Location::STANDARD);
        assert_eq!(1, committed.contents.len());
        assert_eq!(b"text/plain".as_slice(), committed.contents[0].mime);
        assert_eq!(b"Ghostty".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_chunked_data_accumulates() {
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"SGVsbG8=").unwrap(); // "Hello"
        state.data(&wdata(b"text/plain"), b"V29ybGQ=").unwrap(); // "World"

        let committed = state.commit().unwrap();
        assert_eq!(b"HelloWorld".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_multiple_mimes_in_order() {
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"YQ==").unwrap(); // "a"
        state.data(&wdata(b"text/html"), b"Yg==").unwrap(); // "b"

        let committed = state.commit().unwrap();
        assert_eq!(2, committed.contents.len());
        assert_eq!(b"text/plain".as_slice(), committed.contents[0].mime);
        assert_eq!(b"a".as_slice(), committed.contents[0].data);
        assert_eq!(b"text/html".as_slice(), committed.contents[1].mime);
        assert_eq!(b"b".as_slice(), committed.contents[1].data);
    }

    #[test]
    fn write_reused_mime_overwrites() {
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"YQ==").unwrap(); // "a"
        state.data(&wdata(b"text/html"), b"Yg==").unwrap(); // "b"
        state.data(&wdata(b"text/plain"), b"Yw==").unwrap(); // "c"

        let committed = state.commit().unwrap();
        assert_eq!(2, committed.contents.len());
        // Position preserved, data replaced.
        assert_eq!(b"text/plain".as_slice(), committed.contents[0].mime);
        assert_eq!(b"c".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_invalid_base64_chunk_aborts_the_transaction() {
        // Invalid characters anywhere in the stream are DataError::Invalid,
        // which the handlers turn into an EINVAL abort. These mirror
        // kitty's own tests for the spec change.
        let invalid_payloads: [&[u8]; 3] = [
            b"!!!",
            b"SGVs!!!bG8=",
            b"\nZGF0YSB3aXRoIGEgbmV3bGluZQ==",
        ];
        for payload in invalid_payloads {
            let mut state = begin(None);
            assert_eq!(
                Err(DataError::Invalid),
                state.data(&wdata(b"text/plain"), payload)
            );
        }

        // Also after an earlier valid chunk for the same MIME type.
        {
            let mut state = begin(None);
            state.data(&wdata(b"text/plain"), b"Z29vZA==").unwrap(); // "good"
            assert_eq!(
                Err(DataError::Invalid),
                state.data(&wdata(b"text/plain"), b"SGVs!!!bG8=")
            );
        }
    }

    #[test]
    fn write_chunks_split_one_base64_stream_at_arbitrary_boundaries() {
        // The concatenation of all payloads per MIME type is the base64
        // stream; individual packets need not be a multiple of four bytes.
        let encoded: &[u8] = b"c29tZSBkYXRh"; // "some data"
        let splits: [&[usize]; 2] = [&[3, 7], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]];
        for split in splits {
            let mut state = begin(None);

            let mut prev: usize = 0;
            for &end in split {
                state
                    .data(&wdata(b"text/plain"), &encoded[prev..end])
                    .unwrap();
                prev = end;
            }
            state.data(&wdata(b"text/plain"), &encoded[prev..]).unwrap();

            let committed = state.commit().unwrap();
            assert_eq!(b"some data".as_slice(), committed.contents[0].data);
        }
    }

    #[test]
    fn write_incorrectly_padded_stream_aborts_at_commit() {
        let mut state = begin(None);

        // "Hello" without its final padding byte: every chunk decodes,
        // but the stream ends mid-group so the commit reports it.
        state.data(&wdata(b"text/plain"), b"SGVsbG8").unwrap();
        assert_eq!(Err(InvalidError::Invalid), state.commit().map(|_| ()));
    }

    #[test]
    fn write_incorrectly_padded_stream_aborts_at_mime_switch() {
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"SGVsbG8").unwrap();
        assert_eq!(
            Err(DataError::Invalid),
            state.data(&wdata(b"text/html"), b"PGI+aGk8L2I+")
        );
    }

    #[test]
    fn write_independently_padded_chunks_accumulate() {
        // A packet payload ending exactly at terminal padding resets the
        // stream, so clients that base64-encode every chunk independently
        // keep working (matching kitty, which resets its streaming
        // decoder on EOF). Padding followed by more data within a single
        // packet stays invalid.
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"Z29vZA==").unwrap(); // "good"
        state.data(&wdata(b"text/plain"), b"bW9yZQ==").unwrap(); // "more"

        let committed = state.commit().unwrap();
        assert_eq!(b"goodmore".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_data_after_padding_within_one_chunk_aborts() {
        let mut state = begin(None);

        assert_eq!(
            Err(DataError::Invalid),
            state.data(&wdata(b"text/plain"), b"Z29vZA==bW9yZQ==")
        );
    }

    #[test]
    fn write_aliases_resolve_at_commit() {
        let mut state = begin(None);

        state.data(&wdata(b"text/plain"), b"R2hvc3R0eQ==").unwrap(); // "Ghostty"

        // Alias "TEXT UTF8_STRING" -> text/plain.
        state
            .alias(&walias(b"text/plain"), b"VEVYVCBVVEY4X1NUUklORw==")
            .unwrap();

        let committed = state.commit().unwrap();
        assert_eq!(3, committed.contents.len());
        assert_eq!(b"TEXT".as_slice(), committed.contents[1].mime);
        assert_eq!(b"Ghostty".as_slice(), committed.contents[1].data);
        assert_eq!(b"UTF8_STRING".as_slice(), committed.contents[2].mime);
        assert_eq!(b"Ghostty".as_slice(), committed.contents[2].data);
    }

    #[test]
    fn write_repeated_alias_updates_do_not_grow_memory() {
        let mut state = begin(Some(0));

        // Alias "a" -> a max-length target.
        let target = [b'x'; MAX_MIME_LEN];
        let alias_meta = walias(&target);
        state.alias(&alias_meta, b"YQ==").unwrap();
        let capacity = state.aliases.capacity();

        // Overwriting the same alias must not retain superseded targets.
        for _ in 0..1000 {
            state.alias(&alias_meta, b"YQ==").unwrap();
        }
        assert_eq!(capacity, state.aliases.capacity());
        assert_eq!(1, state.aliases.len());
        assert_eq!(target.as_slice(), state.aliases[0].target());
    }

    #[test]
    fn write_alias_payload_must_be_valid_utf8() {
        let mut state = begin(None);

        // Valid base64 encoding of a single 0xff byte.
        assert_eq!(
            Err(InvalidError::Invalid),
            state.alias(&walias(b"text/plain"), b"/w==")
        );
    }

    #[test]
    fn write_default_limit_when_unset() {
        let state = begin(None);
        assert_eq!(MAX_WRITE_SIZE, state.max_size);
    }

    #[test]
    fn write_text_over_limit_rejects_transaction() {
        let mut state = begin(Some(8));

        assert_eq!(
            Err(DataError::TooLarge),
            state.data(&wdata(b"text/plain"), b"SGVsbG9Xb3JsZA==") // "HelloWorld"
        );
    }

    #[test]
    fn write_text_crossing_limit_across_chunks_rejects_transaction() {
        let mut state = begin(Some(8));

        state.data(&wdata(b"text/plain"), b"SGVsbG8=").unwrap(); // "Hello"
        assert_eq!(
            Err(DataError::TooLarge),
            state.data(&wdata(b"text/plain"), b"V29ybGQ=") // "World"
        );
    }

    #[test]
    fn write_data_exactly_at_limit_is_accepted() {
        let mut state = begin(Some(5));

        state.data(&wdata(b"text/plain"), b"SGVsbG8=").unwrap(); // "Hello"

        let committed = state.commit().unwrap();
        assert_eq!(b"Hello".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_non_text_under_limit_is_unaffected() {
        let mut state = begin(Some(8));

        state.data(&wdata(b"image/png"), b"iVBORw==").unwrap(); // "\x89PNG"

        let committed = state.commit().unwrap();
        assert_eq!(b"\x89PNG".as_slice(), committed.contents[0].data);
    }

    #[test]
    fn write_non_text_over_limit_rejects_transaction() {
        let mut state = begin(Some(8));

        assert_eq!(
            Err(DataError::TooLarge),
            state.data(&wdata(b"image/png"), b"SGVsbG9Xb3JsZA==") // "HelloWorld"
        );
    }

    #[test]
    fn write_total_data_across_mime_types_is_limited() {
        let mut state = begin(Some(8));

        state.data(&wdata(b"text/plain"), b"SGVsbG8=").unwrap(); // "Hello"

        assert_eq!(
            Err(DataError::TooLarge),
            state.data(&wdata(b"image/png"), b"V29ybGQ=") // "World"
        );
    }

    #[test]
    fn write_alias_without_data_target_is_dropped() {
        let mut state = begin(None);

        state.alias(&walias(b"text/plain"), b"VEVYVA==").unwrap(); // "TEXT"

        let committed = state.commit().unwrap();
        assert_eq!(0, committed.contents.len());
    }
}
