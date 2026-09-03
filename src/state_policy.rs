use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_secs(3);
pub const STALE_WINDOW_SECS: i64 = 10 * 60;
pub const FLYOUT_REFRESH_AFTER: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
pub struct ClockReading {
    pub monotonic: Instant,
    pub unix_seconds: i64,
}

pub trait Clock {
    fn read(&self) -> ClockReading;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn read(&self) -> ClockReading {
        ClockReading {
            monotonic: Instant::now(),
            unix_seconds: time::OffsetDateTime::now_utc().unix_timestamp(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshTrigger {
    Automatic,
    Flyout,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshGate {
    Ready,
    Cooldown { remaining: Duration },
    Debounced { remaining: Duration },
    InFlight,
}

pub fn refresh_gate(
    _trigger: RefreshTrigger,
    now: ClockReading,
    cooldown_until: Option<Instant>,
    last_fetch: Option<Instant>,
    fetching: bool,
) -> RefreshGate {
    if let Some(until) = cooldown_until.filter(|until| now.monotonic < *until) {
        return RefreshGate::Cooldown {
            remaining: until.saturating_duration_since(now.monotonic),
        };
    }
    if let Some(last) = last_fetch {
        let elapsed = now.monotonic.saturating_duration_since(last);
        if elapsed < DEBOUNCE {
            return RefreshGate::Debounced {
                remaining: DEBOUNCE - elapsed,
            };
        }
    }
    if fetching {
        return RefreshGate::InFlight;
    }
    RefreshGate::Ready
}

pub fn within_stale_window(now: ClockReading, fetched_unix: i64) -> bool {
    now.unix_seconds - fetched_unix < STALE_WINDOW_SECS
}

pub fn flyout_refresh_due(now: ClockReading, last_fetch: Option<Instant>) -> bool {
    last_fetch
        .map(|last| now.monotonic.saturating_duration_since(last) > FLYOUT_REFRESH_AFTER)
        .unwrap_or(true)
}

pub fn rate_limit_delay(retry_after: Option<u64>, consecutive: u32) -> Duration {
    let seconds = retry_after
        .map(|seconds| seconds.clamp(60, 900))
        .unwrap_or_else(|| (60u64 << consecutive.saturating_sub(1).min(4)).min(600));
    Duration::from_secs(seconds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Success,
    RateLimited,
    OtherFailure,
}

pub fn next_rate_limit_streak(current: u32, completion: CompletionKind) -> u32 {
    match completion {
        CompletionKind::Success => 0,
        CompletionKind::RateLimited => current.saturating_add(1),
        CompletionKind::OtherFailure => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClock {
        reading: ClockReading,
    }

    impl FakeClock {
        fn new(unix_seconds: i64) -> Self {
            Self {
                reading: ClockReading {
                    monotonic: Instant::now(),
                    unix_seconds,
                },
            }
        }

        fn advance(&mut self, duration: Duration) {
            self.reading.monotonic += duration;
            self.reading.unix_seconds += duration.as_secs() as i64;
        }
    }

    impl Clock for FakeClock {
        fn read(&self) -> ClockReading {
            self.reading
        }
    }

    #[test]
    fn debounce_uses_injected_monotonic_time_without_sleeping() {
        let mut clock = FakeClock::new(1000);
        let last = clock.read().monotonic;
        clock.advance(Duration::from_millis(2999));
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Manual,
                clock.read(),
                None,
                Some(last),
                false
            ),
            RefreshGate::Debounced {
                remaining: Duration::from_millis(1)
            }
        );
        clock.advance(Duration::from_millis(1));
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Manual,
                clock.read(),
                None,
                Some(last),
                false
            ),
            RefreshGate::Ready
        );
    }

    #[test]
    fn manual_and_automatic_refresh_share_the_current_cooldown() {
        let clock = FakeClock::new(1000);
        let until = clock.read().monotonic + Duration::from_secs(90);
        let expected = RefreshGate::Cooldown {
            remaining: Duration::from_secs(90),
        };
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Manual,
                clock.read(),
                Some(until),
                None,
                false
            ),
            expected
        );
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Automatic,
                clock.read(),
                Some(until),
                None,
                false
            ),
            expected
        );
    }

    #[test]
    fn stale_and_flyout_boundaries_match_current_behavior() {
        let mut clock = FakeClock::new(10_000);
        assert!(within_stale_window(clock.read(), 9_401));
        assert!(!within_stale_window(clock.read(), 9_400));

        let last = clock.read().monotonic;
        clock.advance(Duration::from_secs(15));
        assert!(!flyout_refresh_due(clock.read(), Some(last)));
        clock.advance(Duration::from_millis(1));
        assert!(flyout_refresh_due(clock.read(), Some(last)));
    }

    #[test]
    fn rate_limit_backoff_and_streak_are_characterized() {
        let delays: Vec<_> = (1..=6)
            .map(|streak| rate_limit_delay(None, streak).as_secs())
            .collect();
        assert_eq!(delays, [60, 120, 240, 480, 600, 600]);
        assert_eq!(rate_limit_delay(Some(0), 1).as_secs(), 60);
        assert_eq!(rate_limit_delay(Some(1200), 1).as_secs(), 900);

        assert_eq!(next_rate_limit_streak(3, CompletionKind::Success), 0);
        assert_eq!(next_rate_limit_streak(3, CompletionKind::RateLimited), 4);
        assert_eq!(next_rate_limit_streak(3, CompletionKind::OtherFailure), 3);
    }
}
