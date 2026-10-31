//! Port of `src/os/wasm.zig`.
//!
//! STATUS: ported
//!
//! This file contains helpers for wasm compilation.
//!
//! Like in Zig, the module can be imported on any target (e.g. for
//! [`target::Target`]); only `log` is wasm32-only.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "wasm/log.rs"]
pub mod log;
#[path = "wasm/target.rs"]
pub mod target;
// @generated-mods-end

/// True if we're in shared memory mode. If true, then the memory buffer
/// in JS will be backed by a SharedArrayBuffer and some behaviors change.
///
/// NOTE(port): Zig reads the `wasm_shared` build option; the Rust port has
/// no build-options step (PORTING.md), so this is its default (true).
pub const SHARED_MEM: bool = true;

// Zig: `pub const alloc = std.heap.wasm_allocator`. The return values of
// that allocator should NOT be sent to the host environment unless
// toHostOwned is called on them. In Rust the global allocator is used for
// all allocations; host-owned memory goes through
// `crate::lib::allocator::wasm`.
