//! The hybrid logical clock (docs/design/dn-sync.md §4.2, §4.3).
//!
//! An HLC time is 64 bits: 48 bits of Unix milliseconds and a 16-bit counter. It breaks ties
//! between concurrent writes and orders items for display. It never decides "before" or
//! "after": change numbers and vectors do that (§4.1).

/// Changes more than this far ahead of the receiver's clock are held, not applied (§4.3).
pub const MAX_AHEAD_MS: u64 = 10 * 60 * 1000;

/// One HLC time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hlc(pub u64);

impl Hlc {
    pub fn from_parts(ms: u64, counter: u16) -> Self {
        Self(((ms & 0xFFFF_FFFF_FFFF) << 16) | u64::from(counter))
    }

    /// The Unix milliseconds part.
    pub fn millis(self) -> u64 {
        self.0 >> 16
    }

    pub fn counter(self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    /// Whether this time is more than [`MAX_AHEAD_MS`] ahead of `now_ms`.
    pub fn too_far_ahead(self, now_ms: u64) -> bool {
        self.millis() > now_ms.saturating_add(MAX_AHEAD_MS)
    }
}

/// A device's clock: the last time it used or saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlcClock {
    pub last: Hlc,
}

impl HlcClock {
    /// The time for a new local change: later than anything used or seen, and at least now.
    pub fn send(&mut self, now_ms: u64) -> Hlc {
        let wall = Hlc::from_parts(now_ms, 0);
        let next = Hlc(self.last.0.saturating_add(1)).max(wall);
        self.last = next;
        next
    }

    /// Takes a received time into account. A time too far ahead is refused, and the clock
    /// doesn't move (§4.3): the change waits until the receiver's clock catches up.
    pub fn receive(&mut self, remote: Hlc, now_ms: u64) -> Result<(), Hlc> {
        if remote.too_far_ahead(now_ms) {
            return Err(remote);
        }
        self.last = self.last.max(remote);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_only_go_forward() {
        let mut c = HlcClock::default();
        let a = c.send(1_000);
        let b = c.send(1_000);
        let d = c.send(999);
        assert!(a < b && b < d);
        assert_eq!(a.millis(), 1_000);
        assert_eq!((b.millis(), b.counter()), (1_000, 1));
        assert_eq!(d.millis(), 1_000, "a clock that went back doesn't go back");
        assert_eq!(c.send(2_000), Hlc::from_parts(2_000, 0));
    }

    #[test]
    fn receiving_moves_the_clock_unless_the_time_is_too_far_ahead() {
        let mut c = HlcClock::default();
        c.receive(Hlc::from_parts(5_000, 3), 4_000).unwrap();
        assert!(c.send(4_000) > Hlc::from_parts(5_000, 3));
        let future = Hlc::from_parts(4_000 + MAX_AHEAD_MS + 1, 0);
        let before = c.last;
        assert_eq!(c.receive(future, 4_000), Err(future));
        assert_eq!(c.last, before);
        assert!(!Hlc::from_parts(4_000 + MAX_AHEAD_MS, 0).too_far_ahead(4_000));
    }
}
