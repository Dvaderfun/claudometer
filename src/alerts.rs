//! Native Windows 11 toast alerts when a limit window crosses the warn
//! threshold (75%).
//!
//! Uses the real WinRT toast pipeline (`ToastNotificationManager`) rather than
//! legacy tray balloons: toasts land in Action Center, respect Focus Assist /
//! Do-not-disturb and the per-app notification settings page. An unpackaged
//! Win32 exe qualifies by registering an AppUserModelID under
//! `HKCU\Software\Classes\AppUserModelId` — no MSIX packaging needed.
//!
//! One alert per limit-window *instance*: dedup keys on the row's raw
//! `resets_at` and is persisted in settings.json, so neither polling every
//! minute nor restarting the app repeats an alert within the same window.
//! Balloon fallback (main.rs) only when the WinRT path errors out.

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use windows::core::*;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager,
};

use crate::api::{LimitRow, UsageSnapshot};
use crate::provider::model::{
    AccountContext, AccountKey, IdentityPersistence, LimitId, ProviderId,
};
use crate::runtime_state::AlertReceiptV1;
use crate::{config, util};

/// Fire once per window when a limit row reaches this percent.
pub const WARN_AT: f64 = 75.0;

const AUMID: PCWSTR = w!("Claudometer");
const AUMID_KEY: PCWSTR = w!("Software\\Classes\\AppUserModelId\\Claudometer");

struct AlertState {
    receipts: Vec<AlertReceiptV1>,
    persistence: [Option<IdentityPersistence>; 2],
}

impl AlertState {
    fn load() -> Self {
        let receipts = crate::runtime_state::alert_receipts();
        let mut persistence = [None, None];
        for receipt in &receipts {
            persistence[receipt.provider.index()] = Some(IdentityPersistence::Persistent);
        }
        Self {
            receipts,
            persistence,
        }
    }
}

/// Account-scoped receipts, lazily seeded from sanitized state.json.
static ALERTED: Mutex<Option<AlertState>> = Mutex::new(None);

thread_local! {
    /// The OS routes Activated through the ToastNotification object that was
    /// shown — drop it and clicks stop reaching us. Keep the recent few alive.
    static KEEP: RefCell<Vec<ToastNotification>> = const { RefCell::new(Vec::new()) };
}

/// Once at startup: tie the process to the AUMID and register it for toasts.
pub fn init() {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(AUMID);
        let set = |name: PCWSTR, val: &str| {
            let wide: Vec<u16> = val.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                AUMID_KEY,
                name,
                REG_SZ.0,
                Some(wide.as_ptr() as *const _),
                (wide.len() * 2) as u32,
            );
        };
        set(w!("DisplayName"), "Claudometer");
        if let Some(icon) = ensure_icon() {
            set(w!("IconUri"), &icon.display().to_string());
        }
    }
}

/// Toast header icon must be a file on disk — extract the embedded ico once.
fn ensure_icon() -> Option<std::path::PathBuf> {
    let bytes: &[u8] = include_bytes!("../assets/icon.ico");
    let dir = util::config_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join("icon.ico");
    if std::fs::metadata(&p).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        std::fs::write(&p, bytes).ok()?;
    }
    Some(p)
}

/// Evaluate a fresh (just-fetched) snapshot; toast every limit row newly
/// at/over `WARN_AT` for its current window instance. UI thread only.
pub fn check(provider: ProviderId, account: &AccountContext, snap: &UsageSnapshot) {
    if !config::settings().alerts_enabled {
        return;
    }
    let mut guard = ALERTED.lock().unwrap();
    let state = guard.get_or_insert_with(AlertState::load);
    state.persistence[provider.index()] = Some(account.persistence);

    let mut receipts_changed = false;
    let mut crossed = Vec::new();
    for row in &snap.rows {
        if row.kind == "extra" {
            continue;
        }
        let Some(limit) = LimitId::new(row.kind.clone()) else {
            continue;
        };
        let decision = apply_observation(
            &mut state.receipts,
            provider,
            &account.key,
            limit,
            row.percent,
            row.resets_unix,
        );
        receipts_changed |= decision.receipts_changed;
        if decision.fire {
            crossed.push(row);
        }
    }
    if receipts_changed {
        let persistent: Vec<_> = state
            .receipts
            .iter()
            .filter(|receipt| {
                state.persistence[receipt.provider.index()] == Some(IdentityPersistence::Persistent)
            })
            .cloned()
            .collect();
        let _ = crate::runtime_state::persist_alert_receipts(&persistent);
    }
    if crossed.is_empty() {
        return;
    }

    // Downgrade compatibility only. New code never reads this map after the
    // provider's one-time account-scoped migration marker is set.
    let mut legacy = config::legacy_alert_receipts();
    for row in &crossed {
        legacy.insert(
            format!("{}.{}.{}", provider_name(provider), row.kind, row.label),
            row.resets_unix.unwrap_or(0),
        );
    }
    let _ = config::save_legacy_alert_receipts(&legacy);
    drop(guard);
    notify(provider_name(provider), &crossed);
}

pub fn account_changed(provider: ProviderId, account: Option<&AccountContext>) {
    let persisted =
        crate::runtime_state::select_account(provider, account, &config::legacy_alert_receipts());
    let mut guard = ALERTED.lock().unwrap();
    let state = guard.get_or_insert_with(AlertState::load);
    state
        .receipts
        .retain(|receipt| receipt.provider != provider);
    state.receipts.extend(
        persisted
            .into_iter()
            .filter(|receipt| receipt.provider == provider),
    );
    state.persistence[provider.index()] = account.map(|account| account.persistence);
}

fn provider_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Claude => "Claude",
        ProviderId::Codex => "Codex",
    }
}

/// The API's `resets_at` for an in-flight window drifts by a minute or two
/// between polls (observed: session flipping 20:09 ↔ 20:10). Epochs closer
/// than this are the *same* window; a real rollover jumps hours (session)
/// or days (weekly). The stored epoch is deliberately NOT updated on a
/// within-slop match, so repeated small drifts can't creep past the slop.
const EPOCH_SLOP: i64 = 30 * 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AlertDecision {
    fire: bool,
    receipts_changed: bool,
}

/// Apply one fresh observation to account/provider/limit/threshold-scoped
/// receipts. A timestamp identifies the reset instance directly. Without one,
/// only an observed below-threshold state can re-arm the receipt.
fn apply_observation(
    receipts: &mut Vec<AlertReceiptV1>,
    provider: ProviderId,
    account: &AccountKey,
    limit: LimitId,
    pct: f64,
    reset_instance_unix: Option<i64>,
) -> AlertDecision {
    let same_limit = |receipt: &AlertReceiptV1| {
        receipt.provider == provider
            && &receipt.account == account
            && receipt.limit == limit
            && receipt.threshold_percent == WARN_AT as u8
    };

    if pct < WARN_AT {
        if reset_instance_unix.is_none() {
            let mut changed = false;
            for receipt in receipts
                .iter_mut()
                .filter(|receipt| same_limit(receipt) && receipt.reset_instance_unix.is_none())
            {
                if !receipt.below_threshold_observed {
                    receipt.below_threshold_observed = true;
                    changed = true;
                }
            }
            return AlertDecision {
                receipts_changed: changed,
                ..AlertDecision::default()
            };
        }
        return AlertDecision::default();
    }

    if let Some(receipt) = receipts
        .iter_mut()
        .filter(|receipt| same_limit(receipt))
        .find(
            |receipt| match (receipt.reset_instance_unix, reset_instance_unix) {
                (Some(previous), Some(current)) => (current - previous).abs() <= EPOCH_SLOP,
                (None, None) => true,
                _ => false,
            },
        )
    {
        if reset_instance_unix.is_none() && receipt.below_threshold_observed {
            receipt.below_threshold_observed = false;
            return AlertDecision {
                fire: true,
                receipts_changed: true,
            };
        }
        return AlertDecision::default();
    }

    receipts.retain(|receipt| !same_limit(receipt));
    receipts.push(AlertReceiptV1 {
        provider,
        account: account.clone(),
        limit,
        threshold_percent: WARN_AT as u8,
        reset_instance_unix,
        below_threshold_observed: false,
    });
    AlertDecision {
        fire: true,
        receipts_changed: true,
    }
}

#[cfg(test)]
fn should_fire(
    receipts: &mut Vec<AlertReceiptV1>,
    provider: ProviderId,
    account: &AccountKey,
    limit: LimitId,
    pct: f64,
    reset_instance_unix: Option<i64>,
) -> bool {
    apply_observation(receipts, provider, account, limit, pct, reset_instance_unix).fire
}

fn notify(provider: &str, rows: &[&LimitRow]) {
    let worst = rows
        .iter()
        .max_by(|a, b| a.percent.total_cmp(&b.percent))
        .expect("notify called with rows");
    let title = if rows.len() == 1 {
        format!("{provider}: {} at {:.0}%", worst.label, worst.percent)
    } else {
        format!("{provider} usage is running high")
    };
    let lines: Vec<String> = if rows.len() == 1 {
        if worst.reset_text.is_empty() {
            Vec::new()
        } else {
            vec![prettify_reset(&worst.reset_text)]
        }
    } else {
        rows.iter()
            .take(2)
            .map(|r| {
                if r.reset_text.is_empty() {
                    format!("{} — {:.0}% used", r.label, r.percent)
                } else {
                    format!("{} — {:.0}% used · {}", r.label, r.percent, r.reset_text)
                }
            })
            .collect()
    };
    if show_toast(&title, &lines, &worst.label, worst.percent).is_err() {
        crate::tray_balloon(&title, &lines.join("\n"));
    }
}

/// "resets 18:59" → "Resets 18:59" for standalone body lines.
fn prettify_reset(s: &str) -> String {
    crate::api::prettify(s)
}

/// Build + show one toast: title, optional detail lines, and a native
/// progress bar pinned to the worst limit. Click bounces WM_TOAST_ACTIVATED
/// to the UI thread, which opens the flyout at the tray icon.
fn show_toast(title: &str, lines: &[String], bar_label: &str, bar_pct: f64) -> Result<()> {
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Claudometer"))?;
    // The user turned Claudometer off in Windows notification settings —
    // honor that; the balloon fallback must not resurrect the alert.
    if notifier.Setting() == Ok(NotificationSetting::DisabledForApplication)
        || notifier.Setting() == Ok(NotificationSetting::DisabledForUser)
    {
        return Ok(());
    }

    let mut xml = String::with_capacity(512);
    xml.push_str(
        "<toast activationType=\"foreground\"><visual><binding template=\"ToastGeneric\">",
    );
    xml.push_str(&format!("<text>{}</text>", esc(title)));
    for l in lines {
        xml.push_str(&format!("<text>{}</text>", esc(l)));
    }
    xml.push_str(&format!(
        "<progress title=\"{}\" value=\"{:.2}\" valueStringOverride=\"{:.0}%\" status=\"used\"/>",
        esc(bar_label),
        (bar_pct / 100.0).clamp(0.0, 1.0),
        bar_pct
    ));
    xml.push_str("</binding></visual></toast>");

    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;

    let main = crate::MAIN_HWND.load(Ordering::SeqCst);
    if main != 0 {
        toast.Activated(&TypedEventHandler::new(move |_, _| {
            // WinRT threadpool thread — bounce to the UI thread
            unsafe {
                let _ = PostMessageW(
                    HWND(main as *mut _),
                    crate::WM_TOAST_ACTIVATED,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            Ok(())
        }))?;
    }

    notifier.Show(&toast)?;
    KEEP.with(|k| {
        let mut k = k.borrow_mut();
        k.push(toast);
        if k.len() > 4 {
            k.remove(0);
        }
    });
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `--test-alert`: exercise the whole pipeline (registration, XML, Show)
/// with fake data — verifies notifications without waiting for real 75%.
/// The exe has no console, so the outcome lands in alert-test.txt.
pub fn show_test() {
    let row = LimitRow {
        kind: "session".into(),
        label: "Session (5h)".into(),
        percent: 78.0,
        severity: String::new(),
        reset_text: "resets 18:59".into(),
        resets_unix: None,
    };
    let out = match show_toast(
        &format!("Claude (test): {} at 78%", row.label),
        &[prettify_reset(&row.reset_text)],
        &row.label,
        row.percent,
    ) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("toast failed: {e}"),
    };
    if let Some(dir) = util::config_dir() {
        let _ = std::fs::write(dir.join("alert-test.txt"), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(byte: u8) -> AccountKey {
        AccountKey::from_digest([byte; 32])
    }

    fn limit(value: &str) -> LimitId {
        LimitId::new(value).unwrap()
    }

    #[test]
    fn below_threshold_never_fires() {
        let mut seen = Vec::new();
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account(1),
            limit("session"),
            74.9,
            Some(100),
        ));
        assert!(seen.is_empty());
    }

    #[test]
    fn fires_once_per_window_instance() {
        let mut seen = Vec::new();
        let account = account(1);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            75.0,
            Some(100_000),
        ));
        // same window, climbing percent — stays quiet
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            82.0,
            Some(100_000),
        ));
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            99.0,
            Some(100_000),
        ));
        // window rolled over (resets_at jumped 5h) — fires again
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            76.0,
            Some(118_000),
        ));
    }

    #[test]
    fn resets_at_drift_does_not_refire() {
        let mut seen = Vec::new();
        let account = account(1);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            75.0,
            Some(100_000),
        ));
        // the observed API behavior: resets_at oscillates by ±60 s per poll
        for reset in [100_060, 99_940, 100_060] {
            assert!(!should_fire(
                &mut seen,
                ProviderId::Claude,
                &account,
                limit("session"),
                83.0,
                Some(reset),
            ));
        }
        // anti-creep: stored epoch stays first-seen — repeated small drifts
        // in one direction still count as the same window
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            92.0,
            Some(101_500),
        ));
        // a genuine rollover (hours away) fires
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            76.0,
            Some(100_000 + 5 * 3600),
        ));
    }

    #[test]
    fn windows_are_independent() {
        let mut seen = Vec::new();
        let first = account(1);
        let second = account(2);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &first,
            limit("session"),
            80.0,
            Some(100),
        ));
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &first,
            limit("weekly_all"),
            80.0,
            Some(500),
        ));
        assert!(should_fire(
            &mut seen,
            ProviderId::Codex,
            &first,
            limit("session"),
            80.0,
            Some(100),
        ));
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &second,
            limit("session"),
            80.0,
            Some(100),
        ));
    }

    #[test]
    fn missing_epoch_rearms_only_after_observed_below_threshold() {
        let mut seen = Vec::new();
        let account = account(1);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("other"),
            90.0,
            None,
        ));
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("other"),
            95.0,
            None,
        ));
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("other"),
            20.0,
            None,
        ));
        assert!(seen[0].below_threshold_observed);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("other"),
            90.0,
            None,
        ));
        assert!(!seen[0].below_threshold_observed);
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("other"),
            92.0,
            None,
        ));
    }

    #[test]
    fn below_threshold_does_not_rearm_a_known_reset_instance() {
        let mut seen = Vec::new();
        let account = account(1);
        assert!(should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            80.0,
            Some(100_000),
        ));
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            20.0,
            Some(100_000),
        ));
        assert!(!should_fire(
            &mut seen,
            ProviderId::Claude,
            &account,
            limit("session"),
            80.0,
            Some(100_000),
        ));
    }
}
