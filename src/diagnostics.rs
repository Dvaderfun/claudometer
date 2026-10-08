use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::provider::model::{ProviderId, SourceId, SourceProvenance};
use crate::provider::state::ViewState;

const LOG_BYTES: u64 = 256 * 1024;
const REDACTED: &str = "[redacted]";

// Accept only fixed operational vocabulary. Arbitrary input, including
// unknown future messages, is removed as a whole rather than guessed at.
#[inline(never)]
fn safe_field(value: &str) -> &str {
    const ALLOWED: &str = "none|unknown|portable|portable (demo)|managed|ambiguous|up to date|available|installing|failed|journal pending|compatibility source only|documented source not enabled|disabled|unavailable|idle|fetching|ready|backoff|preparing|credentials_missing|sign_in_expired|usage_scope_missing|provider_rate_limited|request_timeout|connection_failed|response_invalid|local_unavailable|startup|render_failed|render_init_failed|config_failed|registry_failed|provider_success|provider_failed|clipboard_failed|power_failed|update_failed|runtime_failed";
    if ALLOWED.split('|').any(|allowed| allowed == value) {
        value
    } else {
        REDACTED
    }
}
struct LogState {
    path: Option<PathBuf>,
    last_issue: Option<&'static str>,
    write_failed: bool,
}

static LOG: Mutex<LogState> = Mutex::new(LogState {
    path: None,
    last_issue: None,
    write_failed: false,
});

fn append_log(path: &Path, line: &[u8]) -> Result<(), ()> {
    if line.len() as u64 > LOG_BYTES {
        return Err(());
    }
    if std::fs::metadata(path)
        .map_or(0, |metadata| metadata.len())
        .saturating_add(line.len() as u64)
        > LOG_BYTES
    {
        let oldest = path.with_extension("log.2");
        let previous = path.with_extension("log.1");
        match std::fs::remove_file(&oldest) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(()),
        }
        if previous.exists() {
            rotate_file(&previous, &oldest)?;
        }
        if path.exists() {
            rotate_file(path, &previous)?;
        }
    }
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_ALWAYS,
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let handle = CreateFileW(
            windows::core::PCWSTR(path.as_ptr()),
            FILE_APPEND_DATA.0,
            FILE_SHARE_READ,
            None,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(|_| ())?;
        let result = write_handle(handle, line).map_err(|_| ());
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        result
    }
}

fn rotate_file(source: &Path, destination: &Path) -> Result<(), ()> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        windows::Win32::Storage::FileSystem::MoveFileExW(
            windows::core::PCWSTR(source.as_ptr()),
            windows::core::PCWSTR(destination.as_ptr()),
            windows::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING,
        )
        .map_err(|_| ())
    }
}

fn log_line(code: &str) -> String {
    format!(
        "{} {}\n",
        time::OffsetDateTime::now_utc().unix_timestamp(),
        safe_field(code)
    )
}

#[inline(never)]
pub fn record(code: &'static str) {
    if cfg!(test) {
        return;
    }
    if crate::demo::is_active() {
        return;
    }
    let mut log = LOG.lock().unwrap();
    let code = safe_field(code);
    let issue = code != "startup" && code != "provider_success";
    // Rendering may fail on every repaint; do not grow IO with repeated failures.
    if issue && log.last_issue == Some(code) {
        return;
    }
    if issue {
        log.last_issue = Some(code);
    }
    if let Some(path) = &log.path {
        if append_log(path, log_line(code).as_bytes()).is_err() {
            log.write_failed = true;
        }
    }
}

pub fn observe<T, E>(result: Result<T, E>, code: &'static str) -> Result<T, E> {
    if result.is_err() {
        record(code);
    }
    result
}

pub fn initialize_log() {
    if crate::demo::is_active() {
        return;
    }
    let mut log = LOG.lock().unwrap();
    if let Some(directory) = crate::config::config_dir() {
        log.path = Some(directory.join("diagnostics.log"));
    } else {
        log.write_failed = true;
    }
    drop(log);
    record("startup");
    if crate::config::diagnostic().is_some() {
        record("config_failed");
    }
    if crate::runtime_state::diagnostic().is_some() {
        record("runtime_failed");
    }
}
pub fn support_command(args: &[String]) -> Option<windows::core::Result<()>> {
    let version = args.iter().any(|arg| arg == "--version");
    let diagnose = args.iter().any(|arg| arg == "--diagnose");
    if !version && !diagnose {
        return None;
    }
    Some({
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        unsafe {
            AttachConsole(u32::MAX);
        }
        if version {
            write_stdout(concat!("Claudometer ", env!("CARGO_PKG_VERSION"), "\n").as_bytes())
        } else {
            support_snapshot()
        }
    })
}

fn support_snapshot() -> windows::core::Result<()> {
    // This dedicated support process never creates a UI thread/window.
    // Prepared credentials stay local and are dropped without execute.
    crate::config::initialize_read_only();
    crate::runtime_state::initialize_read_only();
    crate::app::load_local_diagnostics();
    write_stdout(text().as_bytes())
}
fn write_stdout(bytes: &[u8]) -> windows::core::Result<()> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(kind: u32) -> *mut std::ffi::c_void;
    }
    let handle = windows::Win32::Foundation::HANDLE(unsafe { GetStdHandle((-11_i32) as u32) });
    write_handle(handle, bytes)
}

fn write_handle(
    handle: windows::Win32::Foundation::HANDLE,
    bytes: &[u8],
) -> windows::core::Result<()> {
    #[link(name = "kernel32")]
    extern "system" {
        fn WriteFile(
            handle: *mut std::ffi::c_void,
            buffer: *const u8,
            count: u32,
            written: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    unsafe {
        let mut offset = 0;
        while offset < bytes.len() {
            let mut written = 0;
            if WriteFile(
                handle.0,
                bytes[offset..].as_ptr(),
                (bytes.len() - offset) as u32,
                &mut written,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(windows::core::Error::from_win32());
            }
            if written == 0 {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "Could not write diagnostics output",
                ));
            }
            offset += written as usize;
        }
    }
    Ok(())
}

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
    #[inline(never)]
    pub fn text(&self) -> String {
        let mut text = format!("Claudometer {} · {}\nWindows build: {}\nInstall: {}\nUpdate: {} · recovery: {}\nPower recovery: {}\nSettings store: {}\nRuntime store: {}\n",
            env!("CARGO_PKG_VERSION"), if cfg!(target_arch="aarch64") { "arm64" } else { "x64" },
            timestamp(self.windows_build.map(i64::from)),
            safe_field(self.channel), safe_field(self.update), safe_field(self.update_recovery), power_status(self.power_recovery), config_status(self.config), runtime_status(self.runtime));
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
            let _ = writeln!(text,
                "\n{name}: {} · {}\nEnabled: {} · detected: {detected} · signed in: {}\nSource: {source}\nSupport: {}\nFallback: {}\nAttempt: {} · success: {}\nObserved: {} · age: {}\nCooldown: {} · next: {}\nError: {}",
                safe_field(provider.phase), freshness(provider.freshness), provider.enabled, provider.authenticated,
                if provider.source.support == crate::provider::model::SourceSupport::Documented { "Documented" } else { "Compatibility" },
                safe_field(provider.fallback_reason), timestamp(provider.last_attempt_unix), timestamp(provider.last_success_unix),
                timestamp(provider.observed_at_unix), timestamp(provider.age_seconds),
                timestamp(provider.retry_at_unix), timestamp(provider.next_retry_unix), safe_field(provider.error_code.unwrap_or("none")));
        }
        let settings = &self.settings;
        let _ = write!(text, "\nRefresh: {}s · Codex: {} · alerts: {}\nUpdate checks: {} · wake lock: {}\nPersistent lid override: {}",
            settings.poll_interval_seconds, settings.codex_enabled, settings.alerts_enabled,
            settings.update_checks_enabled, settings.wake_lock_enabled, settings.persistent_lid_override_enabled);
        if !crate::demo::is_active() {
            let log = LOG.lock().unwrap();
            let _ = writeln!(
                text,
                "\nLast local issue: {}",
                log.last_issue.unwrap_or("none")
            );
            if log.write_failed {
                let _ = writeln!(text, "Diagnostics log: write failed");
            }
        }
        text
    }
}

fn power_status(status: crate::vibecode::PersistentStatus) -> &'static str {
    use crate::vibecode::PersistentStatus::*;
    match status {
        Disabled => "disabled",
        Applied => "applied",
        LegacyRecoveryPending => "legacy pending",
        RecoveryRequired => "recovery required",
        Error => "failed",
    }
}

fn freshness(state: ViewState) -> &'static str {
    match state {
        ViewState::Loading => "Loading",
        ViewState::Fresh => "Fresh",
        ViewState::UpdatingWithData => "Updating",
        ViewState::Cached => "Cached",
        ViewState::Outdated => "Outdated",
        ViewState::Cooldown => "Cooldown",
        ViewState::Unavailable => "Unavailable",
        ViewState::Failed => "Failed",
    }
}
fn config_status(status: crate::config::ConfigStatus) -> &'static str {
    use crate::config::ConfigStatus::*;
    match status {
        Ready => "ready",
        MigratedLegacy => "migrated",
        RecoveredFromBackup | RecoveredAndMigrated => "recovered",
        CorruptDefaults => "corrupt",
        FutureSchema(_) => "future schema",
        InvalidSchema => "invalid",
        PathUnavailable => "path unavailable",
        ReadFailed(_) => "read failed",
        WriteFailed(_) => "write failed",
    }
}

fn runtime_status(status: crate::runtime_state::RuntimeStateStatus) -> &'static str {
    use crate::runtime_state::RuntimeStateStatus::*;
    match status {
        Ready | Created => "ready",
        RecoveredFromBackup => "recovered",
        CorruptRecreated => "corrupt recreated",
        FutureSchema(_) => "future schema",
        Invalid => "invalid",
        PathUnavailable => "path unavailable",
        ReadFailed(_) => "read failed",
        WriteFailed(_) => "write failed",
        EntropyUnavailable => "entropy unavailable",
    }
}

struct Timestamp(Option<i64>);

impl std::fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(value) => value.fmt(formatter),
            None => formatter.write_str("none"),
        }
    }
}

fn timestamp(value: Option<i64>) -> Timestamp {
    Timestamp(value)
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
    let result = copy_text(owner, &text());
    if result.is_err() {
        record("clipboard_failed");
    }
    result
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

    #[test]
    fn redaction_corpus_never_reaches_snapshot_or_log() {
        for sensitive in [
            "Bearer sk-secret-123",
            "refresh_token=rt-secret-987",
            "account_987654321",
            "person@example.com",
            "private-user-name",
            "C:\\Users\\private-user-name",
            "/home/private-user",
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            "{\"response_body\":\"private conversation\"}",
            "access_token secret",
        ] {
            assert_eq!(safe_field(sensitive), REDACTED);
            let line = log_line(sensitive);
            assert!(!line.contains(sensitive));
            let mut snapshot = synthetic_snapshot();
            snapshot.channel = sensitive;
            snapshot.providers[0].phase = sensitive;
            snapshot.providers[0].fallback_reason = sensitive;
            snapshot.providers[0].error_code = Some(sensitive);
            let text = snapshot.text();
            assert!(!text.contains(sensitive));
            assert!(text.contains(REDACTED));
        }
        assert_eq!(safe_field("request_timeout"), "request_timeout");
    }

    #[test]
    fn logs_rotate_at_three_bounded_files_without_sleep_or_network() {
        let directory =
            std::env::temp_dir().join(format!("claudometer-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("diagnostics.log");
        for index in 0..5_u8 {
            let line = vec![b'a' + index; LOG_BYTES as usize];
            append_log(&path, &line).unwrap();
        }
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 3);
        assert_eq!(std::fs::read(&path).unwrap()[0], b'e');
        assert_eq!(
            std::fs::read(path.with_extension("log.1")).unwrap()[0],
            b'd'
        );
        assert_eq!(
            std::fs::read(path.with_extension("log.2")).unwrap()[0],
            b'c'
        );
        for entry in std::fs::read_dir(&directory).unwrap() {
            assert!(entry.unwrap().metadata().unwrap().len() <= LOG_BYTES);
        }
        assert!(append_log(&path, &vec![0; LOG_BYTES as usize + 1]).is_err());
        assert!(directory.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn log_failure_returns_without_mutating_unrelated_state() {
        let path = std::env::temp_dir()
            .join("claudometer-log-missing-parent")
            .join("missing")
            .join("diagnostics.log");
        assert!(append_log(&path, b"synthetic\n").is_err());
    }
}
