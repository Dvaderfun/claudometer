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
use std::collections::HashMap;
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
use crate::util;

/// Fire once per window when a limit row reaches this percent.
pub const WARN_AT: f64 = 75.0;

const AUMID: PCWSTR = w!("Claudometer");
const AUMID_KEY: PCWSTR = w!("Software\\Classes\\AppUserModelId\\Claudometer");

/// limit key → resets_at epoch already alerted; lazily seeded from settings.json
static ALERTED: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

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
pub fn check(provider: &'static str, snap: &UsageSnapshot) {
    if !util::alerts_enabled() {
        return;
    }
    let mut guard = ALERTED.lock().unwrap();
    let seen = guard.get_or_insert_with(util::load_alerted);

    let crossed: Vec<&LimitRow> = snap
        .rows
        .iter()
        .filter(|r| {
            r.kind != "extra" // pay-as-you-go bucket, not a limit window
                && should_fire(
                    seen,
                    &format!("{provider}.{}.{}", r.kind, r.label),
                    r.percent,
                    r.resets_unix.unwrap_or(0),
                )
        })
        .collect();
    if crossed.is_empty() {
        return;
    }
    util::save_alerted(seen);
    drop(guard);
    notify(provider, &crossed);
}

/// Pure dedup decision: fire when over threshold AND this window instance
/// (identified by its resets_at epoch) hasn't fired before. Marks on fire.
fn should_fire(seen: &mut HashMap<String, i64>, key: &str, pct: f64, epoch: i64) -> bool {
    if pct < WARN_AT || seen.get(key) == Some(&epoch) {
        return false;
    }
    seen.insert(key.to_string(), epoch);
    true
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
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Claudometer"))?;
    // The user turned Claudometer off in Windows notification settings —
    // honor that; the balloon fallback must not resurrect the alert.
    if notifier.Setting() == Ok(NotificationSetting::DisabledForApplication)
        || notifier.Setting() == Ok(NotificationSetting::DisabledForUser)
    {
        return Ok(());
    }

    let mut xml = String::with_capacity(512);
    xml.push_str("<toast activationType=\"foreground\"><visual><binding template=\"ToastGeneric\">");
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

    #[test]
    fn below_threshold_never_fires() {
        let mut seen = HashMap::new();
        assert!(!should_fire(&mut seen, "claude.session", 74.9, 100));
        assert!(seen.is_empty());
    }

    #[test]
    fn fires_once_per_window_instance() {
        let mut seen = HashMap::new();
        assert!(should_fire(&mut seen, "claude.session", 75.0, 100));
        // same window, climbing percent — stays quiet
        assert!(!should_fire(&mut seen, "claude.session", 82.0, 100));
        assert!(!should_fire(&mut seen, "claude.session", 99.0, 100));
        // window rolled over (new resets_at) — fires again
        assert!(should_fire(&mut seen, "claude.session", 76.0, 200));
    }

    #[test]
    fn windows_are_independent() {
        let mut seen = HashMap::new();
        assert!(should_fire(&mut seen, "claude.session", 80.0, 100));
        assert!(should_fire(&mut seen, "claude.weekly_all", 80.0, 500));
        assert!(should_fire(&mut seen, "codex.session", 80.0, 100));
    }

    #[test]
    fn missing_epoch_fires_once() {
        let mut seen = HashMap::new();
        assert!(should_fire(&mut seen, "claude.extra", 90.0, 0));
        assert!(!should_fire(&mut seen, "claude.extra", 95.0, 0));
    }
}
