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
    encode_hex, AccountContext, AccountKey, IdentityPersistence, LimitId, ProviderId,
    ACCOUNT_KEY_BYTES,
};
use crate::store::{AtomicJsonStore, FaultInjector, LoadOutcome, StoreError};

const SCHEMA_VERSION: u64 = 1;
const MAX_ALERT_RECEIPTS: usize = 4096;

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
        RuntimeStateStatus::ReadFailed(error) => {
            Some(format!("Runtime state read failed · {:?}", error.stage))
        }
        RuntimeStateStatus::WriteFailed(error) => {
            Some(format!("Runtime state write failed · {:?}", error.stage))
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
