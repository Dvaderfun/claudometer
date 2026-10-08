use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

use crate::provider::model::{
    encode_hex, AccountContext, AccountKey, IdentityPersistence, LimitClass, LimitId, LimitKind,
    ProviderId, SourceId, SourceProvenance, SourceSupport, UsageSnapshot, ACCOUNT_KEY_BYTES,
};
use crate::store::{AtomicJsonStore, FaultInjector, LoadOutcome, StoreError};

const SCHEMA_VERSION: u64 = 1;
const MAX_ALERT_RECEIPTS: usize = 4096;
const CACHE_VERSION: u64 = 1;
const MAX_CACHE_AGE_SECONDS: i64 = 8 * 24 * 60 * 60;
const MAX_TIMESTAMP: i64 = 253_402_300_799;

#[derive(Clone, Deserialize, Serialize)]
pub struct CachedProvider {
    pub provider: ProviderId,
    pub account: AccountKey,
    pub source: SourceProvenance,
    pub snapshot: Option<UsageSnapshot>,
    pub retry_at_unix: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct ProviderCache {
    version: u64,
    entries: Vec<CachedProvider>,
}

fn valid_timestamp(value: i64) -> bool {
    (0..=MAX_TIMESTAMP).contains(&value)
}

fn valid_cache(entry: &CachedProvider) -> bool {
    let source_valid = match entry.source.id {
        SourceId::ClaudeOAuthCompatibility => {
            entry.provider == ProviderId::Claude
                && entry.source.support == SourceSupport::Compatibility
        }
        SourceId::CodexWhamCompatibility => {
            entry.provider == ProviderId::Codex
                && entry.source.support == SourceSupport::Compatibility
        }
        SourceId::CodexAppServer => {
            entry.provider == ProviderId::Codex && entry.source.support == SourceSupport::Documented
        }
    };
    source_valid
        && entry.retry_at_unix.is_none_or(valid_timestamp)
        && entry.snapshot.as_ref().is_none_or(|snapshot| {
            snapshot.provider == entry.provider
                && snapshot.account == entry.account
                && snapshot.source == entry.source
                && valid_timestamp(snapshot.fetched_unix)
                && snapshot.plan.as_ref().is_none_or(|plan| plan.len() <= 512)
                && snapshot.rows.len() <= 64
                && snapshot.rows.iter().enumerate().all(|(index, row)| {
                    row.label.len() <= 512
                        && !snapshot.rows[..index]
                            .iter()
                            .any(|previous| previous.id == row.id)
                        && !matches!(&row.kind, LimitKind::Other(kind) if kind.len() > 512)
                        && (row.class == LimitClass::Spend) == (row.kind == LimitKind::ExtraUsage)
                        && row.resets_unix.is_none_or(valid_timestamp)
                        && row
                            .window_seconds
                            .is_none_or(|seconds| (1..=366 * 24 * 60 * 60).contains(&seconds))
                })
        })
}

fn decode_cache(raw: &Map<String, Value>) -> Vec<CachedProvider> {
    let Some(value) = raw.get("provider_cache") else {
        return Vec::new();
    };
    // Bound provider and row arrays before typed cache decoding.
    if value.get("version").and_then(Value::as_u64) != Some(CACHE_VERSION)
        || value
            .get("entries")
            .and_then(Value::as_array)
            .is_none_or(|entries| entries.len() > 2)
    {
        return Vec::new();
    }
    if value["entries"].as_array().unwrap().iter().any(|entry| {
        entry
            .get("snapshot")
            .filter(|snapshot| !snapshot.is_null())
            .is_some_and(|snapshot| {
                snapshot
                    .get("rows")
                    .and_then(Value::as_array)
                    .is_none_or(|rows| rows.len() > 64)
            })
    }) {
        return Vec::new();
    }
    let Ok(cache) = serde_json::from_value::<ProviderCache>(value.clone()) else {
        return Vec::new();
    };
    if cache.entries.iter().enumerate().any(|(index, entry)| {
        !valid_cache(entry)
            || cache.entries[..index]
                .iter()
                .any(|previous| previous.provider == entry.provider)
    }) {
        return Vec::new();
    }
    cache.entries
}

fn matching_cache(
    raw: &Map<String, Value>,
    provider: ProviderId,
    account: &AccountContext,
    source: SourceProvenance,
    now: i64,
) -> Option<CachedProvider> {
    if account.persistence != IdentityPersistence::Persistent || !valid_timestamp(now) {
        return None;
    }
    let mut entry = decode_cache(raw).into_iter().find(|entry| {
        entry.provider == provider && entry.account == account.key && entry.source == source
    })?;
    entry.snapshot = entry
        .snapshot
        .filter(|snapshot| {
            snapshot.fetched_unix <= now && now - snapshot.fetched_unix <= MAX_CACHE_AGE_SECONDS
        })
        .map(|mut snapshot| {
            snapshot
                .rows
                .retain(|row| row.resets_unix.is_none_or(|reset| reset > now));
            snapshot
        });
    // Provider backoff is capped at 900 seconds. Implausible future deadlines
    // (including a large wall-clock rollback) must not freeze polling.
    entry.retry_at_unix = entry
        .retry_at_unix
        .filter(|deadline| *deadline > now && *deadline - now <= 900);
    (entry.snapshot.is_some() || entry.retry_at_unix.is_some()).then_some(entry)
}

pub fn cached_provider(
    provider: ProviderId,
    account: &AccountContext,
    source: SourceProvenance,
    now: i64,
) -> Option<CachedProvider> {
    let runtime = RUNTIME.get()?.lock().unwrap();
    runtime.install_salt.as_ref()?;
    matching_cache(&runtime.raw, provider, account, source, now)
}

fn cache_update(
    raw: &Map<String, Value>,
    provider: ProviderId,
    entry: Option<CachedProvider>,
) -> Result<Map<String, Value>, RuntimeStateStatus> {
    if raw
        .get("provider_cache")
        .and_then(|cache| cache.get("version"))
        .and_then(Value::as_u64)
        .is_some_and(|version| version > CACHE_VERSION)
    {
        return Err(RuntimeStateStatus::Invalid);
    }
    if entry
        .as_ref()
        .is_some_and(|entry| entry.provider != provider || !valid_cache(entry))
    {
        return Err(RuntimeStateStatus::Invalid);
    }
    let mut entries = decode_cache(raw);
    entries.retain(|entry| entry.provider != provider);
    entries.extend(entry);
    let mut encoded = raw.clone();
    encoded.insert(
        "provider_cache".to_string(),
        serde_json::to_value(ProviderCache {
            version: CACHE_VERSION,
            entries,
        })
        .map_err(|_| RuntimeStateStatus::Invalid)?,
    );
    Ok(encoded)
}

fn save_cache<F: FaultInjector>(
    runtime: &mut RuntimeState,
    store: &AtomicJsonStore<F>,
    provider: ProviderId,
    entry: Option<CachedProvider>,
) -> Result<(), RuntimeStateStatus> {
    runtime.install_salt.as_ref().ok_or(runtime.status)?;
    let encoded = cache_update(&runtime.raw, provider, entry)
        .inspect_err(|status| runtime.status = *status)?;
    if encoded == runtime.raw {
        return Ok(());
    }
    if let Err(error) = store.save(&encoded) {
        crate::diagnostics::record("runtime_failed");
        runtime.status = RuntimeStateStatus::WriteFailed(error);
        return Err(runtime.status);
    }
    runtime.raw = encoded;
    runtime.status = RuntimeStateStatus::Ready;
    Ok(())
}

pub fn persist_provider_cache(
    provider: ProviderId,
    account: &AccountContext,
    source: SourceProvenance,
    snapshot: Option<UsageSnapshot>,
    retry_at_unix: Option<i64>,
) -> Result<(), RuntimeStateStatus> {
    let entry = (account.persistence == IdentityPersistence::Persistent).then(|| CachedProvider {
        provider,
        account: account.key.clone(),
        source,
        snapshot,
        retry_at_unix,
    });
    let mut runtime = RUNTIME
        .get()
        .ok_or(RuntimeStateStatus::PathUnavailable)?
        .lock()
        .unwrap();
    let path = state_path().ok_or(RuntimeStateStatus::PathUnavailable)?;
    save_cache(&mut runtime, &AtomicJsonStore::new(path), provider, entry)
}

pub fn clear_provider_cache(provider: ProviderId) -> Result<(), RuntimeStateStatus> {
    let mut runtime = RUNTIME
        .get()
        .ok_or(RuntimeStateStatus::PathUnavailable)?
        .lock()
        .unwrap();
    let path = state_path().ok_or(RuntimeStateStatus::PathUnavailable)?;
    save_cache(&mut runtime, &AtomicJsonStore::new(path), provider, None)
}

#[derive(Clone, PartialEq, Eq)]
pub struct InstallSalt([u8; ACCOUNT_KEY_BYTES]);

impl InstallSalt {
    pub fn as_bytes(&self) -> &[u8; ACCOUNT_KEY_BYTES] {
        &self.0
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct AlertReceiptV1 {
    pub provider: ProviderId,
    pub account: AccountKey,
    pub limit: LimitId,
    pub threshold_percent: u8,
    pub reset_instance_unix: Option<i64>,
    pub below_threshold_observed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeStateStatus {
    Ready,
    Created,
    RecoveredFromBackup,
    CorruptRecreated,
    FutureSchema(u64),
    Invalid,
    PathUnavailable,
    ReadFailed(StoreError),
    WriteFailed(StoreError),
    EntropyUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntropyError;

impl fmt::Display for EntropyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("system entropy unavailable")
    }
}

impl std::error::Error for EntropyError {}

trait Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyError>;
}

struct SystemEntropy;

impl Entropy for SystemEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyError> {
        let status = unsafe {
            BCryptGenRandom(
                BCRYPT_ALG_HANDLE::default(),
                bytes,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        status.is_ok().then_some(()).ok_or(EntropyError)
    }
}

struct RuntimeState {
    install_salt: Option<InstallSalt>,
    alert_receipts: Vec<AlertReceiptV1>,
    legacy_alert_migrations: Vec<ProviderId>,
    status: RuntimeStateStatus,
    raw: Map<String, Value>,
}

static RUNTIME: OnceLock<Mutex<RuntimeState>> = OnceLock::new();

pub fn initialize() -> RuntimeStateStatus {
    let runtime = RUNTIME.get_or_init(|| {
        let state = match state_path() {
            Some(path) => load_or_create(&AtomicJsonStore::new(path), &SystemEntropy),
            None => RuntimeState {
                install_salt: None,
                alert_receipts: Vec::new(),
                legacy_alert_migrations: Vec::new(),
                status: RuntimeStateStatus::PathUnavailable,
                raw: Map::new(),
            },
        };
        Mutex::new(state)
    });
    runtime.lock().unwrap().status
}

pub fn initialize_read_only() -> RuntimeStateStatus {
    let runtime = RUNTIME.get_or_init(|| {
        let state = state_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .map(|raw| decode(raw, RuntimeStateStatus::Ready))
            .unwrap_or_else(|| invalid(Map::new()));
        Mutex::new(state)
    });
    runtime.lock().unwrap().status
}

pub fn install_salt() -> Option<InstallSalt> {
    RUNTIME
        .get()
        .and_then(|runtime| runtime.lock().unwrap().install_salt.clone())
}

pub fn alert_receipts() -> Vec<AlertReceiptV1> {
    RUNTIME
        .get()
        .map(|runtime| runtime.lock().unwrap().alert_receipts.clone())
        .unwrap_or_default()
}

pub fn select_account(
    provider: ProviderId,
    account: Option<&AccountContext>,
    legacy_receipts: &HashMap<String, i64>,
) -> Vec<AlertReceiptV1> {
    let Some(runtime) = RUNTIME.get() else {
        return Vec::new();
    };
    let mut runtime = runtime.lock().unwrap();
    let (next_receipts, next_migrations) = next_account_state(
        &runtime.alert_receipts,
        &runtime.legacy_alert_migrations,
        provider,
        account,
        legacy_receipts,
    );

    if next_receipts != runtime.alert_receipts || next_migrations != runtime.legacy_alert_migrations
    {
        let encoded = match runtime.install_salt.as_ref() {
            Some(salt) => encode(&runtime.raw, salt, &next_receipts, &next_migrations),
            None => return Vec::new(),
        };
        let Some(path) = state_path() else {
            runtime.status = RuntimeStateStatus::PathUnavailable;
            return Vec::new();
        };
        if let Err(error) = AtomicJsonStore::new(path).save(&encoded) {
            runtime.status = RuntimeStateStatus::WriteFailed(error);
            return next_receipts
                .into_iter()
                .filter(|receipt| account.is_some_and(|account| receipt.account == account.key))
                .collect();
        }
        runtime.raw = encoded;
        runtime.alert_receipts = next_receipts;
        runtime.legacy_alert_migrations = next_migrations;
        runtime.status = RuntimeStateStatus::Ready;
    }
    runtime.alert_receipts.clone()
}

fn next_account_state(
    current_receipts: &[AlertReceiptV1],
    current_migrations: &[ProviderId],
    provider: ProviderId,
    account: Option<&AccountContext>,
    legacy_receipts: &HashMap<String, i64>,
) -> (Vec<AlertReceiptV1>, Vec<ProviderId>) {
    let mut next_receipts = current_receipts.to_vec();
    next_receipts.retain(|receipt| {
        receipt.provider != provider
            || account.is_some_and(|account| {
                account.persistence == IdentityPersistence::Persistent
                    && receipt.account == account.key
            })
    });
    let mut next_migrations = current_migrations.to_vec();
    if let Some(account) = account.filter(|account| {
        account.persistence == IdentityPersistence::Persistent
            && !next_migrations.contains(&provider)
    }) {
        next_receipts.extend(migrate_legacy_receipts(
            provider,
            &account.key,
            legacy_receipts,
        ));
        next_migrations.push(provider);
    }
    (next_receipts, next_migrations)
}

pub fn persist_alert_receipts(receipts: &[AlertReceiptV1]) -> Result<(), RuntimeStateStatus> {
    if receipts.len() > MAX_ALERT_RECEIPTS {
        return Err(RuntimeStateStatus::Invalid);
    }
    let runtime = RUNTIME.get().ok_or(RuntimeStateStatus::PathUnavailable)?;
    let mut runtime = runtime.lock().unwrap();
    let salt = runtime.install_salt.as_ref().ok_or(runtime.status)?;
    let encoded = encode(
        &runtime.raw,
        salt,
        receipts,
        &runtime.legacy_alert_migrations,
    );
    let path = state_path().ok_or(RuntimeStateStatus::PathUnavailable)?;
    if let Err(error) = AtomicJsonStore::new(path).save(&encoded) {
        runtime.status = RuntimeStateStatus::WriteFailed(error);
        return Err(runtime.status);
    }
    runtime.raw = encoded;
    runtime.alert_receipts = receipts.to_vec();
    runtime.status = RuntimeStateStatus::Ready;
    Ok(())
}

pub fn status() -> RuntimeStateStatus {
    RUNTIME
        .get()
        .map(|runtime| runtime.lock().unwrap().status)
        .unwrap_or(RuntimeStateStatus::PathUnavailable)
}

pub fn diagnostic() -> Option<String> {
    match status() {
        RuntimeStateStatus::Ready | RuntimeStateStatus::Created => None,
        RuntimeStateStatus::RecoveredFromBackup => {
            Some("Runtime state recovered from verified backup".to_string())
        }
        RuntimeStateStatus::CorruptRecreated => {
            Some("Invalid runtime state preserved; cache reset".to_string())
        }
        RuntimeStateStatus::FutureSchema(version) => Some(format!(
            "Runtime state schema {version} is newer · cache disabled"
        )),
        RuntimeStateStatus::Invalid => Some("Runtime state invalid · cache disabled".to_string()),
        RuntimeStateStatus::PathUnavailable => {
            Some("Runtime state path unavailable · cache disabled".to_string())
        }
        RuntimeStateStatus::ReadFailed(_) => {
            Some("Runtime state read failed · Copy diagnostics".to_string())
        }
        RuntimeStateStatus::WriteFailed(_) => {
            Some("Runtime state write failed · Copy diagnostics".to_string())
        }
        RuntimeStateStatus::EntropyUnavailable => {
            Some("Runtime state entropy unavailable · cache disabled".to_string())
        }
    }
}

fn state_path() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("state.json"))
}

fn load_or_create<F: FaultInjector>(
    store: &AtomicJsonStore<F>,
    entropy: &impl Entropy,
) -> RuntimeState {
    match store.load::<Map<String, Value>>() {
        Ok(LoadOutcome::Loaded(raw)) => decode(raw, RuntimeStateStatus::Ready),
        Ok(LoadOutcome::RecoveredFromBackup(raw)) => {
            decode(raw, RuntimeStateStatus::RecoveredFromBackup)
        }
        Ok(LoadOutcome::Missing) => create(store, entropy, RuntimeStateStatus::Created),
        Ok(LoadOutcome::CorruptPreserved) => {
            create(store, entropy, RuntimeStateStatus::CorruptRecreated)
        }
        Err(error) => RuntimeState {
            install_salt: None,
            alert_receipts: Vec::new(),
            legacy_alert_migrations: Vec::new(),
            status: RuntimeStateStatus::ReadFailed(error),
            raw: Map::new(),
        },
    }
}

fn create<F: FaultInjector>(
    store: &AtomicJsonStore<F>,
    entropy: &impl Entropy,
    success_status: RuntimeStateStatus,
) -> RuntimeState {
    let mut salt = [0; ACCOUNT_KEY_BYTES];
    if entropy.fill(&mut salt).is_err() {
        return RuntimeState {
            install_salt: None,
            alert_receipts: Vec::new(),
            legacy_alert_migrations: Vec::new(),
            status: RuntimeStateStatus::EntropyUnavailable,
            raw: Map::new(),
        };
    }
    let install_salt = InstallSalt(salt);
    let raw = encode(&Map::new(), &install_salt, &[], &[]);
    if let Err(error) = store.save(&raw) {
        return RuntimeState {
            install_salt: None,
            alert_receipts: Vec::new(),
            legacy_alert_migrations: Vec::new(),
            status: RuntimeStateStatus::WriteFailed(error),
            raw: Map::new(),
        };
    }
    RuntimeState {
        install_salt: Some(install_salt),
        alert_receipts: Vec::new(),
        legacy_alert_migrations: Vec::new(),
        status: success_status,
        raw,
    }
}

fn decode(raw: Map<String, Value>, source_status: RuntimeStateStatus) -> RuntimeState {
    let Some(schema) = raw.get("schema_version").and_then(Value::as_u64) else {
        return invalid(raw);
    };
    if schema > SCHEMA_VERSION {
        return RuntimeState {
            install_salt: None,
            alert_receipts: Vec::new(),
            legacy_alert_migrations: Vec::new(),
            status: RuntimeStateStatus::FutureSchema(schema),
            raw,
        };
    }
    if schema != SCHEMA_VERSION {
        return invalid(raw);
    }

    let Some(salt) = raw
        .get("install_salt")
        .and_then(Value::as_str)
        .and_then(crate::provider::model::decode_hex_32)
        .map(InstallSalt)
    else {
        return invalid(raw);
    };
    let Some(receipt_value) = raw.get("alert_receipts").cloned() else {
        return invalid(raw);
    };
    let Ok(alert_receipts) = serde_json::from_value::<Vec<AlertReceiptV1>>(receipt_value) else {
        return invalid(raw);
    };
    if alert_receipts.len() > MAX_ALERT_RECEIPTS {
        return invalid(raw);
    }
    let legacy_alert_migrations = match raw.get("legacy_alert_migrations") {
        Some(value) => match serde_json::from_value::<Vec<ProviderId>>(value.clone()) {
            Ok(migrations) => migrations,
            Err(_) => return invalid(raw),
        },
        None => Vec::new(),
    };

    RuntimeState {
        install_salt: Some(salt),
        alert_receipts,
        legacy_alert_migrations,
        status: source_status,
        raw,
    }
}

fn invalid(raw: Map<String, Value>) -> RuntimeState {
    RuntimeState {
        install_salt: None,
        alert_receipts: Vec::new(),
        legacy_alert_migrations: Vec::new(),
        status: RuntimeStateStatus::Invalid,
        raw,
    }
}

fn encode(
    raw: &Map<String, Value>,
    install_salt: &InstallSalt,
    alert_receipts: &[AlertReceiptV1],
    legacy_alert_migrations: &[ProviderId],
) -> Map<String, Value> {
    let mut encoded = raw.clone();
    encoded.insert("schema_version".to_string(), Value::from(SCHEMA_VERSION));
    encoded.insert(
        "install_salt".to_string(),
        Value::from(encode_hex(install_salt.as_bytes())),
    );
    encoded.insert(
        "alert_receipts".to_string(),
        serde_json::to_value(alert_receipts).expect("receipt envelope is serializable"),
    );
    encoded.insert(
        "legacy_alert_migrations".to_string(),
        serde_json::to_value(legacy_alert_migrations).expect("migration markers are serializable"),
    );
    encoded
}

fn migrate_legacy_receipts(
    provider: ProviderId,
    account: &AccountKey,
    legacy_receipts: &HashMap<String, i64>,
) -> Vec<AlertReceiptV1> {
    let prefix = match provider {
        ProviderId::Claude => "Claude.",
        ProviderId::Codex => "Codex.",
    };
    legacy_receipts
        .iter()
        .filter_map(|(legacy_key, reset)| {
            let remainder = legacy_key.strip_prefix(prefix)?;
            let (kind, _) = remainder.split_once('.')?;
            if kind == "extra" {
                return None;
            }
            Some(AlertReceiptV1 {
                provider,
                account: account.clone(),
                limit: LimitId::new(kind)?,
                threshold_percent: 75,
                reset_instance_unix: (*reset != 0).then_some(*reset),
                below_threshold_observed: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use crate::store::{FailurePoint, StoreErrorKind, StoreStage};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct FixedEntropy(u8);

    impl Entropy for FixedEntropy {
        fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyError> {
            bytes.fill(self.0);
            Ok(())
        }
    }

    struct FailingEntropy;

    impl Entropy for FailingEntropy {
        fn fill(&self, _bytes: &mut [u8]) -> Result<(), EntropyError> {
            Err(EntropyError)
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

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-state-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("state.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            assert!(self.0.starts_with(std::env::temp_dir()));
            assert!(self.0.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("claudometer-state-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cache_account(byte: u8) -> AccountContext {
        AccountContext {
            key: AccountKey::from_digest([byte; 32]),
            persistence: IdentityPersistence::Persistent,
        }
    }

    fn cache_entry() -> CachedProvider {
        let source = SourceProvenance::compatibility(ProviderId::Claude);
        CachedProvider {
            provider: ProviderId::Claude,
            account: cache_account(1).key.clone(),
            source,
            snapshot: Some(UsageSnapshot {
                provider: ProviderId::Claude,
                account: cache_account(1).key,
                source,
                rows: vec![crate::provider::model::UsageLimit::from_adapter(
                    "session".to_string(),
                    LimitKind::Session,
                    "Session".to_string(),
                    80.0,
                    None,
                    Some(2000),
                    Some(18000),
                )
                .unwrap()],
                plan: Some("Synthetic".to_string()),
                fetched_unix: 1000,
            }),
            retry_at_unix: Some(1200),
        }
    }

    #[test]
    fn cache_round_trip_matches_provider_account_and_selected_source() {
        let raw = cache_update(&Map::new(), ProviderId::Claude, Some(cache_entry())).unwrap();
        let encoded = serde_json::to_vec(&raw).unwrap();
        let raw = serde_json::from_slice(&encoded).unwrap();
        let source = SourceProvenance::compatibility(ProviderId::Claude);
        let restored =
            matching_cache(&raw, ProviderId::Claude, &cache_account(1), source, 1100).unwrap();
        assert_eq!(restored.snapshot.unwrap().fetched_unix, 1000);
        assert_eq!(restored.retry_at_unix, Some(1200));
        assert!(
            matching_cache(&raw, ProviderId::Claude, &cache_account(2), source, 1100).is_none()
        );
        assert!(matching_cache(&raw, ProviderId::Codex, &cache_account(1), source, 1100).is_none());
        assert!(matching_cache(
            &raw,
            ProviderId::Claude,
            &cache_account(1),
            SourceProvenance {
                id: SourceId::CodexAppServer,
                support: SourceSupport::Documented
            },
            1100
        )
        .is_none());
        let mut ephemeral = cache_account(1);
        ephemeral.persistence = IdentityPersistence::MemoryOnly;
        assert!(matching_cache(&raw, ProviderId::Claude, &ephemeral, source, 1100).is_none());
        assert!(
            cache_update(&raw, ProviderId::Claude, None).unwrap()["provider_cache"]["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cache_age_reset_and_retry_boundaries_drop_invalid_values() {
        let mut entry = cache_entry();
        entry.snapshot.as_mut().unwrap().rows.push(
            crate::provider::model::UsageLimit::from_adapter(
                "weekly".to_string(),
                LimitKind::Weekly,
                "Weekly".to_string(),
                60.0,
                None,
                None,
                Some(604800),
            )
            .unwrap(),
        );
        let raw = cache_update(&Map::new(), ProviderId::Claude, Some(entry)).unwrap();
        let load = |now| {
            matching_cache(
                &raw,
                ProviderId::Claude,
                &cache_account(1),
                SourceProvenance::compatibility(ProviderId::Claude),
                now,
            )
        };
        assert_eq!(load(1999).unwrap().snapshot.unwrap().rows.len(), 2);
        assert_eq!(load(2000).unwrap().snapshot.unwrap().rows.len(), 1);
        assert!(load(999).unwrap().snapshot.is_none());
        assert!(load(1000 + MAX_CACHE_AGE_SECONDS).is_some());
        assert!(load(1001 + MAX_CACHE_AGE_SECONDS).is_none());
        assert_eq!(load(1100).unwrap().retry_at_unix, Some(1200));
        assert_eq!(load(1200).unwrap().retry_at_unix, None);
        let mut far_future = cache_entry();
        far_future.retry_at_unix = Some(10000);
        let raw = cache_update(&Map::new(), ProviderId::Claude, Some(far_future)).unwrap();
        assert!(matching_cache(
            &raw,
            ProviderId::Claude,
            &cache_account(1),
            SourceProvenance::compatibility(ProviderId::Claude),
            1100
        )
        .unwrap()
        .retry_at_unix
        .is_none());
    }

    #[test]
    fn malformed_cache_is_optional_and_does_not_poison_existing_state() {
        let raw = cache_update(
            &encode(&Map::new(), &InstallSalt([7; 32]), &[], &[]),
            ProviderId::Claude,
            Some(cache_entry()),
        )
        .unwrap();
        let entry = raw["provider_cache"]["entries"][0].clone();
        for mutation in 0..12 {
            let mut broken = raw.clone();
            match mutation {
                0 => {
                    broken["provider_cache"]["entries"] =
                        serde_json::json!([entry.clone(), entry.clone(), entry.clone()])
                }
                1 => {
                    broken["provider_cache"]["entries"] =
                        serde_json::json!([entry.clone(), entry.clone()])
                }
                2 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"] =
                        Value::Array(vec![entry["snapshot"]["rows"][0].clone(); 65])
                }
                3 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["plan"] =
                        Value::from("x".repeat(513))
                }
                4 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"][0]["percent"] =
                        Value::from(101)
                }
                5 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"][0]["label"] =
                        Value::from("x".repeat(513))
                }
                6 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["fetched_unix"] =
                        Value::from(-1)
                }
                7 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["account"] =
                        Value::from("bad")
                }
                8 => {
                    broken["provider_cache"]["entries"][0]["source"]["support"] =
                        Value::from("Documented")
                }
                9 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"][0]["class"] =
                        Value::from("Spend")
                }
                10 => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"][0]["resets_unix"] =
                        Value::from(i64::MAX)
                }
                _ => {
                    broken["provider_cache"]["entries"][0]["snapshot"]["rows"][0]
                        ["window_seconds"] = Value::from(0)
                }
            }
            assert!(decode_cache(&broken).is_empty(), "case {mutation}");
            assert_eq!(
                decode(broken, RuntimeStateStatus::Ready).status,
                RuntimeStateStatus::Ready
            );
        }
    }

    #[test]
    fn cache_is_additive_and_survives_old_reader_and_receipt_writes() {
        let mut raw = encode(&Map::new(), &InstallSalt([7; 32]), &[], &[]);
        raw.insert(
            "future_setting".to_string(),
            serde_json::json!({"keep":true}),
        );
        let encoded = cache_update(&raw, ProviderId::Claude, Some(cache_entry())).unwrap();
        assert_eq!(encoded["schema_version"], Value::from(1));
        let old_reader = decode(encoded.clone(), RuntimeStateStatus::Ready);
        let old_write = encode(&old_reader.raw, &old_reader.install_salt.unwrap(), &[], &[]);
        assert_eq!(old_write["provider_cache"], encoded["provider_cache"]);
        assert_eq!(old_write["future_setting"], raw["future_setting"]);
        let mut future = encoded;
        future["provider_cache"]["version"] = Value::from(9);
        assert!(cache_update(&future, ProviderId::Claude, None).is_err());
    }

    #[test]
    fn cache_written_fields_exclude_unrecognized_payload_material() {
        let mut raw = cache_update(&Map::new(), ProviderId::Claude, Some(cache_entry())).unwrap();
        raw["provider_cache"]["entries"][0]["raw_body"] = Value::from("synthetic-private-response");
        raw["provider_cache"]["entries"][0]["access_token"] =
            Value::from("synthetic-private-token");
        let entries = decode_cache(&raw);
        let rewritten = cache_update(&raw, ProviderId::Claude, Some(entries[0].clone())).unwrap();
        let text = serde_json::to_string(&rewritten["provider_cache"]).unwrap();
        for private in [
            "raw_body",
            "access_token",
            "synthetic-private-token",
            "synthetic-private-response",
        ] {
            assert!(!text.contains(private));
        }
    }

    #[test]
    fn failed_cache_write_preserves_previous_disk_and_memory_generation() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let mut state = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(1));
        save_cache(
            &mut state,
            &AtomicJsonStore::new(&path),
            ProviderId::Claude,
            Some(cache_entry()),
        )
        .unwrap();
        let previous = std::fs::read(&path).unwrap();
        let raw = state.raw.clone();
        let store = AtomicJsonStore::with_fault_injector(
            &path,
            OneFault(Mutex::new(Some((
                FailurePoint::WriteTemporary,
                io::ErrorKind::StorageFull,
            )))),
        );
        assert!(save_cache(&mut state, &store, ProviderId::Claude, None).is_err());
        assert_eq!(state.raw, raw);
        assert_eq!(std::fs::read(&path).unwrap(), previous);
    }

    #[test]
    fn missing_or_corrupt_cache_state_never_restores_values() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let mut state = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(1));
        save_cache(
            &mut state,
            &AtomicJsonStore::new(&path),
            ProviderId::Claude,
            Some(cache_entry()),
        )
        .unwrap();
        let restarted = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(2));
        assert_eq!(decode_cache(&restarted.raw).len(), 1);
        // Removing the optional store and its verified backup represents a
        // complete cache reset; removing only primary correctly recovers backup.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_file_name("state.json.bak")).unwrap();
        let missing = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(2));
        assert!(decode_cache(&missing.raw).is_empty());
        std::fs::write(&path, b"{broken").unwrap();
        let corrupt = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(3));
        assert!(decode_cache(&corrupt.raw).is_empty());
        assert!(matches!(
            corrupt.status,
            RuntimeStateStatus::CorruptRecreated
        ));
    }

    #[test]
    fn missing_state_creates_a_durable_salt_and_empty_receipt_envelope() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::new(&path);
        let state = load_or_create(&store, &FixedEntropy(0x2a));
        assert_eq!(state.status, RuntimeStateStatus::Created);
        assert_eq!(state.install_salt.unwrap().as_bytes(), &[0x2a; 32]);
        assert!(state.alert_receipts.is_empty());

        let raw: Map<String, Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw.get("schema_version"), Some(&Value::from(1)));
        assert_eq!(raw.get("install_salt").unwrap().as_str().unwrap().len(), 64);
        assert_eq!(raw.get("alert_receipts"), Some(&Value::Array(Vec::new())));

        let restarted = load_or_create(&store, &FixedEntropy(0xff));
        assert_eq!(restarted.status, RuntimeStateStatus::Ready);
        assert_eq!(restarted.install_salt.unwrap().as_bytes(), &[0x2a; 32]);
    }

    #[test]
    fn deleting_state_is_safe_and_generates_a_new_install_salt() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::new(&path);
        let first = load_or_create(&store, &FixedEntropy(1));
        std::fs::remove_file(&path).unwrap();
        let second = load_or_create(&store, &FixedEntropy(2));
        assert_ne!(
            first.install_salt.unwrap().as_bytes(),
            second.install_salt.unwrap().as_bytes()
        );
    }

    #[test]
    fn corrupt_json_is_preserved_before_state_is_recreated() {
        let directory = TestDirectory::new();
        let path = directory.path();
        std::fs::write(&path, b"{malformed").unwrap();
        let state = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(3));
        assert_eq!(state.status, RuntimeStateStatus::CorruptRecreated);
        assert!(path.exists());
        assert_eq!(
            std::fs::read_dir(&directory.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt."))
                .count(),
            1
        );
    }

    #[test]
    fn future_schema_is_never_overwritten() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let bytes = b"{\"schema_version\":9,\"future\":true}\n";
        std::fs::write(&path, bytes).unwrap();
        let state = load_or_create(&AtomicJsonStore::new(&path), &FixedEntropy(4));
        assert_eq!(state.status, RuntimeStateStatus::FutureSchema(9));
        assert!(state.install_salt.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn entropy_failure_writes_nothing_and_exposes_no_salt() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let state = load_or_create(&AtomicJsonStore::new(&path), &FailingEntropy);
        assert_eq!(state.status, RuntimeStateStatus::EntropyUnavailable);
        assert!(state.install_salt.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn state_write_failure_exposes_no_uncommitted_salt() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::with_fault_injector(
            &path,
            OneFault(Mutex::new(Some((
                FailurePoint::WriteTemporary,
                io::ErrorKind::StorageFull,
            )))),
        );
        let state = load_or_create(&store, &FixedEntropy(6));
        assert_eq!(
            state.status,
            RuntimeStateStatus::WriteFailed(StoreError {
                stage: StoreStage::WriteTemporary,
                kind: StoreErrorKind::Io(io::ErrorKind::StorageFull),
            })
        );
        assert!(state.install_salt.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn receipt_envelope_is_account_provider_limit_and_reset_scoped() {
        let salt = InstallSalt([7; ACCOUNT_KEY_BYTES]);
        let receipt = AlertReceiptV1 {
            provider: ProviderId::Codex,
            account: AccountKey::from_digest([8; ACCOUNT_KEY_BYTES]),
            limit: LimitId::new("weekly_all").unwrap(),
            threshold_percent: 75,
            reset_instance_unix: Some(1234),
            below_threshold_observed: false,
        };
        let encoded = encode(&Map::new(), &salt, std::slice::from_ref(&receipt), &[]);
        let decoded = decode(encoded, RuntimeStateStatus::Ready);
        assert_eq!(decoded.alert_receipts.len(), 1);
        let restored = &decoded.alert_receipts[0];
        assert_eq!(restored.provider, ProviderId::Codex);
        assert!(restored.account == receipt.account);
        assert_eq!(restored.limit, receipt.limit);
        assert_eq!(restored.reset_instance_unix, Some(1234));
    }

    #[test]
    fn legacy_receipts_migrate_once_and_never_follow_a_later_account() {
        let account_a = AccountContext {
            key: AccountKey::from_digest([1; ACCOUNT_KEY_BYTES]),
            persistence: IdentityPersistence::Persistent,
        };
        let account_b = AccountContext {
            key: AccountKey::from_digest([2; ACCOUNT_KEY_BYTES]),
            persistence: IdentityPersistence::Persistent,
        };
        let codex_receipt = AlertReceiptV1 {
            provider: ProviderId::Codex,
            account: AccountKey::from_digest([3; ACCOUNT_KEY_BYTES]),
            limit: LimitId::new("session").unwrap(),
            threshold_percent: 75,
            reset_instance_unix: Some(500),
            below_threshold_observed: false,
        };
        let legacy = HashMap::from([
            ("Claude.session.Session (5h)".to_string(), 100),
            ("Claude.extra.Extra usage".to_string(), 200),
            ("Codex.session.Session".to_string(), 300),
        ]);

        let (first_receipts, migrations) = next_account_state(
            std::slice::from_ref(&codex_receipt),
            &[],
            ProviderId::Claude,
            Some(&account_a),
            &legacy,
        );
        assert!(migrations.contains(&ProviderId::Claude));
        assert_eq!(
            first_receipts
                .iter()
                .filter(|receipt| receipt.provider == ProviderId::Claude)
                .count(),
            1
        );
        assert!(first_receipts.iter().any(|receipt| {
            receipt.provider == ProviderId::Claude && receipt.account == account_a.key
        }));
        assert!(first_receipts.contains(&codex_receipt));

        let (second_receipts, second_migrations) = next_account_state(
            &first_receipts,
            &migrations,
            ProviderId::Claude,
            Some(&account_b),
            &legacy,
        );
        assert_eq!(second_migrations, migrations);
        assert!(!second_receipts
            .iter()
            .any(|receipt| receipt.provider == ProviderId::Claude));
        assert!(second_receipts.contains(&codex_receipt));
    }

    #[test]
    fn token_fingerprint_accounts_never_enter_persistent_receipts() {
        let memory_only = AccountContext {
            key: AccountKey::from_digest([4; ACCOUNT_KEY_BYTES]),
            persistence: IdentityPersistence::MemoryOnly,
        };
        let existing = AlertReceiptV1 {
            provider: ProviderId::Claude,
            account: AccountKey::from_digest([5; ACCOUNT_KEY_BYTES]),
            limit: LimitId::new("session").unwrap(),
            threshold_percent: 75,
            reset_instance_unix: Some(100),
            below_threshold_observed: false,
        };
        let (receipts, migrations) = next_account_state(
            &[existing],
            &[],
            ProviderId::Claude,
            Some(&memory_only),
            &HashMap::new(),
        );
        assert!(receipts.is_empty());
        assert!(migrations.is_empty());
    }

    #[test]
    fn unknown_fields_survive_envelope_extension() {
        let mut raw = Map::new();
        raw.insert("future".to_string(), serde_json::json!({ "keep": true }));
        let salt = InstallSalt([5; ACCOUNT_KEY_BYTES]);
        let encoded = encode(&raw, &salt, &[], &[]);
        assert_eq!(encoded.get("future"), raw.get("future"));
    }

    #[test]
    fn system_entropy_produces_nonzero_bytes() {
        let mut bytes = [0; ACCOUNT_KEY_BYTES];
        SystemEntropy.fill(&mut bytes).unwrap();
        assert!(bytes.iter().any(|byte| *byte != 0));
    }
}
