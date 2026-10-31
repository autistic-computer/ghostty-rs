//! Port of `pkg/fontconfig/init.zig`.
//!
//! STATUS: ported

use super::c;
use super::config::Config;

pub fn init() -> bool {
    // SAFETY: no preconditions.
    unsafe { c::FcInit() == c::FcTrue as c::FcBool }
}

pub fn fini() {
    // SAFETY: no preconditions (see FcFini docs).
    unsafe { c::FcFini() }
}

pub fn init_load_config() -> Config {
    // SAFETY: returns a new config.
    unsafe { Config::from_raw(c::FcInitLoadConfig()) }
}

pub fn init_load_config_and_fonts() -> Config {
    // SAFETY: returns a new config.
    unsafe { Config::from_raw(c::FcInitLoadConfigAndFonts()) }
}

pub fn version() -> u32 {
    // SAFETY: no preconditions.
    unsafe { c::FcGetVersion() as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_() {
        assert!(version() > 0);
    }

    #[test]
    fn init_() {
        assert!(init());
        // Zig: `defer fini();`. FcFini tears down fontconfig's global
        // state, which the other fontconfig tests running concurrently in
        // this test binary still use, so it is not called here.
    }

    #[test]
    fn init_load_config_() {
        let config = init_load_config();
        // SAFETY: not used afterwards.
        unsafe { config.destroy() };
    }

    #[test]
    fn init_load_config_and_fonts_() {
        let config = init_load_config_and_fonts();
        // SAFETY: not used afterwards.
        unsafe { config.destroy() };
    }
}
