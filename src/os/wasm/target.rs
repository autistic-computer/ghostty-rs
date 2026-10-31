//! Port of `src/os/wasm/target.zig`.
//!
//! STATUS: ported

/// The wasm target platform. This is used to toggle certain features
/// on and off since the standard triple target is often not specific
/// enough (i.e. we can't tell wasm32-freestanding is for browser or not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Browser,
}

/// Our specific target platform.
///
/// NOTE(port): Zig reads the `wasm_target` build option; the Rust port has
/// no build-options step (PORTING.md), so wasm32 uses its default
/// (`.browser`).
pub const TARGET: Option<Target> = if cfg!(target_arch = "wasm32") {
    Some(Target::Browser)
} else {
    None
};
