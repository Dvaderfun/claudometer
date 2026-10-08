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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsV1 {
    pub poll_interval_seconds: u32,
    pub codex_enabled: bool,
    pub alerts_enabled: bool,
    pub update_checks_enabled: bool,
    pub wake_lock_enabled: bool,
    pub persistent_lid_override_enabled: bool,
}

impl Default for SettingsV1 {
    fn default() -> Self {
        Self {
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
            codex_enabled: true,
            alerts_enabled: true,
            update_checks_enabled: false,
            wake_lock_enabled: false,
            persistent_lid_override_enabled: false,
        }
    }
}

impl SettingsV1 {
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
    update_settings(|settings| settings.poll_interval_seconds = seconds)
}

pub fn set_codex_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_settings(|settings| settings.codex_enabled = enabled)
}

pub fn set_alerts_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_settings(|settings| settings.alerts_enabled = enabled)
}

pub fn set_update_checks_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_settings(|settings| settings.update_checks_enabled = enabled)
}

pub fn set_wake_lock_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_settings(|settings| settings.wake_lock_enabled = enabled)
}

pub fn set_persistent_lid_override_enabled(enabled: bool) -> Result<(), ConfigError> {
    update_settings(|settings| settings.persistent_lid_override_enabled = enabled)
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

fn update_settings(update: impl FnOnce(&mut SettingsV1)) -> Result<(), ConfigError> {
    let runtime = RUNTIME.get().ok_or(ConfigError::NotInitialized)?;
    runtime.lock().unwrap().update_settings(update)
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
    encoded.insert(KEY_SCHEMA.to_string(), Value::from(SCHEMA_VERSION));
    encoded.insert(
        KEY_POLL.to_string(),
        Value::from(settings.poll_interval_seconds),
    );
    encoded.insert(KEY_CODEX.to_string(), Value::from(settings.codex_enabled));
    encoded.insert(KEY_ALERTS.to_string(), Value::from(settings.alerts_enabled));
    encoded.insert(
        KEY_UPDATE_CHECKS.to_string(),
        Value::from(settings.update_checks_enabled),
    );
    encoded.insert(
        KEY_WAKE_LOCK.to_string(),
        Value::from(settings.wake_lock_enabled),
    );
    encoded.insert(
        KEY_PERSISTENT_LID_OVERRIDE.to_string(),
        Value::from(settings.persistent_lid_override_enabled),
    );

    // Compatibility window: v0.7.x and two following releases keep reading
    // these exact unversioned keys. Do not contract them implicitly.
    encoded.insert(
        LEGACY_POLL.to_string(),
        Value::from(settings.poll_interval_seconds),
    );
    encoded.insert(
        LEGACY_CODEX.to_string(),
        Value::from(settings.codex_enabled),
    );
    encoded.insert(
        LEGACY_ALERTS.to_string(),
        Value::from(settings.alerts_enabled),
    );
    encoded.insert(
        LEGACY_WAKE_LOCK.to_string(),
        Value::from(settings.wake_lock_enabled),
    );
    encoded
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
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::store::{FailurePoint, StoreErrorKind};

    use super::*;

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
