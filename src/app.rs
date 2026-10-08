use std::cell::RefCell;
use std::time::Duration;

use crate::api::PreparationFailure;
use crate::provider::model::{
    AccountContext, FetchCompletion, FetchOutcome, Generation, ProviderId, RequestId,
    SourceProvenance, UsageSnapshot,
};
use crate::provider::state::{
    within_stale_window, Clock, ClockReading, FailureKind, FetchError, ProviderEvent,
    ProviderPhase, ProviderState, RefreshGate, RefreshTrigger, SystemClock, Transition,
    UnavailableReason,
};

#[derive(Clone, Copy)]
pub struct Preparation {
    pub provider: ProviderId,
    pub generation: Generation,
    pub operation: RequestId,
    pub trigger: RefreshTrigger,
}

struct ProviderSlot {
    state: ProviderState,
    pending: Option<Preparation>,
    next_operation: u64,
    // Compatibility presentation survives entering Fetching. FRESH-01 will
    // expose the richer reducer view without changing APP-01 screenshots.
    last_error: Option<String>,
    error: Option<FetchError>,
    manual_cooldown_notice: bool,
    detected: Option<bool>,
    last_attempt_unix: Option<i64>,
    last_success_unix: Option<i64>,
}

impl ProviderSlot {
    const fn new(provider: ProviderId) -> Self {
        Self {
            state: ProviderState::new(provider),
            pending: None,
            next_operation: 1,
            last_error: None,
            error: None,
            manual_cooldown_notice: false,
            detected: None,
            last_attempt_unix: None,
            last_success_unix: None,
        }
    }

    fn invalidate(&mut self, disabled: bool, clock: &impl Clock) {
        self.pending = None;
        self.last_error = None;
        self.error = None;
        self.detected = None;
        self.last_attempt_unix = None;
        self.last_success_unix = None;
        self.manual_cooldown_notice = false;
        self.state.reduce(
            if disabled {
                ProviderEvent::Enabled(false)
            } else {
                ProviderEvent::Unavailable(UnavailableReason::MissingCredentials)
            },
            clock,
        );
    }

    fn reserve(
        &mut self,
        provider: ProviderId,
        trigger: RefreshTrigger,
        clock: &impl Clock,
        interval: Duration,
    ) -> Option<Preparation> {
        if matches!(self.state.phase(), ProviderPhase::Disabled) {
            return None;
        }
        let gate = if self.pending.is_some() {
            RefreshGate::InFlight
        } else {
            self.state.refresh_gate(trigger, clock, interval)
        };
        if trigger == RefreshTrigger::Manual {
            self.manual_cooldown_notice = matches!(gate, RefreshGate::Cooldown { .. });
        }
        if gate != RefreshGate::Ready {
            return None;
        }
        let preparation = Preparation {
            provider,
            generation: self.state.generation(),
            operation: RequestId(self.next_operation),
            trigger,
        };
        self.next_operation = self.next_operation.wrapping_add(1).max(1);
        self.pending = Some(preparation);
        self.last_attempt_unix = Some(clock.read().unix_seconds);
        Some(preparation)
    }

    fn matches_preparation(&self, preparation: Preparation) -> bool {
        self.pending.is_some_and(|pending| {
            pending.provider == preparation.provider
                && pending.operation == preparation.operation
                && pending.generation == preparation.generation
        }) && preparation.generation == self.state.generation()
    }

    fn prepared(
        &mut self,
        preparation: Preparation,
        account: AccountContext,
        clock: &impl Clock,
        interval: Duration,
    ) -> Option<FetchCompletion<()>> {
        self.prepared_with_cache(
            preparation,
            account,
            clock,
            interval,
            |provider, account, now| {
                crate::runtime_state::cached_provider(
                    provider,
                    account,
                    SourceProvenance::compatibility(provider),
                    now,
                )
            },
        )
    }

    fn prepared_with_cache(
        &mut self,
        preparation: Preparation,
        account: AccountContext,
        clock: &impl Clock,
        interval: Duration,
        load: impl FnOnce(
            ProviderId,
            &AccountContext,
            i64,
        ) -> Option<crate::runtime_state::CachedProvider>,
    ) -> Option<FetchCompletion<()>> {
        if !self.matches_preparation(preparation) {
            return None;
        }
        self.pending = None;
        self.detected = Some(true);
        if self
            .state
            .account()
            .is_none_or(|current| current.key != account.key)
        {
            self.last_error = None;
            self.error = None;
            self.last_success_unix = None;
            self.manual_cooldown_notice = false;
            let cache = load(preparation.provider, &account, clock.read().unix_seconds);
            self.state
                .reduce(ProviderEvent::CredentialsChanged(account), clock);
            if let Some(cache) = cache {
                if let Some(snapshot) = cache.snapshot {
                    self.state
                        .reduce(ProviderEvent::CacheLoaded(snapshot), clock);
                }
                if let Some(deadline) = cache.retry_at_unix {
                    self.state
                        .reduce(ProviderEvent::RetryDeadlineLoaded(deadline), clock);
                    self.manual_cooldown_notice = true;
                }
            }
        }
        match self.state.reduce(
            ProviderEvent::RefreshRequested {
                trigger: preparation.trigger,
                interval,
            },
            clock,
        ) {
            Transition::StartFetch(ticket) => Some(ticket),
            _ => None,
        }
    }

    fn preparation_failed(
        &mut self,
        preparation: Preparation,
        failure: PreparationFailure,
        clock: &impl Clock,
    ) -> bool {
        if !self.matches_preparation(preparation) {
            return false;
        }
        self.pending = None;
        self.detected = Some(!matches!(
            failure.kind,
            crate::api::PreparationFailureKind::Missing
                | crate::api::PreparationFailureKind::Unsupported
        ));
        let invalidates = failure.invalidates_account();
        self.last_error = Some(failure.message.to_string());
        self.error = Some(failure.error());
        if invalidates {
            self.manual_cooldown_notice = false;
            self.last_success_unix = None;
        }
        self.state.reduce(
            ProviderEvent::PreparationFailed {
                error: failure.error(),
                invalidates_account: invalidates,
            },
            clock,
        );
        true
    }

    fn complete(
        &mut self,
        completion: FetchCompletion<FetchOutcome>,
        clock: &impl Clock,
    ) -> Transition {
        let error = match &completion.payload {
            FetchOutcome::Failure(error) => Some(error.clone()),
            #[cfg(test)]
            FetchOutcome::Err {
                msg,
                retry_after,
                rate_limited,
            } => Some(FetchError {
                kind: if *rate_limited {
                    FailureKind::RateLimited
                } else {
                    FailureKind::Transient
                },
                message: msg.clone(),
                retry_after: *retry_after,
            }),
            _ => None,
        };
        let transition = self.state.reduce(
            ProviderEvent::Completed(FetchCompletion {
                provider: completion.provider,
                generation: completion.generation,
                request_id: completion.request_id,
                account: completion.account,
                payload: completion.payload.into(),
            }),
            clock,
        );
        if !matches!(transition, Transition::Ignored) {
            self.last_error = error.as_ref().map(|error| error.message.clone());
            self.error = error;
            if matches!(transition, Transition::AcceptedSuccess { .. }) {
                self.last_success_unix = Some(clock.read().unix_seconds);
            } else if self.state.account().is_none() {
                self.last_success_unix = None;
            }
        }
        transition
    }

    fn effective(&self, now: ClockReading) -> (Option<UsageSnapshot>, Option<String>) {
        let snapshot = self
            .state
            .snapshot()
            .filter(|snapshot| {
                self.state.is_cached()
                    || self.last_error.is_none()
                    || within_stale_window(now, snapshot.fetched_unix)
            })
            .cloned();
        let error = self
            .last_error
            .clone()
            .or_else(|| match self.state.phase() {
                ProviderPhase::Backoff { error, .. } => Some(error.message.clone()),
                _ => None,
            });
        (snapshot, error)
    }
}

pub fn provider_diagnostics(interval: Duration) -> [crate::diagnostics::ProviderDiagnostic; 2] {
    APP.with_borrow(|app| {
        std::array::from_fn(|index| {
            let provider = if index == 0 {
                ProviderId::Claude
            } else {
                ProviderId::Codex
            };
            let slot = &app.providers[index];
            let view = slot.state.view(&SystemClock, interval);
            crate::diagnostics::ProviderDiagnostic {
                provider,
                enabled: provider != ProviderId::Codex || crate::config::settings().codex_enabled,
                detected: slot.detected,
                authenticated: slot.state.account().is_some(),
                source: slot.state.snapshot().map_or_else(
                    || SourceProvenance::compatibility(provider),
                    |snapshot| snapshot.source,
                ),
                fallback_reason: if provider == ProviderId::Claude {
                    "compatibility source only"
                } else {
                    "documented source not enabled"
                },
                phase: if slot.pending.is_some() {
                    "preparing"
                } else {
                    match slot.state.phase() {
                        ProviderPhase::Disabled => "disabled",
                        ProviderPhase::Unavailable(_) => "unavailable",
                        ProviderPhase::Idle => "idle",
                        ProviderPhase::Fetching { .. } => "fetching",
                        ProviderPhase::Ready => "ready",
                        ProviderPhase::Backoff { .. } => "backoff",
                        ProviderPhase::Failed(_) => "failed",
                    }
                },
                freshness: view.state,
                age_seconds: view.age_seconds,
                last_attempt_unix: slot.last_attempt_unix,
                last_success_unix: slot.last_success_unix,
                observed_at_unix: slot.state.snapshot().map(|snapshot| snapshot.fetched_unix),
                retry_at_unix: view.retry_at_unix,
                next_retry_unix: view.retry_at_unix.or_else(|| {
                    slot.last_attempt_unix
                        .map(|_| slot.state.next_attempt_unix(&SystemClock, interval))
                }),
                error_code: slot
                    .error
                    .as_ref()
                    .map(|error| error.code())
                    .or_else(|| view.error.map(|error| error.code())),
            }
        })
    })
}

pub fn load_local_diagnostics() {
    for provider in [ProviderId::Claude, ProviderId::Codex] {
        if provider == ProviderId::Codex && !crate::config::settings().codex_enabled {
            continue;
        }
        // Support mode prepares local credentials only; execute is never called.
        let identity = match provider {
            ProviderId::Claude => crate::api::prepare().map(|request| request.account().clone()),
            ProviderId::Codex => crate::codex::prepare().map(|request| request.account().clone()),
        };
        APP.with_borrow_mut(|app| {
            let slot = &mut app.providers[provider.index()];
            match identity {
                Ok(account) => {
                    slot.detected = Some(true);
                    let cache = crate::runtime_state::cached_provider(
                        provider,
                        &account,
                        SourceProvenance::compatibility(provider),
                        SystemClock.read().unix_seconds,
                    );
                    slot.state
                        .reduce(ProviderEvent::CredentialsChanged(account), &SystemClock);
                    if let Some(cache) = cache {
                        if let Some(snapshot) = cache.snapshot {
                            slot.state
                                .reduce(ProviderEvent::CacheLoaded(snapshot), &SystemClock);
                        }
                        if let Some(deadline) = cache.retry_at_unix {
                            slot.state
                                .reduce(ProviderEvent::RetryDeadlineLoaded(deadline), &SystemClock);
                        }
                    }
                }
                Err(failure) => {
                    slot.detected =
                        Some(failure.kind != crate::api::PreparationFailureKind::Missing);
                    slot.error = Some(failure.error());
                }
            }
        });
    }
}

pub struct AppState {
    providers: [ProviderSlot; 2],
}

impl AppState {
    const fn new() -> Self {
        Self {
            providers: [
                ProviderSlot::new(ProviderId::Claude),
                ProviderSlot::new(ProviderId::Codex),
            ],
        }
    }
}

thread_local! {
    static APP: RefCell<AppState> = const { RefCell::new(AppState::new()) };
}

pub fn effective(provider: ProviderId) -> (Option<UsageSnapshot>, Option<String>) {
    APP.with_borrow(|app| app.providers[provider.index()].effective(SystemClock.read()))
}

pub fn any_fetching() -> bool {
    APP.with_borrow(|app| {
        app.providers.iter().any(|slot| {
            slot.pending.is_some() || matches!(slot.state.phase(), ProviderPhase::Fetching { .. })
        })
    })
}

pub fn error_text(provider: ProviderId, interval: Duration) -> Option<(String, String)> {
    APP.with_borrow(|app| {
        let slot = &app.providers[provider.index()];
        let error = slot.error.as_ref().or_else(|| match slot.state.phase() {
            ProviderPhase::Backoff { error, .. } => Some(error),
            _ => None,
        })?;
        let retry = slot
            .state
            .view(&SystemClock, interval)
            .retry_at_unix
            .or_else(|| {
                (error.kind == FailureKind::Timeout)
                    .then(|| slot.state.next_attempt_unix(&SystemClock, interval))
            });
        let retry = retry.map(crate::api::fmt_unix_hhmm);
        Some((
            error.message.clone(),
            error.detail(provider, retry.as_deref()),
        ))
    })
}

pub fn error_details(interval: Duration) -> Option<String> {
    if crate::demo::active().is_some_and(|state| state.scenario == crate::demo::Scenario::Error) {
        return Some(FetchError::new(FailureKind::Offline).detail(ProviderId::Claude, None));
    }
    if crate::demo::is_active() {
        return None;
    }
    let details: Vec<_> = [ProviderId::Claude, ProviderId::Codex]
        .into_iter()
        .filter(|provider| *provider != ProviderId::Codex || crate::codex_active())
        .filter_map(|provider| error_text(provider, interval).map(|(_, detail)| detail))
        .collect();
    (!details.is_empty()).then(|| details.join("\n"))
}

pub fn invalidate(provider: ProviderId, disabled: bool) {
    APP.with_borrow_mut(|app| app.providers[provider.index()].invalidate(disabled, &SystemClock));
    crate::alerts::account_changed(provider, None);
    if !disabled {
        let _ = crate::runtime_state::clear_provider_cache(provider);
    }
}

pub fn cached_notes() -> Option<String> {
    APP.with_borrow(|app| {
        let notes: Vec<_> = app
            .providers
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.state.is_cached())
            .map(|(index, _)| {
                format!(
                    "{}: Cached values",
                    if index == 0 { "Claude" } else { "Codex" }
                )
            })
            .collect();
        (!notes.is_empty()).then(|| notes.join(" · "))
    })
}

pub fn refresh(provider: ProviderId, trigger: RefreshTrigger, interval: Duration) {
    if crate::demo::is_active() {
        return;
    }
    let preparation = APP.with_borrow_mut(|app| {
        let slot = &mut app.providers[provider.index()];
        if matches!(slot.state.phase(), ProviderPhase::Disabled) {
            slot.state
                .reduce(ProviderEvent::Enabled(true), &SystemClock);
        }
        slot.reserve(provider, trigger, &SystemClock, interval)
    });
    if let Some(preparation) = preparation {
        crate::poller::spawn(preparation);
    }
}

pub fn manual_cooldown_deadlines(interval: Duration) -> Vec<(ProviderId, i64)> {
    APP.with_borrow_mut(|app| {
        [ProviderId::Claude, ProviderId::Codex]
            .into_iter()
            .filter_map(|provider| {
                let slot = &mut app.providers[provider.index()];
                if !slot.manual_cooldown_notice {
                    return None;
                }
                let deadline = slot.state.view(&SystemClock, interval).retry_at_unix;
                slot.manual_cooldown_notice = deadline.is_some();
                deadline.map(|deadline| (provider, deadline))
            })
            .collect()
    })
}

pub fn drain_events(interval: Duration) -> bool {
    let mut changed = false;
    for event in crate::poller::take_events() {
        match event {
            crate::poller::AppEvent::Prepared {
                preparation,
                account,
                reply,
            } => {
                let (ticket, account_changed) = APP.with_borrow_mut(|app| {
                    let slot = &mut app.providers[preparation.provider.index()];
                    let generation = slot.state.generation();
                    let ticket =
                        slot.prepared(preparation, account.clone(), &SystemClock, interval);
                    let account_changed = generation != slot.state.generation();
                    (ticket, account_changed)
                });
                if ticket.is_some() || account_changed {
                    if account_changed {
                        crate::alerts::account_changed(preparation.provider, Some(&account));
                    }
                    changed = true;
                }
                // No UI borrow is held when the worker is released.
                if std::sync::mpsc::Sender::send(&reply, ticket.clone()).is_err() {
                    if let Some(ticket) = ticket {
                        APP.with_borrow_mut(|app| {
                            app.providers[preparation.provider.index()].complete(
                                FetchCompletion {
                                    provider: ticket.provider,
                                    generation: ticket.generation,
                                    request_id: ticket.request_id,
                                    account: ticket.account,
                                    payload: FetchOutcome::Failure(FetchError::new(
                                        FailureKind::Timeout,
                                    )),
                                },
                                &SystemClock,
                            );
                        });
                    }
                }
            }
            crate::poller::AppEvent::PreparationFailed {
                preparation,
                failure,
            } => {
                let invalidates_account = failure.invalidates_account();
                let accepted = APP.with_borrow_mut(|app| {
                    app.providers[preparation.provider.index()].preparation_failed(
                        preparation,
                        failure,
                        &SystemClock,
                    )
                });
                if accepted {
                    crate::diagnostics::record("provider_failed");
                    if invalidates_account {
                        crate::alerts::account_changed(preparation.provider, None);
                        let _ = crate::runtime_state::clear_provider_cache(preparation.provider);
                    }
                    changed = true;
                }
            }
            crate::poller::AppEvent::Completed(completion) => {
                let provider = completion.provider;
                let accepted = APP.with_borrow_mut(|app| {
                    let slot = &mut app.providers[provider.index()];
                    let transition = slot.complete(completion, &SystemClock);
                    let alert = if matches!(transition, Transition::AcceptedSuccess { .. }) {
                        slot.state
                            .account()
                            .cloned()
                            .zip(slot.state.snapshot().cloned())
                    } else {
                        None
                    };
                    if !matches!(transition, Transition::Ignored) {
                        if let Some(account) = slot.state.account() {
                            let source = slot.state.snapshot().map_or_else(
                                || SourceProvenance::compatibility(provider),
                                |snapshot| snapshot.source,
                            );
                            let retry_at_unix =
                                slot.state.view(&SystemClock, interval).retry_at_unix;
                            let _ = crate::runtime_state::persist_provider_cache(
                                provider,
                                account,
                                source,
                                slot.state.snapshot().cloned(),
                                retry_at_unix,
                            );
                        }
                    }
                    let accepted = !matches!(transition, Transition::Ignored);
                    (accepted, alert, accepted && slot.state.account().is_none())
                });
                if accepted.2 {
                    let _ = crate::runtime_state::clear_provider_cache(provider);
                    crate::alerts::account_changed(provider, None);
                }
                if accepted.0 {
                    crate::diagnostics::record(if accepted.1.is_some() {
                        "provider_success"
                    } else {
                        "provider_failed"
                    });
                }
                if let Some((account, snapshot)) = accepted.1 {
                    if provider != ProviderId::Codex || crate::codex_active() {
                        crate::alerts::check(provider, &account, &snapshot);
                    }
                }
                changed |= accepted.0;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::PreparationFailureKind;
    use crate::provider::model::{AccountKey, IdentityPersistence, SourceProvenance};
    use std::time::Instant;

    struct FakeClock(ClockReading);
    impl Clock for FakeClock {
        fn read(&self) -> ClockReading {
            self.0
        }
    }
    impl FakeClock {
        fn new() -> Self {
            Self(ClockReading {
                monotonic: Instant::now(),
                unix_seconds: 1000,
            })
        }
        fn advance(&mut self, seconds: u64) {
            self.0.monotonic += Duration::from_secs(seconds);
            self.0.unix_seconds += seconds as i64;
        }
    }

    fn account(byte: u8) -> AccountContext {
        AccountContext {
            key: AccountKey::from_digest([byte; 32]),
            persistence: IdentityPersistence::Persistent,
        }
    }

    fn snapshot(byte: u8, time: i64) -> UsageSnapshot {
        UsageSnapshot {
            provider: ProviderId::Claude,
            account: account(byte).key,
            source: SourceProvenance::compatibility(ProviderId::Claude),
            rows: vec![],
            plan: Some("Synthetic".to_string()),
            fetched_unix: time,
        }
    }

    fn request(slot: &mut ProviderSlot, clock: &FakeClock, byte: u8) -> FetchCompletion<()> {
        let preparation = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                clock,
                Duration::from_secs(60),
            )
            .unwrap();
        slot.prepared(preparation, account(byte), clock, Duration::from_secs(60))
            .unwrap()
    }

    fn completion(
        ticket: &FetchCompletion<()>,
        payload: FetchOutcome,
    ) -> FetchCompletion<FetchOutcome> {
        FetchCompletion {
            provider: ticket.provider,
            generation: ticket.generation,
            request_id: ticket.request_id,
            account: ticket.account.clone(),
            payload,
        }
    }

    type RenderedSnapshot = (i64, Option<String>, [u8; 32]);

    fn rendered(
        view: (Option<UsageSnapshot>, Option<String>),
    ) -> (Option<RenderedSnapshot>, Option<String>) {
        (
            view.0.map(|snapshot| {
                (
                    snapshot.fetched_unix,
                    snapshot.plan,
                    *snapshot.account.as_bytes(),
                )
            }),
            view.1,
        )
    }

    #[test]
    fn ui_owned_state_matches_legacy_success_failure_backoff_and_stale_views() {
        for outcomes in [vec![0, 1, 2, 0], vec![0, 2, 2, 1], vec![1, 0, 1, 0]] {
            let mut clock = FakeClock::new();
            let mut slot = ProviderSlot::new(ProviderId::Claude);
            let reference = crate::state_reference::Reference::new();
            for kind in outcomes {
                let ticket = request(&mut slot, &clock, 1);
                let (request_id, generation) = reference.begin(account(1));
                let payload = match kind {
                    0 => FetchOutcome::Ok(snapshot(1, clock.read().unix_seconds)),
                    _ => FetchOutcome::Err {
                        msg: "sanitized".to_string(),
                        retry_after: None,
                        rate_limited: kind == 2,
                    },
                };
                assert!(reference.complete(
                    request_id,
                    generation,
                    account(1).key,
                    payload.clone(),
                    clock.read()
                ));
                assert!(!matches!(
                    slot.complete(completion(&ticket, payload), &clock),
                    Transition::Ignored
                ));
                for seconds in [0, 1, 3, 60, 600] {
                    let at = FakeClock(ClockReading {
                        monotonic: clock.read().monotonic + Duration::from_secs(seconds),
                        unix_seconds: clock.read().unix_seconds + seconds as i64,
                    });
                    assert_eq!(
                        rendered(slot.effective(at.read())),
                        rendered(reference.effective(at.read()))
                    );
                    for trigger in [
                        RefreshTrigger::Manual,
                        RefreshTrigger::Automatic,
                        RefreshTrigger::Flyout,
                    ] {
                        assert_eq!(
                            slot.state
                                .refresh_gate(trigger, &at, Duration::from_secs(60)),
                            reference.gate(trigger, at.read(), Duration::from_secs(60))
                        );
                    }
                }
                clock.advance(900);
            }
        }
    }

    #[test]
    fn invalidated_preparation_cannot_start_a_request_or_change_replacement() {
        let clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let old = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        slot.invalidate(false, &clock);
        let new = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(slot
            .prepared(old, account(1), &clock, Duration::from_secs(60))
            .is_none());
        assert!(!slot.preparation_failed(
            old,
            PreparationFailure {
                kind: PreparationFailureKind::Missing,
                message: "missing"
            },
            &clock
        ));
        assert_eq!(slot.pending.unwrap().operation, new.operation);
        assert!(slot
            .prepared(new, account(2), &clock, Duration::from_secs(60))
            .is_some());
        assert!(slot.state.account().unwrap().key == account(2).key);
    }

    #[test]
    fn obsolete_success_cannot_alert_and_does_not_clear_current_fetch() {
        let clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let old = request(&mut slot, &clock, 1);
        slot.invalidate(false, &clock);
        let current = request(&mut slot, &clock, 2);
        assert!(matches!(
            slot.complete(
                completion(&old, FetchOutcome::Ok(snapshot(1, 1000))),
                &clock
            ),
            Transition::Ignored
        ));
        assert_eq!(
            slot.state.phase(),
            &ProviderPhase::Fetching {
                request_id: current.request_id
            }
        );
        assert!(slot.state.snapshot().is_none());
        let success = || completion(&current, FetchOutcome::Ok(snapshot(2, 1000)));
        assert!(matches!(
            slot.complete(success(), &clock),
            Transition::AcceptedSuccess { .. }
        ));
        assert!(matches!(
            slot.complete(success(), &clock),
            Transition::Ignored
        ));
    }

    #[test]
    fn preparation_errors_preserve_transient_data_and_debounce_missing_credentials() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let ticket = request(&mut slot, &clock, 1);
        slot.complete(
            completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        clock.advance(3);
        let transient = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(slot.preparation_failed(
            transient,
            PreparationFailure {
                kind: PreparationFailureKind::TemporarilyUnreadable,
                message: "temporary"
            },
            &clock
        ));
        assert!(slot.effective(clock.read()).0.is_some());
        assert_eq!(slot.effective(clock.read()).1.as_deref(), Some("temporary"));
        clock.advance(3);
        let missing = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(slot.preparation_failed(
            missing,
            PreparationFailure {
                kind: PreparationFailureKind::Missing,
                message: "missing"
            },
            &clock
        ));
        assert!(slot.effective(clock.read()).0.is_none());
        assert!(slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60)
            )
            .is_none());
        assert!(slot.state.account().is_none());
    }

    #[test]
    fn providers_and_pending_preparation_are_independent() {
        let clock = FakeClock::new();
        let mut app = AppState::new();
        let claude = app.providers[0]
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        let codex = app.providers[1]
            .reserve(
                ProviderId::Codex,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(app.providers[0]
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60)
            )
            .is_none());
        app.providers[1].invalidate(true, &clock);
        assert!(app.providers[1]
            .prepared(codex, account(2), &clock, Duration::from_secs(60))
            .is_none());
        assert!(app.providers[0]
            .prepared(claude, account(1), &clock, Duration::from_secs(60))
            .is_some());
    }

    #[test]
    fn advancing_time_during_preparation_does_not_duplicate_workers() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let preparation = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        clock.advance(900);
        for trigger in [
            RefreshTrigger::Manual,
            RefreshTrigger::Automatic,
            RefreshTrigger::Flyout,
        ] {
            assert!(slot
                .reserve(ProviderId::Claude, trigger, &clock, Duration::from_secs(60))
                .is_none());
        }
        assert!(slot
            .prepared(preparation, account(1), &clock, Duration::from_secs(60))
            .is_some());
    }

    #[test]
    fn replacement_account_failure_never_restores_old_plan_or_values() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let first = request(&mut slot, &clock, 1);
        slot.complete(
            completion(&first, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        clock.advance(3);
        let replacement = request(&mut slot, &clock, 2);
        assert!(slot.effective(clock.read()).0.is_none());
        slot.complete(
            completion(
                &replacement,
                FetchOutcome::Err {
                    msg: "synthetic failure".to_string(),
                    retry_after: None,
                    rate_limited: false,
                },
            ),
            &clock,
        );
        assert!(slot.effective(clock.read()).0.is_none());
        assert!(slot.state.account().unwrap().key == account(2).key);
        assert!(matches!(
            slot.complete(
                completion(&first, FetchOutcome::Ok(snapshot(1, 1000))),
                &clock
            ),
            Transition::Ignored
        ));
        assert!(slot.effective(clock.read()).0.is_none());
    }

    #[test]
    fn restart_restores_cached_age_but_fetches_without_a_live_deadline() {
        let clock = FakeClock::new();
        for deadline in [None, Some(1000), Some(999), Some(1901)] {
            let mut slot = ProviderSlot::new(ProviderId::Claude);
            let preparation = slot
                .reserve(
                    ProviderId::Claude,
                    RefreshTrigger::Automatic,
                    &clock,
                    Duration::from_secs(60),
                )
                .unwrap();
            let ticket = slot
                .prepared_with_cache(
                    preparation,
                    account(1),
                    &clock,
                    Duration::from_secs(60),
                    |provider, identity, _| {
                        Some(crate::runtime_state::CachedProvider {
                            provider,
                            account: identity.key.clone(),
                            source: SourceProvenance::compatibility(provider),
                            snapshot: Some(snapshot(1, 900)),
                            retry_at_unix: deadline,
                        })
                    },
                )
                .unwrap();
            assert!(slot.state.is_cached());
            assert_eq!(slot.effective(clock.read()).0.unwrap().fetched_unix, 900);
            assert_eq!(
                slot.state.view(&clock, Duration::from_secs(60)).age_seconds,
                Some(100)
            );
            assert!(matches!(slot.state.phase(), ProviderPhase::Fetching { .. }));
            assert!(matches!(
                slot.complete(
                    completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
                    &clock
                ),
                Transition::AcceptedSuccess { .. }
            ));
            assert!(!slot.state.is_cached());
        }
    }

    #[test]
    fn restart_inside_429_window_restores_data_and_blocks_first_http_ticket() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let preparation = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Automatic,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        let ticket = slot.prepared_with_cache(
            preparation,
            account(1),
            &clock,
            Duration::from_secs(60),
            |provider, identity, _| {
                Some(crate::runtime_state::CachedProvider {
                    provider,
                    account: identity.key.clone(),
                    source: SourceProvenance::compatibility(provider),
                    snapshot: Some(snapshot(1, 900)),
                    retry_at_unix: Some(1100),
                })
            },
        );
        assert!(ticket.is_none());
        assert!(slot.pending.is_none());
        assert!(slot.state.is_cached());
        assert_eq!(
            slot.state
                .view(&clock, Duration::from_secs(60))
                .retry_at_unix,
            Some(1100)
        );
        for trigger in [
            RefreshTrigger::Automatic,
            RefreshTrigger::Manual,
            RefreshTrigger::Flyout,
        ] {
            assert!(slot
                .reserve(ProviderId::Claude, trigger, &clock, Duration::from_secs(60))
                .is_none());
        }
        clock.advance(100);
        assert!(slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Automatic,
                &clock,
                Duration::from_secs(60)
            )
            .is_some());
    }

    #[test]
    fn typed_authentication_failure_clears_account_and_cached_values() {
        let clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let ticket = request(&mut slot, &clock, 1);
        slot.complete(
            completion(
                &ticket,
                FetchOutcome::Failure(FetchError::new(FailureKind::Authentication)),
            ),
            &clock,
        );
        assert!(slot.state.account().is_none());
        assert!(slot.state.snapshot().is_none());
        assert_eq!(slot.error.as_ref().unwrap().code(), "sign_in_expired");
        assert!(matches!(
            slot.state.phase(),
            ProviderPhase::Unavailable(UnavailableReason::Authentication)
        ));
    }

    #[test]
    fn diagnostic_timestamps_accept_success_once_and_ignore_obsolete_results() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let ticket = request(&mut slot, &clock, 1);
        assert_eq!(slot.detected, Some(true));
        assert_eq!(slot.last_attempt_unix, Some(1000));
        assert_eq!(slot.last_success_unix, None);
        slot.complete(
            completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        assert_eq!(slot.last_success_unix, Some(1000));
        clock.advance(3);
        let next = request(&mut slot, &clock, 1);
        clock.advance(1);
        slot.complete(
            completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        assert_eq!(slot.last_attempt_unix, Some(1003));
        assert_eq!(slot.last_success_unix, Some(1000));
        slot.complete(
            completion(
                &next,
                FetchOutcome::Failure(FetchError::new(FailureKind::Offline)),
            ),
            &clock,
        );
        assert_eq!(slot.last_success_unix, Some(1000));
        assert_eq!(slot.error.unwrap().code(), "connection_failed");
    }

    #[test]
    fn typed_transient_failures_keep_values_and_retry_time_does_not_slide() {
        for kind in [
            FailureKind::Offline,
            FailureKind::Timeout,
            FailureKind::UnexpectedResponse,
            FailureKind::RateLimited,
        ] {
            let mut clock = FakeClock::new();
            let mut slot = ProviderSlot::new(ProviderId::Claude);
            let first = request(&mut slot, &clock, 1);
            slot.complete(
                completion(&first, FetchOutcome::Ok(snapshot(1, 1000))),
                &clock,
            );
            clock.advance(3);
            let next = request(&mut slot, &clock, 1);
            slot.complete(
                completion(&next, FetchOutcome::Failure(FetchError::new(kind))),
                &clock,
            );
            assert!(slot.effective(clock.read()).0.is_some());
            assert_eq!(slot.error.as_ref().unwrap().kind, kind);
            assert_eq!(
                slot.state
                    .next_attempt_unix(&clock, Duration::from_secs(60)),
                1063
            );
            clock.advance(10);
            assert_eq!(
                slot.state
                    .next_attempt_unix(&clock, Duration::from_secs(60)),
                1063
            );
        }
    }

    #[test]
    fn scope_preparation_failure_clears_previous_account_and_keeps_actionable_error() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let ticket = request(&mut slot, &clock, 1);
        slot.complete(
            completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        clock.advance(3);
        let preparation = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        slot.preparation_failed(
            preparation,
            PreparationFailure {
                kind: PreparationFailureKind::UsageScope,
                message: "Sign in again for live usage",
            },
            &clock,
        );
        assert!(slot.state.snapshot().is_none());
        assert!(slot.state.account().is_none());
        assert_eq!(slot.error.unwrap().kind, FailureKind::UsageScope);
    }

    #[test]
    fn obsolete_preparation_never_reads_cache_and_live_data_is_not_replaced() {
        let mut clock = FakeClock::new();
        let mut slot = ProviderSlot::new(ProviderId::Claude);
        let old = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        slot.invalidate(false, &clock);
        assert!(slot
            .prepared_with_cache(
                old,
                account(1),
                &clock,
                Duration::from_secs(60),
                |_, _, _| panic!("obsolete cache read")
            )
            .is_none());
        let ticket = request(&mut slot, &clock, 1);
        slot.complete(
            completion(&ticket, FetchOutcome::Ok(snapshot(1, 1000))),
            &clock,
        );
        clock.advance(3);
        let next = slot
            .reserve(
                ProviderId::Claude,
                RefreshTrigger::Manual,
                &clock,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(slot
            .prepared_with_cache(
                next,
                account(1),
                &clock,
                Duration::from_secs(60),
                |_, _, _| panic!("cache must not reload over live account")
            )
            .is_some());
        assert!(!slot.state.is_cached());
        assert_eq!(slot.state.snapshot().unwrap().fetched_unix, 1000);
    }
}
