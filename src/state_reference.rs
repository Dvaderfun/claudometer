use crate::provider::model::{
    AccountContext, AccountKey, CompletionEvent, FetchCompletion, FetchOutcome, Generation,
    ProviderId, RequestId, UsageSnapshot,
};
use crate::provider::state::{self as provider_state, ClockReading};
use crate::{alerts, api};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) struct Reference(Slot);

impl Reference {
    pub(crate) fn new() -> Self {
        Self(Slot::new())
    }

    pub(crate) fn begin(&self, account: AccountContext) -> (RequestId, Generation) {
        self.0.fetching.store(true, Ordering::SeqCst);
        let request = reserve_request(&self.0);
        let generation = begin_request(&self.0, ProviderId::Claude, request, account).unwrap();
        (request, generation)
    }

    pub(crate) fn complete(
        &self,
        request_id: RequestId,
        generation: Generation,
        account: AccountKey,
        payload: FetchOutcome,
        now: ClockReading,
    ) -> bool {
        record_fetch_completion(
            &self.0,
            ProviderId::Claude,
            FetchCompletion {
                provider: ProviderId::Claude,
                generation,
                request_id,
                account,
                payload,
            },
            now,
        )
    }

    pub(crate) fn effective(&self, now: ClockReading) -> (Option<UsageSnapshot>, Option<String>) {
        effective_at(&self.0, now)
    }

    pub(crate) fn gate(
        &self,
        trigger: crate::provider::state::RefreshTrigger,
        now: ClockReading,
        interval: Duration,
    ) -> crate::provider::state::RefreshGate {
        provider_state::refresh_gate(
            trigger,
            now,
            *self.0.cooldown_until.lock().unwrap(),
            *self.0.last_fetch.lock().unwrap(),
            self.0.fetching.load(Ordering::SeqCst),
            interval,
        )
    }
}
/// Per-provider fetch state; both providers share the resilience rules
/// (recent stale data beats errors, strict 429 backoff, 3 s debounce).
struct Slot {
    identity: Mutex<SlotIdentity>,
    state: Mutex<Option<FetchCompletion<FetchOutcome>>>,
    last_good: Mutex<Option<AccountSnapshot>>,
    preparation_error: Mutex<Option<String>>,
    last_fetch: Mutex<Option<Instant>>,
    /// No requests before this instant (server Retry-After or exponential
    /// backoff). Manual refresh respects it: retries can extend a 429 cooldown.
    cooldown_until: Mutex<Option<Instant>>,
    /// consecutive rate-limited fetches — drives the 429 backoff
    rl_streak: AtomicU32,
    fetching: AtomicBool,
}

struct SlotIdentity {
    account: Option<AccountContext>,
    generation: Generation,
    next_request_id: u64,
    pending_request_id: Option<RequestId>,
}

#[derive(Clone)]
struct AccountSnapshot {
    account: AccountKey,
    snapshot: UsageSnapshot,
}

impl Slot {
    const fn new() -> Self {
        Self {
            identity: Mutex::new(SlotIdentity {
                account: None,
                generation: Generation(0),
                next_request_id: 1,
                pending_request_id: None,
            }),
            state: Mutex::new(None),
            last_good: Mutex::new(None),
            preparation_error: Mutex::new(None),
            last_fetch: Mutex::new(None),
            cooldown_until: Mutex::new(None),
            rl_streak: AtomicU32::new(0),
            fetching: AtomicBool::new(false),
        }
    }
}

fn effective_at(s: &Slot, now: ClockReading) -> (Option<UsageSnapshot>, Option<String>) {
    let identity = s.identity.lock().unwrap();
    let current_account = identity.account.as_ref().map(|account| &account.key);
    let matching_last_good = || {
        s.last_good
            .lock()
            .unwrap()
            .as_ref()
            .filter(|last| current_account.is_some_and(|account| account == &last.account))
            .map(|last| last.snapshot.clone())
    };
    if let Some(error) = s.preparation_error.lock().unwrap().clone() {
        let recent = matching_last_good()
            .filter(|snapshot| provider_state::within_stale_window(now, snapshot.fetched_unix));
        return (recent, Some(error));
    }
    let state = s.state.lock().unwrap();
    match &*state {
        Some(completion)
            if current_account.is_some_and(|account| account == &completion.account)
                && completion.generation == identity.generation =>
        {
            match &completion.payload {
                FetchOutcome::Ok(snapshot) => (Some(snapshot.clone()), None),
                FetchOutcome::Failure(_) => {
                    unreachable!("legacy reference accepts legacy fixtures only")
                }
                FetchOutcome::Err { msg, .. } => {
                    let recent = matching_last_good().filter(|snapshot| {
                        provider_state::within_stale_window(now, snapshot.fetched_unix)
                    });
                    (recent, Some(msg.clone()))
                }
            }
        }
        Some(_) => (None, None),
        None => (matching_last_good(), None),
    }
}

fn alert_candidate(s: &Slot, event: CompletionEvent) -> Option<(AccountContext, UsageSnapshot)> {
    let identity = s.identity.lock().unwrap();
    match &*s.state.lock().unwrap() {
        Some(completion)
            if completion.provider == event.provider
                && completion.request_id == event.request_id
                && completion.generation == identity.generation
                && identity
                    .account
                    .as_ref()
                    .is_some_and(|account| account.key == completion.account) =>
        {
            match &completion.payload {
                FetchOutcome::Ok(snapshot) => {
                    Some((identity.account.as_ref()?.clone(), snapshot.clone()))
                }
                FetchOutcome::Err { .. } => None,
                FetchOutcome::Failure(_) => None,
            }
        }
        Some(_) | None => None,
    }
}

fn reserve_request(s: &Slot) -> RequestId {
    let mut identity = s.identity.lock().unwrap();
    let request_id = RequestId(identity.next_request_id);
    identity.next_request_id = identity.next_request_id.wrapping_add(1).max(1);
    identity.pending_request_id = Some(request_id);
    request_id
}

fn begin_request(
    s: &Slot,
    provider: ProviderId,
    request_id: RequestId,
    account: AccountContext,
) -> Option<Generation> {
    let mut identity = s.identity.lock().unwrap();
    if identity.pending_request_id != Some(request_id) {
        return None;
    }
    let changed = identity
        .account
        .as_ref()
        .is_none_or(|current| current.key != account.key);
    if changed {
        identity.generation = Generation(identity.generation.0.wrapping_add(1));
        clear_account_bound_state(s);
    }
    identity.account = Some(account.clone());
    let generation = identity.generation;
    drop(identity);
    if changed {
        alerts::account_changed(provider, Some(&account));
    }
    Some(generation)
}

fn invalidate_account(s: &Slot, provider: ProviderId) {
    let mut identity = s.identity.lock().unwrap();
    identity.generation = Generation(identity.generation.0.wrapping_add(1));
    identity.account = None;
    identity.pending_request_id = None;
    clear_account_bound_state(s);
    s.fetching.store(false, Ordering::SeqCst);
    drop(identity);
    alerts::account_changed(provider, None);
}

fn clear_account_bound_state(s: &Slot) {
    *s.state.lock().unwrap() = None;
    *s.last_good.lock().unwrap() = None;
    *s.preparation_error.lock().unwrap() = None;
    *s.last_fetch.lock().unwrap() = None;
    *s.cooldown_until.lock().unwrap() = None;
    s.rl_streak.store(0, Ordering::SeqCst);
}

fn record_preparation_failure(
    s: &Slot,
    provider: ProviderId,
    request_id: RequestId,
    failure: api::PreparationFailure,
    completed_at: ClockReading,
) -> bool {
    let mut identity = s.identity.lock().unwrap();
    if identity.pending_request_id != Some(request_id) {
        return false;
    }
    let invalidated = failure.invalidates_account();
    if invalidated {
        identity.generation = Generation(identity.generation.0.wrapping_add(1));
        identity.account = None;
        clear_account_bound_state(s);
    }
    identity.pending_request_id = None;
    *s.preparation_error.lock().unwrap() = Some(failure.message.to_string());
    *s.last_fetch.lock().unwrap() = Some(completed_at.monotonic);
    s.rl_streak.store(
        provider_state::next_rate_limit_streak(
            s.rl_streak.load(Ordering::SeqCst),
            provider_state::CompletionKind::OtherFailure,
        ),
        Ordering::SeqCst,
    );
    s.fetching.store(false, Ordering::SeqCst);
    drop(identity);
    if invalidated {
        alerts::account_changed(provider, None);
    }
    true
}

fn record_fetch_completion(
    s: &Slot,
    expected_provider: ProviderId,
    completion: FetchCompletion<FetchOutcome>,
    completed_at: ClockReading,
) -> bool {
    let mut identity = s.identity.lock().unwrap();
    let matches = completion.provider == expected_provider
        && match &completion.payload {
            FetchOutcome::Ok(snapshot) => {
                snapshot.provider == completion.provider && snapshot.account == completion.account
            }
            FetchOutcome::Err { .. } | FetchOutcome::Failure(_) => true,
        }
        && identity.pending_request_id == Some(completion.request_id)
        && identity.generation == completion.generation
        && identity
            .account
            .as_ref()
            .is_some_and(|account| account.key == completion.account);
    if !matches {
        return false;
    }
    identity.pending_request_id = None;

    match &completion.payload {
        FetchOutcome::Ok(snapshot) => {
            *s.last_good.lock().unwrap() = Some(AccountSnapshot {
                account: completion.account.clone(),
                snapshot: snapshot.clone(),
            });
            *s.cooldown_until.lock().unwrap() = None;
            let streak = provider_state::next_rate_limit_streak(
                s.rl_streak.load(Ordering::SeqCst),
                provider_state::CompletionKind::Success,
            );
            s.rl_streak.store(streak, Ordering::SeqCst);
        }
        FetchOutcome::Err {
            rate_limited: true,
            retry_after,
            ..
        } => {
            let current = s.rl_streak.load(Ordering::SeqCst);
            let consecutive = provider_state::next_rate_limit_streak(
                current,
                provider_state::CompletionKind::RateLimited,
            );
            s.rl_streak.store(consecutive, Ordering::SeqCst);
            let delay = provider_state::rate_limit_delay(*retry_after, consecutive);
            *s.cooldown_until.lock().unwrap() = Some(completed_at.monotonic + delay);
        }
        FetchOutcome::Err { .. } => {
            let streak = provider_state::next_rate_limit_streak(
                s.rl_streak.load(Ordering::SeqCst),
                provider_state::CompletionKind::OtherFailure,
            );
            s.rl_streak.store(streak, Ordering::SeqCst);
        }
        FetchOutcome::Failure(_) => unreachable!("legacy reference accepts legacy fixtures only"),
    }
    *s.preparation_error.lock().unwrap() = None;
    *s.state.lock().unwrap() = Some(completion);
    *s.last_fetch.lock().unwrap() = Some(completed_at.monotonic);
    s.fetching.store(false, Ordering::SeqCst);
    true
}

#[cfg(test)]
mod state_characterization_tests {
    use super::*;
    use crate::provider::model::{IdentityPersistence, SourceProvenance};

    fn reading(monotonic: Instant, unix_seconds: i64) -> ClockReading {
        ClockReading {
            monotonic,
            unix_seconds,
        }
    }

    fn failure(rate_limited: bool, retry_after: Option<u64>) -> FetchOutcome {
        FetchOutcome::Err {
            msg: "sanitized failure".to_string(),
            retry_after,
            rate_limited,
        }
    }

    fn account(byte: u8) -> AccountContext {
        AccountContext {
            key: AccountKey::from_digest([byte; 32]),
            persistence: IdentityPersistence::Persistent,
        }
    }

    fn start_request(
        slot: &Slot,
        account: AccountContext,
    ) -> (RequestId, Generation, AccountContext) {
        slot.fetching.store(true, Ordering::SeqCst);
        let request_id = reserve_request(slot);
        let generation =
            begin_request(slot, ProviderId::Claude, request_id, account.clone()).unwrap();
        (request_id, generation, account)
    }

    fn completion(
        provider: ProviderId,
        request_id: RequestId,
        generation: Generation,
        account: &AccountContext,
        mut payload: FetchOutcome,
    ) -> FetchCompletion<FetchOutcome> {
        if let FetchOutcome::Ok(snapshot) = &mut payload {
            snapshot.provider = provider;
            snapshot.account = account.key.clone();
            snapshot.source = SourceProvenance::compatibility(provider);
        }
        FetchCompletion {
            provider,
            generation,
            request_id,
            account: account.key.clone(),
            payload,
        }
    }

    fn snapshot(fetched_unix: i64) -> UsageSnapshot {
        UsageSnapshot {
            provider: ProviderId::Claude,
            account: AccountKey::from_digest([1; 32]),
            source: SourceProvenance::compatibility(ProviderId::Claude),
            rows: Vec::new(),
            plan: None,
            fetched_unix,
        }
    }

    #[test]
    fn snapshot_identity_mismatch_cannot_be_accepted() {
        let slot = Slot::new();
        let now = Instant::now();
        let (request_id, generation, account) = start_request(&slot, account(1));
        let mut completion = completion(
            ProviderId::Claude,
            request_id,
            generation,
            &account,
            FetchOutcome::Ok(snapshot(100)),
        );
        if let FetchOutcome::Ok(snapshot) = &mut completion.payload {
            snapshot.account = AccountKey::from_digest([2; 32]);
        }
        assert!(!record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion,
            reading(now, 100)
        ));
        assert!(slot.last_good.lock().unwrap().is_none());
        assert!(slot.fetching.load(Ordering::SeqCst));
        assert_eq!(
            slot.identity.lock().unwrap().pending_request_id,
            Some(request_id)
        );
    }

    #[test]
    fn provider_completion_state_is_isolated() {
        let slots = [Slot::new(), Slot::new()];
        let now = Instant::now();
        let (request_id, generation, current) = start_request(&slots[0], account(1));
        assert!(record_fetch_completion(
            &slots[0],
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_id,
                generation,
                &current,
                failure(true, Some(120)),
            ),
            reading(now, 10_000),
        ));

        assert_eq!(slots[0].rl_streak.load(Ordering::SeqCst), 1);
        assert_eq!(
            *slots[0].cooldown_until.lock().unwrap(),
            Some(now + std::time::Duration::from_secs(120))
        );
        assert!(slots[0].state.lock().unwrap().is_some());

        assert_eq!(slots[1].rl_streak.load(Ordering::SeqCst), 0);
        assert_eq!(*slots[1].cooldown_until.lock().unwrap(), None);
        assert!(slots[1].state.lock().unwrap().is_none());
        assert!(slots[1].last_fetch.lock().unwrap().is_none());
    }

    #[test]
    fn every_non_rate_limited_result_resets_the_429_streak() {
        let slot = Slot::new();
        let now = Instant::now();
        let current = account(1);
        let (request_id, generation, _) = start_request(&slot, current.clone());
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_id,
                generation,
                &current,
                failure(true, None),
            ),
            reading(now, 100),
        ));
        assert_eq!(slot.rl_streak.load(Ordering::SeqCst), 1);

        let (request_id, generation, _) = start_request(&slot, current.clone());
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_id,
                generation,
                &current,
                failure(false, None),
            ),
            reading(now + Duration::from_secs(60), 160),
        ));
        assert_eq!(slot.rl_streak.load(Ordering::SeqCst), 0);

        slot.rl_streak.store(2, Ordering::SeqCst);
        slot.fetching.store(true, Ordering::SeqCst);
        let request_id = reserve_request(&slot);
        assert!(record_preparation_failure(
            &slot,
            ProviderId::Claude,
            request_id,
            api::PreparationFailure {
                kind: api::PreparationFailureKind::TemporarilyUnreadable,
                message: "temporary",
            },
            reading(now + Duration::from_secs(120), 220),
        ));
        assert_eq!(slot.rl_streak.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stale_data_expires_at_the_characterized_boundary() {
        let slot = Slot::new();
        let (request_id, generation, current) = start_request(&slot, account(1));
        *slot.last_good.lock().unwrap() = Some(AccountSnapshot {
            account: current.key.clone(),
            snapshot: snapshot(9_401),
        });
        *slot.state.lock().unwrap() = Some(completion(
            ProviderId::Claude,
            request_id,
            generation,
            &current,
            failure(false, None),
        ));
        slot.identity.lock().unwrap().pending_request_id = None;
        slot.fetching.store(false, Ordering::SeqCst);

        let (still_visible, error) = effective_at(&slot, reading(Instant::now(), 10_000));
        assert!(still_visible.is_some());
        assert_eq!(error.as_deref(), Some("sanitized failure"));

        let (expired, error) = effective_at(&slot, reading(Instant::now(), 10_001));
        assert!(expired.is_none());
        assert_eq!(error.as_deref(), Some("sanitized failure"));
    }

    #[test]
    fn only_the_exact_success_completion_is_an_alert_candidate() {
        let slot = Slot::new();
        let (request_id, generation, current) = start_request(&slot, account(1));
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_id,
                generation,
                &current,
                FetchOutcome::Ok(snapshot(123)),
            ),
            reading(Instant::now(), 123),
        ));

        let accepted = CompletionEvent {
            provider: ProviderId::Claude,
            request_id,
        };
        assert_eq!(
            alert_candidate(&slot, accepted).unwrap().1.fetched_unix,
            123
        );
        assert!(alert_candidate(
            &slot,
            CompletionEvent {
                provider: ProviderId::Codex,
                request_id,
            }
        )
        .is_none());
        assert!(alert_candidate(
            &slot,
            CompletionEvent {
                provider: ProviderId::Claude,
                request_id: RequestId(request_id.0 + 1),
            }
        )
        .is_none());
    }

    #[test]
    fn account_change_clears_all_bound_state_before_replacement_fetch() {
        let slot = Slot::new();
        let now = Instant::now();
        let (request_a, generation_a, account_a) = start_request(&slot, account(1));
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_a,
                generation_a,
                &account_a,
                FetchOutcome::Ok(snapshot(100)),
            ),
            reading(now, 100),
        ));
        *slot.cooldown_until.lock().unwrap() = Some(now + std::time::Duration::from_secs(60));
        slot.rl_streak.store(2, Ordering::SeqCst);

        let (_, generation_b, account_b) = start_request(&slot, account(2));
        assert_ne!(generation_b, generation_a);
        assert!(account_b.key != account_a.key);
        assert!(slot.state.lock().unwrap().is_none());
        assert!(slot.last_good.lock().unwrap().is_none());
        assert!(slot.cooldown_until.lock().unwrap().is_none());
        assert!(slot.last_fetch.lock().unwrap().is_none());
        assert_eq!(slot.rl_streak.load(Ordering::SeqCst), 0);
        assert!(effective_at(&slot, reading(now, 101)).0.is_none());
    }

    #[test]
    fn obsolete_completion_cannot_touch_new_account_state_or_fetch_flag() {
        let slot = Slot::new();
        let now = Instant::now();
        let (old_request, old_generation, old_account) = start_request(&slot, account(1));
        invalidate_account(&slot, ProviderId::Claude);
        let (new_request, new_generation, new_account) = start_request(&slot, account(2));

        assert!(!record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                old_request,
                old_generation,
                &old_account,
                failure(true, Some(900)),
            ),
            reading(now, 100),
        ));
        assert!(slot.state.lock().unwrap().is_none());
        assert!(slot.cooldown_until.lock().unwrap().is_none());
        assert_eq!(slot.rl_streak.load(Ordering::SeqCst), 0);
        assert!(slot.fetching.load(Ordering::SeqCst));
        assert_eq!(
            slot.identity.lock().unwrap().pending_request_id,
            Some(new_request)
        );
        assert_eq!(slot.identity.lock().unwrap().generation, new_generation);
        assert!(slot
            .identity
            .lock()
            .unwrap()
            .account
            .as_ref()
            .is_some_and(|current| current.key == new_account.key));
    }

    #[test]
    fn failed_first_fetch_after_switch_cannot_restore_previous_snapshot_or_plan() {
        let slot = Slot::new();
        let now = Instant::now();
        let (request_a, generation_a, account_a) = start_request(&slot, account(1));
        let mut old = snapshot(100);
        old.plan = Some("Old plan".to_string());
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_a,
                generation_a,
                &account_a,
                FetchOutcome::Ok(old),
            ),
            reading(now, 100),
        ));

        let (request_b, generation_b, account_b) = start_request(&slot, account(2));
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request_b,
                generation_b,
                &account_b,
                failure(false, None),
            ),
            reading(now, 101),
        ));
        let (visible, error) = effective_at(&slot, reading(now, 101));
        assert!(visible.is_none());
        assert_eq!(error.as_deref(), Some("sanitized failure"));
        assert!(alert_candidate(
            &slot,
            CompletionEvent {
                provider: ProviderId::Claude,
                request_id: request_b,
            }
        )
        .is_none());
    }

    #[test]
    fn invalidating_preparation_failure_clears_account_but_transient_failure_does_not() {
        let slot = Slot::new();
        let now = Instant::now();
        let (request, generation, current) = start_request(&slot, account(1));
        assert!(record_fetch_completion(
            &slot,
            ProviderId::Claude,
            completion(
                ProviderId::Claude,
                request,
                generation,
                &current,
                FetchOutcome::Ok(snapshot(100)),
            ),
            reading(now, 100),
        ));

        slot.fetching.store(true, Ordering::SeqCst);
        let transient_request = reserve_request(&slot);
        assert!(record_preparation_failure(
            &slot,
            ProviderId::Claude,
            transient_request,
            api::PreparationFailure {
                kind: api::PreparationFailureKind::TemporarilyUnreadable,
                message: "temporary",
            },
            reading(now, 101),
        ));
        assert!(slot.identity.lock().unwrap().account.is_some());
        assert!(effective_at(&slot, reading(now, 101)).0.is_some());

        slot.fetching.store(true, Ordering::SeqCst);
        let missing_request = reserve_request(&slot);
        assert!(record_preparation_failure(
            &slot,
            ProviderId::Claude,
            missing_request,
            api::PreparationFailure {
                kind: api::PreparationFailureKind::Missing,
                message: "missing",
            },
            reading(now, 102),
        ));
        assert!(slot.identity.lock().unwrap().account.is_none());
        assert!(effective_at(&slot, reading(now, 102)).0.is_none());
    }
}
