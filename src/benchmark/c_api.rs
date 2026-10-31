//! Port of `src/benchmark/CApi.zig`.
//!
//! STATUS: ported

use std::ffi::{c_char, CStr};

use super::cli;
use crate::cli::args::CliEnum;

/// Run the Ghostty benchmark CLI with the given action and arguments.
///
/// # Safety
///
/// Both arguments must be valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn ghostty_benchmark_cli(
    action_name_: *const c_char,
    args: *const c_char,
) -> bool {
    // SAFETY: caller contract.
    let action_name = unsafe { CStr::from_ptr(action_name_) }.to_string_lossy();
    let Some(action) = <cli::Action as CliEnum>::from_tag(&action_name) else {
        log::warn!(target: "benchmark", "unknown action={}", action_name);
        return false;
    };

    // SAFETY: caller contract.
    let args = unsafe { CStr::from_ptr(args) }.to_string_lossy().into_owned();
    if let Err(err) = cli::main_action(action, cli::Args::String(args)) {
        log::warn!(
            target: "benchmark",
            "failed to run action={} err={}",
            action.tag_name(),
            err,
        );
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_cli() {
        // SAFETY: literals are NUL-terminated.
        unsafe {
            assert!(ghostty_benchmark_cli(c"terminal-parser".as_ptr(), c"".as_ptr()));
            assert!(!ghostty_benchmark_cli(c"nope".as_ptr(), c"".as_ptr()));
            assert!(!ghostty_benchmark_cli(
                c"terminal-parser".as_ptr(),
                c"--bogus=1".as_ptr()
            ));
        }
    }
}
