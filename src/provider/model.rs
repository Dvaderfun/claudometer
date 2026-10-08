use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows::core::PCWSTR;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptGenRandom, BCryptHash, BCryptOpenAlgorithmProvider,
    BCRYPT_ALG_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

pub const ACCOUNT_KEY_BYTES: usize = 32;
pub const MAX_LIMIT_ID_BYTES: usize = 128;
const ACCOUNT_DOMAIN: &[u8] = b"claudometer-account-v1\0";

static EPHEMERAL_ACCOUNT_SALT: OnceLock<Option<[u8; ACCOUNT_KEY_BYTES]>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Claude,
    Codex,
}

impl ProviderId {
    pub const fn index(self) -> usize {
        match self {
            Self::Claude => 0,
            Self::Codex => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SourceId {
    ClaudeOAuthCompatibility,
    CodexWhamCompatibility,
    CodexAppServer,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SourceSupport {
    Compatibility,
    Documented,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SourceProvenance {
    pub id: SourceId,
    pub support: SourceSupport,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum LimitKind {
    Session,
    Weekly,
    Model,
    ExtraUsage,
    Other(String),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum LimitClass {
    Quota,
    Spend,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ProviderSeverity {
    Critical,
    Warning,
    Normal,
}

impl ProviderSeverity {
    pub fn from_hint(value: &str) -> Option<Self> {
        if value.is_empty() {
            return None;
        }
        let value = value.to_ascii_lowercase();
        Some(
            if value.contains("exceed") || value.contains("critical") || value.contains("error") {
                Self::Critical
            } else if value.contains("warn") || value.contains("elevated") {
                Self::Warning
            } else {
                Self::Normal
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct Percent(f64);

impl<'de> Deserialize<'de> for Percent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err(D::Error::custom("invalid cached percentage"));
        }
        Ok(Self(value))
    }
}

impl Percent {
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then(|| Self(value.clamp(0.0, 100.0)))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct UsageLimit {
    pub id: LimitId,
    pub kind: LimitKind,
    pub class: LimitClass,
    pub label: String,
    pub percent: Percent,
    pub severity: Option<ProviderSeverity>,
    pub resets_unix: Option<i64>,
    pub window_seconds: Option<u32>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct UsageSnapshot {
    pub provider: ProviderId,
    pub account: AccountKey,
    pub source: SourceProvenance,
    pub rows: Vec<UsageLimit>,
    pub plan: Option<String>,
    pub fetched_unix: i64,
}

#[derive(Clone)]
pub enum FetchOutcome {
    Ok(UsageSnapshot),
    Err {
        msg: String,
        retry_after: Option<u64>,
        rate_limited: bool,
    },
}

impl SourceProvenance {
    pub fn compatibility(provider: ProviderId) -> Self {
        Self {
            id: match provider {
                ProviderId::Claude => SourceId::ClaudeOAuthCompatibility,
                ProviderId::Codex => SourceId::CodexWhamCompatibility,
            },
            support: SourceSupport::Compatibility,
        }
    }
}

impl UsageLimit {
    pub fn from_adapter(
        identity: String,
        kind: LimitKind,
        label: String,
        percent: f64,
        severity: Option<ProviderSeverity>,
        resets_unix: Option<i64>,
        window_seconds: Option<u32>,
    ) -> Option<Self> {
        let id = LimitId::new(identity.clone()).or_else(|| {
            // Preserve bounded stable identity even for an unknown long/empty kind.
            sha256(identity.as_bytes())
                .ok()
                .and_then(|digest| LimitId::new(encode_hex(&digest)))
        })?;
        Some(Self {
            id,
            class: if kind == LimitKind::ExtraUsage {
                LimitClass::Spend
            } else {
                LimitClass::Quota
            },
            kind,
            label,
            percent: Percent::new(percent)?,
            severity,
            resets_unix,
            window_seconds,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AccountKey([u8; ACCOUNT_KEY_BYTES]);

impl AccountKey {
    pub fn from_digest(digest: [u8; ACCOUNT_KEY_BYTES]) -> Self {
        Self(digest)
    }

    pub fn as_bytes(&self) -> &[u8; ACCOUNT_KEY_BYTES] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPersistence {
    Persistent,
    MemoryOnly,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AccountContext {
    pub key: AccountKey,
    pub persistence: IdentityPersistence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityError;

pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        // SAFETY: replacing existing UTF-8 bytes with zero preserves the
        // allocation's UTF-8 validity and length until String drops it.
        unsafe { self.0.as_mut_vec().fill(0) };
    }
}

pub fn derive_account_context(
    install_salt: Option<&[u8; ACCOUNT_KEY_BYTES]>,
    provider: ProviderId,
    stable_identifier: Option<&SecretString>,
    access_token: &SecretString,
) -> Result<AccountContext, IdentityError> {
    let stable_identifier = stable_identifier.filter(|identifier| !identifier.is_empty());
    let (material, material_tag, persistence) = if let Some(identifier) = stable_identifier {
        (
            identifier.expose().as_bytes(),
            b's',
            if install_salt.is_some() {
                IdentityPersistence::Persistent
            } else {
                IdentityPersistence::MemoryOnly
            },
        )
    } else {
        if access_token.is_empty() {
            return Err(IdentityError);
        }
        (
            access_token.expose().as_bytes(),
            b't',
            IdentityPersistence::MemoryOnly,
        )
    };
    let salt = match install_salt {
        Some(salt) => salt,
        None => EPHEMERAL_ACCOUNT_SALT
            .get_or_init(generate_ephemeral_salt)
            .as_ref()
            .ok_or(IdentityError)?,
    };

    let mut input = Vec::with_capacity(ACCOUNT_DOMAIN.len() + 2 + salt.len() + material.len());
    input.extend_from_slice(ACCOUNT_DOMAIN);
    input.push(match provider {
        ProviderId::Claude => 1,
        ProviderId::Codex => 2,
    });
    input.push(material_tag);
    input.extend_from_slice(salt);
    input.extend_from_slice(material);
    let digest = sha256(&input);
    input.fill(0);

    Ok(AccountContext {
        key: AccountKey::from_digest(digest?),
        persistence,
    })
}

fn generate_ephemeral_salt() -> Option<[u8; ACCOUNT_KEY_BYTES]> {
    let mut salt = [0; ACCOUNT_KEY_BYTES];
    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut salt,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    status.is_ok().then_some(salt)
}

fn sha256(input: &[u8]) -> Result<[u8; ACCOUNT_KEY_BYTES], IdentityError> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let opened = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    };
    if !opened.is_ok() {
        return Err(IdentityError);
    }

    let mut digest = [0; ACCOUNT_KEY_BYTES];
    let hashed = unsafe { BCryptHash(algorithm, None, input, &mut digest) };
    let _ = unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
    hashed.is_ok().then_some(digest).ok_or(IdentityError)
}

impl Hash for AccountKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Serialize for AccountKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for AccountKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        decode_hex_32(&value)
            .map(Self)
            .ok_or_else(|| D::Error::custom("invalid opaque account key"))
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LimitId(String);

impl LimitId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty() && value.len() <= MAX_LIMIT_ID_BYTES).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for LimitId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| D::Error::custom("invalid stable limit id"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionEvent {
    pub provider: ProviderId,
    pub request_id: RequestId,
}

#[derive(Clone)]
pub struct FetchCompletion<T> {
    pub provider: ProviderId,
    pub generation: Generation,
    pub request_id: RequestId,
    pub account: AccountKey,
    pub payload: T,
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn decode_hex_32(value: &str) -> Option<[u8; ACCOUNT_KEY_BYTES]> {
    if value.len() != ACCOUNT_KEY_BYTES * 2 {
        return None;
    }
    let mut decoded = [0; ACCOUNT_KEY_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(decoded)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rejects_nonfinite_and_clamps_at_boundary() {
        assert!(Percent::new(f64::NAN).is_none());
        assert!(Percent::new(f64::INFINITY).is_none());
        assert_eq!(Percent::new(-3.0).unwrap().get(), 0.0);
        assert_eq!(Percent::new(104.0).unwrap().get(), 100.0);
        assert_eq!(Percent::new(42.5).unwrap().get(), 42.5);
    }

    #[test]
    fn adapter_identity_is_stable_and_spend_is_explicit() {
        let make = |label| {
            UsageLimit::from_adapter(
                "x".repeat(MAX_LIMIT_ID_BYTES + 1),
                LimitKind::ExtraUsage,
                label,
                75.0,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let first = make("Original label".into());
        let second = make("Changed label".into());
        assert_eq!(first.id, second.id);
        assert!(first.id.as_str().len() <= MAX_LIMIT_ID_BYTES);
        assert_eq!(first.class, LimitClass::Spend);
    }

    #[test]
    fn opaque_account_keys_round_trip_without_debug_or_display_contracts() {
        let key = AccountKey::from_digest([0xab; ACCOUNT_KEY_BYTES]);
        let encoded = serde_json::to_string(&key).unwrap();
        assert_eq!(encoded.len(), ACCOUNT_KEY_BYTES * 2 + 2);
        assert!(serde_json::from_str::<AccountKey>(&encoded).unwrap() == key);
        assert!(serde_json::from_str::<AccountKey>("\"raw-account-id\"").is_err());
    }

    #[test]
    fn stable_limit_ids_are_nonempty_and_bounded() {
        assert!(LimitId::new("session").is_some());
        assert!(LimitId::new("").is_none());
        assert!(LimitId::new("x".repeat(MAX_LIMIT_ID_BYTES + 1)).is_none());
    }

    #[test]
    fn provider_indices_are_stable() {
        assert_eq!(ProviderId::Claude.index(), 0);
        assert_eq!(ProviderId::Codex.index(), 1);
    }

    #[test]
    fn account_keys_are_domain_separated_and_stability_is_explicit() {
        let salt = [1; ACCOUNT_KEY_BYTES];
        let stable = SecretString::new("provider-account".to_string());
        let token = SecretString::new("worker-token".to_string());
        let claude =
            derive_account_context(Some(&salt), ProviderId::Claude, Some(&stable), &token).unwrap();
        let codex =
            derive_account_context(Some(&salt), ProviderId::Codex, Some(&stable), &token).unwrap();
        assert!(claude.key != codex.key);
        assert_eq!(claude.persistence, IdentityPersistence::Persistent);

        let fallback =
            derive_account_context(Some(&salt), ProviderId::Claude, None, &token).unwrap();
        assert!(fallback.key != claude.key);
        assert_eq!(fallback.persistence, IdentityPersistence::MemoryOnly);
    }
}
