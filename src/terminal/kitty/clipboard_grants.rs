//! Port of `src/terminal/kitty/clipboard_grants.zig`.
//!
//! STATUS: ported
//!
//! Kitty clipboard protocol (OSC 5522) session password grants:
//! requests carrying a granted password skip the permission prompt, and
//! paste events mint one-time passwords.
//!
//! Deviation: `std.Io` is represented by its entropy source function
//! (`sys::IoRandomSecureFn`); `std.Io.failing` is a function that always
//! fails.

use crate::terminal::sys;

use super::clipboard_command::MAX_PW_LEN;

/// Session password grants, used to skip permission prompts for
/// requests carrying a known pw. Callers can choose to scope these
/// however they want, e.g. Kitty does it per window and the spec
/// doesn't demand anything.
#[derive(Debug, Clone, Default)]
pub struct Grants {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Read,
    Write,
}

#[derive(Debug, Clone)]
struct Entry {
    pw: Vec<u8>,
    read: bool,
    write: bool,
    one_time: bool,
}

impl Grants {
    const MAX_ENTRIES: usize = 32;

    /// Record a grant for pw. An existing grant for the same password
    /// gains the new direction.
    pub fn grant(&mut self, pw: &[u8], dir: Direction, one_time: bool) {
        if pw.is_empty() || pw.len() > MAX_PW_LEN {
            return;
        }

        let entry: &mut Entry = if let Some(idx) = self.find_index(pw) {
            let entry = &mut self.entries[idx];
            entry.one_time = entry.one_time && one_time;
            entry
        } else {
            // Evict the oldest grant once full.
            if self.entries.len() >= Self::MAX_ENTRIES {
                self.entries.remove(0);
            }

            self.entries.push(Entry {
                pw: pw.to_vec(),
                read: false,
                write: false,
                one_time,
            });
            self.entries.last_mut().unwrap()
        };

        match dir {
            Direction::Read => entry.read = true,
            Direction::Write => entry.write = true,
        }
    }

    /// Check whether pw grants the given direction. A one-time grant is
    /// consumed by this check even when the direction doesn't match,
    /// matching kitty's pop-on-check behavior.
    pub fn r#use(&mut self, pw: &[u8], dir: Direction) -> bool {
        if pw.is_empty() {
            return false;
        }
        let Some(idx) = self.find_index(pw) else {
            return false;
        };
        let entry = &self.entries[idx];
        let allowed = match dir {
            Direction::Read => entry.read,
            Direction::Write => entry.write,
        };
        if entry.one_time {
            self.entries.swap_remove(idx);
        }
        allowed
    }

    /// The number of recorded grants (Zig: `entries.items.len`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no grants are recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find_index(&self, pw: &[u8]) -> Option<usize> {
        self.entries.iter().position(|entry| entry.pw == pw)
    }
}

/// The length of a one-time password generated for paste events.
pub const OTP_LEN: usize = 22;

/// The one-time password alphabet. This matches kitty (alphanumeric
/// without easily-confused characters), but the spec doesn't demand
/// this.
pub const OTP_ALPHABET: &[u8] = b"23456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";

/// Generate a one-time password for a paste event.
///
/// The password is a secret: a program that learns it can read the
/// clipboard without a prompt. Entropy comes from `sys::RANDOM_SECURE`
/// if set, otherwise from the Io; see `sys::random_secure`.
pub fn generate_otp(io: sys::IoRandomSecureFn) -> Result<[u8; OTP_LEN], sys::RandomSecureError> {
    let mut result = [0u8; OTP_LEN];
    let mut len: usize = 0;
    while len < result.len() {
        let mut raw = [0u8; 2 * OTP_LEN];
        sys::random_secure(io, &mut raw)?;
        let limit = (u8::MAX as usize + 1) / OTP_ALPHABET.len() * OTP_ALPHABET.len();
        for byte in raw {
            if byte as usize >= limit {
                continue;
            }
            result[len] = OTP_ALPHABET[byte as usize % OTP_ALPHABET.len()];
            len += 1;
            if len == result.len() {
                break;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// `std.Io.failing`
    fn failing_io(_: &mut [u8]) -> Result<(), sys::RandomSecureError> {
        Err(sys::RandomSecureError::EntropyUnavailable)
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        sys::RANDOM_SECURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn grants_basic_grant_and_use() {
        let mut grants = Grants::default();
        assert!(!grants.r#use(b"pw1", Direction::Read));

        grants.grant(b"pw1", Direction::Read, false);
        assert!(grants.r#use(b"pw1", Direction::Read));
        // Persistent grants survive use.
        assert!(grants.r#use(b"pw1", Direction::Read));
        assert!(!grants.r#use(b"pw1", Direction::Write));
    }

    #[test]
    fn grants_one_time_consumed_on_check() {
        let mut grants = Grants::default();
        grants.grant(b"otp", Direction::Read, true);
        assert!(grants.r#use(b"otp", Direction::Read));
        assert!(!grants.r#use(b"otp", Direction::Read));
    }

    #[test]
    fn grants_one_time_consumed_even_on_direction_mismatch() {
        let mut grants = Grants::default();
        grants.grant(b"otp", Direction::Read, true);
        assert!(!grants.r#use(b"otp", Direction::Write));
        assert!(!grants.r#use(b"otp", Direction::Read));
    }

    #[test]
    fn grants_directions_are_independent() {
        let mut grants = Grants::default();
        grants.grant(b"pw", Direction::Read, false);
        grants.grant(b"pw", Direction::Write, false);
        assert!(grants.r#use(b"pw", Direction::Read));
        assert!(grants.r#use(b"pw", Direction::Write));
    }

    #[test]
    fn grants_capacity_evicts_the_oldest() {
        let mut grants = Grants::default();

        for i in 0..Grants::MAX_ENTRIES + 1 {
            let pw = format!("pw{i}");
            grants.grant(pw.as_bytes(), Direction::Read, false);
        }

        // The oldest grant was evicted; the newest survives.
        assert!(!grants.r#use(b"pw0", Direction::Read));
        let newest = format!("pw{}", Grants::MAX_ENTRIES);
        assert!(grants.r#use(newest.as_bytes(), Direction::Read));
    }

    #[test]
    fn generate_otp_length_and_alphabet_with_a_real_io() {
        let _guard = lock();

        let otp = generate_otp(sys::os_random_secure).unwrap();
        assert_eq!(OTP_LEN, otp.len());
        for c in otp {
            assert!(OTP_ALPHABET.contains(&c));
        }

        // Two passwords don't collide (a repeat would mean no entropy).
        let other = generate_otp(sys::os_random_secure).unwrap();
        assert_ne!(otp, other);
    }

    #[test]
    fn generate_otp_no_entropy_is_an_error_never_a_weak_password() {
        let _guard = lock();
        assert_eq!(
            Err(sys::RandomSecureError::EntropyUnavailable),
            generate_otp(failing_io)
        );
    }

    #[test]
    fn generate_otp_sys_override_supplies_entropy_without_an_io_source() {
        let _guard = lock();

        static COUNTER: AtomicU8 = AtomicU8::new(0);
        fn fill(buffer: &mut [u8]) -> Result<(), sys::RandomSecureError> {
            for b in buffer.iter_mut() {
                *b = COUNTER.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        }
        sys::set_random_secure(Some(fill));
        let otp = generate_otp(failing_io);
        sys::set_random_secure(None);

        let otp = otp.unwrap();
        for c in otp {
            assert!(OTP_ALPHABET.contains(&c));
        }
    }
}
