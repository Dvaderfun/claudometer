use std::collections::VecDeque;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::app::Preparation;
use crate::provider::model::{
    AccountContext, FetchCompletion, FetchOutcome, ProviderId, SourceProvenance,
};

pub enum AppEvent {
    Prepared {
        preparation: Preparation,
        account: AccountContext,
        source: SourceProvenance,
        fallback_reason: &'static str,
        reply: mpsc::Sender<Option<FetchCompletion<()>>>,
    },
    PreparationFailed {
        preparation: Preparation,
        failure: crate::api::PreparationFailure,
    },
    Completed(FetchCompletion<FetchOutcome>),
}

static EVENTS: Mutex<VecDeque<AppEvent>> = Mutex::new(VecDeque::new());

pub fn take_events() -> VecDeque<AppEvent> {
    std::mem::take(&mut *EVENTS.lock().unwrap())
}

fn post(event: AppEvent) -> bool {
    let handle = crate::MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if handle == 0 {
        return false;
    }
    let mut events = EVENTS.lock().unwrap();
    events.push_back(event);
    // Retain the event even when Windows cannot enqueue the wakeup. The
    // regular polling tick drains this same queue; no completion is lost.
    unsafe {
        PostMessageW(
            HWND(handle as *mut _),
            crate::WM_DATA_READY,
            WPARAM(0),
            LPARAM(0),
        )
        .ok();
    }
    true
}

enum PreparedFetch {
    Claude(crate::api::PreparedRequest),
    Codex(crate::codex::PreparedRequest),
}

impl PreparedFetch {
    fn selection(&self) -> (SourceProvenance, &'static str) {
        match self {
            Self::Claude(_) => (
                SourceProvenance::compatibility(ProviderId::Claude),
                "compatibility source only",
            ),
            Self::Codex(request) => (request.source(), request.fallback_reason),
        }
    }
    fn account(&self) -> &AccountContext {
        match self {
            Self::Claude(request) => request.account(),
            Self::Codex(request) => request.account(),
        }
    }

    fn execute(self) -> FetchOutcome {
        match self {
            Self::Claude(request) => crate::api::fetch(request),
            Self::Codex(request) => crate::codex::fetch(request),
        }
    }
}

fn prepare(provider: ProviderId) -> Result<PreparedFetch, crate::api::PreparationFailure> {
    match provider {
        ProviderId::Claude => crate::api::prepare().map(PreparedFetch::Claude),
        ProviderId::Codex => crate::codex::prepare_poll().map(PreparedFetch::Codex),
    }
}

pub fn spawn(preparation: Preparation) {
    let started = std::thread::Builder::new().spawn(move || {
        let prepared = match prepare(preparation.provider) {
            Ok(prepared) => prepared,
            Err(failure) => {
                post(AppEvent::PreparationFailed {
                    preparation,
                    failure,
                });
                return;
            }
        };
        let selection = prepared.selection();
        run_prepared(
            preparation,
            prepared.account().clone(),
            selection,
            || prepared.execute(),
            post,
        );
    });
    if started.is_err() {
        post(AppEvent::PreparationFailed {
            preparation,
            failure: crate::api::PreparationFailure {
                kind: crate::api::PreparationFailureKind::TemporarilyUnreadable,
                message: "Could not start usage check",
            },
        });
    }
}

fn run_prepared(
    preparation: Preparation,
    account: AccountContext,
    selection: (SourceProvenance, &'static str),
    execute: impl FnOnce() -> FetchOutcome,
    mut publish: impl FnMut(AppEvent) -> bool,
) {
    let (reply, accepted) = mpsc::channel();
    if !publish(AppEvent::Prepared {
        preparation,
        account,
        source: selection.0,
        fallback_reason: selection.1,
        reply,
    }) {
        return;
    }
    let Ok(Some(ticket)) = accepted.recv_timeout(Duration::from_secs(10)) else {
        return;
    };
    publish(AppEvent::Completed(FetchCompletion {
        provider: ticket.provider,
        generation: ticket.generation,
        request_id: ticket.request_id,
        account: ticket.account,
        payload: execute(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::model::{AccountKey, Generation, IdentityPersistence, RequestId};
    use crate::provider::state::RefreshTrigger;

    #[test]
    fn worker_executes_only_after_ui_acceptance_and_publishes_exact_ticket() {
        for accept in [true, false] {
            let preparation = Preparation {
                provider: ProviderId::Claude,
                generation: Generation(1),
                operation: RequestId(2),
                trigger: RefreshTrigger::Manual,
            };
            let account = AccountContext {
                key: AccountKey::from_digest([1; 32]),
                persistence: IdentityPersistence::Persistent,
            };
            let mut executed = false;
            let mut completions = 0;
            run_prepared(
                preparation,
                account.clone(),
                (SourceProvenance::compatibility(ProviderId::Claude), "none"),
                || {
                    executed = true;
                    FetchOutcome::Err {
                        msg: "synthetic".to_string(),
                        retry_after: None,
                        rate_limited: false,
                    }
                },
                |event| {
                    match event {
                        AppEvent::Prepared {
                            preparation: event,
                            account: identity,
                            reply,
                            ..
                        } => {
                            assert_eq!(event.operation, preparation.operation);
                            assert!(identity.key == account.key);
                            reply
                                .send(accept.then(|| FetchCompletion {
                                    provider: ProviderId::Claude,
                                    generation: Generation(3),
                                    request_id: RequestId(4),
                                    account: account.key.clone(),
                                    payload: (),
                                }))
                                .unwrap();
                        }
                        AppEvent::Completed(completion) => {
                            completions += 1;
                            assert_eq!(completion.generation, Generation(3));
                            assert_eq!(completion.request_id, RequestId(4));
                            assert!(completion.account == account.key);
                        }
                        _ => panic!("unexpected event"),
                    }
                    true
                },
            );
            assert_eq!(executed, accept);
            assert_eq!(completions, usize::from(accept));
        }
    }

    #[test]
    fn failed_publication_or_dropped_ui_reply_performs_no_provider_work() {
        for posted in [true, false] {
            run_prepared(
                Preparation {
                    provider: ProviderId::Claude,
                    generation: Generation(1),
                    operation: RequestId(1),
                    trigger: RefreshTrigger::Manual,
                },
                AccountContext {
                    key: AccountKey::from_digest([1; 32]),
                    persistence: IdentityPersistence::Persistent,
                },
                (SourceProvenance::compatibility(ProviderId::Claude), "none"),
                || panic!("must not execute without UI ticket"),
                |_| posted,
            );
        }
    }
}
