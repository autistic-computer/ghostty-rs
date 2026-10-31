//! Port of `src/input/KeymapNoop.zig`.
//!
//! STATUS: ported
//!
//! A noop implementation of the keymap interface so that the embedded
//! library can compile on non-macOS platforms.

use std::convert::Infallible;

use super::key::Mods;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeymapNoop;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translation<'a> {
    pub text: &'a [u8],
    pub composing: bool,
    pub mods: Mods,
}

impl Default for Translation<'_> {
    fn default() -> Self {
        Translation {
            text: b"",
            composing: false,
            mods: Mods::EMPTY,
        }
    }
}

impl KeymapNoop {
    pub fn init() -> Result<KeymapNoop, Infallible> {
        Ok(KeymapNoop)
    }

    pub fn deinit(&self) {}

    pub fn reload(&mut self) -> Result<(), Infallible> {
        Ok(())
    }

    pub fn translate<'a>(
        &self,
        out: &'a mut [u8],
        state: &mut State,
        code: u16,
        mods: Mods,
    ) -> Result<Translation<'a>, Infallible> {
        let _ = (out, state, code, mods);
        Ok(Translation {
            text: b"",
            composing: false,
            ..Translation::default()
        })
    }
}
