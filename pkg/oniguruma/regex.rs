//! Port of `pkg/oniguruma/regex.zig`.
//!
//! STATUS: ported

use super::c;
use super::errors::{self, Error, ErrorInfo};
use super::match_param::MatchParam;
use super::region::Region;
use super::types::{self, Encoding, Syntax};

/// Owns a compiled Oniguruma regex; freed on drop (Zig: explicit `deinit`).
#[derive(Debug)]
pub struct Regex {
    pub value: c::OnigRegex,
}

// SAFETY: the regex is exclusively owned; Oniguruma regex objects may be
// used from any thread (one at a time).
unsafe impl Send for Regex {}

impl Regex {
    pub fn init(
        pattern: &[u8],
        options: types::Option,
        enc: &Encoding,
        syntax: &Syntax,
        err: Option<&mut ErrorInfo>,
    ) -> Result<Regex, Error> {
        let mut value: c::OnigRegex = std::ptr::null_mut();
        // SAFETY: pattern is valid for its length; ErrorInfo has the
        // layout of OnigErrorInfo.
        errors::convert_error(unsafe {
            c::onig_new(
                &mut value,
                pattern.as_ptr(),
                pattern.as_ptr().add(pattern.len()),
                options.int(),
                enc.cval(),
                syntax.cval(),
                err.map_or(std::ptr::null_mut(), |e| {
                    e as *mut ErrorInfo as *mut c::OnigErrorInfo
                }),
            )
        })?;
        Ok(Regex { value })
    }

    /// Search an entire string for matches. This always returns a region
    /// which may heap allocate (C allocator).
    pub fn search(&mut self, str: &[u8], options: types::Option) -> Result<Region, Error> {
        self.search_with_param(str, options, None)
    }

    /// Search an entire string for matches. This always returns a region
    /// which may heap allocate (C allocator).
    pub fn search_with_param(
        &mut self,
        str: &[u8],
        options: types::Option,
        match_param: Option<&mut MatchParam>,
    ) -> Result<Region, Error> {
        // As part of the searchAdvanced API call below, onig may allocate
        // memory even if we fail to match. The region frees it on drop if
        // there are any errors.
        let mut region = Region::default();
        self.search_advanced_with_param(str, 0, str.len(), &mut region, options, match_param)?;
        Ok(region)
    }

    /// onig_search directly
    pub fn search_advanced(
        &mut self,
        str: &[u8],
        start: usize,
        end: usize,
        region: &mut Region,
        options: types::Option,
    ) -> Result<usize, Error> {
        self.search_advanced_with_param(str, start, end, region, options, None)
    }

    /// onig_search_with_param directly
    pub fn search_advanced_with_param(
        &mut self,
        str: &[u8],
        start: usize,
        end: usize,
        region: &mut Region,
        options: types::Option,
        match_param: Option<&mut MatchParam>,
    ) -> Result<usize, Error> {
        assert!(start <= str.len() && end <= str.len());
        let s = str.as_ptr();
        let region = region as *mut Region as *mut c::OnigRegion;
        // SAFETY: all pointers are within `str`; Region has the layout of
        // OnigRegion.
        let pos = errors::convert_error(unsafe {
            match match_param {
                Some(param) => c::onig_search_with_param(
                    self.value,
                    s,
                    s.add(str.len()),
                    s.add(start),
                    s.add(end),
                    region,
                    options.int(),
                    param.value,
                ),
                None => c::onig_search(
                    self.value,
                    s,
                    s.add(str.len()),
                    s.add(start),
                    s.add(end),
                    region,
                    options.int(),
                ),
            }
        })?;

        Ok(pos as usize)
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: owned regex, freed exactly once.
        unsafe { c::onig_free(self.value) }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::ensure_init;
    use super::*;

    #[test]
    fn regex() {
        ensure_init().unwrap();
        let mut re = Regex::init(
            b"foo",
            types::Option::default(),
            Encoding::utf8(),
            Syntax::default(),
            None,
        )
        .unwrap();

        let reg = re
            .search(b"hello foo bar", types::Option::default())
            .unwrap();
        assert_eq!(1usize, reg.count());

        assert_eq!(
            Err(Error::Mismatch),
            re.search(b"hello", types::Option::default()).map(|_| ())
        );

        let mut match_param = MatchParam::init().unwrap();
        match_param.set_retry_limit_in_search(1000).unwrap();

        let reg_param = re
            .search_with_param(
                b"hello foo bar",
                types::Option::default(),
                Some(&mut match_param),
            )
            .unwrap();
        assert_eq!(1usize, reg_param.count());
    }
}
