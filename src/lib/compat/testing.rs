//! Port of `src/lib/compat/testing.zig`.
//!
//! STATUS: ported
//!
//! Code taken from Zig 0.15.2 `std.testing`. See README.md for license and
//! details.
//!
//! Zig only semantically analyzes declarations that are referenced, so
//! `refAllDeclsRecursive` forces analysis of every declaration in a type.
//! Rust type-checks every item unconditionally, so the port is a no-op kept
//! for API parity.

/// Given a type, recursively references all the declarations inside, so
/// that the semantic analyzer sees them. A no-op in Rust (see module docs).
pub fn ref_all_decls_recursive<T: ?Sized>() {}

#[cfg(test)]
mod tests {
    #[test]
    fn ref_all_decls_recursive_is_noop() {
        super::ref_all_decls_recursive::<str>();
    }
}
