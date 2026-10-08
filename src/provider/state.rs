use std::time::{Duration, Instant};

use super::model::{
    AccountContext, FetchCompletion, FetchOutcome, Generation, ProviderId, RequestId, UsageSnapshot,
};

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
    now.unix_seconds.saturating_sub(fetched_unix) < STALE_WINDOW_SECS
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    MissingCredentials,
    Authentication,
}

// Adapter-specific categories and copy belong to ERR-01. The reducer needs
// only enough classification to decide whether data may survive a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Transient,
    RateLimited,
    Authentication,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FailureKind,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl From<FetchOutcome> for Result<UsageSnapshot, FetchError> {
    fn from(outcome: FetchOutcome) -> Self {
        match outcome {
            FetchOutcome::Ok(snapshot) => Ok(snapshot),
            FetchOutcome::Err {
                msg,
                retry_after,
                rate_limited,
            } => Err(FetchError {
                kind: if rate_limited {
                    FailureKind::RateLimited
                } else {
                    FailureKind::Transient
                },
                message: msg,
                retry_after,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderPhase {
    Disabled,
    Unavailable(UnavailableReason),
    Idle,
    Fetching { request_id: RequestId },
    Ready,
    Backoff { until: Instant, error: FetchError },
    Failed(FetchError),
}

pub enum ProviderEvent {
    Enabled(bool),
    // Send even when a credential replacement resolves to the same account.
    CredentialsChanged(AccountContext),
    Unavailable(UnavailableReason),
    CacheLoaded(UsageSnapshot),
    RefreshRequested {
        trigger: RefreshTrigger,
        interval: Duration,
    },
    Completed(FetchCompletion<Result<UsageSnapshot, FetchError>>),
    CooldownExpired,
}

pub enum Transition {
    Ignored,
    Changed,
    RefreshBlocked(RefreshGate),
    StartFetch(FetchCompletion<()>),
    // Only this event may drive alerts and snapshot persistence in the shell.
    AcceptedSuccess { request_id: RequestId },
    AcceptedFailure { retry_at_unix: Option<i64> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewState {
    Loading,
    Fresh,
    UpdatingWithData,
    Cached,
    Outdated,
    Cooldown,
    Unavailable,
    Failed,
}

pub struct ProviderView<'a> {
    pub state: ViewState,
    pub snapshot: Option<&'a UsageSnapshot>,
    pub error: Option<&'a FetchError>,
    pub age_seconds: Option<i64>,
    pub retry_at_unix: Option<i64>,
}

pub struct ProviderState {
    provider: ProviderId,
    phase: ProviderPhase,
    account: Option<AccountContext>,
    generation: Generation,
    next_request_id: u64,
    snapshot: Option<UsageSnapshot>,
    cached: bool,
    last_attempt: Option<Instant>,
    rate_limit_streak: u32,
}

impl ProviderState {
    pub const fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            phase: ProviderPhase::Unavailable(UnavailableReason::MissingCredentials),
            account: None,
            generation: Generation(0),
            next_request_id: 1,
            snapshot: None,
            cached: false,
            last_attempt: None,
            rate_limit_streak: 0,
        }
    }

    pub fn phase(&self) -> &ProviderPhase {
        &self.phase
    }

    pub fn account(&self) -> Option<&AccountContext> {
        self.account.as_ref()
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn snapshot(&self) -> Option<&UsageSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn reduce(&mut self, event: ProviderEvent, clock: &impl Clock) -> Transition {
        let now = clock.read();
        match event {
            ProviderEvent::Enabled(enabled) => {
                if enabled == !matches!(self.phase, ProviderPhase::Disabled) {
                    return Transition::Ignored;
                }
                self.clear_account();
                self.phase = if enabled {
                    ProviderPhase::Unavailable(UnavailableReason::MissingCredentials)
                } else {
                    ProviderPhase::Disabled
                };
                Transition::Changed
            }
            ProviderEvent::CredentialsChanged(account) => {
                if matches!(self.phase, ProviderPhase::Disabled) {
                    return Transition::Ignored;
                }
                self.clear_account();
                self.account = Some(account);
                self.phase = ProviderPhase::Idle;
                Transition::Changed
            }
            ProviderEvent::Unavailable(reason) => {
                if matches!(self.phase, ProviderPhase::Disabled) {
                    return Transition::Ignored;
                }
                self.clear_account();
                self.phase = ProviderPhase::Unavailable(reason);
                Transition::Changed
            }
            ProviderEvent::CacheLoaded(snapshot) => {
                // A late disk read must never replace a fetch or live data.
                if !matches!(self.phase, ProviderPhase::Idle)
                    || self.snapshot.is_some()
                    || !self.matches_snapshot(&snapshot)
                {
                    return Transition::Ignored;
                }
                self.snapshot = Some(snapshot);
                self.cached = true;
                Transition::Changed
            }
            ProviderEvent::RefreshRequested { trigger, interval } => {
                if self.account.is_none()
                    || matches!(
                        self.phase,
                        ProviderPhase::Disabled | ProviderPhase::Unavailable(_)
                    )
                {
                    return Transition::Ignored;
                }
                let gate = refresh_gate(
                    trigger,
                    now,
                    self.cooldown(),
                    self.last_attempt,
                    matches!(self.phase, ProviderPhase::Fetching { .. }),
                    interval,
                );
                if gate != RefreshGate::Ready {
                    return Transition::RefreshBlocked(gate);
                }
                let request_id = RequestId(self.next_request_id);
                self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
                self.phase = ProviderPhase::Fetching { request_id };
                self.last_attempt = Some(now.monotonic);
                Transition::StartFetch(FetchCompletion {
                    provider: self.provider,
                    generation: self.generation,
                    request_id,
                    account: self.account.as_ref().unwrap().key.clone(),
                    payload: (),
                })
            }
            ProviderEvent::Completed(completion) => self.complete(completion, now),
            ProviderEvent::CooldownExpired => {
                if self.cooldown().is_some_and(|until| now.monotonic >= until) {
                    self.phase = ProviderPhase::Idle;
                    Transition::Changed
                } else {
                    Transition::Ignored
                }
            }
        }
    }

    pub fn view(&self, clock: &impl Clock, interval: Duration) -> ProviderView<'_> {
        let now = clock.read();
        let age_seconds = self.snapshot.as_ref().map(|snapshot| {
            now.unix_seconds
                .saturating_sub(snapshot.fetched_unix)
                .max(0)
        });
        let outdated_after = interval
            .as_secs()
            .saturating_mul(2)
            .max(STALE_WINDOW_SECS as u64);
        let outdated = age_seconds.is_some_and(|age| age as u64 >= outdated_after);
        let error = match &self.phase {
            ProviderPhase::Backoff { error, .. } | ProviderPhase::Failed(error) => Some(error),
            _ => None,
        };
        let state = match &self.phase {
            ProviderPhase::Disabled | ProviderPhase::Unavailable(_) => ViewState::Unavailable,
            ProviderPhase::Fetching { .. } if self.snapshot.is_some() => {
                ViewState::UpdatingWithData
            }
            ProviderPhase::Fetching { .. } => ViewState::Loading,
            ProviderPhase::Backoff { until, .. } if now.monotonic < *until => ViewState::Cooldown,
            _ if outdated => ViewState::Outdated,
            ProviderPhase::Failed(_) => ViewState::Failed,
            _ if self.cached => ViewState::Cached,
            _ if self.snapshot.is_some() => ViewState::Fresh,
            _ => ViewState::Loading,
        };
        ProviderView {
            state,
            snapshot: self.snapshot.as_ref(),
            error,
            age_seconds,
            retry_at_unix: cooldown_deadline_unix(now, self.cooldown()),
        }
    }

    fn clear_account(&mut self) {
        self.generation = Generation(self.generation.0.wrapping_add(1));
        self.account = None;
        self.snapshot = None;
        self.cached = false;
        self.last_attempt = None;
        self.rate_limit_streak = 0;
    }

    fn cooldown(&self) -> Option<Instant> {
        match &self.phase {
            ProviderPhase::Backoff { until, .. } => Some(*until),
            _ => None,
        }
    }

    fn matches_snapshot(&self, snapshot: &UsageSnapshot) -> bool {
        snapshot.provider == self.provider
            && self
                .account
                .as_ref()
                .is_some_and(|account| account.key == snapshot.account)
    }

    fn complete(
        &mut self,
        completion: FetchCompletion<Result<UsageSnapshot, FetchError>>,
        now: ClockReading,
    ) -> Transition {
        if completion.provider != self.provider
            || completion.generation != self.generation
            || self.phase
                != (ProviderPhase::Fetching {
                    request_id: completion.request_id,
                })
            || !self
                .account
                .as_ref()
                .is_some_and(|account| account.key == completion.account)
            || completion
                .payload
                .as_ref()
                .is_ok_and(|snapshot| !self.matches_snapshot(snapshot))
        {
            return Transition::Ignored;
        }
        // Match legacy scheduling: the interval is measured from completion.
        self.last_attempt = Some(now.monotonic);
        match completion.payload {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.cached = false;
                self.rate_limit_streak = 0;
                self.phase = ProviderPhase::Ready;
                Transition::AcceptedSuccess {
                    request_id: completion.request_id,
                }
            }
            Err(error) => {
                if error.kind == FailureKind::Authentication {
                    self.clear_account();
                    self.phase = ProviderPhase::Unavailable(UnavailableReason::Authentication);
                    return Transition::AcceptedFailure {
                        retry_at_unix: None,
                    };
                }
                if error.kind == FailureKind::RateLimited {
                    self.rate_limit_streak =
                        next_rate_limit_streak(self.rate_limit_streak, CompletionKind::RateLimited);
                    let delay = rate_limit_delay(error.retry_after, self.rate_limit_streak);
                    self.phase = ProviderPhase::Backoff {
                        until: now.monotonic + delay,
                        error,
                    };
                    Transition::AcceptedFailure {
                        retry_at_unix: Some(deadline_unix(now, delay)),
                    }
                } else {
                    self.rate_limit_streak = 0;
                    self.phase = ProviderPhase::Failed(error);
                    Transition::AcceptedFailure {
                        retry_at_unix: None,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::model::{AccountKey, IdentityPersistence, SourceProvenance};

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

    #[test]
    fn refresh_gate_precedence_is_characterized() {
        let clock = FakeClock::new(1000);
        let now = clock.read();
        let interval = Duration::from_secs(60);
        for trigger in [
            RefreshTrigger::Manual,
            RefreshTrigger::Automatic,
            RefreshTrigger::Flyout,
        ] {
            assert!(matches!(
                refresh_gate(
                    trigger,
                    now,
                    Some(now.monotonic + interval),
                    Some(now.monotonic),
                    true,
                    interval
                ),
                RefreshGate::Cooldown { .. }
            ));
            assert_eq!(
                refresh_gate(trigger, now, None, Some(now.monotonic), true, interval),
                RefreshGate::InFlight
            );
            assert_eq!(
                refresh_gate(trigger, now, Some(now.monotonic), None, false, interval),
                RefreshGate::Ready
            );
        }
    }

    fn account(byte: u8) -> AccountContext {
        AccountContext {
            key: AccountKey::from_digest([byte; 32]),
            persistence: IdentityPersistence::Persistent,
        }
    }

    fn snapshot(byte: u8, fetched_unix: i64) -> UsageSnapshot {
        UsageSnapshot {
            provider: ProviderId::Claude,
            account: account(byte).key,
            source: SourceProvenance::compatibility(ProviderId::Claude),
            plan: Some("Synthetic plan".to_string()),
            rows: Vec::new(),
            fetched_unix,
        }
    }

    fn failure(kind: FailureKind) -> FetchError {
        FetchError {
            kind,
            message: "sanitized failure".to_string(),
            retry_after: None,
        }
    }

    fn ready_state(clock: &impl Clock) -> ProviderState {
        let mut state = ProviderState::new(ProviderId::Claude);
        state.reduce(ProviderEvent::CredentialsChanged(account(1)), clock);
        state
    }

    fn refresh(state: &mut ProviderState, clock: &impl Clock) -> Transition {
        state.reduce(
            ProviderEvent::RefreshRequested {
                trigger: RefreshTrigger::Manual,
                interval: Duration::from_secs(60),
            },
            clock,
        )
    }

    fn request(state: &mut ProviderState, clock: &impl Clock) -> FetchCompletion<()> {
        match refresh(state, clock) {
            Transition::StartFetch(ticket) => ticket,
            _ => panic!("expected fetch ticket"),
        }
    }

    fn completed(
        ticket: &FetchCompletion<()>,
        payload: Result<UsageSnapshot, FetchError>,
    ) -> ProviderEvent {
        ProviderEvent::Completed(FetchCompletion {
            provider: ticket.provider,
            generation: ticket.generation,
            request_id: ticket.request_id,
            account: ticket.account.clone(),
            payload,
        })
    }

    fn observe(state: &ProviderState, clock: &impl Clock) -> ViewState {
        state.view(clock, Duration::from_secs(60)).state
    }

    #[test]
    fn disable_and_credential_changes_clear_every_account_bound_field() {
        let clock = FakeClock::new(1000);
        for change in [
            ProviderEvent::Enabled(false),
            ProviderEvent::CredentialsChanged(account(2)),
            ProviderEvent::CredentialsChanged(account(1)),
            ProviderEvent::Unavailable(UnavailableReason::MissingCredentials),
        ] {
            let mut state = ready_state(&clock);
            let ticket = request(&mut state, &clock);
            state.snapshot = Some(snapshot(1, 1000));
            state.cached = true;
            state.reduce(
                completed(&ticket, Err(failure(FailureKind::RateLimited))),
                &clock,
            );
            let generation = state.generation();
            assert!(matches!(state.reduce(change, &clock), Transition::Changed));
            assert_ne!(state.generation(), generation);
            assert!(state.snapshot().is_none());
            assert!(!state.cached);
            assert!(state.cooldown().is_none());
            assert!(state.last_attempt.is_none());
            assert_eq!(state.rate_limit_streak, 0);
            assert!(state.view(&clock, Duration::from_secs(60)).error.is_none());
            assert!(matches!(
                state.reduce(completed(&ticket, Ok(snapshot(1, 1000))), &clock),
                Transition::Ignored
            ));
        }
    }

    #[test]
    fn disabled_and_unavailable_states_cannot_fetch_or_load_data() {
        let clock = FakeClock::new(1000);
        let mut state = ProviderState::new(ProviderId::Claude);
        assert_eq!(observe(&state, &clock), ViewState::Unavailable);
        assert!(matches!(refresh(&mut state, &clock), Transition::Ignored));
        state.reduce(ProviderEvent::Enabled(false), &clock);
        assert!(matches!(state.phase(), ProviderPhase::Disabled));
        assert!(matches!(
            state.reduce(ProviderEvent::CredentialsChanged(account(1)), &clock),
            Transition::Ignored
        ));
        assert!(matches!(
            state.reduce(ProviderEvent::CacheLoaded(snapshot(1, 1000)), &clock),
            Transition::Ignored
        ));
        assert!(matches!(refresh(&mut state, &clock), Transition::Ignored));
        state.reduce(ProviderEvent::Enabled(true), &clock);
        assert!(matches!(state.phase(), ProviderPhase::Unavailable(_)));
        state.reduce(ProviderEvent::CredentialsChanged(account(1)), &clock);
        assert!(matches!(
            refresh(&mut state, &clock),
            Transition::StartFetch(_)
        ));
    }

    #[test]
    fn cache_matches_identity_keeps_true_age_and_never_prevents_first_fetch() {
        let clock = FakeClock::new(1000);
        for cached in [snapshot(2, 950), {
            let mut wrong_provider = snapshot(1, 950);
            wrong_provider.provider = ProviderId::Codex;
            wrong_provider
        }] {
            let mut state = ready_state(&clock);
            assert!(matches!(
                state.reduce(ProviderEvent::CacheLoaded(cached), &clock),
                Transition::Ignored
            ));
            assert!(state.snapshot().is_none());
        }
        let mut state = ready_state(&clock);
        assert!(matches!(
            state.reduce(ProviderEvent::CacheLoaded(snapshot(1, 950)), &clock),
            Transition::Changed
        ));
        let view = state.view(&clock, Duration::from_secs(60));
        assert_eq!(view.state, ViewState::Cached);
        assert_eq!(view.age_seconds, Some(50));
        assert!(matches!(
            refresh(&mut state, &clock),
            Transition::StartFetch(_)
        ));
        assert_eq!(observe(&state, &clock), ViewState::UpdatingWithData);
        assert!(matches!(
            state.reduce(ProviderEvent::CacheLoaded(snapshot(1, 999)), &clock),
            Transition::Ignored
        ));
        assert_eq!(state.snapshot().unwrap().fetched_unix, 950);
    }

    #[test]
    fn refresh_transitions_are_clock_gated_and_success_is_accepted_once() {
        let mut clock = FakeClock::new(1000);
        let mut state = ready_state(&clock);
        let ticket = request(&mut state, &clock);
        assert_eq!(observe(&state, &clock), ViewState::Loading);
        assert!(matches!(
            refresh(&mut state, &clock),
            Transition::RefreshBlocked(RefreshGate::InFlight)
        ));
        let success = || completed(&ticket, Ok(snapshot(1, 1000)));
        assert!(
            matches!(state.reduce(success(), &clock), Transition::AcceptedSuccess { request_id } if request_id == ticket.request_id)
        );
        assert_eq!(state.phase(), &ProviderPhase::Ready);
        assert_eq!(observe(&state, &clock), ViewState::Fresh);
        assert!(matches!(
            state.reduce(success(), &clock),
            Transition::Ignored
        ));
        assert!(matches!(
            refresh(&mut state, &clock),
            Transition::RefreshBlocked(RefreshGate::Debounced { .. })
        ));
        clock.advance(DEBOUNCE);
        assert!(matches!(
            refresh(&mut state, &clock),
            Transition::StartFetch(_)
        ));
        assert_eq!(observe(&state, &clock), ViewState::UpdatingWithData);
    }

    #[test]
    fn completions_validate_every_envelope_field_before_any_mutation() {
        let clock = FakeClock::new(1000);
        for mismatch in 0..6 {
            let mut state = ready_state(&clock);
            let ticket = request(&mut state, &clock);
            let mut event = completed(&ticket, Ok(snapshot(1, 1000)));
            let ProviderEvent::Completed(ref mut completion) = event else {
                unreachable!()
            };
            match mismatch {
                0 => completion.provider = ProviderId::Codex,
                1 => completion.generation = Generation(ticket.generation.0 + 1),
                2 => completion.request_id = RequestId(ticket.request_id.0 + 1),
                3 => completion.account = account(2).key,
                4 => completion.payload.as_mut().ok().unwrap().provider = ProviderId::Codex,
                5 => completion.payload.as_mut().ok().unwrap().account = account(2).key,
                _ => unreachable!(),
            }
            assert!(matches!(state.reduce(event, &clock), Transition::Ignored));
            assert_eq!(
                state.phase(),
                &ProviderPhase::Fetching {
                    request_id: ticket.request_id
                }
            );
            assert!(state.snapshot().is_none());
            assert!(state.cooldown().is_none());
            assert_eq!(state.rate_limit_streak, 0);
            assert_eq!(state.last_attempt, Some(clock.read().monotonic));
            assert!(matches!(
                state.reduce(completed(&ticket, Ok(snapshot(1, 1000))), &clock),
                Transition::AcceptedSuccess { .. }
            ));
        }
    }

    #[test]
    fn rate_limit_preserves_data_and_cooldown_expiry_does_not_start_work() {
        let mut clock = FakeClock::new(1000);
        let mut state = ready_state(&clock);
        state.snapshot = Some(snapshot(1, 950));
        let ticket = request(&mut state, &clock);
        let mut error = failure(FailureKind::RateLimited);
        error.retry_after = Some(90);
        assert!(matches!(
            state.reduce(completed(&ticket, Err(error)), &clock),
            Transition::AcceptedFailure {
                retry_at_unix: Some(1090)
            }
        ));
        assert_eq!(state.rate_limit_streak, 1);
        let view = state.view(&clock, Duration::from_secs(60));
        assert_eq!(view.state, ViewState::Cooldown);
        assert_eq!(view.retry_at_unix, Some(1090));
        assert!(view.snapshot.is_some());
        assert!(view.error.is_some());
        for trigger in [
            RefreshTrigger::Manual,
            RefreshTrigger::Automatic,
            RefreshTrigger::Flyout,
        ] {
            assert!(matches!(
                state.reduce(
                    ProviderEvent::RefreshRequested {
                        trigger,
                        interval: Duration::from_secs(60)
                    },
                    &clock
                ),
                Transition::RefreshBlocked(RefreshGate::Cooldown {
                    next_eligible_unix: 1090,
                    ..
                })
            ));
        }
        assert!(matches!(
            state.reduce(ProviderEvent::CooldownExpired, &clock),
            Transition::Ignored
        ));
        clock.advance(Duration::from_secs(90));
        assert!(matches!(
            state.reduce(ProviderEvent::CooldownExpired, &clock),
            Transition::Changed
        ));
        assert_eq!(state.phase(), &ProviderPhase::Idle);
        assert_eq!(state.rate_limit_streak, 1);
        assert!(state.snapshot().is_some());
        assert!(state
            .view(&clock, Duration::from_secs(60))
            .retry_at_unix
            .is_none());
    }

    #[test]
    fn all_non_429_results_reset_streak_and_authentication_clears_identity() {
        let mut clock = FakeClock::new(1000);
        for kind in [
            None,
            Some(FailureKind::Transient),
            Some(FailureKind::Authentication),
        ] {
            let mut state = ready_state(&clock);
            state.snapshot = Some(snapshot(1, 1000));
            let ticket = request(&mut state, &clock);
            state.reduce(
                completed(&ticket, Err(failure(FailureKind::RateLimited))),
                &clock,
            );
            clock.advance(Duration::from_secs(60));
            let next = request(&mut state, &clock);
            let payload = kind.map_or_else(
                || Ok(snapshot(1, clock.read().unix_seconds)),
                |kind| Err(failure(kind)),
            );
            state.reduce(completed(&next, payload), &clock);
            assert_eq!(state.rate_limit_streak, 0);
            assert!(state.cooldown().is_none());
            if kind == Some(FailureKind::Authentication) {
                assert!(state.account().is_none());
                assert!(state.snapshot().is_none());
                assert!(state.last_attempt.is_none());
                assert_eq!(observe(&state, &clock), ViewState::Unavailable);
            } else {
                assert!(state.account().is_some());
                assert!(state.snapshot().is_some());
            }
        }
    }

    #[test]
    fn failed_fetch_keeps_same_account_snapshot_and_becomes_outdated_by_age() {
        let mut clock = FakeClock::new(1000);
        let mut state = ready_state(&clock);
        state.snapshot = Some(snapshot(1, 1000));
        let ticket = request(&mut state, &clock);
        state.reduce(
            completed(&ticket, Err(failure(FailureKind::Transient))),
            &clock,
        );
        assert_eq!(observe(&state, &clock), ViewState::Failed);
        assert_eq!(
            state.snapshot().unwrap().plan.as_deref(),
            Some("Synthetic plan")
        );
        clock.advance(Duration::from_secs(600));
        assert_eq!(observe(&state, &clock), ViewState::Outdated);
        assert!(state
            .view(&clock, Duration::from_secs(60))
            .snapshot
            .is_some());
        let mut empty = ready_state(&clock);
        let empty_ticket = request(&mut empty, &clock);
        empty.reduce(
            completed(&empty_ticket, Err(failure(FailureKind::Transient))),
            &clock,
        );
        assert_eq!(observe(&empty, &clock), ViewState::Failed);
        assert!(empty.snapshot().is_none());
    }

    #[test]
    fn view_age_uses_poll_interval_and_saturates_extreme_wall_times() {
        let mut clock = FakeClock::new(1000);
        let mut state = ready_state(&clock);
        state.snapshot = Some(snapshot(1, 1000));
        state.phase = ProviderPhase::Ready;
        clock.advance(Duration::from_secs(600));
        assert_eq!(observe(&state, &clock), ViewState::Outdated);
        assert_eq!(
            state.view(&clock, Duration::from_secs(400)).state,
            ViewState::Fresh
        );
        clock.advance(Duration::from_secs(200));
        assert_eq!(
            state.view(&clock, Duration::from_secs(400)).state,
            ViewState::Outdated
        );
        state.snapshot = Some(snapshot(1, i64::MIN));
        assert_eq!(
            state.view(&clock, Duration::from_secs(60)).age_seconds,
            Some(i64::MAX)
        );
        state.snapshot = Some(snapshot(1, i64::MAX));
        assert_eq!(
            state.view(&clock, Duration::from_secs(60)).age_seconds,
            Some(0)
        );
        assert!(!within_stale_window(clock.read(), i64::MIN));
    }

    #[test]
    fn obsolete_results_cannot_mutate_a_new_account_or_pending_request() {
        let clock = FakeClock::new(1000);
        let mut state = ready_state(&clock);
        let old = request(&mut state, &clock);
        state.reduce(ProviderEvent::CredentialsChanged(account(2)), &clock);
        let new = request(&mut state, &clock);
        for payload in [
            Ok(snapshot(1, 1000)),
            Err(failure(FailureKind::RateLimited)),
            Err(failure(FailureKind::Authentication)),
        ] {
            assert!(matches!(
                state.reduce(completed(&old, payload), &clock),
                Transition::Ignored
            ));
            assert_eq!(state.generation(), new.generation);
            assert_eq!(
                state.phase(),
                &ProviderPhase::Fetching {
                    request_id: new.request_id
                }
            );
            assert!(state.account().unwrap().key == new.account);
            assert!(state.snapshot().is_none());
            assert!(state.cooldown().is_none());
        }
    }

    #[test]
    fn roadmap_transition_table() {
        for (event, expected_phase, expected_view, keeps_data) in [
            ("disabled", "disabled", ViewState::Unavailable, false),
            ("credentials", "idle", ViewState::Loading, false),
            ("cache", "idle", ViewState::Cached, true),
            ("refresh", "fetching", ViewState::UpdatingWithData, true),
            ("success", "ready", ViewState::Fresh, true),
            ("429", "backoff", ViewState::Cooldown, true),
            ("transient", "failed", ViewState::Failed, true),
            (
                "authentication",
                "unavailable",
                ViewState::Unavailable,
                false,
            ),
            ("obsolete", "fetching", ViewState::UpdatingWithData, true),
            ("expiry", "idle", ViewState::Fresh, true),
        ] {
            let mut clock = FakeClock::new(1000);
            let mut state = ready_state(&clock);
            if event != "cache" {
                state.snapshot = Some(snapshot(1, 950));
            }
            let transition = match event {
                "disabled" => state.reduce(ProviderEvent::Enabled(false), &clock),
                "credentials" => {
                    state.reduce(ProviderEvent::CredentialsChanged(account(2)), &clock)
                }
                "cache" => state.reduce(ProviderEvent::CacheLoaded(snapshot(1, 950)), &clock),
                "refresh" => refresh(&mut state, &clock),
                _ => {
                    let mut ticket = request(&mut state, &clock);
                    let payload = match event {
                        "success" | "obsolete" => Ok(snapshot(1, 1000)),
                        "authentication" => Err(failure(FailureKind::Authentication)),
                        "transient" => Err(failure(FailureKind::Transient)),
                        _ => Err(failure(FailureKind::RateLimited)),
                    };
                    if event == "obsolete" {
                        ticket.generation.0 += 1;
                    }
                    let transition = state.reduce(completed(&ticket, payload), &clock);
                    if event == "expiry" {
                        clock.advance(Duration::from_secs(60));
                        state.reduce(ProviderEvent::CooldownExpired, &clock)
                    } else {
                        transition
                    }
                }
            };
            let phase = match state.phase() {
                ProviderPhase::Disabled => "disabled",
                ProviderPhase::Unavailable(_) => "unavailable",
                ProviderPhase::Idle => "idle",
                ProviderPhase::Fetching { .. } => "fetching",
                ProviderPhase::Ready => "ready",
                ProviderPhase::Backoff { .. } => "backoff",
                ProviderPhase::Failed(_) => "failed",
            };
            assert_eq!(phase, expected_phase, "{event}");
            assert_eq!(observe(&state, &clock), expected_view, "{event}");
            assert_eq!(state.snapshot().is_some(), keeps_data, "{event}");
            assert_eq!(
                matches!(transition, Transition::Ignored),
                event == "obsolete",
                "{event}"
            );
        }
    }

    #[test]
    fn generated_event_orders_preserve_identity_and_ignored_event_invariants() {
        // Fixed seeds, synthetic accounts, no wall-clock waits or IO. Old
        // tickets stay in the pool to exercise late and duplicated workers.
        for seed in 1..=128_u64 {
            let mut random = seed;
            let mut clock = FakeClock::new(1000);
            let mut state = ready_state(&clock);
            let mut tickets = vec![request(&mut state, &clock)];
            for _ in 0..96 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let old = &tickets[(random >> 16) as usize % tickets.len()];
                let event = match random % 10 {
                    0 => ProviderEvent::Enabled(false),
                    1 => ProviderEvent::Enabled(true),
                    2 => ProviderEvent::CredentialsChanged(account(1 + ((random >> 24) % 2) as u8)),
                    3 => ProviderEvent::CacheLoaded(snapshot(1, 900)),
                    4 => ProviderEvent::RefreshRequested {
                        trigger: RefreshTrigger::Manual,
                        interval: Duration::from_secs(60),
                    },
                    5 => completed(old, Ok(snapshot(old.account.as_bytes()[0], 1000))),
                    6 => completed(old, Err(failure(FailureKind::RateLimited))),
                    7 => completed(old, Err(failure(FailureKind::Authentication))),
                    8 => ProviderEvent::CooldownExpired,
                    _ => ProviderEvent::Unavailable(UnavailableReason::MissingCredentials),
                };
                let before = (
                    state.phase.clone(),
                    state.generation,
                    state.last_attempt,
                    state.rate_limit_streak,
                    state
                        .snapshot
                        .as_ref()
                        .map(|s| (s.fetched_unix, *s.account.as_bytes())),
                    state.account.as_ref().map(|a| *a.key.as_bytes()),
                );
                let transition = state.reduce(event, &clock);
                if matches!(transition, Transition::Ignored) {
                    assert_eq!(
                        before,
                        (
                            state.phase.clone(),
                            state.generation,
                            state.last_attempt,
                            state.rate_limit_streak,
                            state
                                .snapshot
                                .as_ref()
                                .map(|s| (s.fetched_unix, *s.account.as_bytes())),
                            state.account.as_ref().map(|a| *a.key.as_bytes())
                        )
                    );
                }
                if let Transition::StartFetch(ticket) = transition {
                    tickets.push(ticket);
                }
                if let Some(snapshot) = state.snapshot() {
                    assert!(state.matches_snapshot(snapshot));
                }
                if state.account().is_none() {
                    assert!(state.snapshot().is_none());
                    assert!(state.cooldown().is_none());
                    assert_eq!(state.rate_limit_streak, 0);
                }
                clock.advance(Duration::from_secs(random % 121));
            }
        }
    }
}
