use std::fmt::Write as _;
use std::time::Duration;

use crate::provider::model::{ProviderId, SourceId, SourceProvenance};
use crate::provider::state::ViewState;

pub struct ProviderDiagnostic {
    pub provider: ProviderId,
    pub enabled: bool,
    pub detected: Option<bool>,
    pub authenticated: bool,
    pub source: SourceProvenance,
    pub fallback_reason: &'static str,
    pub phase: &'static str,
    pub freshness: ViewState,
    pub age_seconds: Option<i64>,
    pub last_attempt_unix: Option<i64>,
    pub last_success_unix: Option<i64>,
    pub observed_at_unix: Option<i64>,
    pub retry_at_unix: Option<i64>,
    pub next_retry_unix: Option<i64>,
    pub error_code: Option<&'static str>,
}

pub struct Snapshot {
    pub windows_build: Option<u32>,
    pub channel: &'static str,
    pub update: &'static str,
    pub update_recovery: &'static str,
    pub power_recovery: crate::vibecode::PersistentStatus,
    pub config: crate::config::ConfigStatus,
    pub runtime: crate::runtime_state::RuntimeStateStatus,
    pub settings: crate::config::SettingsV1,
    pub providers: [ProviderDiagnostic; 2],
}

pub fn snapshot() -> Snapshot {
    if crate::demo::is_active() {
        return synthetic_snapshot();
    }
    let settings = crate::config::settings();
    Snapshot {
        windows_build: windows_build(),
        channel: crate::updater::diagnostic_channel(),
        update: match crate::updater::status() {
            crate::updater::Status::UpToDate => "up to date",
            crate::updater::Status::Available(_) => "available",
            crate::updater::Status::Installing => "installing",
            crate::updater::Status::Failed(_, _) => "failed",
        },
        update_recovery: crate::updater::diagnostic_recovery(),
        power_recovery: crate::vibecode::persistent_status(),
        config: crate::config::status(),
        runtime: crate::runtime_state::status(),
        providers: crate::app::provider_diagnostics(Duration::from_secs(u64::from(
            settings.poll_interval_seconds,
        ))),
        settings,
    }
}

fn synthetic_snapshot() -> Snapshot {
    Snapshot {
        windows_build: Some(28020),
        channel: "portable (demo)",
        update: "up to date",
        update_recovery: "none",
        power_recovery: crate::vibecode::PersistentStatus::Disabled,
        config: crate::config::ConfigStatus::Ready,
        runtime: crate::runtime_state::RuntimeStateStatus::Ready,
        settings: crate::config::SettingsV1::default(),
        providers: std::array::from_fn(|index| {
            let provider = if index == 0 {
                ProviderId::Claude
            } else {
                ProviderId::Codex
            };
            ProviderDiagnostic {
                provider,
                enabled: true,
                detected: Some(true),
                authenticated: true,
                source: SourceProvenance::compatibility(provider),
                fallback_reason: if provider == ProviderId::Claude {
                    "compatibility source only"
                } else {
                    "documented source not enabled"
                },
                phase: "ready",
                freshness: ViewState::Fresh,
                age_seconds: Some(5),
                last_attempt_unix: Some(1000),
                last_success_unix: Some(1000),
                observed_at_unix: Some(1000),
                retry_at_unix: None,
                next_retry_unix: Some(1060),
                error_code: None,
            }
        }),
    }
}

impl Snapshot {
    pub fn text(&self) -> String {
        let mut text = format!("Claudometer {} · {}\nWindows build: {}\nInstall: {}\nUpdate: {} · recovery: {}\nPower recovery: {:?}\nSettings store: {:?}\nRuntime store: {:?}\n",
            env!("CARGO_PKG_VERSION"), if cfg!(target_arch="aarch64") { "arm64" } else { "x64" },
            self.windows_build.map_or_else(|| "unknown".to_string(), |build| build.to_string()),
            self.channel, self.update, self.update_recovery, self.power_recovery, self.config, self.runtime);
        for provider in &self.providers {
            let name = if provider.provider == ProviderId::Claude {
                "Claude"
            } else {
                "Codex"
            };
            let detected = match provider.detected {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            };
            let source = match provider.source.id {
                SourceId::ClaudeOAuthCompatibility => "Claude OAuth compatibility",
                SourceId::CodexWhamCompatibility => "Codex WHAM compatibility",
                SourceId::CodexAppServer => "Codex app-server",
            };
            let _ = writeln!(
                text,
                "\n{name}: {} · {:?}",
                provider.phase, provider.freshness
            );
            let _ = writeln!(
                text,
                "Enabled: {} · detected: {detected} · signed in: {}",
                provider.enabled, provider.authenticated
            );
            let _ = writeln!(text, "Source: {source}");
            let _ = writeln!(text, "Support: {:?}", provider.source.support);
            let _ = writeln!(text, "Fallback: {}", provider.fallback_reason);
            let _ = writeln!(
                text,
                "Attempt: {} · success: {}",
                timestamp(provider.last_attempt_unix),
                timestamp(provider.last_success_unix)
            );
            let _ = writeln!(
                text,
                "Observed: {} · age: {}s",
                timestamp(provider.observed_at_unix),
                timestamp(provider.age_seconds)
            );
            let _ = writeln!(
                text,
                "Cooldown: {} · next: {}",
                timestamp(provider.retry_at_unix),
                timestamp(provider.next_retry_unix)
            );
            let _ = writeln!(text, "Error: {}", provider.error_code.unwrap_or("none"));
        }
        let settings = &self.settings;
        let _ = write!(text, "\nRefresh: {}s · Codex: {} · alerts: {}\nUpdate checks: {} · wake lock: {}\nPersistent lid override: {}",
            settings.poll_interval_seconds, settings.codex_enabled, settings.alerts_enabled,
            settings.update_checks_enabled, settings.wake_lock_enabled, settings.persistent_lid_override_enabled);
        text
    }
}

fn timestamp(value: Option<i64>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

pub fn text() -> String {
    snapshot().text()
}

fn windows_build() -> Option<u32> {
    #[repr(C)]
    struct Version {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(version: *mut Version) -> i32;
    }
    let mut version = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    (unsafe { RtlGetVersion(&mut version) } >= 0).then_some(version.build)
}

pub fn copy(owner: windows::Win32::Foundation::HWND) -> windows::core::Result<()> {
    if crate::demo::is_active() {
        return Ok(());
    }
    copy_text(owner, &text())
}

fn copy_text(owner: windows::Win32::Foundation::HWND, value: &str) -> windows::core::Result<()> {
    let text: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        use windows::Win32::Foundation::{GlobalFree, E_FAIL, HANDLE};
        use windows::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };
        let allocation = GlobalAlloc(GMEM_MOVEABLE, text.len() * std::mem::size_of::<u16>())?;
        let pointer = GlobalLock(allocation);
        if pointer.is_null() {
            let _ = GlobalFree(allocation);
            return Err(windows::core::Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast::<u16>(), text.len());
        let _ = GlobalUnlock(allocation);
        if let Err(error) = OpenClipboard(owner) {
            let _ = GlobalFree(allocation);
            return Err(error);
        }
        let result =
            EmptyClipboard().and_then(|()| SetClipboardData(13, HANDLE(allocation.0)).map(|_| ()));
        let _ = CloseClipboard();
        if result.is_err() {
            let _ = GlobalFree(allocation);
        }
        result.map_err(|_| windows::core::Error::new(E_FAIL, "Could not copy diagnostics"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_covers_required_operational_fields_without_personal_data() {
        let text = synthetic_snapshot().text();
        for field in [
            "Claudometer",
            "Windows build",
            "Install:",
            "Update:",
            "recovery:",
            "Settings store:",
            "Runtime store:",
            "Claude:",
            "Codex:",
            "Enabled:",
            "detected:",
            "signed in:",
            "Source:",
            "Fallback:",
            "Attempt:",
            "success:",
            "Observed:",
            "age:",
            "Cooldown:",
            "next:",
            "Error:",
            "Refresh:",
            "Update checks:",
            "wake lock:",
            "Persistent lid override:",
        ] {
            assert!(text.contains(field), "missing {field}");
        }
        for forbidden in [
            "access_token",
            "refresh_token",
            "Bearer",
            "account",
            "email",
            "username",
            "USERPROFILE",
            "C:\\Users",
            "raw_body",
        ] {
            assert!(!text.contains(forbidden), "unexpected {forbidden}");
        }
        assert!(text.len() < 8192);
    }

    #[test]
    fn diagnostics_render_unknown_disabled_cached_and_error_states() {
        let mut snapshot = synthetic_snapshot();
        snapshot.providers[0].detected = None;
        snapshot.providers[0].authenticated = false;
        snapshot.providers[0].freshness = ViewState::Cached;
        snapshot.providers[0].last_success_unix = None;
        snapshot.providers[0].error_code = Some("request_timeout");
        snapshot.providers[1].enabled = false;
        snapshot.providers[1].phase = "disabled";
        let text = snapshot.text();
        assert!(text.contains("detected: unknown"));
        assert!(text.contains("success: none"));
        assert!(text.contains("Cached"));
        assert!(text.contains("request_timeout"));
        assert!(text.contains("Codex: disabled"));
    }
}
