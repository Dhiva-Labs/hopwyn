use std::fmt;
use std::time::{Duration, Instant};

/// How long "Add device" stays open.
pub const WINDOW: Duration = Duration::from_secs(120);
/// Wrong codes allowed before lockout.
pub const MAX_FAILURES: u8 = 3;
/// How long pairing stays locked after too many wrong codes.
pub const LOCKOUT: Duration = Duration::from_secs(300);

/// Why a pairing attempt may not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimiterError {
    /// "Add device" is not open, so pairing requests are ignored.
    WindowClosed,
    /// Too many wrong codes; try again after the remaining time.
    LockedOut { remaining: Duration },
}

impl fmt::Display for LimiterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowClosed => f.write_str("pairing is not open on this device"),
            Self::LockedOut { remaining } => {
                write!(
                    f,
                    "too many wrong codes, try again in {} s",
                    remaining.as_secs().max(1)
                )
            }
        }
    }
}

impl std::error::Error for LimiterError {}

/// Enforces the pairing window and the wrong-code lockout.
///
/// Time is passed in so the rules can be tested without waiting.
#[derive(Debug, Default)]
pub struct PairingLimiter {
    window_until: Option<Instant>,
    failures: u8,
    locked_until: Option<Instant>,
}

impl PairingLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The user opened "Add device".
    pub fn open(&mut self, now: Instant) {
        self.window_until = Some(now + WINDOW);
    }

    /// The user closed "Add device", or pairing finished.
    pub fn close(&mut self) {
        self.window_until = None;
    }

    /// Call before starting each pairing attempt.
    pub fn check(&mut self, now: Instant) -> Result<(), LimiterError> {
        if let Some(until) = self.locked_until {
            if now < until {
                return Err(LimiterError::LockedOut {
                    remaining: until - now,
                });
            }
            self.locked_until = None;
            self.failures = 0;
        }
        match self.window_until {
            Some(until) if now < until => Ok(()),
            _ => {
                self.window_until = None;
                Err(LimiterError::WindowClosed)
            }
        }
    }

    /// Record a failed attempt (wrong code or tampering).
    pub fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= MAX_FAILURES {
            self.locked_until = Some(now + LOCKOUT);
            self.window_until = None;
        }
    }

    /// Record a successful pairing: resets the counter and closes the window.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_by_default() {
        let mut l = PairingLimiter::new();
        assert_eq!(l.check(Instant::now()), Err(LimiterError::WindowClosed));
    }

    #[test]
    fn window_expires_after_two_minutes() {
        let t0 = Instant::now();
        let mut l = PairingLimiter::new();
        l.open(t0);
        assert_eq!(l.check(t0 + Duration::from_secs(119)), Ok(()));
        assert_eq!(l.check(t0 + WINDOW), Err(LimiterError::WindowClosed));
    }

    #[test]
    fn three_failures_lock_for_five_minutes() {
        let t0 = Instant::now();
        let mut l = PairingLimiter::new();
        l.open(t0);
        for _ in 0..MAX_FAILURES {
            assert_eq!(l.check(t0), Ok(()));
            l.record_failure(t0);
        }
        assert_eq!(
            l.check(t0),
            Err(LimiterError::LockedOut { remaining: LOCKOUT })
        );
        // Reopening the window does not bypass the lockout.
        l.open(t0 + Duration::from_secs(10));
        assert!(matches!(
            l.check(t0 + Duration::from_secs(10)),
            Err(LimiterError::LockedOut { .. })
        ));
        // After the lockout the counter resets, but the window must be reopened.
        let later = t0 + LOCKOUT;
        assert_eq!(l.check(later), Err(LimiterError::WindowClosed));
        l.open(later);
        assert_eq!(l.check(later), Ok(()));
        l.record_failure(later);
        assert_eq!(l.check(later), Ok(()));
    }

    #[test]
    fn success_resets_and_closes() {
        let t0 = Instant::now();
        let mut l = PairingLimiter::new();
        l.open(t0);
        l.record_failure(t0);
        l.record_success();
        assert_eq!(l.check(t0), Err(LimiterError::WindowClosed));
        l.open(t0);
        l.record_failure(t0);
        l.record_failure(t0);
        assert_eq!(l.check(t0), Ok(()));
    }
}
