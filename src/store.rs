use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    REPLACEFILE_WRITE_THROUGH,
};

static NEXT_UNIQUE_NAME: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreStage {
    Serialize,
    CreateDirectory,
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    ValidateExisting,
    ReplaceTarget,
    ValidateCommitted,
    ValidateBackup,
    RestoreBackup,
    ReadTarget,
    PreserveCorrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreErrorKind {
    Io(io::ErrorKind),
    InvalidJson,
    Platform(u32),
    InvalidPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreError {
    pub stage: StoreStage,
    pub kind: StoreErrorKind,
}

impl StoreError {
    fn io(stage: StoreStage, error: &io::Error) -> Self {
        Self {
            stage,
            kind: StoreErrorKind::Io(error.kind()),
        }
    }

    fn json(stage: StoreStage) -> Self {
        Self {
            stage,
            kind: StoreErrorKind::InvalidJson,
        }
    }

    fn platform(stage: StoreStage, error: &windows::core::Error) -> Self {
        let code = error.code().0 as u32;
        let kind = if code & 0xffff_0000 == 0x8007_0000 {
            let io_error = io::Error::from_raw_os_error((code & 0xffff) as i32);
            StoreErrorKind::Io(io_error.kind())
        } else {
            StoreErrorKind::Platform(code)
        };
        Self { stage, kind }
    }

    fn invalid_path(stage: StoreStage) -> Self {
        Self {
            stage,
            kind: StoreErrorKind::InvalidPath,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "JSON store failed at {:?}: {:?}",
            self.stage, self.kind
        )
    }
}

impl std::error::Error for StoreError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOutcome<T> {
    Loaded(T),
    RecoveredFromBackup(T),
    Missing,
    CorruptPreserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailurePoint {
    CreateDirectory,
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    ValidateExisting,
    ReplaceTarget,
    ValidateCommitted,
    ValidateBackup,
    RestoreBackup,
    ReadTarget,
    PreserveCorrupt,
}

pub trait FaultInjector {
    fn check(&self, point: FailurePoint) -> io::Result<()>;
}

#[derive(Clone, Copy, Default)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn check(&self, _point: FailurePoint) -> io::Result<()> {
        Ok(())
    }
}

pub struct AtomicJsonStore<F = NoFaults> {
    path: PathBuf,
    faults: F,
}

impl AtomicJsonStore<NoFaults> {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            faults: NoFaults,
        }
    }
}

impl<F: FaultInjector> AtomicJsonStore<F> {
    pub fn with_fault_injector(path: impl Into<PathBuf>, faults: F) -> Self {
        Self {
            path: path.into(),
            faults,
        }
    }

    pub fn save<T>(&self, value: &T) -> Result<(), StoreError>
    where
        T: Serialize + DeserializeOwned,
    {
        let mut encoded = serde_json::to_vec_pretty(value)
            .map_err(|_| StoreError::json(StoreStage::Serialize))?;
        encoded.push(b'\n');
        serde_json::from_slice::<T>(&encoded)
            .map_err(|_| StoreError::json(StoreStage::Serialize))?;

        self.inject(FailurePoint::CreateDirectory, StoreStage::CreateDirectory)?;
        std::fs::create_dir_all(self.parent())
            .map_err(|error| StoreError::io(StoreStage::CreateDirectory, &error))?;

        let temp_path = self.unique_sibling("tmp", StoreStage::CreateTemporary)?;
        let mut temp_guard = TemporaryGuard::new(temp_path.clone());
        self.inject(FailurePoint::CreateTemporary, StoreStage::CreateTemporary)?;
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| StoreError::io(StoreStage::CreateTemporary, &error))?;

        self.inject(FailurePoint::WriteTemporary, StoreStage::WriteTemporary)?;
        temp.write_all(&encoded)
            .map_err(|error| StoreError::io(StoreStage::WriteTemporary, &error))?;
        self.inject(FailurePoint::FlushTemporary, StoreStage::FlushTemporary)?;
        temp.sync_all()
            .map_err(|error| StoreError::io(StoreStage::FlushTemporary, &error))?;
        drop(temp);

        let had_existing = match std::fs::read(&self.path) {
            Ok(existing) => {
                self.inject(FailurePoint::ValidateExisting, StoreStage::ValidateExisting)?;
                serde_json::from_slice::<T>(&existing)
                    .map_err(|_| StoreError::json(StoreStage::ValidateExisting))?;
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(StoreError::io(StoreStage::ValidateExisting, &error)),
        };

        self.inject(FailurePoint::ReplaceTarget, StoreStage::ReplaceTarget)?;
        if had_existing {
            self.replace_existing(&temp_path)?;
        } else {
            self.move_into_place(&temp_path, false, StoreStage::ReplaceTarget)?;
        }
        temp_guard.disarm();

        let validation = self
            .inject(
                FailurePoint::ValidateCommitted,
                StoreStage::ValidateCommitted,
            )
            .and_then(|()| self.validate_path::<T>(&self.path, StoreStage::ValidateCommitted))
            .and_then(|()| {
                if had_existing {
                    self.inject(FailurePoint::ValidateBackup, StoreStage::ValidateBackup)?;
                    self.validate_path::<T>(&self.backup_path()?, StoreStage::ValidateBackup)?;
                }
                Ok(())
            });

        if let Err(validation_error) = validation {
            if had_existing {
                self.restore_verified_backup::<T>()?;
            } else {
                std::fs::remove_file(&self.path)
                    .map_err(|error| StoreError::io(StoreStage::RestoreBackup, &error))?;
            }
            return Err(validation_error);
        }

        Ok(())
    }

    pub fn load<T>(&self) -> Result<LoadOutcome<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        self.inject(FailurePoint::ReadTarget, StoreStage::ReadTarget)?;
        match self.read_value::<T>(&self.path, StoreStage::ReadTarget) {
            Ok(value) => Ok(LoadOutcome::Loaded(value)),
            Err(error) if error.kind == StoreErrorKind::Io(io::ErrorKind::NotFound) => {
                self.load_backup_or_missing()
            }
            Err(error) if error.kind == StoreErrorKind::InvalidJson => {
                self.preserve_corrupt_target()?;
                match self.load_backup_or_missing()? {
                    LoadOutcome::Loaded(value) | LoadOutcome::RecoveredFromBackup(value) => {
                        Ok(LoadOutcome::RecoveredFromBackup(value))
                    }
                    LoadOutcome::Missing | LoadOutcome::CorruptPreserved => {
                        Ok(LoadOutcome::CorruptPreserved)
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    fn load_backup_or_missing<T>(&self) -> Result<LoadOutcome<T>, StoreError>
    where
        T: DeserializeOwned,
    {
        let backup = self.backup_path()?;
        match self.read_value::<T>(&backup, StoreStage::ValidateBackup) {
            Ok(value) => {
                self.restore_verified_backup::<T>()?;
                Ok(LoadOutcome::RecoveredFromBackup(value))
            }
            Err(error) if error.kind == StoreErrorKind::Io(io::ErrorKind::NotFound) => {
                Ok(LoadOutcome::Missing)
            }
            Err(error) if error.kind == StoreErrorKind::InvalidJson => {
                Ok(LoadOutcome::CorruptPreserved)
            }
            Err(error) => Err(error),
        }
    }

    fn preserve_corrupt_target(&self) -> Result<(), StoreError> {
        self.inject(FailurePoint::PreserveCorrupt, StoreStage::PreserveCorrupt)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let unique = NEXT_UNIQUE_NAME.fetch_add(1, Ordering::Relaxed);
        let corrupt = self.sibling(
            &format!("corrupt.{timestamp}.{}.{}", std::process::id(), unique),
            StoreStage::PreserveCorrupt,
        )?;
        move_file(&self.path, &corrupt, false)
            .map_err(|error| StoreError::platform(StoreStage::PreserveCorrupt, &error))
    }

    fn replace_existing(&self, temp_path: &Path) -> Result<(), StoreError> {
        let target = wide(&self.path);
        let temp = wide(temp_path);
        let backup_path = self.backup_path()?;
        let backup = wide(&backup_path);
        unsafe {
            ReplaceFileW(
                PCWSTR(target.as_ptr()),
                PCWSTR(temp.as_ptr()),
                PCWSTR(backup.as_ptr()),
                REPLACEFILE_WRITE_THROUGH,
                None,
                None,
            )
            .map_err(|error| StoreError::platform(StoreStage::ReplaceTarget, &error))
        }
    }

    fn move_into_place(
        &self,
        source: &Path,
        replace: bool,
        stage: StoreStage,
    ) -> Result<(), StoreError> {
        move_file(source, &self.path, replace).map_err(|error| StoreError::platform(stage, &error))
    }

    fn restore_verified_backup<T>(&self) -> Result<(), StoreError>
    where
        T: DeserializeOwned,
    {
        self.inject(FailurePoint::RestoreBackup, StoreStage::RestoreBackup)?;
        let backup = self.backup_path()?;
        self.validate_path::<T>(&backup, StoreStage::RestoreBackup)?;
        let temp_path = self.unique_sibling("restore", StoreStage::RestoreBackup)?;
        let mut temp_guard = TemporaryGuard::new(temp_path.clone());

        let mut source = File::open(&backup)
            .map_err(|error| StoreError::io(StoreStage::RestoreBackup, &error))?;
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| StoreError::io(StoreStage::RestoreBackup, &error))?;
        io::copy(&mut source, &mut temp)
            .map_err(|error| StoreError::io(StoreStage::RestoreBackup, &error))?;
        temp.sync_all()
            .map_err(|error| StoreError::io(StoreStage::RestoreBackup, &error))?;
        drop(temp);

        self.move_into_place(&temp_path, self.path.exists(), StoreStage::RestoreBackup)?;
        temp_guard.disarm();
        self.validate_path::<T>(&self.path, StoreStage::RestoreBackup)?;
        self.validate_path::<T>(&backup, StoreStage::RestoreBackup)
    }

    fn validate_path<T>(&self, path: &Path, stage: StoreStage) -> Result<(), StoreError>
    where
        T: DeserializeOwned,
    {
        self.read_value::<T>(path, stage).map(|_| ())
    }

    fn read_value<T>(&self, path: &Path, stage: StoreStage) -> Result<T, StoreError>
    where
        T: DeserializeOwned,
    {
        let mut file = File::open(path).map_err(|error| StoreError::io(stage, &error))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| StoreError::io(stage, &error))?;
        serde_json::from_slice(&bytes).map_err(|_| StoreError::json(stage))
    }

    fn inject(&self, point: FailurePoint, stage: StoreStage) -> Result<(), StoreError> {
        self.faults
            .check(point)
            .map_err(|error| StoreError::io(stage, &error))
    }

    fn parent(&self) -> &Path {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn backup_path(&self) -> Result<PathBuf, StoreError> {
        self.sibling("bak", StoreStage::ValidateBackup)
    }

    fn unique_sibling(&self, kind: &str, stage: StoreStage) -> Result<PathBuf, StoreError> {
        let unique = NEXT_UNIQUE_NAME.fetch_add(1, Ordering::Relaxed);
        self.sibling(&format!("{kind}.{}.{}", std::process::id(), unique), stage)
    }

    fn sibling(&self, suffix: &str, stage: StoreStage) -> Result<PathBuf, StoreError> {
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| StoreError::invalid_path(stage))?;
        let mut sibling_name = file_name.to_os_string();
        sibling_name.push(".");
        sibling_name.push(suffix);
        Ok(self.parent().join(sibling_name))
    }
}

fn move_file(source: &Path, destination: &Path, replace: bool) -> windows::core::Result<()> {
    let source = wide(source);
    let destination = wide(destination);
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()), flags) }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

struct TemporaryGuard {
    path: PathBuf,
    armed: bool,
}

impl TemporaryGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
    struct Document {
        generation: u32,
        label: String,
    }

    impl Document {
        fn new(generation: u32) -> Self {
            Self {
                generation,
                label: format!("generation {generation}"),
            }
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_UNIQUE_NAME.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-store-test-{}-{unique}",
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
            let temp = std::env::temp_dir();
            assert!(self.0.starts_with(&temp));
            assert!(self.0.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("claudometer-store-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct PlannedFaults(Mutex<VecDeque<(FailurePoint, io::ErrorKind)>>);

    impl PlannedFaults {
        fn one(point: FailurePoint, kind: io::ErrorKind) -> Self {
            Self(Mutex::new(VecDeque::from([(point, kind)])))
        }

        fn many(faults: impl IntoIterator<Item = (FailurePoint, io::ErrorKind)>) -> Self {
            Self(Mutex::new(faults.into_iter().collect()))
        }
    }

    impl FaultInjector for PlannedFaults {
        fn check(&self, point: FailurePoint) -> io::Result<()> {
            let mut faults = self.0.lock().unwrap();
            if faults
                .front()
                .is_some_and(|(expected, _)| *expected == point)
            {
                let (_, kind) = faults.pop_front().unwrap();
                return Err(io::Error::from(kind));
            }
            Ok(())
        }
    }

    fn loaded(path: &Path) -> Document {
        match AtomicJsonStore::new(path).load().unwrap() {
            LoadOutcome::Loaded(value) | LoadOutcome::RecoveredFromBackup(value) => value,
            LoadOutcome::Missing | LoadOutcome::CorruptPreserved => panic!("document missing"),
        }
    }

    #[test]
    fn first_create_and_replace_keep_a_verified_backup() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::new(&path);
        store.save(&Document::new(1)).unwrap();
        assert_eq!(loaded(&path), Document::new(1));

        store.save(&Document::new(2)).unwrap();
        assert_eq!(loaded(&path), Document::new(2));
        let mut backup: Document =
            serde_json::from_slice(&std::fs::read(path.with_file_name("state.json.bak")).unwrap())
                .unwrap();
        assert_eq!(backup, Document::new(1));

        store.save(&Document::new(3)).unwrap();
        assert_eq!(loaded(&path), Document::new(3));
        backup =
            serde_json::from_slice(&std::fs::read(path.with_file_name("state.json.bak")).unwrap())
                .unwrap();
        assert_eq!(backup, Document::new(2));
    }

    #[test]
    fn precommit_failures_leave_the_previous_file_intact() {
        for (point, kind) in [
            (
                FailurePoint::CreateDirectory,
                io::ErrorKind::PermissionDenied,
            ),
            (
                FailurePoint::CreateTemporary,
                io::ErrorKind::PermissionDenied,
            ),
            (FailurePoint::WriteTemporary, io::ErrorKind::StorageFull),
            (FailurePoint::FlushTemporary, io::ErrorKind::Interrupted),
            (FailurePoint::ValidateExisting, io::ErrorKind::Other),
            (FailurePoint::ReplaceTarget, io::ErrorKind::PermissionDenied),
        ] {
            let directory = TestDirectory::new();
            let path = directory.path();
            AtomicJsonStore::new(&path).save(&Document::new(1)).unwrap();
            let failing =
                AtomicJsonStore::with_fault_injector(&path, PlannedFaults::one(point, kind));
            let error = failing.save(&Document::new(2)).unwrap_err();
            assert_eq!(error.kind, StoreErrorKind::Io(kind));
            assert_eq!(loaded(&path), Document::new(1), "failure at {point:?}");
            assert_eq!(
                std::fs::read_dir(&directory.0)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
                    .count(),
                0,
                "temporary debris after failure at {point:?}"
            );
        }
    }

    #[test]
    fn postcommit_validation_failure_restores_and_retains_backup() {
        let directory = TestDirectory::new();
        let path = directory.path();
        AtomicJsonStore::new(&path).save(&Document::new(1)).unwrap();
        let failing = AtomicJsonStore::with_fault_injector(
            &path,
            PlannedFaults::one(FailurePoint::ValidateCommitted, io::ErrorKind::InvalidData),
        );
        let error = failing.save(&Document::new(2)).unwrap_err();
        assert_eq!(error.stage, StoreStage::ValidateCommitted);
        assert_eq!(loaded(&path), Document::new(1));
        let backup: Document =
            serde_json::from_slice(&std::fs::read(path.with_file_name("state.json.bak")).unwrap())
                .unwrap();
        assert_eq!(backup, Document::new(1));
    }

    #[test]
    fn failed_first_commit_validation_removes_the_unverified_file() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let failing = AtomicJsonStore::with_fault_injector(
            &path,
            PlannedFaults::one(FailurePoint::ValidateCommitted, io::ErrorKind::InvalidData),
        );

        let error = failing.save(&Document::new(1)).unwrap_err();
        assert_eq!(error.stage, StoreStage::ValidateCommitted);
        assert!(!path.exists());
    }

    #[test]
    fn interrupted_restore_leaves_a_valid_target_and_recoverable_backup() {
        let directory = TestDirectory::new();
        let path = directory.path();
        AtomicJsonStore::new(&path).save(&Document::new(1)).unwrap();
        let failing = AtomicJsonStore::with_fault_injector(
            &path,
            PlannedFaults::many([
                (FailurePoint::ValidateCommitted, io::ErrorKind::InvalidData),
                (FailurePoint::RestoreBackup, io::ErrorKind::Interrupted),
            ]),
        );
        let error = failing.save(&Document::new(2)).unwrap_err();
        assert_eq!(error.stage, StoreStage::RestoreBackup);
        assert_eq!(loaded(&path), Document::new(2));
        let backup: Document =
            serde_json::from_slice(&std::fs::read(path.with_file_name("state.json.bak")).unwrap())
                .unwrap();
        assert_eq!(backup, Document::new(1));
    }

    #[test]
    fn malformed_target_is_preserved_and_verified_backup_is_restored() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::new(&path);
        store.save(&Document::new(1)).unwrap();
        store.save(&Document::new(2)).unwrap();
        std::fs::write(&path, b"{malformed").unwrap();

        assert_eq!(
            store.load().unwrap(),
            LoadOutcome::RecoveredFromBackup(Document::new(1))
        );
        assert_eq!(loaded(&path), Document::new(1));
        let corrupt_count = std::fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt."))
            .count();
        assert_eq!(corrupt_count, 1);
    }

    #[test]
    fn missing_target_is_recovered_from_the_verified_backup() {
        let directory = TestDirectory::new();
        let path = directory.path();
        let store = AtomicJsonStore::new(&path);
        store.save(&Document::new(1)).unwrap();
        store.save(&Document::new(2)).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            store.load().unwrap(),
            LoadOutcome::RecoveredFromBackup(Document::new(1))
        );
        assert_eq!(loaded(&path), Document::new(1));
    }

    #[test]
    fn malformed_first_generation_is_preserved_before_defaults() {
        let directory = TestDirectory::new();
        let path = directory.path();
        std::fs::write(&path, b"{malformed").unwrap();
        let store = AtomicJsonStore::new(&path);

        assert_eq!(
            store.load::<Document>().unwrap(),
            LoadOutcome::CorruptPreserved
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

    #[test]
    fn corrupt_preservation_failure_never_discards_the_source() {
        let directory = TestDirectory::new();
        let path = directory.path();
        std::fs::write(&path, b"{malformed").unwrap();
        let store = AtomicJsonStore::with_fault_injector(
            &path,
            PlannedFaults::one(
                FailurePoint::PreserveCorrupt,
                io::ErrorKind::PermissionDenied,
            ),
        );

        let error = store.load::<Document>().unwrap_err();
        assert_eq!(error.stage, StoreStage::PreserveCorrupt);
        assert_eq!(std::fs::read(&path).unwrap(), b"{malformed");
    }
}
