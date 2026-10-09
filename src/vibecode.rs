//! Vibecode wake lock and crash-safe persistent lid-policy transaction.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows::core::GUID;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::System::Power::*;
use windows::Win32::System::Registry::HKEY;

use crate::config;
use crate::store::{AtomicJsonStore, LoadOutcome};

const JOURNAL_SCHEMA: u32 = 1;
const SUB_BUTTONS: GUID = GUID::from_u128(0x4f971e89_eebd_4455_a8de_9e59040e7347);
const LID_CLOSE_ACTION: GUID = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);
const LID_DO_NOTHING: LidValues = LidValues { ac: 0, dc: 0 };

static WAKE_LOCK_ON: AtomicBool = AtomicBool::new(false);
static DEMO_MODE_ON: AtomicBool = AtomicBool::new(false);
static PERSISTENT_STATUS: AtomicU8 = AtomicU8::new(PersistentStatus::Disabled as u8);
static LAST_ERROR: Mutex<Option<VibeErrorCode>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PersistentStatus {
    Disabled = 0,
    Applied = 1,
    LegacyRecoveryPending = 2,
    RecoveryRequired = 3,
    Error = 4,
}

impl PersistentStatus {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Applied,
            2 => Self::LegacyRecoveryPending,
            3 => Self::RecoveryRequired,
            4 => Self::Error,
            _ => Self::Disabled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VibeErrorCode {
    WakeLock,
    Config,
    JournalPath,
    JournalRead,
    JournalWrite,
    JournalDelete,
    JournalInvalid,
    JournalAlreadyPresent,
    ActiveScheme,
    ReadValues,
    WriteAc,
    WriteDc,
    ActivateScheme,
    ActiveSchemeChanged,
    VerifyApplied,
    VerifyRestore,
    RecoveryIncomplete,
    NoLegacyRecovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VibeError {
    pub code: VibeErrorCode,
}

impl VibeError {
    const fn new(code: VibeErrorCode) -> Self {
        Self { code }
    }
}

impl fmt::Display for VibeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Vibecode operation failed: {:?}", self.code)
    }
}

impl std::error::Error for VibeError {}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct LidValues {
    ac: u32,
    dc: u32,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct SchemeId(u128);

impl SchemeId {
    fn from_guid(guid: &GUID) -> Self {
        Self(guid.to_u128())
    }

    fn to_guid(self) -> GUID {
        GUID::from_u128(self.0)
    }
}

impl Serialize for SchemeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:032x}", self.0))
    }
}

impl<'de> Deserialize<'de> for SchemeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 32 {
            return Err(D::Error::custom("invalid power scheme identifier"));
        }
        u128::from_str_radix(&value, 16)
            .map(Self)
            .map_err(|_| D::Error::custom("invalid power scheme identifier"))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    PersistentOverride,
    LegacyRestore,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Prepared,
    Applied,
    Restoring,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FieldResolution {
    Restored,
    RelinquishedExternalChange,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct RestorationProgress {
    ac: Option<FieldResolution>,
    dc: Option<FieldResolution>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct PowerJournal {
    schema_version: u32,
    operation: OperationKind,
    scheme_guid: SchemeId,
    original: LidValues,
    applied: LidValues,
    phase: JournalPhase,
    restoration: Option<RestorationProgress>,
    started_at_unix: i64,
    app_version: String,
}

trait PowerApi {
    fn active_scheme(&mut self) -> Result<SchemeId, VibeError>;
    fn read_values(&mut self, scheme: SchemeId) -> Result<LidValues, VibeError>;
    fn write_ac(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError>;
    fn write_dc(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError>;
    fn activate(&mut self, scheme: SchemeId) -> Result<(), VibeError>;
}

trait JournalApi {
    fn load(&mut self) -> Result<Option<PowerJournal>, VibeError>;
    fn save(&mut self, journal: &PowerJournal) -> Result<(), VibeError>;
    fn delete(&mut self) -> Result<(), VibeError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    PreparedPersisted,
    AcWritten,
    DcWritten,
    SchemeActivated,
    AppliedReadbackVerified,
    AppliedPersisted,
    RestoringPersisted,
    AcRestored,
    AcResolutionPersisted,
    DcRestored,
    DcResolutionPersisted,
    RestoreActivated,
    RestoreVerified,
    JournalDeleted,
}

trait BoundaryHook {
    fn reached(&mut self, boundary: Boundary);
}

struct NoHook;

impl BoundaryHook for NoHook {
    fn reached(&mut self, _boundary: Boundary) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryOutcome {
    NothingToDo,
    Restored,
    LegacyNeedsCommit,
}

struct Controller<'a, P, J, H> {
    power: &'a mut P,
    journal: &'a mut J,
    hook: &'a mut H,
}

impl<P: PowerApi, J: JournalApi, H: BoundaryHook> Controller<'_, P, J, H> {
    fn apply(
        &mut self,
        operation: OperationKind,
        desired: LidValues,
        started_at_unix: i64,
    ) -> Result<(), VibeError> {
        if self.journal.load()?.is_some() {
            return Err(VibeError::new(VibeErrorCode::JournalAlreadyPresent));
        }

        let scheme = self.power.active_scheme()?;
        let original = self.power.read_values(scheme)?;
        let mut journal = PowerJournal {
            schema_version: JOURNAL_SCHEMA,
            operation,
            scheme_guid: scheme,
            original,
            applied: desired,
            phase: JournalPhase::Prepared,
            restoration: None,
            started_at_unix,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        self.journal.save(&journal)?;
        self.hook.reached(Boundary::PreparedPersisted);

        let apply_result = self.apply_prepared(&mut journal);
        if let Err(error) = apply_result {
            if self.recover().is_err() {
                return Err(VibeError::new(VibeErrorCode::RecoveryIncomplete));
            }
            return Err(error);
        }
        Ok(())
    }

    fn apply_prepared(&mut self, journal: &mut PowerJournal) -> Result<(), VibeError> {
        if self.power.active_scheme()? != journal.scheme_guid {
            return Err(VibeError::new(VibeErrorCode::ActiveSchemeChanged));
        }
        self.power
            .write_ac(journal.scheme_guid, journal.applied.ac)?;
        self.hook.reached(Boundary::AcWritten);
        self.power
            .write_dc(journal.scheme_guid, journal.applied.dc)?;
        self.hook.reached(Boundary::DcWritten);

        if self.power.active_scheme()? != journal.scheme_guid {
            return Err(VibeError::new(VibeErrorCode::ActiveSchemeChanged));
        }
        self.power.activate(journal.scheme_guid)?;
        self.hook.reached(Boundary::SchemeActivated);
        if self.power.read_values(journal.scheme_guid)? != journal.applied {
            return Err(VibeError::new(VibeErrorCode::VerifyApplied));
        }
        self.hook.reached(Boundary::AppliedReadbackVerified);

        journal.phase = JournalPhase::Applied;
        self.journal.save(journal)?;
        self.hook.reached(Boundary::AppliedPersisted);
        Ok(())
    }

    fn recover(&mut self) -> Result<RecoveryOutcome, VibeError> {
        let Some(mut journal) = self.journal.load()? else {
            return Ok(RecoveryOutcome::NothingToDo);
        };
        if journal.operation == OperationKind::LegacyRestore
            && journal.phase == JournalPhase::Applied
        {
            return Ok(RecoveryOutcome::LegacyNeedsCommit);
        }

        self.restore(&mut journal)?;
        self.journal.delete()?;
        self.hook.reached(Boundary::JournalDeleted);
        Ok(RecoveryOutcome::Restored)
    }

    fn restore(&mut self, journal: &mut PowerJournal) -> Result<(), VibeError> {
        journal.phase = JournalPhase::Restoring;
        journal
            .restoration
            .get_or_insert_with(RestorationProgress::default);
        self.journal.save(journal)?;
        self.hook.reached(Boundary::RestoringPersisted);

        let ac_resolution = self.restore_field(journal, true)?;
        journal.restoration.as_mut().unwrap().ac = Some(ac_resolution);
        self.journal.save(journal)?;
        self.hook.reached(Boundary::AcResolutionPersisted);

        let dc_resolution = self.restore_field(journal, false)?;
        journal.restoration.as_mut().unwrap().dc = Some(dc_resolution);
        self.journal.save(journal)?;
        self.hook.reached(Boundary::DcResolutionPersisted);

        if self.power.active_scheme()? == journal.scheme_guid {
            self.power.activate(journal.scheme_guid)?;
            self.hook.reached(Boundary::RestoreActivated);
        }

        let current = self.power.read_values(journal.scheme_guid)?;
        let progress = journal.restoration.as_ref().unwrap();
        if progress.ac == Some(FieldResolution::Restored) && current.ac != journal.original.ac {
            return Err(VibeError::new(VibeErrorCode::VerifyRestore));
        }
        if progress.dc == Some(FieldResolution::Restored) && current.dc != journal.original.dc {
            return Err(VibeError::new(VibeErrorCode::VerifyRestore));
        }
        self.hook.reached(Boundary::RestoreVerified);
        Ok(())
    }

    fn restore_field(
        &mut self,
        journal: &PowerJournal,
        ac: bool,
    ) -> Result<FieldResolution, VibeError> {
        let current = self.power.read_values(journal.scheme_guid)?;
        let (current_value, applied, original) = if ac {
            (current.ac, journal.applied.ac, journal.original.ac)
        } else {
            (current.dc, journal.applied.dc, journal.original.dc)
        };

        if current_value == original {
            return Ok(FieldResolution::Restored);
        }
        if current_value != applied {
            return Ok(FieldResolution::RelinquishedExternalChange);
        }

        if ac {
            self.power.write_ac(journal.scheme_guid, original)?;
            self.hook.reached(Boundary::AcRestored);
        } else {
            self.power.write_dc(journal.scheme_guid, original)?;
            self.hook.reached(Boundary::DcRestored);
        }
        let verified = self.power.read_values(journal.scheme_guid)?;
        let verified_value = if ac { verified.ac } else { verified.dc };
        if verified_value != original {
            return Err(VibeError::new(VibeErrorCode::VerifyRestore));
        }
        Ok(FieldResolution::Restored)
    }

    fn commit_legacy(&mut self) -> Result<(), VibeError> {
        let Some(journal) = self.journal.load()? else {
            return Ok(());
        };
        if journal.operation != OperationKind::LegacyRestore
            || journal.phase != JournalPhase::Applied
        {
            return Err(VibeError::new(VibeErrorCode::JournalInvalid));
        }
        self.journal.delete()?;
        self.hook.reached(Boundary::JournalDeleted);
        Ok(())
    }
}

struct WindowsPower;

impl PowerApi for WindowsPower {
    fn active_scheme(&mut self) -> Result<SchemeId, VibeError> {
        unsafe {
            let mut pointer: *mut GUID = std::ptr::null_mut();
            let result = PowerGetActiveScheme(HKEY::default(), &mut pointer);
            if result != ERROR_SUCCESS || pointer.is_null() {
                if !pointer.is_null() {
                    let _ = LocalFree(HLOCAL(pointer as *mut _));
                }
                return Err(VibeError::new(VibeErrorCode::ActiveScheme));
            }
            let scheme = SchemeId::from_guid(&*pointer);
            let _ = LocalFree(HLOCAL(pointer as *mut _));
            Ok(scheme)
        }
    }

    fn read_values(&mut self, scheme: SchemeId) -> Result<LidValues, VibeError> {
        unsafe {
            let guid = scheme.to_guid();
            let mut ac = 0;
            let mut dc = 0;
            if PowerReadACValueIndex(
                HKEY::default(),
                Some(&guid),
                Some(&SUB_BUTTONS),
                Some(&LID_CLOSE_ACTION),
                &mut ac,
            ) != ERROR_SUCCESS
                || PowerReadDCValueIndex(
                    HKEY::default(),
                    Some(&guid),
                    Some(&SUB_BUTTONS),
                    Some(&LID_CLOSE_ACTION),
                    &mut dc,
                ) != 0
            {
                return Err(VibeError::new(VibeErrorCode::ReadValues));
            }
            Ok(LidValues { ac, dc })
        }
    }

    fn write_ac(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError> {
        let guid = scheme.to_guid();
        let result = unsafe {
            PowerWriteACValueIndex(
                HKEY::default(),
                &guid,
                Some(&SUB_BUTTONS),
                Some(&LID_CLOSE_ACTION),
                value,
            )
        };
        (result == ERROR_SUCCESS)
            .then_some(())
            .ok_or_else(|| VibeError::new(VibeErrorCode::WriteAc))
    }

    fn write_dc(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError> {
        let guid = scheme.to_guid();
        let result = unsafe {
            PowerWriteDCValueIndex(
                HKEY::default(),
                &guid,
                Some(&SUB_BUTTONS),
                Some(&LID_CLOSE_ACTION),
                value,
            )
        };
        (result == 0)
            .then_some(())
            .ok_or_else(|| VibeError::new(VibeErrorCode::WriteDc))
    }

    fn activate(&mut self, scheme: SchemeId) -> Result<(), VibeError> {
        let guid = scheme.to_guid();
        let result = unsafe { PowerSetActiveScheme(HKEY::default(), Some(&guid)) };
        (result == ERROR_SUCCESS)
            .then_some(())
            .ok_or_else(|| VibeError::new(VibeErrorCode::ActivateScheme))
    }
}

struct FileJournal {
    path: PathBuf,
    store: AtomicJsonStore,
}

impl FileJournal {
    fn new(path: PathBuf) -> Self {
        Self {
            store: AtomicJsonStore::new(&path),
            path,
        }
    }

    fn backup_path(&self) -> Result<PathBuf, VibeError> {
        let name = self
            .path
            .file_name()
            .ok_or_else(|| VibeError::new(VibeErrorCode::JournalPath))?;
        let mut backup = name.to_os_string();
        backup.push(".bak");
        Ok(self.path.with_file_name(backup))
    }

    fn has_corrupt_journal(&self) -> bool {
        let Some(parent) = self.path.parent() else {
            return false;
        };
        let Some(name) = self.path.file_name() else {
            return false;
        };
        let prefix = format!("{}.corrupt.", name.to_string_lossy());
        std::fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
    }
}

impl JournalApi for FileJournal {
    fn load(&mut self) -> Result<Option<PowerJournal>, VibeError> {
        let raw = match self
            .store
            .load::<serde_json::Map<String, serde_json::Value>>()
            .map_err(|_| VibeError::new(VibeErrorCode::JournalRead))?
        {
            LoadOutcome::Loaded(raw) | LoadOutcome::RecoveredFromBackup(raw) => raw,
            LoadOutcome::Missing if self.has_corrupt_journal() => {
                return Err(VibeError::new(VibeErrorCode::JournalInvalid));
            }
            LoadOutcome::Missing => return Ok(None),
            LoadOutcome::CorruptPreserved => {
                return Err(VibeError::new(VibeErrorCode::JournalInvalid));
            }
        };
        let journal: PowerJournal = serde_json::from_value(serde_json::Value::Object(raw))
            .map_err(|_| VibeError::new(VibeErrorCode::JournalInvalid))?;
        if journal.schema_version != JOURNAL_SCHEMA {
            return Err(VibeError::new(VibeErrorCode::JournalInvalid));
        }
        Ok(Some(journal))
    }

    fn save(&mut self, journal: &PowerJournal) -> Result<(), VibeError> {
        self.store
            .save(journal)
            .map_err(|_| VibeError::new(VibeErrorCode::JournalWrite))
    }

    fn delete(&mut self) -> Result<(), VibeError> {
        remove_if_exists(&self.backup_path()?)?;
        remove_if_exists(&self.path)?;
        if self.path.exists() || self.backup_path()?.exists() {
            return Err(VibeError::new(VibeErrorCode::JournalDelete));
        }
        Ok(())
    }
}

fn remove_if_exists(path: &Path) -> Result<(), VibeError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(VibeError::new(VibeErrorCode::JournalDelete)),
    }
}

pub fn init() {
    if config::settings().wake_lock_enabled {
        set_wake_lock(true);
    }
    if recover_and_finalize().is_err() {
        set_persistent_status(PersistentStatus::RecoveryRequired);
        return;
    }
    if config::legacy_lid_recovery().is_some() {
        set_persistent_status(PersistentStatus::LegacyRecoveryPending);
        return;
    }
    if config::settings().persistent_lid_override_enabled {
        if let Err(error) = apply_real(OperationKind::PersistentOverride, LID_DO_NOTHING) {
            set_error(error);
        } else {
            set_persistent_status(PersistentStatus::Applied);
        }
    } else {
        set_persistent_status(PersistentStatus::Disabled);
    }
}

pub fn is_on() -> bool {
    WAKE_LOCK_ON.load(Ordering::SeqCst)
}

fn set(on: bool) -> bool {
    if crate::demo::is_active() {
        return false;
    }
    let previous = config::settings().wake_lock_enabled;
    if config::set_wake_lock_enabled(on).is_ok() {
        if set_wake_lock(on) {
            return true;
        }
        let _ = config::set_wake_lock_enabled(previous);
        return false;
    }
    if !on {
        set_wake_lock(false);
    }
    false
}

#[inline(never)]
pub fn mode_is_on() -> bool {
    if crate::demo::is_active() {
        return DEMO_MODE_ON.load(Ordering::SeqCst);
    }
    is_on() && persistent_status() == PersistentStatus::Applied
}

trait ModeApi {
    fn wake(&mut self, on: bool) -> bool;
    fn lid(&mut self, on: bool) -> bool;
}

fn change_mode(api: &mut impl ModeApi, on: bool, previous_wake: bool, previous_lid: bool) -> bool {
    if on {
        if !api.wake(true) {
            return false;
        }
        if previous_lid || api.lid(true) {
            return true;
        }
        api.wake(previous_wake);
        false
    } else {
        // Always drop wake before restoring lid policy, even if saving fails.
        let wake = api.wake(false);
        let lid = api.lid(false);
        wake && lid
    }
}

#[inline(never)]
pub fn set_mode(on: bool) {
    if crate::demo::is_active() {
        DEMO_MODE_ON.store(on, Ordering::SeqCst);
        return;
    }
    struct Real;
    impl ModeApi for Real {
        fn wake(&mut self, on: bool) -> bool {
            set(on)
        }
        fn lid(&mut self, on: bool) -> bool {
            set_persistent_override(on)
        }
    }
    change_mode(
        &mut Real,
        on,
        is_on(),
        persistent_status() == PersistentStatus::Applied,
    );
}

pub fn persistent_status() -> PersistentStatus {
    PersistentStatus::from_u8(PERSISTENT_STATUS.load(Ordering::SeqCst))
}

pub fn persistent_preference_enabled() -> bool {
    config::settings().persistent_lid_override_enabled
}

pub fn set_persistent_override(on: bool) -> bool {
    if crate::demo::is_active() {
        return false;
    }
    if on {
        if config::legacy_lid_recovery().is_some() {
            set_persistent_status(PersistentStatus::LegacyRecoveryPending);
            return false;
        }
        if config::set_persistent_lid_override_enabled(true).is_err() {
            set_error(VibeError::new(VibeErrorCode::Config));
            return false;
        }
        match apply_real(OperationKind::PersistentOverride, LID_DO_NOTHING) {
            Ok(()) => {
                set_persistent_status(PersistentStatus::Applied);
                true
            }
            Err(error) => {
                let _ = config::set_persistent_lid_override_enabled(false);
                set_error(error);
                false
            }
        }
    } else {
        if config::set_persistent_lid_override_enabled(false).is_err() {
            set_error(VibeError::new(VibeErrorCode::Config));
            return false;
        }
        match recover_and_finalize() {
            Ok(()) => {
                set_persistent_status(if config::legacy_lid_recovery().is_some() {
                    PersistentStatus::LegacyRecoveryPending
                } else {
                    PersistentStatus::Disabled
                });
                true
            }
            Err(error) => {
                set_recovery_error(error);
                false
            }
        }
    }
}

pub fn restore_legacy_to_current_scheme() -> bool {
    if crate::demo::is_active() {
        return false;
    }
    let Some((ac, dc)) = config::legacy_lid_recovery() else {
        set_error(VibeError::new(VibeErrorCode::NoLegacyRecovery));
        return false;
    };
    if recover_and_finalize().is_err() {
        set_persistent_status(PersistentStatus::RecoveryRequired);
        return false;
    }
    match apply_real(OperationKind::LegacyRestore, LidValues { ac, dc })
        .and_then(|()| finish_legacy_real())
    {
        Ok(()) => {
            set_persistent_status(PersistentStatus::Disabled);
            true
        }
        Err(error) => {
            set_recovery_error(error);
            false
        }
    }
}

pub fn recover_command() -> bool {
    if recover_and_finalize().is_err() {
        set_persistent_status(PersistentStatus::RecoveryRequired);
        return false;
    }
    if config::legacy_lid_recovery().is_some() {
        return restore_legacy_to_current_scheme();
    }
    set_persistent_status(PersistentStatus::Disabled);
    true
}

pub fn retry_recovery() -> bool {
    if crate::demo::is_active() {
        return false;
    }
    if let Err(error) = recover_and_finalize() {
        set_recovery_error(error);
        return false;
    }
    if config::legacy_lid_recovery().is_some() {
        set_persistent_status(PersistentStatus::LegacyRecoveryPending);
        return true;
    }
    if config::settings().persistent_lid_override_enabled {
        match apply_real(OperationKind::PersistentOverride, LID_DO_NOTHING) {
            Ok(()) => set_persistent_status(PersistentStatus::Applied),
            Err(error) => {
                set_error(error);
                return false;
            }
        }
    } else {
        set_persistent_status(PersistentStatus::Disabled);
    }
    true
}

pub fn reconcile_active_scheme() {
    if persistent_status() != PersistentStatus::Applied {
        return;
    }
    let Ok(path) = journal_path() else {
        set_error(VibeError::new(VibeErrorCode::JournalPath));
        return;
    };
    let mut journal = FileJournal::new(path);
    let Ok(Some(record)) = journal.load() else {
        set_persistent_status(PersistentStatus::RecoveryRequired);
        return;
    };
    let mut power = WindowsPower;
    if power.active_scheme() == Ok(record.scheme_guid) {
        return;
    }
    if let Err(error) = recover_and_finalize() {
        set_recovery_error(error);
        return;
    }
    if config::settings().persistent_lid_override_enabled {
        match apply_real(OperationKind::PersistentOverride, LID_DO_NOTHING) {
            Ok(()) => set_persistent_status(PersistentStatus::Applied),
            Err(error) => set_error(error),
        }
    }
}

pub fn restore_for_exit() {
    set_wake_lock(false);
    if let Err(error) = recover_and_finalize() {
        set_recovery_error(error);
    } else if config::legacy_lid_recovery().is_some() {
        set_persistent_status(PersistentStatus::LegacyRecoveryPending);
    } else {
        set_persistent_status(PersistentStatus::Disabled);
    }
}

pub fn flyout_caption() -> &'static str {
    if crate::demo::is_active() {
        return caption_for(
            mode_is_on(),
            if mode_is_on() {
                PersistentStatus::Applied
            } else {
                PersistentStatus::Disabled
            },
        );
    }
    caption_for(is_on(), persistent_status())
}

fn caption_for(wake_lock_on: bool, status: PersistentStatus) -> &'static str {
    match (wake_lock_on, status) {
        (true, PersistentStatus::Applied) => "On · stays awake with lid closed",
        (false, PersistentStatus::Applied) => "Lid protected · idle sleep allowed",
        (_, PersistentStatus::LegacyRecoveryPending) => "Restore saved lid settings first",
        (_, PersistentStatus::RecoveryRequired) => "Lid settings need recovery",
        (_, PersistentStatus::Error) => "Lid protection failed · open Settings",
        (true, PersistentStatus::Disabled) => "Awake · closing lid may sleep",
        (false, PersistentStatus::Disabled) => "Keep running with lid closed",
    }
}

pub fn diagnostic() -> Option<String> {
    LAST_ERROR
        .lock()
        .ok()
        .and_then(|error| error.map(|code| format!("Vibecode recovery · {code:?}")))
}

fn set_wake_lock(on: bool) -> bool {
    let success = unsafe {
        let flags = if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        SetThreadExecutionState(flags).0 != 0
    };
    if success || !on {
        WAKE_LOCK_ON.store(on && success, Ordering::SeqCst);
    }
    if !success {
        set_error(VibeError::new(VibeErrorCode::WakeLock));
    }
    success
}

fn journal_path() -> Result<PathBuf, VibeError> {
    crate::config::config_dir()
        .map(|directory| directory.join("power-override.v1.json"))
        .ok_or_else(|| VibeError::new(VibeErrorCode::JournalPath))
}

fn apply_real(operation: OperationKind, desired: LidValues) -> Result<(), VibeError> {
    let mut power = WindowsPower;
    let mut journal = FileJournal::new(journal_path()?);
    let mut hook = NoHook;
    let mut controller = Controller {
        power: &mut power,
        journal: &mut journal,
        hook: &mut hook,
    };
    controller.apply(
        operation,
        desired,
        time::OffsetDateTime::now_utc().unix_timestamp(),
    )
}

fn recover_and_finalize() -> Result<(), VibeError> {
    let mut power = WindowsPower;
    let mut journal = FileJournal::new(journal_path()?);
    let mut hook = NoHook;
    let mut controller = Controller {
        power: &mut power,
        journal: &mut journal,
        hook: &mut hook,
    };
    if controller.recover()? == RecoveryOutcome::LegacyNeedsCommit {
        config::clear_legacy_lid_recovery().map_err(|_| VibeError::new(VibeErrorCode::Config))?;
        controller.commit_legacy()?;
    }
    Ok(())
}

fn finish_legacy_real() -> Result<(), VibeError> {
    config::clear_legacy_lid_recovery().map_err(|_| VibeError::new(VibeErrorCode::Config))?;
    let mut power = WindowsPower;
    let mut journal = FileJournal::new(journal_path()?);
    let mut hook = NoHook;
    Controller {
        power: &mut power,
        journal: &mut journal,
        hook: &mut hook,
    }
    .commit_legacy()
}

fn set_persistent_status(status: PersistentStatus) {
    PERSISTENT_STATUS.store(status as u8, Ordering::SeqCst);
    if status != PersistentStatus::Error && status != PersistentStatus::RecoveryRequired {
        if let Ok(mut error) = LAST_ERROR.lock() {
            *error = None;
        }
    }
}

fn set_error(error: VibeError) {
    if let Ok(mut last_error) = LAST_ERROR.lock() {
        *last_error = Some(error.code);
    }
    let status = if journal_path().is_ok_and(|path| {
        let journal = FileJournal::new(path);
        journal.path.exists() || journal.has_corrupt_journal()
    }) || error.code == VibeErrorCode::JournalInvalid
    {
        PersistentStatus::RecoveryRequired
    } else {
        PersistentStatus::Error
    };
    PERSISTENT_STATUS.store(status as u8, Ordering::SeqCst);
}

fn set_recovery_error(error: VibeError) {
    if let Ok(mut last_error) = LAST_ERROR.lock() {
        *last_error = Some(error.code);
    }
    PERSISTENT_STATUS.store(PersistentStatus::RecoveryRequired as u8, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const SCHEME_A: SchemeId = SchemeId(1);
    const SCHEME_B: SchemeId = SchemeId(2);
    const ORIGINAL: LidValues = LidValues { ac: 1, dc: 2 };
    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-vibecode-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn journal_path(&self) -> PathBuf {
            self.0.join("power-override.v1.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            assert!(self.0.starts_with(std::env::temp_dir()));
            assert!(self.0.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("claudometer-vibecode-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PowerCall {
        Active,
        Read,
        WriteAc,
        WriteDc,
        Activate,
    }

    struct FakePower {
        active: SchemeId,
        values: HashMap<SchemeId, LidValues>,
        calls: Vec<PowerCall>,
        activations: Vec<SchemeId>,
        failures: VecDeque<PowerCall>,
        change_active_after_ac: Option<SchemeId>,
    }

    impl FakePower {
        fn new() -> Self {
            Self {
                active: SCHEME_A,
                values: HashMap::from([
                    (SCHEME_A, ORIGINAL),
                    (SCHEME_B, LidValues { ac: 3, dc: 4 }),
                ]),
                calls: Vec::new(),
                activations: Vec::new(),
                failures: VecDeque::new(),
                change_active_after_ac: None,
            }
        }

        fn fail(&mut self, call: PowerCall) -> Result<(), VibeError> {
            self.calls.push(call);
            if self.failures.front() == Some(&call) {
                self.failures.pop_front();
                return Err(VibeError::new(match call {
                    PowerCall::Active => VibeErrorCode::ActiveScheme,
                    PowerCall::Read => VibeErrorCode::ReadValues,
                    PowerCall::WriteAc => VibeErrorCode::WriteAc,
                    PowerCall::WriteDc => VibeErrorCode::WriteDc,
                    PowerCall::Activate => VibeErrorCode::ActivateScheme,
                }));
            }
            Ok(())
        }
    }

    impl PowerApi for FakePower {
        fn active_scheme(&mut self) -> Result<SchemeId, VibeError> {
            self.fail(PowerCall::Active)?;
            Ok(self.active)
        }

        fn read_values(&mut self, scheme: SchemeId) -> Result<LidValues, VibeError> {
            self.fail(PowerCall::Read)?;
            self.values
                .get(&scheme)
                .copied()
                .ok_or_else(|| VibeError::new(VibeErrorCode::ReadValues))
        }

        fn write_ac(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError> {
            self.fail(PowerCall::WriteAc)?;
            self.values.get_mut(&scheme).unwrap().ac = value;
            if let Some(next) = self.change_active_after_ac.take() {
                self.active = next;
            }
            Ok(())
        }

        fn write_dc(&mut self, scheme: SchemeId, value: u32) -> Result<(), VibeError> {
            self.fail(PowerCall::WriteDc)?;
            self.values.get_mut(&scheme).unwrap().dc = value;
            Ok(())
        }

        fn activate(&mut self, scheme: SchemeId) -> Result<(), VibeError> {
            self.fail(PowerCall::Activate)?;
            self.activations.push(scheme);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeJournal {
        current: Option<PowerJournal>,
        save_count: usize,
        fail_save_at: Option<usize>,
        fail_delete: bool,
    }

    impl JournalApi for FakeJournal {
        fn load(&mut self) -> Result<Option<PowerJournal>, VibeError> {
            Ok(self.current.clone())
        }

        fn save(&mut self, journal: &PowerJournal) -> Result<(), VibeError> {
            self.save_count += 1;
            if self.fail_save_at == Some(self.save_count) {
                return Err(VibeError::new(VibeErrorCode::JournalWrite));
            }
            self.current = Some(journal.clone());
            Ok(())
        }

        fn delete(&mut self) -> Result<(), VibeError> {
            if self.fail_delete {
                return Err(VibeError::new(VibeErrorCode::JournalDelete));
            }
            self.current = None;
            Ok(())
        }
    }

    struct CrashAt(Boundary);

    impl BoundaryHook for CrashAt {
        fn reached(&mut self, boundary: Boundary) {
            if boundary == self.0 {
                panic!("simulated interruption after {boundary:?}");
            }
        }
    }

    fn controller<'a, H: BoundaryHook>(
        power: &'a mut FakePower,
        journal: &'a mut FakeJournal,
        hook: &'a mut H,
    ) -> Controller<'a, FakePower, FakeJournal, H> {
        Controller {
            power,
            journal,
            hook,
        }
    }

    fn apply_normally(power: &mut FakePower, journal: &mut FakeJournal) {
        controller(power, journal, &mut NoHook)
            .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
            .unwrap();
    }

    fn sample_journal(phase: JournalPhase) -> PowerJournal {
        PowerJournal {
            schema_version: JOURNAL_SCHEMA,
            operation: OperationKind::PersistentOverride,
            scheme_guid: SCHEME_A,
            original: ORIGINAL,
            applied: LID_DO_NOTHING,
            phase,
            restoration: None,
            started_at_unix: 100,
            app_version: "0.7.3-test".to_string(),
        }
    }

    #[test]
    fn prepared_journal_failure_performs_zero_power_writes() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal {
            fail_save_at: Some(1),
            ..Default::default()
        };
        let error = controller(&mut power, &mut journal, &mut NoHook)
            .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
            .unwrap_err();
        assert_eq!(error.code, VibeErrorCode::JournalWrite);
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert!(!power.calls.contains(&PowerCall::WriteAc));
        assert!(!power.calls.contains(&PowerCall::WriteDc));
    }

    #[test]
    fn checked_power_failures_roll_back_or_leave_zero_mutations() {
        for failed_call in [
            PowerCall::Active,
            PowerCall::Read,
            PowerCall::WriteAc,
            PowerCall::WriteDc,
            PowerCall::Activate,
        ] {
            let mut power = FakePower::new();
            power.failures.push_back(failed_call);
            let mut journal = FakeJournal::default();
            assert!(controller(&mut power, &mut journal, &mut NoHook)
                .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
                .is_err());
            assert_eq!(
                power.values[&SCHEME_A], ORIGINAL,
                "failure at {failed_call:?}"
            );
            assert!(journal.current.is_none(), "failure at {failed_call:?}");
        }
    }

    #[test]
    fn interruption_after_every_apply_boundary_recovers_originals() {
        for boundary in [
            Boundary::PreparedPersisted,
            Boundary::AcWritten,
            Boundary::DcWritten,
            Boundary::SchemeActivated,
            Boundary::AppliedReadbackVerified,
            Boundary::AppliedPersisted,
        ] {
            let mut power = FakePower::new();
            let mut journal = FakeJournal::default();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                controller(&mut power, &mut journal, &mut CrashAt(boundary))
                    .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
                    .unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            controller(&mut power, &mut journal, &mut NoHook)
                .recover()
                .unwrap();
            assert_eq!(power.values[&SCHEME_A], ORIGINAL, "boundary {boundary:?}");
            assert!(journal.current.is_none(), "boundary {boundary:?}");
        }
    }

    #[test]
    fn interruption_after_every_restore_boundary_converges() {
        for boundary in [
            Boundary::RestoringPersisted,
            Boundary::AcRestored,
            Boundary::AcResolutionPersisted,
            Boundary::DcRestored,
            Boundary::DcResolutionPersisted,
            Boundary::RestoreActivated,
            Boundary::RestoreVerified,
            Boundary::JournalDeleted,
        ] {
            let mut power = FakePower::new();
            let mut journal = FakeJournal::default();
            apply_normally(&mut power, &mut journal);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                controller(&mut power, &mut journal, &mut CrashAt(boundary))
                    .recover()
                    .unwrap();
            }));
            assert!(result.is_err(), "boundary {boundary:?}");
            controller(&mut power, &mut journal, &mut NoHook)
                .recover()
                .unwrap();
            assert_eq!(power.values[&SCHEME_A], ORIGINAL, "boundary {boundary:?}");
            assert!(journal.current.is_none(), "boundary {boundary:?}");
        }
    }

    #[test]
    fn retry_reactivates_active_scheme_after_values_were_already_restored() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        apply_normally(&mut power, &mut journal);
        power.activations.clear();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            controller(
                &mut power,
                &mut journal,
                &mut CrashAt(Boundary::DcResolutionPersisted),
            )
            .recover()
            .unwrap();
        }));
        assert!(result.is_err());
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert!(power.activations.is_empty());

        controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .unwrap();
        assert_eq!(power.activations, [SCHEME_A]);
        assert!(journal.current.is_none());
    }

    #[test]
    fn external_changes_are_relinquished_not_overwritten() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        apply_normally(&mut power, &mut journal);
        power.values.get_mut(&SCHEME_A).unwrap().ac = 9;

        controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .unwrap();
        assert_eq!(power.values[&SCHEME_A], LidValues { ac: 9, dc: 2 });
        assert!(journal.current.is_none());
    }

    #[test]
    fn inactive_old_scheme_is_restored_without_becoming_active() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        apply_normally(&mut power, &mut journal);
        power.active = SCHEME_B;
        power.activations.clear();

        controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .unwrap();
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert_eq!(power.active, SCHEME_B);
        assert!(power.activations.is_empty());
    }

    #[test]
    fn active_scheme_change_during_apply_rolls_back_exact_old_scheme() {
        let mut power = FakePower::new();
        power.change_active_after_ac = Some(SCHEME_B);
        let mut journal = FakeJournal::default();

        let error = controller(&mut power, &mut journal, &mut NoHook)
            .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
            .unwrap_err();
        assert_eq!(error.code, VibeErrorCode::ActiveSchemeChanged);
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert_eq!(power.values[&SCHEME_B], LidValues { ac: 3, dc: 4 });
        assert!(journal.current.is_none());
    }

    #[test]
    fn applied_journal_write_failure_rolls_back() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal {
            fail_save_at: Some(2),
            ..Default::default()
        };

        assert!(controller(&mut power, &mut journal, &mut NoHook)
            .apply(OperationKind::PersistentOverride, LID_DO_NOTHING, 100)
            .is_err());
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert!(journal.current.is_none());
    }

    #[test]
    fn recovery_failure_retains_actionable_journal() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        apply_normally(&mut power, &mut journal);
        journal.fail_save_at = Some(journal.save_count + 1);

        assert!(controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .is_err());
        assert_eq!(power.values[&SCHEME_A], LID_DO_NOTHING);
        assert!(journal.current.is_some());
    }

    #[test]
    fn every_recovery_journal_save_failure_is_retryable() {
        for save_offset in 1..=3 {
            let mut power = FakePower::new();
            let mut journal = FakeJournal::default();
            apply_normally(&mut power, &mut journal);
            journal.fail_save_at = Some(journal.save_count + save_offset);

            assert!(controller(&mut power, &mut journal, &mut NoHook)
                .recover()
                .is_err());
            assert!(journal.current.is_some(), "save offset {save_offset}");
            controller(&mut power, &mut journal, &mut NoHook)
                .recover()
                .unwrap();
            assert_eq!(
                power.values[&SCHEME_A], ORIGINAL,
                "save offset {save_offset}"
            );
            assert!(journal.current.is_none(), "save offset {save_offset}");
        }
    }

    #[test]
    fn restore_write_and_delete_failures_keep_a_retryable_journal() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        apply_normally(&mut power, &mut journal);
        power.failures.push_back(PowerCall::WriteAc);
        assert!(controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .is_err());
        assert!(journal.current.is_some());

        controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .unwrap();
        apply_normally(&mut power, &mut journal);
        journal.fail_delete = true;
        assert!(controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .is_err());
        assert_eq!(power.values[&SCHEME_A], ORIGINAL);
        assert!(journal.current.is_some());
        journal.fail_delete = false;
        controller(&mut power, &mut journal, &mut NoHook)
            .recover()
            .unwrap();
        assert!(journal.current.is_none());
    }

    #[test]
    fn legacy_restore_commits_desired_values_instead_of_undoing_them() {
        let mut power = FakePower::new();
        let mut journal = FakeJournal::default();
        let legacy = LidValues { ac: 1, dc: 3 };
        controller(&mut power, &mut journal, &mut NoHook)
            .apply(OperationKind::LegacyRestore, legacy, 100)
            .unwrap();
        assert_eq!(power.values[&SCHEME_A], legacy);
        assert_eq!(
            controller(&mut power, &mut journal, &mut NoHook)
                .recover()
                .unwrap(),
            RecoveryOutcome::LegacyNeedsCommit
        );
        assert!(journal.current.is_some());
        controller(&mut power, &mut journal, &mut NoHook)
            .commit_legacy()
            .unwrap();
        assert_eq!(power.values[&SCHEME_A], legacy);
        assert!(journal.current.is_none());
    }

    #[test]
    fn scheme_identifier_round_trips_as_fixed_safe_hex() {
        let scheme = SchemeId(0x1234_5678_90ab_cdef_0123_4567_89ab_cdef);
        let json = serde_json::to_string(&scheme).unwrap();
        assert_eq!(json, "\"1234567890abcdef0123456789abcdef\"");
        assert_eq!(serde_json::from_str::<SchemeId>(&json).unwrap(), scheme);
    }

    #[test]
    fn file_journal_round_trip_and_terminal_delete_remove_backup_generation() {
        let directory = TestDirectory::new();
        let path = directory.journal_path();
        let mut journal = FileJournal::new(path.clone());
        journal
            .save(&sample_journal(JournalPhase::Prepared))
            .unwrap();
        journal
            .save(&sample_journal(JournalPhase::Applied))
            .unwrap();
        assert_eq!(
            journal.load().unwrap().unwrap().phase,
            JournalPhase::Applied
        );
        assert!(path.with_file_name("power-override.v1.json.bak").exists());

        journal.delete().unwrap();
        assert!(!path.exists());
        assert!(!path.with_file_name("power-override.v1.json.bak").exists());
    }

    #[test]
    fn preserved_corrupt_safety_journal_blocks_every_later_load() {
        let directory = TestDirectory::new();
        let path = directory.journal_path();
        std::fs::write(&path, b"{malformed").unwrap();
        let mut journal = FileJournal::new(path.clone());
        assert_eq!(
            journal.load().unwrap_err().code,
            VibeErrorCode::JournalInvalid
        );
        assert!(!path.exists());
        assert_eq!(
            journal.load().unwrap_err().code,
            VibeErrorCode::JournalInvalid
        );
    }

    #[test]
    fn captions_never_conflate_wake_lock_and_persistent_override() {
        assert_eq!(
            caption_for(true, PersistentStatus::Disabled),
            "Awake · closing lid may sleep"
        );
        assert_eq!(
            caption_for(false, PersistentStatus::Applied),
            "Lid protected · idle sleep allowed"
        );
        assert_eq!(
            caption_for(true, PersistentStatus::RecoveryRequired),
            "Lid settings need recovery"
        );
    }

    #[test]
    fn unified_mode_orders_protection_and_rolls_back_failed_enable() {
        struct Fake {
            calls: Vec<(char, bool)>,
            fail_wake: bool,
            fail_lid: bool,
        }
        impl ModeApi for Fake {
            fn wake(&mut self, on: bool) -> bool {
                self.calls.push(('w', on));
                !self.fail_wake
            }
            fn lid(&mut self, on: bool) -> bool {
                self.calls.push(('l', on));
                !self.fail_lid
            }
        }
        for (on, wake, lid, fail_wake, fail_lid, expected, calls) in [
            (
                true,
                false,
                false,
                false,
                false,
                true,
                vec![('w', true), ('l', true)],
            ),
            (true, false, false, true, false, false, vec![('w', true)]),
            (
                true,
                false,
                false,
                false,
                true,
                false,
                vec![('w', true), ('l', true), ('w', false)],
            ),
            (
                true,
                true,
                false,
                false,
                true,
                false,
                vec![('w', true), ('l', true), ('w', true)],
            ),
            (true, false, true, false, false, true, vec![('w', true)]),
            (
                false,
                true,
                true,
                true,
                false,
                false,
                vec![('w', false), ('l', false)],
            ),
            (
                false,
                true,
                true,
                false,
                true,
                false,
                vec![('w', false), ('l', false)],
            ),
            (
                false,
                true,
                true,
                false,
                false,
                true,
                vec![('w', false), ('l', false)],
            ),
        ] {
            let mut api = Fake {
                calls: vec![],
                fail_wake,
                fail_lid,
            };
            assert_eq!(change_mode(&mut api, on, wake, lid), expected);
            assert_eq!(api.calls, calls);
        }
        assert_eq!(
            caption_for(true, PersistentStatus::Applied),
            "On · stays awake with lid closed"
        );
        assert_eq!(
            caption_for(false, PersistentStatus::Disabled),
            "Keep running with lid closed"
        );
    }
}
