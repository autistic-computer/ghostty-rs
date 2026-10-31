//! Port of `src/terminal/c/result.zig`.
//!
//! STATUS: ported

crate::c_api_enum! {
    /// C: GhosttyResult
    pub enum Result {
        Success = 0,
        OutOfMemory = -1,
        InvalidValue = -2,
        OutOfSpace = -3,
        NoValue = -4,
        IoError = -5,
        LimitExceeded = -6,
        Rejected = -7,
    }
}
