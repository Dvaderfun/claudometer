use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_secs(3);
pub const STALE_WINDOW_SECS: i64 = 10 * 60;

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
    Cooldown {
        remaining: Duration,
        next_eligible_unix: i64,
    },
    NotDue {
        remaining: Duration,
    },
    Debounced {
        remaining: Duration,
    },
    InFlight,
}

pub fn refresh_gate(
    trigger: RefreshTrigger,
    now: ClockReading,
    cooldown_until: Option<Instant>,
    last_fetch: Option<Instant>,
    fetching: bool,
    refresh_interval: Duration,
) -> RefreshGate {
    if let Some(until) = cooldown_until.filter(|until| now.monotonic < *until) {
        let remaining = until.saturating_duration_since(now.monotonic);
        return RefreshGate::Cooldown {
            remaining,
            next_eligible_unix: deadline_unix(now, remaining),
        };
    }
    if fetching {
        return RefreshGate::InFlight;
    }
    if let Some(last) = last_fetch {
        let elapsed = now.monotonic.saturating_duration_since(last);
        match trigger {
            RefreshTrigger::Manual if elapsed < DEBOUNCE => {
                return RefreshGate::Debounced {
                    remaining: DEBOUNCE - elapsed,
                };
            }
            RefreshTrigger::Automatic | RefreshTrigger::Flyout if elapsed < refresh_interval => {
                return RefreshGate::NotDue {
                    remaining: refresh_interval - elapsed,
                };
            }
            RefreshTrigger::Manual | RefreshTrigger::Automatic | RefreshTrigger::Flyout => {}
        }
    }
    RefreshGate::Ready
}

pub fn cooldown_deadline_unix(now: ClockReading, cooldown_until: Option<Instant>) -> Option<i64> {
    let remaining = cooldown_until?
        .checked_duration_since(now.monotonic)
        .filter(|remaining| !remaining.is_zero())?;
    Some(deadline_unix(now, remaining))
}

fn deadline_unix(now: ClockReading, remaining: Duration) -> i64 {
    let seconds = remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() != 0));
    now.unix_seconds
        .saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX))
}

pub fn within_stale_window(now: ClockReading, fetched_unix: i64) -> bool {
    now.unix_seconds - fetched_unix < STALE_WINDOW_SECS
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
        CompletionKind::Success | CompletionKind::OtherFailure => 0,
        CompletionKind::RateLimited => current.saturating_add(1),
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
                false,
                Duration::from_secs(60),
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
                false,
                Duration::from_secs(60),
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
            next_eligible_unix: 1090,
        };
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Manual,
                clock.read(),
                Some(until),
                None,
                false,
                Duration::from_secs(60),
            ),
            expected
        );
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Automatic,
                clock.read(),
                Some(until),
                None,
                false,
                Duration::from_secs(60),
            ),
            expected
        );
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Flyout,
                clock.read(),
                Some(until),
                None,
                false,
                Duration::from_secs(60),
            ),
            expected
        );
    }

    #[test]
    fn flyout_and_automatic_refresh_share_the_selected_interval() {
        let mut clock = FakeClock::new(10_000);
        assert!(within_stale_window(clock.read(), 9_401));
        assert!(!within_stale_window(clock.read(), 9_400));

        let last = clock.read().monotonic;
        clock.advance(Duration::from_secs(15));
        let not_due = RefreshGate::NotDue {
            remaining: Duration::from_secs(45),
        };
        for trigger in [RefreshTrigger::Flyout, RefreshTrigger::Automatic] {
            assert_eq!(
                refresh_gate(
                    trigger,
                    clock.read(),
                    None,
                    Some(last),
                    false,
                    Duration::from_secs(60),
                ),
                not_due
            );
        }
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Manual,
                clock.read(),
                None,
                Some(last),
                false,
                Duration::from_secs(60),
            ),
            RefreshGate::Ready
        );
        clock.advance(Duration::from_secs(45));
        assert_eq!(
            refresh_gate(
                RefreshTrigger::Flyout,
                clock.read(),
                None,
                Some(last),
                false,
                Duration::from_secs(60),
            ),
            RefreshGate::Ready
        );
    }

    #[test]
    fn cooldown_deadline_uses_fake_wall_and_monotonic_clocks() {
        let mut clock = FakeClock::new(20_000);
        let until = clock.read().monotonic + Duration::from_millis(90_001);
        assert_eq!(
            cooldown_deadline_unix(clock.read(), Some(until)),
            Some(20_091)
        );
        clock.advance(Duration::from_secs(91));
        assert_eq!(cooldown_deadline_unix(clock.read(), Some(until)), None);
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
        assert_eq!(next_rate_limit_streak(3, CompletionKind::OtherFailure), 0);
    }
}
