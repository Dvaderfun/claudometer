use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

use crate::store::{AtomicJsonStore, FaultInjector, LoadOutcome, NoFaults, StoreError};

const SCHEMA_VERSION: u64 = 1;
const DEFAULT_POLL_INTERVAL_SECONDS: u32 = 60;
const MIN_POLL_INTERVAL_SECONDS: u32 = 30;
const MAX_POLL_INTERVAL_SECONDS: u32 = 300;

const KEY_SCHEMA: &str = "schema_version";
const KEY_POLL: &str = "poll_interval_seconds";
const KEY_CODEX: &str = "codex_enabled";
const KEY_CODEX_SERVER: &str = "codex_app_server_enabled";
const KEY_PACE: &str = "pace_colors_enabled";
const KEY_RESET_FORMAT: &str = "reset_format";
const KEY_QUOTA_DISPLAY: &str = "quota_display";
const KEY_ALERTS: &str = "alerts_enabled";
const KEY_UPDATE_CHECKS: &str = "update_checks_enabled";
const KEY_WAKE_LOCK: &str = "wake_lock_enabled";
const KEY_PERSISTENT_LID_OVERRIDE: &str = "persistent_lid_override_enabled";

const LEGACY_POLL: &str = "poll_secs";
const LEGACY_CODEX: &str = "show_codex";
const LEGACY_ALERTS: &str = "alerts";
const LEGACY_WAKE_LOCK: &str = "vibecode";
const LEGACY_LID_RECOVERY: &str = "vibecode_lid";
const LEGACY_ALERT_RECEIPTS: &str = "alerted";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResetFormat {
    #[default]
    Clock,
    Countdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuotaDisplay {
    #[default]
    Used,
    Left,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsV1 {
    pub poll_interval_seconds: u32,
    pub codex_enabled: bool,
    pub codex_app_server_enabled: bool,
    pub pace_colors_enabled: bool,
    pub reset_format: ResetFormat,
    pub quota_display: QuotaDisplay,
    pub alerts_enabled: bool,
    pub update_checks_enabled: bool,
    pub wake_lock_enabled: bool,
    pub persistent_lid_override_enabled: bool,
}

impl ResetFormat {
    pub fn label(self) -> &'static str {
        if self == Self::Clock {
            "Clock"
        } else {
            "Countdown"
        }
    }
}
impl QuotaDisplay {
    pub fn label(self) -> &'static str {
        if self == Self::Used {
            "Used"
        } else {
            "Left"
        }
    }
}

impl Default for SettingsV1 {
    fn default() -> Self {
        Self {
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
            codex_enabled: true,
            codex_app_server_enabled: false,
            pace_colors_enabled: true,
            reset_format: ResetFormat::Clock,
            quota_display: QuotaDisplay::Used,
            alerts_enabled: true,
            update_checks_enabled: false,
            wake_lock_enabled: false,
            persistent_lid_override_enabled: false,
        }
    }
}

impl SettingsV1 {
    pub fn toggle_row_format(&mut self, reset: bool) {
        if reset {
            self.reset_format = if self.reset_format == ResetFormat::Clock {
                ResetFormat::Countdown
            } else {
                ResetFormat::Clock
            };
        } else {
            self.quota_display = if self.quota_display == QuotaDisplay::Used {
                QuotaDisplay::Left
            } else {
                QuotaDisplay::Used
            };
        }
    }
    fn validated(mut self) -> Self {
        self.poll_interval_seconds = self
            .poll_interval_seconds
            .clamp(MIN_POLL_INTERVAL_SECONDS, MAX_POLL_INTERVAL_SECONDS);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessMode {
    Writable,
    FutureSchema(u64),
    InvalidSchema,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigStatus {
    Ready,
    MigratedLegacy,
    RecoveredFromBackup,
    RecoveredAndMigrated,
    CorruptDefaults,
    FutureSchema(u64),
    InvalidSchema,
    PathUnavailable,
    ReadFailed(StoreError),
    WriteFailed(StoreError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    NotInitialized,
    PathUnavailable,
    FutureSchema(u64),
    InvalidSchema,
    Store(StoreError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => formatter.write_str("settings are not initialized"),
            Self::PathUnavailable => formatter.write_str("settings path is unavailable"),
            Self::FutureSchema(version) => {
                write!(
                    formatter,
                    "settings schema {version} is newer and read-only"
                )
            }
            Self::InvalidSchema => formatter.write_str("settings schema is invalid and read-only"),
            Self::Store(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ConfigError {}

struct ConfigState {
    settings: SettingsV1,
    raw: Map<String, Value>,
    access: AccessMode,
    status: ConfigStatus,
    pending_migration_status: Option<ConfigStatus>,
}

enum Backend<F> {
    Store(AtomicJsonStore<F>),
    Unavailable,
}

struct Runtime<F> {
    backend: Backend<F>,
    state: ConfigState,
}

static RUNTIME: OnceLock<Mutex<Runtime<NoFaults>>> = OnceLock::new();

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("Claudometer"))
}

fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("settings.json"))
}

pub fn initialize() -> ConfigStatus {
    initialize_with_migrations(true)
}

pub fn initialize_read_only() -> ConfigStatus {
    let runtime = RUNTIME.get_or_init(|| {
        let (raw, status, existing) = match config_path().map(std::fs::read) {
            Some(Ok(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(raw) => (raw, ConfigStatus::Ready, true),
                Err(_) => (Map::new(), ConfigStatus::CorruptDefaults, true),
            },
            Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                (Map::new(), ConfigStatus::Ready, false)
            }
            _ => (Map::new(), ConfigStatus::PathUnavailable, false),
        };
        let decoded = decode(raw, existing);
        Mutex::new(Runtime {
            backend: Backend::Unavailable,
            state: ConfigState {
                settings: decoded.settings,
                raw: decoded.raw,
                access: decoded.access,
                status: status_for_access(decoded.access, status),
                pending_migration_status: None,
            },
        })
    });
    runtime.lock().unwrap().state.status
}

/// Loads settings without persisting schema normalization. Update candidates
/// use this until the watchdog has durably committed their executable.
pub fn initialize_compatibility() -> ConfigStatus {
    initialize_with_migrations(false)
}

fn initialize_with_migrations(allow_migrations: bool) -> ConfigStatus {
    let runtime = RUNTIME.get_or_init(|| {
        let backend = config_path()
            .map(AtomicJsonStore::new)
            .map(Backend::Store)
            .unwrap_or(Backend::Unavailable);
        Mutex::new(Runtime::load_with_migrations(backend, allow_migrations))
    });
    runtime.lock().unwrap().state.status
}

pub fn commit_pending_migration() -> ConfigStatus {
    let Some(runtime) = RUNTIME.get() else {
        return ConfigStatus::PathUnavailable;
    };
    runtime.lock().unwrap().commit_pending_migration()
}

pub fn settings() -> SettingsV1 {
    RUNTIME
        .get()
        .map(|runtime| runtime.lock().unwrap().state.settings.clone())
        .unwrap_or_default()
}

pub fn status() -> ConfigStatus {
    RUNTIME
        .get()
        .map(|runtime| runtime.lock().unwrap().state.status)
        .unwrap_or(ConfigStatus::PathUnavailable)
}

pub fn diagnostic() -> Option<String> {
    match status() {
        ConfigStatus::Ready | ConfigStatus::MigratedLegacy => None,
        ConfigStatus::RecoveredFromBackup | ConfigStatus::RecoveredAndMigrated => {
            Some("Settings recovered from verified backup".to_string())
        }
        ConfigStatus::CorruptDefaults => {
            Some("Invalid settings preserved; using defaults".to_string())
        }
        ConfigStatus::FutureSchema(version) => {
            Some(format!("Settings schema {version} is newer · read-only"))
        }
        ConfigStatus::InvalidSchema => Some("Settings schema invalid · read-only".to_string()),
        ConfigStatus::PathUnavailable => Some("Settings path unavailable · read-only".to_string()),
        ConfigStatus::ReadFailed(_) => Some("Settings read failed · Copy diagnostics".to_string()),
        ConfigStatus::WriteFailed(_) => {
            Some("Settings write failed · Copy diagnostics".to_string())
        }
    }
}

pub fn set_poll_interval_seconds(seconds: u32) -> Result<(), ConfigError> {
    let runtime = RUNTIME.get().ok_or(ConfigError::NotInitialized)?;
    runtime
        .lock()
        .unwrap()
        .update_settings(|settings| settings.poll_interval_seconds = seconds)
}

pub fn set_codex_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.codex_enabled, enabled)
}

pub fn set_codex_app_server_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.codex_app_server_enabled, enabled)
}

pub fn set_alerts_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.alerts_enabled, enabled)
}

pub fn set_pace_colors_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.pace_colors_enabled, enabled)
}

pub fn toggle_row_format(reset: bool) -> Result<(), ConfigError> {
    let runtime = RUNTIME.get().ok_or(ConfigError::NotInitialized)?;
    let mut runtime = runtime.lock().unwrap();
    if crate::demo::is_active() {
        runtime.state.settings.toggle_row_format(reset);
        return Ok(());
    }
    runtime.update_settings(|settings| settings.toggle_row_format(reset))
}

pub fn initialize_demo() {
    RUNTIME.get_or_init(|| Mutex::new(Runtime::load_with_migrations(Backend::Unavailable, false)));
}

pub fn set_update_checks_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.update_checks_enabled, enabled)
}

pub fn set_wake_lock_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(|settings| &mut settings.wake_lock_enabled, enabled)
}

pub fn set_persistent_lid_override_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_bool_setting(
        |settings| &mut settings.persistent_lid_override_enabled,
        enabled,
    )
}

pub fn legacy_lid_recovery() -> Option<(u32, u32)> {
    let runtime = RUNTIME.get()?;
    legacy_lid_from_raw(&runtime.lock().unwrap().state.raw)
}

pub fn clear_legacy_lid_recovery() -> Result<(), ConfigError> {
    update_raw(|raw| {
        raw.remove(LEGACY_LID_RECOVERY);
    })
}

pub fn legacy_alert_receipts() -> HashMap<String, i64> {
    RUNTIME
        .get()
        .map(|runtime| legacy_alerts_from_raw(&runtime.lock().unwrap().state.raw))
        .unwrap_or_default()
}

pub fn save_legacy_alert_receipts(receipts: &HashMap<String, i64>) -> Result<(), ConfigError> {
    let value = serde_json::to_value(receipts).expect("string/i64 map is serializable");
    update_raw(|raw| {
        raw.insert(LEGACY_ALERT_RECEIPTS.to_string(), value);
    })
}

#[inline(never)]
fn update_bool_setting(
    field: fn(&mut SettingsV1) -> &mut bool,
    enabled: bool,
) -> Result<(), ConfigError> {
    let runtime = RUNTIME.get().ok_or(ConfigError::NotInitialized)?;
    runtime
        .lock()
        .unwrap()
        .update_settings(|settings| *field(settings) = enabled)
}

fn update_raw(update: impl FnOnce(&mut Map<String, Value>)) -> Result<(), ConfigError> {
    let runtime = RUNTIME.get().ok_or(ConfigError::NotInitialized)?;
    runtime.lock().unwrap().update_raw(update)
}

impl<F: FaultInjector> Runtime<F> {
    fn load_with_migrations(backend: Backend<F>, allow_migrations: bool) -> Self {
        let Backend::Store(store) = &backend else {
            return Self {
                backend,
                state: ConfigState {
                    settings: SettingsV1::default(),
                    raw: Map::new(),
                    access: AccessMode::Writable,
                    status: ConfigStatus::PathUnavailable,
                    pending_migration_status: None,
                },
            };
        };

        let outcome = store.load::<Map<String, Value>>();
        let (raw, source_status, had_document, existing_install) = match outcome {
            Ok(LoadOutcome::Loaded(raw)) => (raw, ConfigStatus::Ready, true, true),
            Ok(LoadOutcome::RecoveredFromBackup(raw)) => {
                (raw, ConfigStatus::RecoveredFromBackup, true, true)
            }
            Ok(LoadOutcome::Missing) => (Map::new(), ConfigStatus::Ready, false, false),
            Ok(LoadOutcome::CorruptPreserved) => {
                (Map::new(), ConfigStatus::CorruptDefaults, false, true)
            }
            Err(error) => (Map::new(), ConfigStatus::ReadFailed(error), false, true),
        };

        let decoded = decode(raw, existing_install);
        let mut runtime = Self {
            backend,
            state: ConfigState {
                settings: decoded.settings,
                raw: decoded.raw,
                access: decoded.access,
                status: status_for_access(decoded.access, source_status),
                pending_migration_status: None,
            },
        };

        if had_document && decoded.needs_migration && decoded.access == AccessMode::Writable {
            let recovered = source_status == ConfigStatus::RecoveredFromBackup;
            let migration_status = if recovered {
                ConfigStatus::RecoveredAndMigrated
            } else {
                ConfigStatus::MigratedLegacy
            };
            if allow_migrations {
                match runtime.persist_current() {
                    Ok(()) => runtime.state.status = migration_status,
                    Err(ConfigError::Store(error)) => {
                        runtime.state.status = ConfigStatus::WriteFailed(error);
                    }
                    Err(_) => {}
                }
            } else {
                runtime.state.pending_migration_status = Some(migration_status);
            }
        }

        runtime
    }

    fn commit_pending_migration(&mut self) -> ConfigStatus {
        let Some(success_status) = self.state.pending_migration_status else {
            return self.state.status;
        };
        match self.persist_current() {
            Ok(()) => {
                self.state.status = success_status;
                self.state.pending_migration_status = None;
            }
            Err(ConfigError::Store(error)) => {
                self.state.status = ConfigStatus::WriteFailed(error);
            }
            Err(_) => {}
        }
        self.state.status
    }

    fn update_settings(&mut self, update: impl FnOnce(&mut SettingsV1)) -> Result<(), ConfigError> {
        self.ensure_writable()?;
        let mut next_settings = self.state.settings.clone();
        update(&mut next_settings);
        self.save_settings(next_settings)
    }

    #[inline(never)]
    fn save_settings(&mut self, mut next_settings: SettingsV1) -> Result<(), ConfigError> {
        next_settings = next_settings.validated();
        let next_raw = encode(&self.state.raw, &next_settings);
        self.persist(&next_raw)?;
        self.state.settings = next_settings;
        self.state.raw = next_raw;
        self.state.status = ConfigStatus::Ready;
        Ok(())
    }

    fn update_raw(
        &mut self,
        update: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<(), ConfigError> {
        self.ensure_writable()?;
        let mut next_raw = self.state.raw.clone();
        update(&mut next_raw);
        next_raw = encode(&next_raw, &self.state.settings);
        self.persist(&next_raw)?;
        self.state.raw = next_raw;
        self.state.status = ConfigStatus::Ready;
        Ok(())
    }

    fn ensure_writable(&self) -> Result<(), ConfigError> {
        match self.state.access {
            AccessMode::Writable => match self.backend {
                Backend::Store(_) => Ok(()),
                Backend::Unavailable => Err(ConfigError::PathUnavailable),
            },
            AccessMode::FutureSchema(version) => Err(ConfigError::FutureSchema(version)),
            AccessMode::InvalidSchema => Err(ConfigError::InvalidSchema),
        }
    }

    fn persist_current(&mut self) -> Result<(), ConfigError> {
        let next_raw = encode(&self.state.raw, &self.state.settings);
        self.persist(&next_raw)?;
        self.state.raw = next_raw;
        Ok(())
    }

    fn persist(&mut self, raw: &Map<String, Value>) -> Result<(), ConfigError> {
        let Backend::Store(store) = &self.backend else {
            return Err(ConfigError::PathUnavailable);
        };
        if let Err(error) = store.save(raw) {
            crate::diagnostics::record("config_failed");
            self.state.status = ConfigStatus::WriteFailed(error);
            return Err(ConfigError::Store(error));
        }
        Ok(())
    }
}

struct Decoded {
    settings: SettingsV1,
    raw: Map<String, Value>,
    access: AccessMode,
    needs_migration: bool,
}

fn decode(raw: Map<String, Value>, existing_install: bool) -> Decoded {
    let schema = raw.get(KEY_SCHEMA);
    let (access, legacy_schema) = match schema {
        None => (AccessMode::Writable, true),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(0 | SCHEMA_VERSION) => (AccessMode::Writable, number.as_u64() == Some(0)),
            Some(version) if version > SCHEMA_VERSION => (AccessMode::FutureSchema(version), false),
            _ => (AccessMode::InvalidSchema, false),
        },
        Some(_) => (AccessMode::InvalidSchema, false),
    };

    let settings = SettingsV1 {
        poll_interval_seconds: u32_value(raw.get(KEY_POLL))
            .or_else(|| u32_value(raw.get(LEGACY_POLL)))
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
        codex_enabled: bool_value(raw.get(KEY_CODEX))
            .or_else(|| bool_value(raw.get(LEGACY_CODEX)))
            .unwrap_or(true),
        codex_app_server_enabled: bool_value(raw.get(KEY_CODEX_SERVER)).unwrap_or(false),
        pace_colors_enabled: bool_value(raw.get(KEY_PACE)).unwrap_or(true),
        reset_format: if raw.get(KEY_RESET_FORMAT).and_then(Value::as_str) == Some("countdown") {
            ResetFormat::Countdown
        } else {
            ResetFormat::Clock
        },
        quota_display: if raw.get(KEY_QUOTA_DISPLAY).and_then(Value::as_str) == Some("left") {
            QuotaDisplay::Left
        } else {
            QuotaDisplay::Used
        },
        alerts_enabled: bool_value(raw.get(KEY_ALERTS))
            .or_else(|| bool_value(raw.get(LEGACY_ALERTS)))
            .unwrap_or(true),
        // A pre-control document represents an installation that already
        // received automatic checks. Even a corrupt/unreadable document proves
        // this is not a genuinely new install; only absence opts out.
        update_checks_enabled: bool_value(raw.get(KEY_UPDATE_CHECKS)).unwrap_or(existing_install),
        wake_lock_enabled: bool_value(raw.get(KEY_WAKE_LOCK))
            .or_else(|| bool_value(raw.get(LEGACY_WAKE_LOCK)))
            .unwrap_or(false),
        persistent_lid_override_enabled: bool_value(raw.get(KEY_PERSISTENT_LID_OVERRIDE))
            .unwrap_or(false),
    }
    .validated();

    let normalized = encode(&raw, &settings);
    let needs_migration = legacy_schema || (access == AccessMode::Writable && normalized != raw);
    Decoded {
        settings,
        raw,
        access,
        needs_migration,
    }
}

fn encode(raw: &Map<String, Value>, settings: &SettingsV1) -> Map<String, Value> {
    let mut encoded = raw.clone();
    encode_field(&mut encoded, KEY_SCHEMA, Value::from(SCHEMA_VERSION));
    for (key, value) in [
        (
            KEY_RESET_FORMAT,
            if settings.reset_format == ResetFormat::Clock {
                "clock"
            } else {
                "countdown"
            },
        ),
        (
            KEY_QUOTA_DISPLAY,
            if settings.quota_display == QuotaDisplay::Used {
                "used"
            } else {
                "left"
            },
        ),
    ] {
        encode_field(&mut encoded, key, Value::from(value));
    }
    encode_field(
        &mut encoded,
        KEY_POLL,
        Value::from(settings.poll_interval_seconds),
    );
    for (key, enabled) in [
        (KEY_CODEX, settings.codex_enabled),
        (KEY_CODEX_SERVER, settings.codex_app_server_enabled),
        (KEY_ALERTS, settings.alerts_enabled),
        (KEY_PACE, settings.pace_colors_enabled),
        (KEY_UPDATE_CHECKS, settings.update_checks_enabled),
        (KEY_WAKE_LOCK, settings.wake_lock_enabled),
        (
            KEY_PERSISTENT_LID_OVERRIDE,
            settings.persistent_lid_override_enabled,
        ),
        (LEGACY_CODEX, settings.codex_enabled),
        (LEGACY_ALERTS, settings.alerts_enabled),
        (LEGACY_WAKE_LOCK, settings.wake_lock_enabled),
    ] {
        encode_field(&mut encoded, key, Value::from(enabled));
    }

    // Compatibility window: v0.7.x and two following releases keep reading
    // these exact unversioned keys. Do not contract them implicitly.
    encode_field(
        &mut encoded,
        LEGACY_POLL,
        Value::from(settings.poll_interval_seconds),
    );
    encoded
}

fn encode_field(encoded: &mut Map<String, Value>, key: &str, value: Value) {
    encoded.insert(key.to_string(), value);
}

fn status_for_access(access: AccessMode, source: ConfigStatus) -> ConfigStatus {
    match access {
        AccessMode::Writable => source,
        AccessMode::FutureSchema(version) => ConfigStatus::FutureSchema(version),
        AccessMode::InvalidSchema => ConfigStatus::InvalidSchema,
    }
}

fn u32_value(value: Option<&Value>) -> Option<u32> {
    value?.as_u64()?.try_into().ok()
}

fn bool_value(value: Option<&Value>) -> Option<bool> {
    value?.as_bool()
}

fn legacy_lid_from_raw(raw: &Map<String, Value>) -> Option<(u32, u32)> {
    let object = raw.get(LEGACY_LID_RECOVERY)?.as_object()?;
    Some((u32_value(object.get("ac"))?, u32_value(object.get("dc"))?))
}

fn legacy_alerts_from_raw(raw: &Map<String, Value>) -> HashMap<String, i64> {
    raw.get(LEGACY_ALERT_RECEIPTS)
        .and_then(Value::as_object)
        .map(|receipts| {
            receipts
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_row_choice_write_preserves_disk_and_memory() {
        for reset in [true, false] {
            let directory = TestDirectory::new();
            let path = directory.settings_path();
            AtomicJsonStore::new(&path)
                .save(&encode(&Map::new(), &SettingsV1::default()))
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let faults = OneFault(Mutex::new(Some((
                FailurePoint::WriteTemporary,
                io::ErrorKind::StorageFull,
            ))));
            let mut runtime = Runtime::load_with_migrations(
                Backend::Store(AtomicJsonStore::with_fault_injector(&path, faults)),
                true,
            );
            let settings = runtime.state.settings.clone();
            assert!(runtime
                .update_settings(|settings| settings.toggle_row_format(reset))
                .is_err());
            assert_eq!(runtime.state.settings, settings);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn row_choices_default_validate_persist_and_survive_older_writes() {
        let defaults = decode(Map::new(), false).settings;
        assert_eq!(defaults.reset_format, ResetFormat::Clock);
        assert_eq!(defaults.quota_display, QuotaDisplay::Used);
        for invalid in [Value::Null, Value::Bool(true), Value::from("unknown")] {
            let mut raw = Map::new();
            raw.insert(KEY_RESET_FORMAT.into(), invalid.clone());
            raw.insert(KEY_QUOTA_DISPLAY.into(), invalid);
            let settings = decode(raw, true).settings;
            assert_eq!(settings.reset_format, ResetFormat::Clock);
            assert_eq!(settings.quota_display, QuotaDisplay::Used);
        }
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let mut runtime = load_runtime(&path);
        runtime
            .update_settings(|settings| settings.toggle_row_format(true))
            .unwrap();
        runtime
            .update_settings(|settings| settings.toggle_row_format(false))
            .unwrap();
        let mut restarted = load_runtime(&path);
        assert_eq!(
            restarted.state.settings.reset_format,
            ResetFormat::Countdown
        );
        assert_eq!(restarted.state.settings.quota_display, QuotaDisplay::Left);
        restarted
            .update_settings(|settings| settings.alerts_enabled = false)
            .unwrap();
        let mut old: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        old.insert(LEGACY_POLL.into(), Value::from(120));
        old.insert("unknown".into(), Value::from("preserved"));
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut restarted = load_runtime(&path);
        assert_eq!(
            restarted.state.settings.reset_format,
            ResetFormat::Countdown
        );
        assert_eq!(restarted.state.settings.quota_display, QuotaDisplay::Left);
        restarted
            .update_settings(|settings| {
                settings.toggle_row_format(true);
                settings.toggle_row_format(false);
            })
            .unwrap();
        assert_eq!(
            load_runtime(&path).state.settings.reset_format,
            ResetFormat::Clock
        );
        assert_eq!(
            load_runtime(&path).state.settings.quota_display,
            QuotaDisplay::Used
        );
        assert_eq!(
            load_runtime(&path).state.raw.get("unknown"),
            Some(&Value::from("preserved"))
        );
    }
    #[test]
    fn pace_defaults_on_and_disabled_preference_survives_restart_and_downgrade() {
        use super::*;
        assert!(decode(Map::new(), false).settings.pace_colors_enabled);
        let settings = SettingsV1 {
            pace_colors_enabled: false,
            ..SettingsV1::default()
        };
        let raw = encode(&Map::new(), &settings);
        assert!(!decode(raw.clone(), true).settings.pace_colors_enabled);
        let mut older_reader = raw;
        older_reader.insert(LEGACY_POLL.into(), Value::from(120));
        assert!(!decode(older_reader, true).settings.pace_colors_enabled);
        let mut malformed = Map::new();
        malformed.insert(KEY_PACE.into(), Value::from("false"));
        assert!(decode(malformed, true).settings.pace_colors_enabled);
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let mut runtime = load_runtime(&path);
        runtime
            .update_settings(|settings| settings.pace_colors_enabled = false)
            .unwrap();
        assert!(!load_runtime(&path).state.settings.pace_colors_enabled);
    }
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::store::{FailurePoint, StoreErrorKind};

    use super::*;
    #[test]
    fn app_server_preference_is_additive_and_preserved_for_downgrade() {
        let legacy = decode(Map::new(), false);
        assert!(!legacy.settings.codex_app_server_enabled);
        let mut settings = legacy.settings;
        settings.codex_app_server_enabled = true;
        let raw = encode(&Map::new(), &settings);
        assert!(decode(raw.clone(), true).settings.codex_app_server_enabled);
        let mut old = raw;
        old.insert("poll_secs".into(), Value::from(120));
        assert!(decode(old, true).settings.codex_app_server_enabled);
    }

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-config-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn settings_path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let temp = std::env::temp_dir();
            assert!(self.0.starts_with(&temp));
            assert!(self.0.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("claudometer-config-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct OneFault(Mutex<Option<(FailurePoint, io::ErrorKind)>>);

    impl FaultInjector for OneFault {
        fn check(&self, point: FailurePoint) -> io::Result<()> {
            let mut fault = self.0.lock().unwrap();
            if fault
                .as_ref()
                .is_some_and(|(expected, _)| *expected == point)
            {
                let (_, kind) = fault.take().unwrap();
                return Err(io::Error::from(kind));
            }
            Ok(())
        }
    }

    fn load_runtime(path: &Path) -> Runtime<NoFaults> {
        Runtime::load_with_migrations(Backend::Store(AtomicJsonStore::new(path)), true)
    }

    #[test]
    fn migrates_legacy_keys_and_proves_downgrade_compatibility() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let legacy = serde_json::json!({
            "poll_secs": 120,
            "show_codex": false,
            "alerts": false,
            "vibecode": true,
            "vibecode_lid": { "ac": 1, "dc": 2 },
            "alerted": { "Claude.session": 1234 },
            "future_unknown": { "preserve": true }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let runtime = load_runtime(&path);
        assert_eq!(runtime.state.status, ConfigStatus::MigratedLegacy);
        assert_eq!(
            runtime.state.settings,
            SettingsV1 {
                poll_interval_seconds: 120,
                codex_enabled: false,
                codex_app_server_enabled: false,
                pace_colors_enabled: true,
                reset_format: ResetFormat::Clock,
                quota_display: QuotaDisplay::Used,
                alerts_enabled: false,
                update_checks_enabled: true,
                wake_lock_enabled: true,
                persistent_lid_override_enabled: false,
            }
        );
        assert_eq!(legacy_lid_from_raw(&runtime.state.raw), Some((1, 2)));
        assert_eq!(
            legacy_alerts_from_raw(&runtime.state.raw).get("Claude.session"),
            Some(&1234)
        );

        let saved: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.get(KEY_SCHEMA), Some(&Value::from(1)));
        assert_eq!(saved.get(KEY_POLL), Some(&Value::from(120)));
        assert_eq!(saved.get(LEGACY_POLL), Some(&Value::from(120)));
        assert_eq!(saved.get(KEY_CODEX), Some(&Value::from(false)));
        assert_eq!(saved.get(LEGACY_CODEX), Some(&Value::from(false)));
        assert_eq!(saved.get(KEY_UPDATE_CHECKS), Some(&Value::from(true)));
        assert_eq!(saved.get("future_unknown"), legacy.get("future_unknown"));
        assert_eq!(
            saved.get(LEGACY_LID_RECOVERY),
            legacy.get(LEGACY_LID_RECOVERY)
        );
        assert_eq!(
            saved.get(LEGACY_ALERT_RECEIPTS),
            legacy.get(LEGACY_ALERT_RECEIPTS)
        );
    }

    #[test]
    fn compatibility_startup_defers_settings_migration_until_commit() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let legacy = serde_json::json!({
            "poll_secs": 120,
            "show_codex": false,
            "future_unknown": true
        });
        let original = serde_json::to_vec_pretty(&legacy).unwrap();
        std::fs::write(&path, &original).unwrap();

        let mut runtime =
            Runtime::load_with_migrations(Backend::Store(AtomicJsonStore::new(&path)), false);
        assert_eq!(runtime.state.settings.poll_interval_seconds, 120);
        assert_eq!(runtime.state.status, ConfigStatus::Ready);
        assert_eq!(std::fs::read(&path).unwrap(), original);

        assert_eq!(
            runtime.commit_pending_migration(),
            ConfigStatus::MigratedLegacy
        );
        let saved: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.get(KEY_SCHEMA), Some(&Value::from(1)));
        assert_eq!(saved.get("future_unknown"), Some(&Value::Bool(true)));
    }

    #[test]
    fn genuinely_new_install_defaults_update_checks_off_without_writing() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();

        let runtime = load_runtime(&path);

        assert_eq!(runtime.state.status, ConfigStatus::Ready);
        assert!(!runtime.state.settings.update_checks_enabled);
        assert!(!path.exists());
    }

    #[test]
    fn existing_v1_install_migrates_with_update_checks_enabled() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let existing = serde_json::json!({
            "schema_version": 1,
            "poll_interval_seconds": 60,
            "codex_enabled": true,
            "alerts_enabled": true,
            "wake_lock_enabled": false,
            "persistent_lid_override_enabled": false,
            "poll_secs": 60,
            "show_codex": true,
            "alerts": true,
            "vibecode": false
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&existing).unwrap()).unwrap();

        let runtime = load_runtime(&path);

        assert_eq!(runtime.state.status, ConfigStatus::MigratedLegacy);
        assert!(runtime.state.settings.update_checks_enabled);
        let saved: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.get(KEY_UPDATE_CHECKS), Some(&Value::from(true)));
    }

    #[test]
    fn validates_values_and_preserves_unknown_fields_on_write() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let raw = serde_json::json!({
            "schema_version": 1,
            "poll_interval_seconds": 999,
            "codex_enabled": true,
            "alerts_enabled": true,
            "wake_lock_enabled": false,
            "unknown": [1, 2, 3]
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        let mut runtime = load_runtime(&path);
        assert_eq!(runtime.state.settings.poll_interval_seconds, 300);
        runtime
            .update_settings(|settings| settings.alerts_enabled = false)
            .unwrap();
        let saved: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.get("unknown"), raw.get("unknown"));
        assert_eq!(saved.get(KEY_POLL), Some(&Value::from(300)));
        assert_eq!(saved.get(LEGACY_POLL), Some(&Value::from(300)));
    }

    #[test]
    fn future_schema_is_read_only_and_byte_for_byte_unchanged() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        let bytes = br#"{
  "schema_version": 9,
  "poll_interval_seconds": 90,
  "future_shape": {"do_not_touch": true}
}
"#;
        std::fs::write(&path, bytes).unwrap();

        let mut runtime = load_runtime(&path);
        assert_eq!(runtime.state.status, ConfigStatus::FutureSchema(9));
        assert_eq!(runtime.state.settings.poll_interval_seconds, 90);
        assert_eq!(
            runtime
                .update_settings(|settings| settings.poll_interval_seconds = 30)
                .unwrap_err(),
            ConfigError::FutureSchema(9)
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn failed_write_does_not_modify_validated_in_memory_settings() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        AtomicJsonStore::new(&path)
            .save(&encode(&Map::new(), &SettingsV1::default()))
            .unwrap();
        let faults = OneFault(Mutex::new(Some((
            FailurePoint::WriteTemporary,
            io::ErrorKind::StorageFull,
        ))));
        let mut runtime = Runtime::load_with_migrations(
            Backend::Store(AtomicJsonStore::with_fault_injector(&path, faults)),
            true,
        );
        let before = runtime.state.settings.clone();

        let error = runtime
            .update_settings(|settings| settings.codex_enabled = false)
            .unwrap_err();
        assert_eq!(
            error,
            ConfigError::Store(StoreError {
                stage: crate::store::StoreStage::WriteTemporary,
                kind: StoreErrorKind::Io(io::ErrorKind::StorageFull),
            })
        );
        assert_eq!(runtime.state.settings, before);
        assert_eq!(load_runtime(&path).state.settings, before);
    }

    #[test]
    fn malformed_file_is_preserved_before_defaults_are_used() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        std::fs::write(&path, b"{malformed").unwrap();

        let runtime = load_runtime(&path);
        assert_eq!(runtime.state.status, ConfigStatus::CorruptDefaults);
        assert_eq!(
            runtime.state.settings,
            SettingsV1 {
                update_checks_enabled: true,
                ..SettingsV1::default()
            }
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_dir(&directory.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt."))
                .count(),
            1
        );
    }
}
