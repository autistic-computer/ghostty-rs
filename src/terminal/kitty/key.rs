//! Port of `src/terminal/kitty/key.zig`.
//!
//! STATUS: ported
//!
//! Kitty keyboard protocol support.

/// Stack for the key flags. This implements the push/pop behavior
/// of the CSI > u and CSI < u sequences. We implement the stack as
/// fixed size to avoid heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagStack {
    pub flags: [Flags; FlagStack::LEN],
    /// A u3 index; arithmetic on it wraps modulo `LEN`.
    pub idx: u8,
}

impl Default for FlagStack {
    fn default() -> Self {
        Self {
            flags: [Flags::DISABLED; FlagStack::LEN],
            idx: 0,
        }
    }
}

impl FlagStack {
    const LEN: usize = 8;

    /// Return the current stack value
    pub fn current(&self) -> Flags {
        self.flags[self.idx as usize]
    }

    /// Perform the "set" operation as described in the spec for
    /// the CSI = u sequence.
    pub fn set(&mut self, mode: SetMode, v: Flags) {
        let i = self.idx as usize;
        match mode {
            SetMode::Set => self.flags[i] = v,
            SetMode::Or => self.flags[i] = Flags::from_int(self.flags[i].int() | v.int()),
            SetMode::Not => self.flags[i] = Flags::from_int(self.flags[i].int() & !v.int()),
        }
    }

    /// Push a new set of flags onto the stack. If the stack is full
    /// then the oldest entry is evicted.
    pub fn push(&mut self, flags: Flags) {
        // Overflow and wrap around if we're full, which evicts
        // the oldest entry.
        self.idx = (self.idx.wrapping_add(1)) & 0b111;
        self.flags[self.idx as usize] = flags;
    }

    /// Pop `n` entries from the stack. This will just wrap around
    /// if `n` is greater than the amount in the stack.
    pub fn pop(&mut self, n: usize) {
        // If n is more than our length then we just reset the stack.
        // This also avoids a DoS vector where a malicious client
        // could send a huge number of pop commands to waste cpu.
        if n >= self.flags.len() {
            self.idx = 0;
            self.flags = [Flags::DISABLED; FlagStack::LEN];
            return;
        }

        for _ in 0..n {
            self.flags[self.idx as usize] = Flags::DISABLED;
            self.idx = (self.idx.wrapping_sub(1)) & 0b111;
        }
    }
}

/// The possible flags for the Kitty keyboard protocol.
///
/// `packed struct(u5)`: bit 0 is `disambiguate`, then `report_events`,
/// `report_alternates`, `report_all`, `report_associated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags {
    pub disambiguate: bool,
    pub report_events: bool,
    pub report_alternates: bool,
    pub report_all: bool,
    pub report_associated: bool,
}

impl Flags {
    /// Kitty keyboard protocol disabled (all flags off).
    pub const DISABLED: Flags = Flags {
        disambiguate: false,
        report_events: false,
        report_alternates: false,
        report_all: false,
        report_associated: false,
    };

    /// Sets all modes on.
    pub const TRUE: Flags = Flags {
        disambiguate: true,
        report_events: true,
        report_alternates: true,
        report_all: true,
        report_associated: true,
    };

    /// The packed u5 value (`@bitCast`).
    pub const fn int(self) -> u8 {
        (self.disambiguate as u8)
            | (self.report_events as u8) << 1
            | (self.report_alternates as u8) << 2
            | (self.report_all as u8) << 3
            | (self.report_associated as u8) << 4
    }

    /// Unpack from a u5 value (`@bitCast`); bits above 5 are ignored.
    pub const fn from_int(v: u8) -> Flags {
        Flags {
            disambiguate: v & 0b1 != 0,
            report_events: v & 0b10 != 0,
            report_alternates: v & 0b100 != 0,
            report_all: v & 0b1000 != 0,
            report_associated: v & 0b10000 != 0,
        }
    }
}

/// The possible modes for setting the key flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetMode {
    Set,
    Or,
    Not,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Make sure we the overflow works as expected
    #[test]
    fn flag_stack_overflow() {
        let mut stack = FlagStack::default();
        stack.idx = (stack.flags.len() - 1) as u8;
        stack.idx = stack.idx.wrapping_add(1) & 0b111;
        assert!(stack.idx == 0);

        stack.idx = 0;
        stack.idx = stack.idx.wrapping_sub(1) & 0b111;
        assert!(stack.idx as usize == stack.flags.len() - 1);
    }

    // Its easy to get packed struct ordering wrong so this test checks.
    #[test]
    fn flags_ordering() {
        assert_eq!(
            0b1u8,
            Flags {
                disambiguate: true,
                ..Default::default()
            }
            .int()
        );
        assert_eq!(
            0b10u8,
            Flags {
                report_events: true,
                ..Default::default()
            }
            .int()
        );
        assert_eq!(0b11111, Flags::TRUE.int());
    }

    #[test]
    fn flag_stack_push_pop() {
        let mut stack = FlagStack::default();
        stack.push(Flags {
            disambiguate: true,
            ..Default::default()
        });
        assert_eq!(
            Flags {
                disambiguate: true,
                ..Default::default()
            },
            stack.current()
        );

        stack.pop(1);
        assert_eq!(Flags::default(), stack.current());
    }

    #[test]
    fn flag_stack_pop_big_number() {
        let mut stack = FlagStack::default();
        stack.pop(100);
        assert_eq!(Flags::default(), stack.current());
    }

    #[test]
    fn flag_stack_set() {
        let mut stack = FlagStack::default();
        stack.set(
            SetMode::Set,
            Flags {
                disambiguate: true,
                ..Default::default()
            },
        );
        assert_eq!(
            Flags {
                disambiguate: true,
                ..Default::default()
            },
            stack.current()
        );

        stack.set(
            SetMode::Or,
            Flags {
                report_events: true,
                ..Default::default()
            },
        );
        assert_eq!(
            Flags {
                disambiguate: true,
                report_events: true,
                ..Default::default()
            },
            stack.current()
        );

        stack.set(
            SetMode::Not,
            Flags {
                report_events: true,
                ..Default::default()
            },
        );
        assert_eq!(
            Flags {
                disambiguate: true,
                ..Default::default()
            },
            stack.current()
        );
    }
}
