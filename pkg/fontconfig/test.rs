//! Port of `pkg/fontconfig/test.zig`.
//!
//! STATUS: ported

#[cfg(test)]
mod tests {
    use crate::pkg::fontconfig;

    #[test]
    fn fc_list() {
        let cfg = fontconfig::init_load_config_and_fonts();
        let pat = fontconfig::Pattern::create();
        let os = fontconfig::ObjectSet::create();
        let fs = cfg.font_list(pat, os);

        // Note: this is environmental, but in general we expect all our
        // testing environments to have at least one font.
        assert!(!fs.fonts().is_empty());

        // SAFETY: not used afterwards.
        unsafe {
            fs.destroy();
            os.destroy();
            pat.destroy();
            cfg.destroy();
        }
    }

    #[test]
    fn fc_match() {
        let cfg = fontconfig::init_load_config_and_fonts();

        let pat = fontconfig::Pattern::create();
        assert!(cfg.substitute_with_pat(pat, fontconfig::MatchKind::Pattern));
        pat.default_substitute();

        let result = cfg.font_sort(pat, false, None);

        let fs = fontconfig::FontSet::create();

        {
            let fonts = result.fs.fonts();
            assert!(!fonts.is_empty());
            for &font in fonts {
                let pat_prep = cfg.font_render_prepare(pat, font).unwrap();
                assert!(fs.add(pat_prep));
            }
            // SAFETY: not used afterwards.
            unsafe {
                result.fs.destroy();
                pat.destroy();
            }
        }

        {
            for font in fs.fonts() {
                let mut it = font.object_iterator();
                while it.next() {
                    assert!(!it.object().is_empty());
                    assert!(it.value_len() > 0);
                    let mut value_it = it.value_iterator();
                    while let Some(entry) = value_it.next() {
                        assert!(entry.value != fontconfig::Value::Unknown);
                    }
                }
            }
        }

        // Zig additionally `destroy()`s every font in `fs` before
        // `fs.destroy()`; FcFontSetDestroy already destroys the patterns it
        // owns, so that would be a double free here.
        // SAFETY: not used afterwards.
        unsafe {
            fs.destroy();
            cfg.destroy();
        }
    }
}
