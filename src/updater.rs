//! Self-update from GitHub Releases — passive, transparent, user-initiated.
//!
//! Check: `releases/latest` locates fixed manifest assets. A bounded manifest is
//! accepted only after an embedded-root Ed25519 signature and all local policy
//! checks pass. Checks run at most once per day (and once at launch) when
//! enabled; failures are silent. No nag or toast — the only surfaces are the
//! Settings toggle, About card, and a dot on the flyout gear.
//!
//! Install (only when the user clicks): a verified portable install downloads
//! the fixed checksum and executable into bounded memory, checks the signed
//! exact size/hash plus PE magic, then stages and re-verifies the file before
//! a journaled, write-through rename swap. Attempt-unique candidate and backup
//! names plus hash-based startup reconciliation keep the previous executable
//! through durable commit. Managed and ambiguous installs fail closed to an
//! explicit release-page action; they never rename the executable in place.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HWND, LPARAM, WPARAM,
};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptGenRandom, BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_HASH_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, MoveFileExW, VerQueryValueW,
    MOVEFILE_WRITE_THROUGH, VS_FIXEDFILEINFO,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

const UA: &str = concat!("claudometer/", env!("CARGO_PKG_VERSION"));
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
const MAX_RELEASE_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 1024;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
const MAX_ROLLBACK_BYTES: u64 = 8 * 1024;
const MAX_EXE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const CHANNEL_MARKER: &str = "claudometer.install-channel";
const MANAGED_CHANNEL: &str = "managed";
const UPDATE_JOURNAL: &str = "update-operation.v1.json";
const UPDATE_SCHEMA: u64 = 1;
const ATTEMPT_ID_BYTES: usize = 16;
const READINESS_NONCE_BYTES: usize = 32;
const READINESS_SCHEMA: u64 = 1;
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(5);
const WATCHDOG_POLL: Duration = Duration::from_millis(50);
const UNINSTALL_KEY: PCWSTR =
    windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Claudometer_is1");

#[cfg(target_arch = "x86_64")]
const RELEASE_ARCH: &str = "x64";
#[cfg(target_arch = "aarch64")]
const RELEASE_ARCH: &str = "arm64";
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("Claudometer releases support only x64 and ARM64 Windows");

#[derive(Clone)]
pub struct Release {
    pub tag: String,
    version: (u16, u16, u16),
    expires_at: OffsetDateTime,
    asset_name: String,
    asset_size: u64,
    asset_sha256: String,
    exe_url: String,
    sha_url: String,
    pub page_url: String,
}

#[derive(Clone, Default)]
pub enum Status {
    #[default]
    UpToDate,
    Available(Release),
    Installing,
    /// transient failure caption; release page still reachable via the button
    Failed(String, Option<String>),
}

static STATUS: Mutex<Status> = Mutex::new(Status::UpToDate);
static LAST_CHECK: Mutex<Option<Instant>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

pub fn has_update() -> bool {
    matches!(*STATUS.lock().unwrap(), Status::Available(_))
}

pub fn can_self_update() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|exe| install_channel(&exe) == InstallChannel::Portable)
}

fn set_status(s: Status) {
    *STATUS.lock().unwrap() = s;
    let h = crate::MAIN_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), crate::WM_UPDATE, WPARAM(0), LPARAM(0));
        }
    }
}

fn check_due(enabled: bool, last_check: Option<Duration>) -> bool {
    enabled && last_check.is_none_or(|elapsed| elapsed >= CHECK_EVERY)
}

/// Called at startup and on every poll tick. Disabled means no request; when
/// enabled, checks happen at most once per CHECK_EVERY.
pub fn maybe_check() {
    let last_check = LAST_CHECK.lock().unwrap().map(|checked| checked.elapsed());
    if !check_due(crate::config::settings().update_checks_enabled, last_check) {
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let found = check_inner();
        *LAST_CHECK.lock().unwrap() = Some(Instant::now());
        BUSY.store(false, Ordering::SeqCst);
        match found {
            Ok(Some(rel)) => set_status(Status::Available(rel)),
            // silent: up to date, no releases yet, network down, rate limited…
            Ok(None) => {
                // …but never downgrade an already-known update to UpToDate
                if !has_update() {
                    set_status(Status::UpToDate);
                }
            }
            Err(_) => {}
        }
    });
}

// ---------- check ----------

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: Option<String>,
    draft: Option<bool>,
    prerelease: Option<bool>,
    assets: Option<Vec<ApiAsset>>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: Option<String>,
}

fn agent(timeout_secs: u64) -> Option<ureq::Agent> {
    let tls = native_tls::TlsConnector::new().ok()?;
    Some(
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(tls))
            .timeout(Duration::from_secs(timeout_secs))
            .redirects(0)
            .build(),
    )
}

fn check_inner() -> Result<Option<Release>, String> {
    let agent = agent(10).ok_or("TLS init failed")?;
    let resp = restricted_get(
        &agent,
        crate::network::GITHUB_LATEST_RELEASE_URL,
        crate::network::GITHUB_API_HOSTS,
        Some("application/vnd.github+json"),
    )?;
    let body = read_string_bounded(resp.into_reader(), MAX_RELEASE_METADATA_BYTES)?;
    let rel: ApiRelease = serde_json::from_str(&body).map_err(|_| "release metadata malformed")?;
    fetch_verified_release(&agent, &rel)
}

struct ManifestAssets {
    tag: String,
    manifest_url: String,
    signatures_url: String,
    rollback_url: Option<String>,
    rollback_signatures_url: Option<String>,
}

fn pick_manifest_assets(rel: &ApiRelease) -> Option<ManifestAssets> {
    if rel.draft != Some(false) || rel.prerelease != Some(false) {
        return None;
    }
    let tag = rel.tag_name.clone()?;
    let version = tag
        .strip_prefix('v')
        .and_then(crate::release_manifest::strict_version)?;
    if tag != format!("v{}.{}.{}", version.0, version.1, version.2) {
        return None;
    }
    let stem = format!("claudometer-{tag}-windows-{RELEASE_ARCH}");
    let manifest_name = format!("{stem}.manifest.json");
    let signatures_name = format!("{stem}.manifest.signatures.json");
    let rollback_name = format!("{stem}.rollback.json");
    let rollback_signatures_name = format!("{stem}.rollback.signatures.json");
    let assets = rel.assets.as_deref().unwrap_or(&[]);
    let exactly_one = |name: &str| {
        assets
            .iter()
            .filter(|asset| asset.name.as_deref() == Some(name))
            .count()
            == 1
    };
    if !exactly_one(&manifest_name) || !exactly_one(&signatures_name) {
        return None;
    }
    let base = crate::network::GITHUB_REPOSITORY_URL;
    let download_base = format!("{base}/releases/download/{tag}");
    let rollback_present = exactly_one(&rollback_name);
    let rollback_signatures_present = exactly_one(&rollback_signatures_name);
    if rollback_present != rollback_signatures_present {
        return None;
    }
    Some(ManifestAssets {
        tag,
        manifest_url: format!("{download_base}/{manifest_name}"),
        signatures_url: format!("{download_base}/{signatures_name}"),
        rollback_url: rollback_present.then(|| format!("{download_base}/{rollback_name}")),
        rollback_signatures_url: rollback_signatures_present
            .then(|| format!("{download_base}/{rollback_signatures_name}")),
    })
}

fn fetch_verified_release(
    agent: &ureq::Agent,
    metadata: &ApiRelease,
) -> Result<Option<Release>, String> {
    let Some(assets) = pick_manifest_assets(metadata) else {
        return Ok(None);
    };
    let manifest_bytes = get_bounded(agent, &assets.manifest_url, MAX_MANIFEST_BYTES)?;
    let signature_bytes = get_bounded(agent, &assets.signatures_url, MAX_SIGNATURE_BYTES)?;
    let current_version = crate::release_manifest::strict_version(env!("CARGO_PKG_VERSION"))
        .ok_or("current updater version malformed")?;
    let needs_rollback =
        serde_json::from_slice::<crate::release_manifest::ReleaseManifest>(&manifest_bytes)
            .ok()
            .and_then(|manifest| crate::release_manifest::strict_version(&manifest.version))
            .is_some_and(|version| version < current_version);
    let rollback_bytes = if needs_rollback {
        match (&assets.rollback_url, &assets.rollback_signatures_url) {
            (Some(authorization), Some(signatures)) => Some((
                get_bounded(agent, authorization, MAX_ROLLBACK_BYTES)?,
                get_bounded(agent, signatures, MAX_SIGNATURE_BYTES)?,
            )),
            _ => None,
        }
    } else {
        None
    };
    let rollback = rollback_bytes
        .as_ref()
        .map(|(bytes, signatures)| crate::release_manifest::RollbackProof { bytes, signatures });
    let verified = verify_release_manifest(
        &manifest_bytes,
        &signature_bytes,
        rollback,
        option_env!("CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX"),
        option_env!("CLAUDOMETER_RELEASE_SEQUENCE"),
        current_version,
        OffsetDateTime::now_utc(),
    )?;
    let manifest = verified.manifest;
    if manifest.tag != assets.tag {
        return Err("release metadata tag mismatch".into());
    }
    let release_assets = metadata.assets.as_deref().unwrap_or(&[]);
    let exactly_one = |name: &str| {
        release_assets
            .iter()
            .filter(|asset| asset.name.as_deref() == Some(name))
            .count()
            == 1
    };
    if !exactly_one(&manifest.asset) || !exactly_one(&format!("{}.sha256", manifest.asset)) {
        return Err("signed release asset missing or duplicated".into());
    }
    let version = crate::release_manifest::strict_version(&manifest.version)
        .ok_or("release version malformed")?;
    let expires_at = OffsetDateTime::parse(
        &manifest.policy_expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|_| "release expiry malformed")?;
    let base = crate::network::GITHUB_REPOSITORY_URL;
    let download_base = format!("{base}/releases/download/{}", manifest.tag);
    Ok(Some(Release {
        tag: manifest.tag.clone(),
        version,
        expires_at,
        asset_name: manifest.asset.clone(),
        asset_size: manifest.size,
        asset_sha256: manifest.sha256,
        exe_url: format!("{download_base}/{}", manifest.asset),
        sha_url: format!("{download_base}/{}.sha256", manifest.asset),
        page_url: format!("{base}/releases/tag/{}", manifest.tag),
    }))
}

fn verify_release_manifest(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    rollback: Option<crate::release_manifest::RollbackProof<'_>>,
    public_key_hex: Option<&str>,
    release_sequence: Option<&str>,
    current_version: (u16, u16, u16),
    now: OffsetDateTime,
) -> Result<crate::release_manifest::VerifiedRelease, String> {
    let public_key_hex = public_key_hex.ok_or("release trust root not provisioned")?;
    let trusted_public_key = decode_lower_hex::<32>(public_key_hex)
        .ok_or_else(|| "embedded release public key invalid".to_string())?;
    let current_sequence = release_sequence
        .ok_or("release sequence not provisioned")?
        .parse::<u64>()
        .map_err(|_| "embedded release sequence invalid".to_string())?;
    if current_sequence == 0 {
        return Err("release sequence not provisioned".into());
    }
    crate::release_manifest::verify(
        manifest_bytes,
        signature_bytes,
        rollback,
        crate::release_manifest::VerificationPolicy {
            trusted_public_key,
            channel: crate::release_manifest::RELEASE_CHANNEL,
            architecture: RELEASE_ARCH,
            current_sequence,
            current_version,
            now,
            max_asset_size: MAX_EXE_BYTES,
        },
    )
    .map_err(Into::into)
}

// ---------- crash-safe install operation ----------

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum UpdatePhase {
    Verified,
    CurrentMoved,
    CandidateInstalled,
    CandidateReady,
    Committed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateOperation {
    schema_version: u64,
    attempt_id: String,
    readiness_nonce: String,
    candidate_pid: Option<u32>,
    phase: UpdatePhase,
    canonical_name: String,
    candidate_name: String,
    backup_name: String,
    current_sha256: String,
    candidate_sha256: String,
    candidate_version: String,
}

impl UpdateOperation {
    fn new(
        canonical: &Path,
        attempt_id: String,
        readiness_nonce: String,
        current_sha256: String,
        candidate_sha256: String,
        candidate_version: (u16, u16, u16),
    ) -> Result<Self, String> {
        let canonical_name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("executable name is not valid Unicode")?
            .to_string();
        let (candidate_name, backup_name) = update_names(&canonical_name, &attempt_id)?;
        Ok(Self {
            schema_version: UPDATE_SCHEMA,
            attempt_id,
            readiness_nonce,
            candidate_pid: None,
            phase: UpdatePhase::Verified,
            canonical_name,
            candidate_name,
            backup_name,
            current_sha256,
            candidate_sha256,
            candidate_version: format!(
                "{}.{}.{}",
                candidate_version.0, candidate_version.1, candidate_version.2
            ),
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != UPDATE_SCHEMA
            || !valid_lower_hex(&self.attempt_id, ATTEMPT_ID_BYTES)
            || !valid_lower_hex(&self.readiness_nonce, READINESS_NONCE_BYTES)
            || self.candidate_pid == Some(0)
            || !valid_lower_hex(&self.current_sha256, 32)
            || !valid_lower_hex(&self.candidate_sha256, 32)
            || crate::release_manifest::strict_version(&self.candidate_version).is_none()
        {
            return Err("update journal is invalid".into());
        }
        let (candidate, backup) = update_names(&self.canonical_name, &self.attempt_id)?;
        if self.candidate_name != candidate || self.backup_name != backup {
            return Err("update journal paths are invalid".into());
        }
        Ok(())
    }
}

fn valid_lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn update_names(canonical_name: &str, attempt_id: &str) -> Result<(String, String), String> {
    let canonical = Path::new(canonical_name);
    if canonical.components().count() != 1
        || canonical.file_name().and_then(|name| name.to_str()) != Some(canonical_name)
        || !canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        || !valid_lower_hex(attempt_id, ATTEMPT_ID_BYTES)
    {
        return Err("update journal paths are invalid".into());
    }
    let stem = canonical
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("update journal paths are invalid")?;
    Ok((
        format!("{stem}.{attempt_id}.candidate.exe"),
        format!("{stem}.{attempt_id}.backup.exe"),
    ))
}

fn random_lower_hex<const N: usize>() -> Result<String, String> {
    let mut bytes = [0u8; N];
    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut bytes,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if !status.is_ok() {
        return Err("system entropy unavailable".into());
    }
    let mut encoded = String::with_capacity(N * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").map_err(|_| "attempt ID encoding failed")?;
    }
    Ok(encoded)
}

fn new_attempt_id() -> Result<String, String> {
    random_lower_hex::<ATTEMPT_ID_BYTES>()
}

fn new_readiness_nonce() -> Result<String, String> {
    random_lower_hex::<READINESS_NONCE_BYTES>()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateBoundary {
    VerifiedPersisted,
    CurrentRenamed,
    CurrentMovedPersisted,
    CandidateRenamed,
    CandidateInstalledPersisted,
    CandidatePidPersisted,
    ReadinessPersisted,
    CandidateReturned,
    BackupRestored,
    CandidateReadyPersisted,
    CommittedPersisted,
    BackupRemoved,
    ReadinessRemoved,
    JournalRemoved,
}

trait UpdateBoundaryHook {
    fn after(&mut self, _boundary: UpdateBoundary) {}
}

struct NoUpdateHook;
impl UpdateBoundaryHook for NoUpdateHook {}

struct OperationPaths {
    canonical: PathBuf,
    candidate: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
    journal_backup: PathBuf,
    readiness: PathBuf,
    readiness_backup: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateReadiness {
    schema_version: u64,
    attempt_id: String,
    nonce: String,
    pid: u32,
    version: String,
    sha256: String,
}

fn readiness_matches(readiness: &CandidateReadiness, operation: &UpdateOperation) -> bool {
    readiness.schema_version == READINESS_SCHEMA
        && readiness.attempt_id == operation.attempt_id
        && readiness.nonce == operation.readiness_nonce
        && Some(readiness.pid) == operation.candidate_pid
        && readiness.pid != 0
        && readiness.version == operation.candidate_version
        && readiness.sha256 == operation.candidate_sha256
}

fn operation_paths(
    directory: &Path,
    operation: &UpdateOperation,
) -> Result<OperationPaths, String> {
    operation.validate()?;
    Ok(OperationPaths {
        canonical: directory.join(&operation.canonical_name),
        candidate: directory.join(&operation.candidate_name),
        backup: directory.join(&operation.backup_name),
        journal: directory.join(UPDATE_JOURNAL),
        journal_backup: directory.join(format!("{UPDATE_JOURNAL}.bak")),
        readiness: directory.join(format!("update-readiness.{}.json", operation.attempt_id)),
        readiness_backup: directory.join(format!(
            "update-readiness.{}.json.bak",
            operation.attempt_id
        )),
    })
}

fn save_update_phase(
    store: &crate::store::AtomicJsonStore,
    operation: &mut UpdateOperation,
    phase: UpdatePhase,
    boundary: UpdateBoundary,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    let mut next = operation.clone();
    next.phase = phase;
    store
        .save(&next)
        .map_err(|error| format!("update journal write failed: {error}"))?;
    *operation = next;
    hook.after(boundary);
    Ok(())
}

fn save_candidate_pid(
    paths: &OperationPaths,
    operation: &mut UpdateOperation,
    pid: u32,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    if pid == 0 || operation.phase != UpdatePhase::CandidateInstalled {
        return Err("candidate process identity is invalid".into());
    }
    let mut next = operation.clone();
    next.candidate_pid = Some(pid);
    crate::store::AtomicJsonStore::new(&paths.journal)
        .save(&next)
        .map_err(|error| format!("update journal write failed: {error}"))?;
    *operation = next;
    hook.after(UpdateBoundary::CandidatePidPersisted);
    Ok(())
}

fn load_candidate_readiness(paths: &OperationPaths) -> Result<Option<CandidateReadiness>, String> {
    use crate::store::LoadOutcome;

    match crate::store::AtomicJsonStore::new(&paths.readiness)
        .load::<CandidateReadiness>()
        .map_err(|error| format!("readiness read failed: {error}"))?
    {
        LoadOutcome::Loaded(readiness) | LoadOutcome::RecoveredFromBackup(readiness) => {
            Ok(Some(readiness))
        }
        LoadOutcome::Missing | LoadOutcome::CorruptPreserved => Ok(None),
    }
}

fn load_authenticated_readiness(
    paths: &OperationPaths,
    operation: &UpdateOperation,
) -> Result<Option<CandidateReadiness>, String> {
    Ok(
        load_candidate_readiness(paths)?
            .filter(|readiness| readiness_matches(readiness, operation)),
    )
}

fn persist_candidate_readiness(
    paths: &OperationPaths,
    readiness: &CandidateReadiness,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    crate::store::AtomicJsonStore::new(&paths.readiness)
        .save(readiness)
        .map_err(|error| format!("readiness write failed: {error}"))?;
    hook.after(UpdateBoundary::ReadinessPersisted);
    Ok(())
}

fn load_update_operation(directory: &Path) -> Result<Option<UpdateOperation>, String> {
    use crate::store::LoadOutcome;

    let store = crate::store::AtomicJsonStore::new(directory.join(UPDATE_JOURNAL));
    let operation = match store
        .load::<UpdateOperation>()
        .map_err(|error| format!("update journal read failed: {error}"))?
    {
        LoadOutcome::Loaded(operation) | LoadOutcome::RecoveredFromBackup(operation) => operation,
        LoadOutcome::Missing if has_preserved_update_journal(directory) => {
            return Err("update journal is corrupt".into())
        }
        LoadOutcome::Missing => return Ok(None),
        LoadOutcome::CorruptPreserved => return Err("update journal is corrupt".into()),
    };
    operation.validate()?;
    Ok(Some(operation))
}

fn has_preserved_update_journal(directory: &Path) -> bool {
    let prefix = format!("{UPDATE_JOURNAL}.corrupt.");
    std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn move_update_file(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Err("update destination already exists".into());
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| "update rename failed".into())
}

fn start_handover(
    directory: &Path,
    operation: &mut UpdateOperation,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    let paths = operation_paths(directory, operation)?;
    let store = crate::store::AtomicJsonStore::new(&paths.journal);
    save_update_phase(
        &store,
        operation,
        UpdatePhase::Verified,
        UpdateBoundary::VerifiedPersisted,
        hook,
    )?;
    move_update_file(&paths.canonical, &paths.backup)?;
    hook.after(UpdateBoundary::CurrentRenamed);
    save_update_phase(
        &store,
        operation,
        UpdatePhase::CurrentMoved,
        UpdateBoundary::CurrentMovedPersisted,
        hook,
    )?;
    move_update_file(&paths.candidate, &paths.canonical)?;
    hook.after(UpdateBoundary::CandidateRenamed);
    save_update_phase(
        &store,
        operation,
        UpdatePhase::CandidateInstalled,
        UpdateBoundary::CandidateInstalledPersisted,
        hook,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstalledImage {
    Missing,
    Current,
    Candidate,
    Unknown,
}

fn installed_image(path: &Path, current_sha256: &str, candidate_sha256: &str) -> InstalledImage {
    if !path.exists() {
        return InstalledImage::Missing;
    }
    match sha256_of(path).as_deref() {
        Some(hash) if hash == current_sha256 => InstalledImage::Current,
        Some(hash) if hash == candidate_sha256 => InstalledImage::Candidate,
        _ => InstalledImage::Unknown,
    }
}

fn remove_update_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("update cleanup failed".into()),
    }
}

fn delete_update_journal(
    paths: &OperationPaths,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    // Remove the store backup first: if interrupted, the committed primary
    // journal still makes cleanup retryable on the next launch.
    remove_update_file(&paths.journal_backup)?;
    remove_update_file(&paths.journal)?;
    hook.after(UpdateBoundary::JournalRemoved);
    Ok(())
}

fn finish_committed(
    paths: &OperationPaths,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    remove_update_file(&paths.backup)?;
    hook.after(UpdateBoundary::BackupRemoved);
    remove_update_file(&paths.candidate)?;
    remove_update_file(&paths.readiness_backup)?;
    remove_update_file(&paths.readiness)?;
    hook.after(UpdateBoundary::ReadinessRemoved);
    delete_update_journal(paths, hook)
}

fn commit_candidate(
    operation: &mut UpdateOperation,
    paths: &OperationPaths,
    readiness: &CandidateReadiness,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    if installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    ) != InstalledImage::Candidate
        || installed_image(
            &paths.backup,
            &operation.current_sha256,
            &operation.candidate_sha256,
        ) != InstalledImage::Current
    {
        return Err("update images do not match the journal".into());
    }
    if !readiness_matches(readiness, operation) {
        return Err("candidate readiness does not match the update attempt".into());
    }
    let store = crate::store::AtomicJsonStore::new(&paths.journal);
    if operation.phase < UpdatePhase::CandidateInstalled {
        save_update_phase(
            &store,
            operation,
            UpdatePhase::CandidateInstalled,
            UpdateBoundary::CandidateInstalledPersisted,
            hook,
        )?;
    }
    if operation.phase < UpdatePhase::CandidateReady {
        save_update_phase(
            &store,
            operation,
            UpdatePhase::CandidateReady,
            UpdateBoundary::CandidateReadyPersisted,
            hook,
        )?;
    }
    if operation.phase < UpdatePhase::Committed {
        save_update_phase(
            &store,
            operation,
            UpdatePhase::Committed,
            UpdateBoundary::CommittedPersisted,
            hook,
        )?;
    }
    Ok(())
}

fn rollback_update(
    operation: &UpdateOperation,
    paths: &OperationPaths,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    match installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    ) {
        InstalledImage::Current => {}
        InstalledImage::Missing => {
            if installed_image(
                &paths.backup,
                &operation.current_sha256,
                &operation.candidate_sha256,
            ) != InstalledImage::Current
            {
                return Err("last verified executable is unavailable".into());
            }
            move_update_file(&paths.backup, &paths.canonical)?;
            hook.after(UpdateBoundary::BackupRestored);
        }
        InstalledImage::Candidate => {
            if installed_image(
                &paths.backup,
                &operation.current_sha256,
                &operation.candidate_sha256,
            ) != InstalledImage::Current
            {
                return Err("last verified executable is unavailable".into());
            }
            if paths.candidate.exists() {
                if installed_image(
                    &paths.candidate,
                    &operation.current_sha256,
                    &operation.candidate_sha256,
                ) != InstalledImage::Candidate
                {
                    return Err("update candidate path is occupied".into());
                }
                remove_update_file(&paths.candidate)?;
            }
            move_update_file(&paths.canonical, &paths.candidate)?;
            hook.after(UpdateBoundary::CandidateReturned);
            move_update_file(&paths.backup, &paths.canonical)?;
            hook.after(UpdateBoundary::BackupRestored);
        }
        InstalledImage::Unknown => {
            return Err("canonical executable does not match the journal".into())
        }
    }
    if installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    ) != InstalledImage::Current
    {
        return Err("last verified executable could not be restored".into());
    }
    remove_update_file(&paths.candidate)?;
    remove_update_file(&paths.backup)?;
    remove_update_file(&paths.readiness_backup)?;
    remove_update_file(&paths.readiness)?;
    delete_update_journal(paths, hook)
}

trait WatchdogPlatform {
    fn spawn_candidate(
        &mut self,
        canonical: &Path,
        operation: &UpdateOperation,
    ) -> Result<u32, String>;
    fn await_readiness(
        &mut self,
        paths: &OperationPaths,
        operation: &UpdateOperation,
    ) -> Result<CandidateReadiness, String>;
    fn terminate_candidate(&mut self);
    fn restart_previous(&mut self, canonical: &Path) -> Result<(), String>;
}

struct WindowsWatchdog {
    child: Option<Child>,
}

impl WatchdogPlatform for WindowsWatchdog {
    fn spawn_candidate(
        &mut self,
        canonical: &Path,
        operation: &UpdateOperation,
    ) -> Result<u32, String> {
        let child = std::process::Command::new(canonical)
            .arg("--swap-wait")
            .arg("--update-attempt")
            .arg(&operation.attempt_id)
            .arg("--update-nonce")
            .arg(&operation.readiness_nonce)
            .spawn()
            .map_err(|_| "candidate spawn failed".to_string())?;
        let pid = child.id();
        self.child = Some(child);
        Ok(pid)
    }

    fn await_readiness(
        &mut self,
        paths: &OperationPaths,
        operation: &UpdateOperation,
    ) -> Result<CandidateReadiness, String> {
        let deadline = Instant::now() + READINESS_TIMEOUT;
        loop {
            if self
                .child
                .as_mut()
                .ok_or("candidate process is unavailable")?
                .try_wait()
                .map_err(|_| "candidate process status failed")?
                .is_some()
            {
                return Err("candidate exited before readiness".into());
            }
            if let Some(readiness) = load_authenticated_readiness(paths, operation)? {
                if self
                    .child
                    .as_mut()
                    .ok_or("candidate process is unavailable")?
                    .try_wait()
                    .map_err(|_| "candidate process status failed")?
                    .is_none()
                {
                    return Ok(readiness);
                }
                return Err("candidate exited before readiness commit".into());
            }
            if Instant::now() >= deadline {
                return Err("candidate readiness timed out".into());
            }
            std::thread::sleep(WATCHDOG_POLL);
        }
    }

    fn terminate_candidate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn restart_previous(&mut self, canonical: &Path) -> Result<(), String> {
        std::process::Command::new(canonical)
            .arg("--swap-wait")
            .spawn()
            .map(|_| ())
            .map_err(|_| "previous version restart failed".into())
    }
}

fn rollback_and_restart(
    primary: String,
    operation: &UpdateOperation,
    paths: &OperationPaths,
    platform: &mut impl WatchdogPlatform,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    platform.terminate_candidate();
    let rollback = rollback_update(operation, paths, hook);
    let restart = rollback
        .as_ref()
        .ok()
        .and_then(|()| platform.restart_previous(&paths.canonical).err());
    match (rollback, restart) {
        (Ok(()), None) => Err(format!("{primary} — previous version restored")),
        (Ok(()), Some(restart)) => Err(format!("{primary}; {restart}")),
        (Err(rollback), _) => Err(format!("{primary}; {rollback}")),
    }
}

fn supervise_candidate(
    operation: &mut UpdateOperation,
    paths: &OperationPaths,
    platform: &mut impl WatchdogPlatform,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    let pid = match platform.spawn_candidate(&paths.canonical, operation) {
        Ok(pid) => pid,
        Err(error) => return rollback_and_restart(error, operation, paths, platform, hook),
    };
    if let Err(error) = save_candidate_pid(paths, operation, pid, hook) {
        return rollback_and_restart(error, operation, paths, platform, hook);
    }
    let readiness = match platform.await_readiness(paths, operation) {
        Ok(readiness) => readiness,
        Err(error) => return rollback_and_restart(error, operation, paths, platform, hook),
    };
    if let Err(error) = commit_candidate(operation, paths, &readiness, hook) {
        return rollback_and_restart(error, operation, paths, platform, hook);
    }
    Ok(())
}

fn update_argument<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    let positions = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == name).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err("update invocation is malformed".into());
    }
    let Some(index) = positions.first().copied() else {
        return Ok(None);
    };
    let value = args
        .get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .ok_or("update invocation is malformed")?;
    Ok(Some(value))
}

fn update_invocation(args: &[String]) -> Result<Option<(&str, &str)>, String> {
    match (
        update_argument(args, "--update-attempt")?,
        update_argument(args, "--update-nonce")?,
    ) {
        (None, None) => Ok(None),
        (Some(attempt), Some(nonce)) => Ok(Some((attempt, nonce))),
        _ => Err("update invocation is incomplete".into()),
    }
}

pub fn run_watchdog_if_requested(args: &[String]) -> Option<Result<(), String>> {
    if !args.iter().any(|arg| arg == "--update-watchdog") {
        return None;
    }
    Some(run_watchdog(args))
}

fn run_watchdog(args: &[String]) -> Result<(), String> {
    let (attempt, nonce) = update_invocation(args)?.ok_or("watchdog invocation is incomplete")?;
    let running_exe = std::env::current_exe().map_err(|_| "can't locate watchdog executable")?;
    let directory = running_exe.parent().ok_or("can't locate exe folder")?;
    let mut operation = load_update_operation(directory)?.ok_or("update journal is missing")?;
    let paths = operation_paths(directory, &operation)?;
    if attempt != operation.attempt_id
        || nonce != operation.readiness_nonce
        || operation.phase != UpdatePhase::CandidateInstalled
        || !paths_equal(&running_exe, &paths.backup)
        || installed_image(
            &paths.backup,
            &operation.current_sha256,
            &operation.candidate_sha256,
        ) != InstalledImage::Current
        || installed_image(
            &paths.canonical,
            &operation.current_sha256,
            &operation.candidate_sha256,
        ) != InstalledImage::Candidate
        || file_version(&paths.canonical)
            != crate::release_manifest::strict_version(&operation.candidate_version)
    {
        return Err("watchdog update identity is invalid".into());
    }
    remove_update_file(&paths.readiness_backup)?;
    remove_update_file(&paths.readiness)?;
    supervise_candidate(
        &mut operation,
        &paths,
        &mut WindowsWatchdog { child: None },
        &mut NoUpdateHook,
    )
}

fn recover_update(running_exe: &Path, hook: &mut impl UpdateBoundaryHook) -> Result<(), String> {
    let directory = running_exe.parent().ok_or("can't locate exe folder")?;
    let Some(operation) = load_update_operation(directory)? else {
        return Ok(());
    };
    let paths = operation_paths(directory, &operation)?;
    if !paths_equal(running_exe, &paths.canonical) && !paths_equal(running_exe, &paths.backup) {
        return Err("update journal is not bound to this executable".into());
    }
    require_portable(&paths.canonical)?;
    let canonical_image = installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    );
    let running_candidate =
        paths_equal(running_exe, &paths.canonical) && canonical_image == InstalledImage::Candidate;

    if operation.phase == UpdatePhase::Committed {
        if canonical_image == InstalledImage::Candidate {
            return finish_committed(&paths, hook);
        }
        return rollback_update(&operation, &paths, hook);
    }
    if running_candidate && paths_equal(running_exe, &paths.canonical) {
        return Err("candidate cannot commit without authenticated readiness".into());
    }
    rollback_update(&operation, &paths, hook)
}

pub struct StartupGuard {
    mode: StartupMode,
}

enum StartupMode {
    Normal,
    Candidate(Box<CandidateStartup>),
    Cleanup { paths: Box<OperationPaths> },
}

struct CandidateStartup {
    operation: UpdateOperation,
    paths: OperationPaths,
    running_exe: PathBuf,
}

impl StartupGuard {
    pub fn compatibility_mode(&self) -> bool {
        matches!(self.mode, StartupMode::Candidate(_))
    }
}

pub fn prepare_startup(args: &[String]) -> Result<StartupGuard, String> {
    let running_exe = std::env::current_exe().map_err(|_| "can't locate exe")?;
    prepare_startup_at(&running_exe, args, &mut NoUpdateHook)
}

fn prepare_startup_at(
    running_exe: &Path,
    args: &[String],
    hook: &mut impl UpdateBoundaryHook,
) -> Result<StartupGuard, String> {
    let invocation = update_invocation(args)?;
    let directory = running_exe.parent().ok_or("can't locate exe folder")?;
    let Some(operation) = load_update_operation(directory)? else {
        if invocation.is_some() {
            return Err("stale update invocation rejected".into());
        }
        return Ok(StartupGuard {
            mode: StartupMode::Normal,
        });
    };
    let paths = operation_paths(directory, &operation)?;
    require_portable(&paths.canonical)?;
    let canonical_image = installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    );

    if operation.phase == UpdatePhase::Committed {
        if invocation.is_some() {
            return Err("stale update invocation rejected".into());
        }
        if paths_equal(running_exe, &paths.canonical)
            && canonical_image == InstalledImage::Candidate
        {
            return Ok(StartupGuard {
                mode: StartupMode::Cleanup {
                    paths: Box::new(paths),
                },
            });
        }
        rollback_update(&operation, &paths, hook)?;
        return Ok(StartupGuard {
            mode: StartupMode::Normal,
        });
    }

    let running_candidate =
        paths_equal(running_exe, &paths.canonical) && canonical_image == InstalledImage::Candidate;
    if running_candidate {
        let Some((attempt, nonce)) = invocation else {
            rollback_update(&operation, &paths, hook)?;
            std::process::Command::new(&paths.canonical)
                .arg("--swap-wait")
                .spawn()
                .map_err(|_| "previous version restart failed")?;
            return Err("candidate update identity is missing — previous version restored".into());
        };
        let expected_version =
            crate::release_manifest::strict_version(&operation.candidate_version);
        if operation.phase != UpdatePhase::CandidateInstalled
            || operation.candidate_pid != Some(std::process::id())
            || attempt != operation.attempt_id
            || nonce != operation.readiness_nonce
            || file_version(running_exe) != expected_version
            || sha256_of(running_exe).as_deref() != Some(&operation.candidate_sha256)
        {
            rollback_update(&operation, &paths, hook)?;
            std::process::Command::new(&paths.canonical)
                .arg("--swap-wait")
                .spawn()
                .map_err(|_| "previous version restart failed")?;
            return Err("candidate update identity is invalid — previous version restored".into());
        }
        return Ok(StartupGuard {
            mode: StartupMode::Candidate(Box::new(CandidateStartup {
                operation,
                paths,
                running_exe: running_exe.to_path_buf(),
            })),
        });
    }

    if invocation.is_some() {
        return Err("update invocation is not bound to the candidate".into());
    }
    rollback_update(&operation, &paths, hook)?;
    Ok(StartupGuard {
        mode: StartupMode::Normal,
    })
}

pub fn complete_startup(guard: StartupGuard) -> Result<(), String> {
    complete_startup_with_hook(guard, &mut NoUpdateHook)
}

fn complete_startup_with_hook(
    guard: StartupGuard,
    hook: &mut impl UpdateBoundaryHook,
) -> Result<(), String> {
    match guard.mode {
        StartupMode::Normal => {}
        StartupMode::Cleanup { paths } => finish_committed(&paths, hook)?,
        StartupMode::Candidate(candidate) => {
            let CandidateStartup {
                operation,
                paths,
                running_exe,
            } = *candidate;
            let readiness = CandidateReadiness {
                schema_version: READINESS_SCHEMA,
                attempt_id: operation.attempt_id.clone(),
                nonce: operation.readiness_nonce.clone(),
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                sha256: sha256_of(&running_exe)
                    .ok_or("candidate readiness hash could not be verified")?,
            };
            if !readiness_matches(&readiness, &operation) {
                return Err("candidate readiness identity is invalid".into());
            }
            persist_candidate_readiness(&paths, &readiness, hook)?;

            let deadline = Instant::now() + COMMIT_TIMEOUT;
            loop {
                let committed =
                    load_update_operation(running_exe.parent().ok_or("can't locate exe folder")?)?
                        .is_some_and(|current| {
                            current.phase == UpdatePhase::Committed
                                && current.attempt_id == operation.attempt_id
                                && current.readiness_nonce == operation.readiness_nonce
                                && current.candidate_pid == operation.candidate_pid
                                && current.candidate_sha256 == operation.candidate_sha256
                                && current.candidate_version == operation.candidate_version
                        });
                if committed {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("update commit acknowledgement timed out".into());
                }
                std::thread::sleep(WATCHDOG_POLL);
            }
        }
    }
    cleanup_legacy_backup();
    Ok(())
}

fn cleanup_legacy_backup() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(directory) = exe.parent() else {
        return;
    };
    let legacy_backup = directory.join("claudometer.old.exe");
    if legacy_backup.exists() {
        for _ in 0..10 {
            if std::fs::remove_file(&legacy_backup).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

// ---------- install ----------

/// User clicked Install. Runs in a worker thread; progress lands in `Status`
/// and repaints via WM_UPDATE. On success the thread posts WM_UPDATE with
/// wparam 1 — the UI thread quits and the freshly spawned exe takes over.
pub fn install() {
    let Status::Available(rel) = status() else {
        return;
    };
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    set_status(Status::Installing);
    std::thread::spawn(move || {
        let r = install_inner(&rel);
        BUSY.store(false, Ordering::SeqCst);
        match r {
            Ok(()) => {
                let h = crate::MAIN_HWND.load(Ordering::SeqCst);
                unsafe {
                    let _ = PostMessageW(HWND(h as *mut _), crate::WM_UPDATE, WPARAM(1), LPARAM(0));
                }
            }
            Err(msg) => {
                set_status(Status::Failed(msg, Some(rel.page_url.clone())));
            }
        }
    });
}

fn install_inner(rel: &Release) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|_| "can't locate exe")?;
    require_portable(&exe)?;
    recover_update(&exe, &mut NoUpdateHook)?;
    if rel.expires_at <= OffsetDateTime::now_utc() {
        return Err("release policy expired — check for updates again".into());
    }
    let dir = exe.parent().ok_or("can't locate exe folder")?;
    let attempt_id = new_attempt_id()?;
    let readiness_nonce = new_readiness_nonce()?;
    let current_sha256 = sha256_of(&exe).ok_or("couldn't verify current executable")?;
    let mut operation = UpdateOperation::new(
        &exe,
        attempt_id,
        readiness_nonce,
        current_sha256,
        rel.asset_sha256.clone(),
        rel.version,
    )?;
    let paths = operation_paths(dir, &operation)?;
    if paths.candidate.exists() || paths.backup.exists() {
        return Err("unique update paths are already occupied".into());
    }

    let agent = agent(180).ok_or("TLS init failed")?;
    let checksum = get_bounded(&agent, &rel.sha_url, MAX_CHECKSUM_BYTES)
        .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "hash asset is not UTF-8".into()))?;
    let expected = parse_checksum(&checksum, &rel.asset_name)
        .ok_or_else(|| "hash asset malformed".to_string())?;
    if expected != rel.asset_sha256 {
        return Err("checksum does not match signed manifest".into());
    }
    let candidate = get_exact(&agent, &rel.exe_url, rel.asset_size)
        .map_err(|error| format!("download failed: {error}"))?;
    validate_candidate_bytes(&candidate, rel)?;
    if rel.expires_at <= OffsetDateTime::now_utc() {
        return Err("release policy expired during download".into());
    }

    let cleanup_candidate = |msg: &str| -> String {
        let _ = std::fs::remove_file(&paths.candidate);
        msg.to_string()
    };
    write_candidate(&paths.candidate, &candidate, rel)?;
    if file_version(&paths.candidate) != Some(rel.version) {
        return Err(cleanup_candidate("downloaded exe version mismatch"));
    }
    if sha256_of(&paths.candidate).as_deref() != Some(&rel.asset_sha256) {
        return Err(cleanup_candidate("written executable hash mismatch"));
    }
    require_portable(&exe).map_err(|message| cleanup_candidate(&message))?;
    if let Err(error) = start_handover(dir, &mut operation, &mut NoUpdateHook) {
        return match rollback_update(&operation, &paths, &mut NoUpdateHook) {
            Ok(()) => Err(error),
            Err(recovery) => Err(format!("{error}; {recovery}")),
        };
    }

    if installed_image(
        &paths.canonical,
        &operation.current_sha256,
        &operation.candidate_sha256,
    ) != InstalledImage::Candidate
        || installed_image(
            &paths.backup,
            &operation.current_sha256,
            &operation.candidate_sha256,
        ) != InstalledImage::Current
        || file_version(&paths.canonical) != Some(rel.version)
    {
        return match rollback_update(&operation, &paths, &mut NoUpdateHook) {
            Ok(()) => {
                Err("installed candidate verification failed — previous version restored".into())
            }
            Err(recovery) => Err(format!(
                "installed candidate verification failed; {recovery}"
            )),
        };
    }

    // The preserved old executable supervises candidate spawn, readiness,
    // commit, and rollback after this UI process exits and releases its mutex.
    if std::process::Command::new(&paths.backup)
        .arg("--update-watchdog")
        .arg("--update-attempt")
        .arg(&operation.attempt_id)
        .arg("--update-nonce")
        .arg(&operation.readiness_nonce)
        .spawn()
        .is_err()
    {
        return match rollback_update(&operation, &paths, &mut NoUpdateHook) {
            Ok(()) => Err("watchdog spawn failed — previous version restored".into()),
            Err(recovery) => Err(format!("watchdog spawn failed; {recovery}")),
        };
    }
    Ok(())
}

fn validate_candidate_bytes(candidate: &[u8], rel: &Release) -> Result<(), String> {
    if candidate.len() as u64 != rel.asset_size {
        return Err("download size mismatch".into());
    }
    if candidate.get(..2) != Some(b"MZ") {
        return Err("downloaded file is not an exe".into());
    }
    let actual = sha256_reader(std::io::Cursor::new(candidate)).ok_or("hashing failed")?;
    if actual != rel.asset_sha256 {
        return Err("SHA256 mismatch".into());
    }
    Ok(())
}

fn write_candidate(path: &Path, candidate: &[u8], rel: &Release) -> Result<(), String> {
    validate_candidate_bytes(candidate, rel)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "candidate path is not uniquely writable")?;
    if file
        .write_all(candidate)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err("write failed".into());
    }
    Ok(())
}

fn require_portable(exe: &Path) -> Result<(), String> {
    match install_channel(exe) {
        InstallChannel::Portable => {}
        InstallChannel::Managed => {
            return Err("managed install — use its signed installer or package manager".into())
        }
        InstallChannel::Ambiguous => {
            return Err("install channel could not be verified — use the signed installer".into())
        }
    }
    Ok(())
}

fn get_bounded(agent: &ureq::Agent, url: &str, max: u64) -> Result<Vec<u8>, String> {
    let resp = restricted_get(agent, url, crate::network::GITHUB_RELEASE_HOSTS, None)?;
    if resp
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max)
    {
        return Err("download too large".into());
    }
    read_bounded(resp.into_reader(), max)
}

fn get_exact(agent: &ureq::Agent, url: &str, expected: u64) -> Result<Vec<u8>, String> {
    let resp = restricted_get(agent, url, crate::network::GITHUB_RELEASE_HOSTS, None)?;
    if resp
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length != expected)
    {
        return Err("download size mismatch".into());
    }
    let bytes = read_bounded(resp.into_reader(), expected)?;
    if bytes.len() as u64 != expected {
        return Err("download size mismatch".into());
    }
    Ok(bytes)
}

fn restricted_get(
    agent: &ureq::Agent,
    url: &str,
    allowed_hosts: &[&str],
    accept: Option<&str>,
) -> Result<ureq::Response, String> {
    let mut current = url.to_string();
    for redirects in 0..=MAX_REDIRECTS {
        let parsed = agent
            .get(&current)
            .request_url()
            .map_err(|_| "invalid URL")?;
        if !allowed_url(&parsed, allowed_hosts) {
            return Err("URL destination not allowed".into());
        }

        let mut request = crate::network::get(agent, &current).set("User-Agent", UA);
        if let Some(value) = accept {
            request = request.set("Accept", value);
        }
        let response = request.call().map_err(|error| match error {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            _ => "network error".to_string(),
        })?;
        if (200..300).contains(&response.status()) {
            return Ok(response);
        }
        if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            return Err(format!("unexpected HTTP {}", response.status()));
        }
        if redirects == MAX_REDIRECTS {
            return Err("too many redirects".into());
        }
        let location = response
            .header("Location")
            .ok_or("redirect missing location")?;
        current = parsed
            .as_url()
            .join(location)
            .map_err(|_| "invalid redirect URL")?
            .to_string();
    }
    unreachable!()
}

fn allowed_url(url: &ureq::RequestUrl, allowed_hosts: &[&str]) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.as_url().username().is_empty()
        && url.as_url().password().is_none()
        && url.as_url().fragment().is_none()
        && allowed_hosts
            .iter()
            .any(|host| url.host().eq_ignore_ascii_case(host))
}

fn read_string_bounded(reader: impl Read, max: u64) -> Result<String, String> {
    let bytes = read_bounded(reader, max)?;
    String::from_utf8(bytes).map_err(|_| "response is not UTF-8".into())
}

fn read_bounded(reader: impl Read, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "read failed")?;
    if bytes.len() as u64 > max {
        return Err("response too large".into());
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallChannel {
    Portable,
    Managed,
    Ambiguous,
}

#[derive(Debug, PartialEq, Eq)]
enum ChannelMarker {
    Missing,
    Managed,
    Invalid,
}

fn install_channel(exe: &Path) -> InstallChannel {
    let Some(dir) = exe.parent() else {
        return InstallChannel::Ambiguous;
    };
    let marker = read_channel_marker(dir);
    let Some(local_appdata) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return InstallChannel::Ambiguous;
    };
    classify_install_channel(
        dir,
        &local_appdata.join("Programs").join("Claudometer"),
        marker,
        || read_registered_install_location().ok().flatten(),
    )
}

fn classify_install_channel(
    exe_dir: &Path,
    managed_root: &Path,
    marker: ChannelMarker,
    registered_location: impl FnOnce() -> Option<PathBuf>,
) -> InstallChannel {
    let at_managed_root = paths_equal(exe_dir, managed_root);
    if marker == ChannelMarker::Missing && !at_managed_root {
        return InstallChannel::Portable;
    }
    if marker != ChannelMarker::Managed || !at_managed_root {
        return InstallChannel::Ambiguous;
    }
    match registered_location() {
        Some(location) if paths_equal(exe_dir, &location) => InstallChannel::Managed,
        _ => InstallChannel::Ambiguous,
    }
}

fn read_channel_marker(dir: &Path) -> ChannelMarker {
    let path = dir.join(CHANNEL_MARKER);
    match std::fs::File::open(path) {
        Ok(file) => match read_string_bounded(file, 32) {
            Ok(value) if value == MANAGED_CHANNEL => ChannelMarker::Managed,
            _ => ChannelMarker::Invalid,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ChannelMarker::Missing,
        Err(_) => ChannelMarker::Invalid,
    }
}

fn read_registered_install_location() -> Result<Option<PathBuf>, ()> {
    unsafe {
        let mut bytes = 0u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            UNINSTALL_KEY,
            windows::core::w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        );
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if status.is_err() || bytes == 0 || bytes > 64 * 1024 {
            return Err(());
        }
        let mut value = vec![0u16; bytes.div_ceil(2) as usize];
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            UNINSTALL_KEY,
            windows::core::w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        );
        if status.is_err() {
            return Err(());
        }
        let length = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        let location = String::from_utf16(&value[..length]).map_err(|_| ())?;
        if location.is_empty() {
            return Err(());
        }
        Ok(Some(PathBuf::from(location)))
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    normalize(left).eq_ignore_ascii_case(&normalize(right))
}

pub fn open_url(url: &str) {
    unsafe {
        let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        windows::Win32::UI::Shell::ShellExecuteW(
            HWND::default(),
            windows::core::w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

// ---------- verification helpers ----------

/// (major, minor, patch) from the exe's VERSIONINFO resource.
fn file_version(path: &Path) -> Option<(u16, u16, u16)> {
    unsafe {
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, buf.as_mut_ptr() as *mut _).ok()?;
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        if !VerQueryValueW(
            buf.as_ptr() as *const _,
            windows::core::w!("\\"),
            &mut ptr,
            &mut len,
        )
        .as_bool()
            || ptr.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*(ptr as *const VS_FIXEDFILEINFO);
        if info.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some((
            (info.dwFileVersionMS >> 16) as u16,
            (info.dwFileVersionMS & 0xFFFF) as u16,
            (info.dwFileVersionLS >> 16) as u16,
        ))
    }
}

fn sha256_of(path: &Path) -> Option<String> {
    sha256_reader(std::fs::File::open(path).ok()?)
}

fn sha256_reader(mut reader: impl Read) -> Option<String> {
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        if !BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .is_ok()
        {
            return None;
        }

        let result = (|| {
            let mut hash = BCRYPT_HASH_HANDLE::default();
            if !BCryptCreateHash(algorithm, &mut hash, None, None, 0).is_ok() {
                return None;
            }
            let digest = (|| {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let read = reader.read(&mut buffer).ok()?;
                    if read == 0 {
                        break;
                    }
                    if !BCryptHashData(hash, &buffer[..read], 0).is_ok() {
                        return None;
                    }
                }
                let mut digest = [0u8; 32];
                BCryptFinishHash(hash, &mut digest, 0)
                    .is_ok()
                    .then_some(digest)
            })();
            let _ = BCryptDestroyHash(hash);
            digest
        })();
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);

        let mut hex = String::with_capacity(64);
        for byte in result? {
            write!(&mut hex, "{byte:02x}").ok()?;
        }
        Some(hex)
    }
}

fn parse_checksum(contents: &str, asset_name: &str) -> Option<String> {
    let mut fields = contents.split_whitespace();
    let digest = fields.next()?;
    let filename = fields.next()?;
    if fields.next().is_some()
        || digest.len() != 64
        || !digest.chars().all(|ch| ch.is_ascii_hexdigit())
        || filename != format!("*{asset_name}")
    {
        return None;
    }
    Some(digest.to_ascii_lowercase())
}

fn decode_lower_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut output = [0u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let nibble = |value| match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            _ => None,
        };
        *byte =
            nibble(value.as_bytes()[index * 2])? << 4 | nibble(value.as_bytes()[index * 2 + 1])?;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_checks_require_opt_in_and_respect_daily_gate() {
        assert!(!check_due(false, None));
        assert!(!check_due(false, Some(CHECK_EVERY)));
        assert!(check_due(true, None));
        assert!(!check_due(true, Some(CHECK_EVERY - Duration::from_secs(1))));
        assert!(check_due(true, Some(CHECK_EVERY)));
    }

    #[test]
    fn cng_sha256_matches_known_digest() {
        assert_eq!(
            sha256_reader(std::io::Cursor::new(b"abc")).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn checksum_requires_exact_digest_and_asset_name() {
        let digest = "D2B2F2A1C3E4556677889900AABBCCDDEEFF00112233445566778899AABBCCDD";
        assert_eq!(
            parse_checksum(
                &format!("{digest} *claudometer-v9.9.9-windows-x64.exe"),
                "claudometer-v9.9.9-windows-x64.exe"
            )
            .as_deref(),
            Some("d2b2f2a1c3e4556677889900aabbccddeeff00112233445566778899aabbccdd")
        );
        assert!(parse_checksum(digest, "asset.exe").is_none());
        assert!(parse_checksum(&format!("{digest} *other.exe"), "asset.exe").is_none());
        assert!(parse_checksum(&format!("{digest} *asset.exe extra"), "asset.exe").is_none());
    }

    #[test]
    fn release_metadata_builds_fixed_manifest_urls_and_requires_signature() {
        let stem = format!("claudometer-v9.9.9-windows-{RELEASE_ARCH}");
        let manifest = format!("{stem}.manifest.json");
        let signatures = format!("{stem}.manifest.signatures.json");
        let rel = ApiRelease {
            tag_name: Some("v9.9.9".into()),
            draft: Some(false),
            prerelease: Some(false),
            assets: Some(vec![
                ApiAsset {
                    name: Some(manifest.clone()),
                },
                ApiAsset {
                    name: Some(signatures.clone()),
                },
            ]),
        };
        let picked = pick_manifest_assets(&rel).expect("complete manifest assets picked");
        assert_eq!(picked.tag, "v9.9.9");
        assert_eq!(
            picked.manifest_url,
            format!("https://github.com/Dvaderfun/claudometer/releases/download/v9.9.9/{manifest}")
        );
        assert_eq!(
            picked.signatures_url,
            format!(
                "https://github.com/Dvaderfun/claudometer/releases/download/v9.9.9/{signatures}"
            )
        );

        let missing_signature = ApiRelease {
            assets: Some(vec![ApiAsset {
                name: Some(manifest.clone()),
            }]),
            ..rel
        };
        assert!(pick_manifest_assets(&missing_signature).is_none());

        let incomplete = ApiRelease {
            draft: None,
            ..missing_signature
        };
        assert!(pick_manifest_assets(&incomplete).is_none());

        let noncanonical_tag = ApiRelease {
            tag_name: Some("9.9.9".into()),
            draft: Some(false),
            assets: Some(vec![
                ApiAsset {
                    name: Some(manifest),
                },
                ApiAsset {
                    name: Some(signatures),
                },
            ]),
            ..incomplete
        };
        assert!(pick_manifest_assets(&noncanonical_tag).is_none());

        let prerelease = ApiRelease {
            prerelease: Some(true),
            ..noncanonical_tag
        };
        assert!(pick_manifest_assets(&prerelease).is_none());
    }

    #[test]
    fn unprovisioned_updater_cannot_accept_even_a_valid_manifest() {
        use ed25519_dalek::{Signer, SigningKey};
        use serde_json::json;
        use time::macros::datetime;

        let current_version = crate::release_manifest::strict_version(env!("CARGO_PKG_VERSION"))
            .expect("package version is canonical");
        let target_version = format!(
            "{}.{}.{}",
            current_version.0,
            current_version.1,
            current_version.2 + 1
        );
        let key = SigningKey::from_bytes(&[42; 32]);
        let key_hex: String = key
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let manifest = serde_json::to_vec(&json!({
            "schema": crate::release_manifest::MANIFEST_SCHEMA,
            "channel": crate::release_manifest::RELEASE_CHANNEL,
            "sequence": 2,
            "version": target_version,
            "tag": format!("v{target_version}"),
            "issued_at": "2026-09-03T11:00:00Z",
            "policy_expires_at": "2026-09-10T11:00:00Z",
            "architecture": RELEASE_ARCH,
            "asset": format!("claudometer-v{target_version}-windows-{RELEASE_ARCH}.exe"),
            "size": 123456,
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "minimum_updater_version": env!("CARGO_PKG_VERSION")
        }))
        .unwrap();
        let signatures = serde_json::to_vec(&json!({
            "schema": crate::release_manifest::SIGNATURE_SCHEMA,
            "signatures": [{
                "public_key": key_hex,
                "signature": key.sign(&manifest).to_bytes().iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            }]
        }))
        .unwrap();
        let now = datetime!(2026-09-03 12:00 UTC);

        assert!(verify_release_manifest(
            &manifest,
            &signatures,
            None,
            Some(&key_hex),
            Some("1"),
            current_version,
            now,
        )
        .is_ok());
        assert_eq!(
            verify_release_manifest(
                &manifest,
                &signatures,
                None,
                None,
                None,
                current_version,
                now,
            )
            .unwrap_err(),
            "release trust root not provisioned"
        );
        assert_eq!(
            verify_release_manifest(
                &manifest,
                &signatures,
                None,
                Some(&key_hex),
                Some("0"),
                current_version,
                now,
            )
            .unwrap_err(),
            "release sequence not provisioned"
        );
    }

    #[test]
    fn request_urls_require_https_and_allowlisted_exact_hosts() {
        let allowed = |url: &str| {
            let parsed = ureq::get(url).request_url().unwrap();
            allowed_url(&parsed, crate::network::GITHUB_RELEASE_HOSTS)
        };
        assert!(allowed(
            "https://github.com/owner/repo/releases/download/v1/asset"
        ));
        assert!(allowed(
            "https://release-assets.githubusercontent.com/object?token=x"
        ));
        assert!(!allowed("http://github.com/owner/repo"));
        assert!(!allowed("https://github.com.evil.example/owner/repo"));
        assert!(!allowed("https://user@github.com/owner/repo"));
        assert!(!allowed("https://github.com:444/owner/repo"));
        assert!(!allowed("https://github.com/owner/repo#fragment"));
    }

    #[test]
    fn bounded_reads_reject_max_plus_one_without_reading_further() {
        assert_eq!(
            read_bounded(std::io::Cursor::new(b"1234"), 4),
            Ok(b"1234".to_vec())
        );
        assert!(read_bounded(std::io::Cursor::new(b"123456"), 4).is_err());
        assert!(read_string_bounded(std::io::Cursor::new(b"12345"), 4).is_err());
    }

    #[test]
    fn candidate_file_is_created_only_after_signed_byte_checks_pass() {
        let candidate = b"MZsigned candidate";
        let digest = sha256_reader(std::io::Cursor::new(candidate)).unwrap();
        let mut release = Release {
            tag: "v9.9.9".into(),
            version: (9, 9, 9),
            expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
            asset_name: format!("claudometer-v9.9.9-windows-{RELEASE_ARCH}.exe"),
            asset_size: candidate.len() as u64,
            asset_sha256: digest,
            exe_url: String::new(),
            sha_url: String::new(),
            page_url: String::new(),
        };
        let path =
            std::env::temp_dir().join(format!("claudometer-candidate-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);

        release.asset_size += 1;
        assert!(write_candidate(&path, candidate, &release).is_err());
        assert!(!path.exists());

        release.asset_size -= 1;
        release.asset_sha256 = "00".repeat(32);
        assert!(write_candidate(&path, candidate, &release).is_err());
        assert!(!path.exists());

        release.asset_sha256 = sha256_reader(std::io::Cursor::new(candidate)).unwrap();
        write_candidate(&path, candidate, &release).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), candidate);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn install_channel_requires_all_managed_signals_to_agree() {
        let exe_dir = Path::new(r"C:\Users\me\AppData\Local\Programs\Claudometer");
        let same_with_case = PathBuf::from(r"c:\users\me\appdata\local\programs\claudometer\");
        assert_eq!(
            classify_install_channel(exe_dir, exe_dir, ChannelMarker::Managed, || Some(
                same_with_case
            )),
            InstallChannel::Managed
        );
        assert_eq!(
            classify_install_channel(exe_dir, exe_dir, ChannelMarker::Missing, || Some(
                exe_dir.into()
            )),
            InstallChannel::Ambiguous
        );
        assert_eq!(
            classify_install_channel(
                Path::new(r"D:\Tools"),
                exe_dir,
                ChannelMarker::Missing,
                || panic!("portable classification must not inspect managed registration")
            ),
            InstallChannel::Portable
        );
        assert_eq!(
            classify_install_channel(
                Path::new(r"D:\Tools"),
                exe_dir,
                ChannelMarker::Managed,
                || Some(PathBuf::from(r"D:\Tools"))
            ),
            InstallChannel::Ambiguous
        );
    }

    static NEXT_UPDATE_TEST_DIRECTORY: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);

    struct UpdateTestDirectory(PathBuf);

    impl UpdateTestDirectory {
        fn new() -> Self {
            let unique = NEXT_UPDATE_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-update-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn setup(&self) -> (UpdateOperation, OperationPaths) {
            let canonical = self.0.join("app.exe");
            std::fs::write(&canonical, b"verified current executable").unwrap();
            let operation = UpdateOperation::new(
                &canonical,
                "11".repeat(ATTEMPT_ID_BYTES),
                "22".repeat(READINESS_NONCE_BYTES),
                sha256_of(&canonical).unwrap(),
                sha256_reader(std::io::Cursor::new(b"verified candidate executable")).unwrap(),
                (9, 9, 9),
            )
            .unwrap();
            let paths = operation_paths(&self.0, &operation).unwrap();
            std::fs::write(&paths.candidate, b"verified candidate executable").unwrap();
            (operation, paths)
        }
    }

    impl Drop for UpdateTestDirectory {
        fn drop(&mut self) {
            let temp = std::env::temp_dir();
            assert!(self.0.starts_with(&temp));
            assert!(self.0.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("claudometer-update-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct CrashAfter(UpdateBoundary);

    impl UpdateBoundaryHook for CrashAfter {
        fn after(&mut self, boundary: UpdateBoundary) {
            if boundary == self.0 {
                panic!("injected crash after {boundary:?}");
            }
        }
    }

    fn assert_recovery_complete(paths: &OperationPaths, expected: &[u8]) {
        assert_eq!(std::fs::read(&paths.canonical).unwrap(), expected);
        assert!(!paths.candidate.exists());
        assert!(!paths.backup.exists());
        assert!(!paths.journal.exists());
        assert!(!paths.journal_backup.exists());
        assert!(!paths.readiness.exists());
        assert!(!paths.readiness_backup.exists());
    }

    fn matching_readiness(operation: &UpdateOperation) -> CandidateReadiness {
        CandidateReadiness {
            schema_version: READINESS_SCHEMA,
            attempt_id: operation.attempt_id.clone(),
            nonce: operation.readiness_nonce.clone(),
            pid: operation.candidate_pid.unwrap(),
            version: operation.candidate_version.clone(),
            sha256: operation.candidate_sha256.clone(),
        }
    }

    #[derive(Clone, Copy)]
    enum FakeReadiness {
        Ready,
        Crash,
        Timeout,
    }

    struct FakeWatchdog {
        spawn_fails: bool,
        readiness: FakeReadiness,
        terminated: bool,
        restarted: bool,
    }

    impl FakeWatchdog {
        fn ready() -> Self {
            Self {
                spawn_fails: false,
                readiness: FakeReadiness::Ready,
                terminated: false,
                restarted: false,
            }
        }
    }

    impl WatchdogPlatform for FakeWatchdog {
        fn spawn_candidate(
            &mut self,
            _canonical: &Path,
            _operation: &UpdateOperation,
        ) -> Result<u32, String> {
            if self.spawn_fails {
                Err("injected candidate spawn failure".into())
            } else {
                Ok(4242)
            }
        }

        fn await_readiness(
            &mut self,
            _paths: &OperationPaths,
            operation: &UpdateOperation,
        ) -> Result<CandidateReadiness, String> {
            match self.readiness {
                FakeReadiness::Ready => Ok(matching_readiness(operation)),
                FakeReadiness::Crash => Err("injected candidate crash".into()),
                FakeReadiness::Timeout => Err("injected readiness timeout".into()),
            }
        }

        fn terminate_candidate(&mut self) {
            self.terminated = true;
        }

        fn restart_previous(&mut self, _canonical: &Path) -> Result<(), String> {
            self.restarted = true;
            Ok(())
        }
    }

    #[test]
    fn every_pre_ready_journal_write_and_rename_boundary_recovers() {
        for boundary in [
            UpdateBoundary::VerifiedPersisted,
            UpdateBoundary::CurrentRenamed,
            UpdateBoundary::CurrentMovedPersisted,
            UpdateBoundary::CandidateRenamed,
            UpdateBoundary::CandidateInstalledPersisted,
        ] {
            let directory = UpdateTestDirectory::new();
            let (mut operation, paths) = directory.setup();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                start_handover(&directory.0, &mut operation, &mut CrashAfter(boundary)).unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            assert!(
                [&paths.canonical, &paths.backup]
                    .iter()
                    .any(|path| std::fs::read(path)
                        .is_ok_and(|bytes| bytes == b"verified current executable")),
                "last verified executable lost at {boundary:?}"
            );

            let running = if paths.backup.exists() {
                &paths.backup
            } else {
                &paths.canonical
            };
            recover_update(running, &mut NoUpdateHook).unwrap();
            assert_recovery_complete(&paths, b"verified current executable");
        }
    }

    #[test]
    fn candidate_ready_and_committed_writes_are_retryable() {
        for boundary in [
            UpdateBoundary::CandidateReadyPersisted,
            UpdateBoundary::CommittedPersisted,
        ] {
            let directory = UpdateTestDirectory::new();
            let (mut operation, paths) = directory.setup();
            start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();
            save_candidate_pid(&paths, &mut operation, 4242, &mut NoUpdateHook).unwrap();
            let readiness = matching_readiness(&operation);

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                commit_candidate(
                    &mut operation,
                    &paths,
                    &readiness,
                    &mut CrashAfter(boundary),
                )
                .unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            assert_eq!(
                std::fs::read(&paths.backup).unwrap(),
                b"verified current executable",
                "the last verified executable must survive until commit"
            );

            commit_candidate(&mut operation, &paths, &readiness, &mut NoUpdateHook).unwrap();
            assert!(
                paths.backup.exists(),
                "cleanup is deferred to a later launch"
            );
            finish_committed(&paths, &mut NoUpdateHook).unwrap();
            assert_recovery_complete(&paths, b"verified candidate executable");
        }
    }

    #[test]
    fn watchdog_spawn_failure_and_timeout_restore_and_restart_previous() {
        for (spawn_fails, readiness) in [
            (true, FakeReadiness::Ready),
            (false, FakeReadiness::Crash),
            (false, FakeReadiness::Timeout),
        ] {
            let directory = UpdateTestDirectory::new();
            let (mut operation, paths) = directory.setup();
            start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();
            let mut platform = FakeWatchdog {
                spawn_fails,
                readiness,
                terminated: false,
                restarted: false,
            };

            assert!(
                supervise_candidate(&mut operation, &paths, &mut platform, &mut NoUpdateHook)
                    .is_err()
            );
            assert!(platform.terminated);
            assert!(platform.restarted);
            assert_recovery_complete(&paths, b"verified current executable");
        }
    }

    #[test]
    fn readiness_requires_exact_attempt_pid_version_hash_and_nonce() {
        let directory = UpdateTestDirectory::new();
        let (mut operation, paths) = directory.setup();
        operation.candidate_pid = Some(4242);
        let valid = matching_readiness(&operation);
        assert!(readiness_matches(&valid, &operation));

        let mutations: [fn(&mut CandidateReadiness); 5] = [
            |value| value.attempt_id = "33".repeat(ATTEMPT_ID_BYTES),
            |value| value.nonce = "44".repeat(READINESS_NONCE_BYTES),
            |value| value.pid += 1,
            |value| value.version = "1.2.3".into(),
            |value| value.sha256 = "55".repeat(32),
        ];
        for mutate in mutations {
            let mut stale_or_spoofed = valid.clone();
            mutate(&mut stale_or_spoofed);
            assert!(!readiness_matches(&stale_or_spoofed, &operation));
            crate::store::AtomicJsonStore::new(&paths.readiness)
                .save(&stale_or_spoofed)
                .unwrap();
            assert!(load_authenticated_readiness(&paths, &operation)
                .unwrap()
                .is_none());
        }
        crate::store::AtomicJsonStore::new(&paths.readiness)
            .save(&valid)
            .unwrap();
        assert_eq!(
            load_authenticated_readiness(&paths, &operation).unwrap(),
            Some(valid)
        );
    }

    #[test]
    fn candidate_pid_and_readiness_write_boundaries_remain_recoverable() {
        let directory = UpdateTestDirectory::new();
        let (mut operation, paths) = directory.setup();
        start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();

        let pid_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            save_candidate_pid(
                &paths,
                &mut operation,
                4242,
                &mut CrashAfter(UpdateBoundary::CandidatePidPersisted),
            )
            .unwrap();
        }));
        assert!(pid_result.is_err());

        let readiness = matching_readiness(&operation);
        let readiness_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            persist_candidate_readiness(
                &paths,
                &readiness,
                &mut CrashAfter(UpdateBoundary::ReadinessPersisted),
            )
            .unwrap();
        }));
        assert!(readiness_result.is_err());
        assert_eq!(
            load_authenticated_readiness(&paths, &operation).unwrap(),
            Some(readiness)
        );

        recover_update(&paths.backup, &mut NoUpdateHook).unwrap();
        assert_recovery_complete(&paths, b"verified current executable");
    }

    #[test]
    fn candidate_ready_without_commit_rolls_back_idempotently() {
        let directory = UpdateTestDirectory::new();
        let (mut operation, paths) = directory.setup();
        start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();
        save_candidate_pid(&paths, &mut operation, 4242, &mut NoUpdateHook).unwrap();
        let readiness = matching_readiness(&operation);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            commit_candidate(
                &mut operation,
                &paths,
                &readiness,
                &mut CrashAfter(UpdateBoundary::CandidateReadyPersisted),
            )
            .unwrap();
        }));
        assert!(result.is_err());

        recover_update(&paths.backup, &mut NoUpdateHook).unwrap();
        recover_update(&paths.canonical, &mut NoUpdateHook).unwrap();
        assert_recovery_complete(&paths, b"verified current executable");
    }

    #[test]
    fn successful_watchdog_commit_keeps_backup_until_retryable_later_cleanup() {
        for boundary in [
            UpdateBoundary::BackupRemoved,
            UpdateBoundary::ReadinessRemoved,
            UpdateBoundary::JournalRemoved,
        ] {
            let directory = UpdateTestDirectory::new();
            let (mut operation, paths) = directory.setup();
            start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();
            let mut platform = FakeWatchdog::ready();
            supervise_candidate(&mut operation, &paths, &mut platform, &mut NoUpdateHook).unwrap();
            assert_eq!(operation.phase, UpdatePhase::Committed);
            assert_eq!(
                std::fs::read(&paths.backup).unwrap(),
                b"verified current executable"
            );

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                finish_committed(&paths, &mut CrashAfter(boundary)).unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            finish_committed(&paths, &mut NoUpdateHook).unwrap();
            assert_recovery_complete(&paths, b"verified candidate executable");
        }
    }

    #[test]
    fn every_rollback_rename_boundary_is_idempotent() {
        for boundary in [
            UpdateBoundary::CandidateReturned,
            UpdateBoundary::BackupRestored,
        ] {
            let directory = UpdateTestDirectory::new();
            let (mut operation, paths) = directory.setup();
            start_handover(&directory.0, &mut operation, &mut NoUpdateHook).unwrap();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                recover_update(&paths.backup, &mut CrashAfter(boundary)).unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            recover_update(&paths.backup, &mut NoUpdateHook).unwrap();
            assert_recovery_complete(&paths, b"verified current executable");
        }
    }

    #[test]
    fn attempt_names_are_unique_and_journal_paths_cannot_escape() {
        let first = update_names("app.exe", &"11".repeat(ATTEMPT_ID_BYTES)).unwrap();
        let second = update_names("app.exe", &"22".repeat(ATTEMPT_ID_BYTES)).unwrap();
        assert_ne!(first, second);
        assert!(update_names(r"..\app.exe", &"11".repeat(ATTEMPT_ID_BYTES)).is_err());
        assert!(update_names("app.exe", &"GG".repeat(ATTEMPT_ID_BYTES)).is_err());
    }

    #[test]
    fn journal_phase_names_are_the_v1_contract() {
        for (phase, expected) in [
            (UpdatePhase::Verified, "verified"),
            (UpdatePhase::CurrentMoved, "current_moved"),
            (UpdatePhase::CandidateInstalled, "candidate_installed"),
            (UpdatePhase::CandidateReady, "candidate_ready"),
            (UpdatePhase::Committed, "committed"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), expected);
        }
    }

    #[test]
    fn preserved_corrupt_update_journal_blocks_later_handover_attempts() {
        let directory = UpdateTestDirectory::new();
        let canonical = directory.0.join("app.exe");
        std::fs::write(&canonical, b"verified current executable").unwrap();
        std::fs::write(directory.0.join(UPDATE_JOURNAL), b"{malformed").unwrap();

        assert!(recover_update(&canonical, &mut NoUpdateHook).is_err());
        assert!(!directory.0.join(UPDATE_JOURNAL).exists());
        assert!(has_preserved_update_journal(&directory.0));
        assert!(recover_update(&canonical, &mut NoUpdateHook).is_err());
    }
}
