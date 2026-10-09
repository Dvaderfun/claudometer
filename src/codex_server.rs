//! Audited native app-server, ephemeral external auth, and bounded private stdio.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::*;

use crate::api::{bounded_text, MAX_LIMIT_ROWS, MAX_PROVIDER_RESPONSE_BYTES};
use crate::provider::error::{FailureKind, FetchError};
use crate::provider::model::{
    AccountKey, LimitClass, LimitKind, SecretString, SourceProvenance, UsageLimit, UsageSnapshot,
};

const DEADLINE: Duration = Duration::from_secs(10);
const MAX_FRAMES: usize = 64;
const MAX_REQUEST: usize = 16 * 1024;
const REPARSE_POINT: u32 = 0x400;
const SCRATCH_PREFIX: &str = "claudometer-codex-run-";
const AUDITED: &[&str] = &[
    "1203922d910426522182b35a52402085d0955101bb585a87bd7c88110d8d68d8", // 0.159.1 x64
    "fdda5fa3cf3fb3d000b876720742857676293e4315e4b045fae6f8bd7e866d1d", // 0.160.0 x64
];

/// Explicit maintainer-only live sample. Never used by automated tests/startup.
pub fn measure() -> windows::core::Result<()> {
    crate::runtime_state::initialize_read_only();
    let started = Instant::now();
    let mut request = match crate::codex::prepare() {
        Ok(request) => request,
        Err(failure) => {
            return crate::diagnostics::write_stdout(
                format!("{{\"error\":\"{}\"}}\n", failure.error().code()).as_bytes(),
            )
        }
    };
    let executable = match discover() {
        Ok(executable) => executable,
        Err(_) => {
            return crate::diagnostics::write_stdout(b"{\"error\":\"app_server_unavailable\"}\n")
        }
    };
    request.server = Some(executable);
    let outcome = crate::codex::fetch(request);
    let elapsed = started.elapsed().as_millis();
    let result = match outcome {
        crate::provider::model::FetchOutcome::Ok(snapshot) => format!(
            "{{\"source\":\"app_server\",\"elapsed_ms\":{elapsed},\"rows\":{},\"error\":null}}\n",
            snapshot.rows.len()
        ),
        crate::provider::model::FetchOutcome::Failure(error) => format!(
            "{{\"source\":\"app_server\",\"elapsed_ms\":{elapsed},\"error\":\"{}\"}}\n",
            error.code()
        ),
        #[cfg(test)]
        _ => unreachable!(),
    };
    crate::diagnostics::write_stdout(result.as_bytes())
}

pub struct Executable {
    path: PathBuf,
    // Keep the image open without write/delete sharing through process startup.
    _image: File,
}

#[inline(never)]
fn local_error() -> FetchError {
    FetchError::new(FailureKind::Transient)
}
#[inline(never)]
fn invalid() -> FetchError {
    FetchError::new(FailureKind::UnexpectedResponse)
}

fn machine_configuration_absent() -> bool {
    use windows::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let Ok(path) = SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) else {
            return false;
        };
        let text = path.to_string();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0.cast()));
        let Ok(text) = text else { return false };
        let dir = PathBuf::from(text).join("OpenAI/Codex");
        ["config.toml", "requirements.toml"].iter().all(|name| {
            matches!(fs::metadata(dir.join(name)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        })
    }
}

fn audited(path: PathBuf) -> Option<Executable> {
    use std::os::windows::fs::OpenOptionsExt;
    let image = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&path)
        .ok()?;
    let hash = crate::updater::sha256_of(&path)?;
    AUDITED.contains(&hash.as_str()).then_some(Executable {
        path,
        _image: image,
    })
}

pub fn discover() -> Result<Executable, &'static str> {
    if !machine_configuration_absent() {
        return Err("app-server machine configuration");
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        candidates.push(PathBuf::from(home).join(".local/bin/codex.exe"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("codex.exe"));
            // Official npm payload; never execute Node or a batch shim.
            for root in [dir.join("node_modules/@openai"), dir.join("../@openai")] {
                for package in [
                    root.join("codex-win32-x64"),
                    root.join("codex/node_modules/@openai/codex-win32-x64"),
                ] {
                    candidates.push(package.join("vendor/x86_64-pc-windows-msvc/bin/codex.exe"));
                }
            }
        }
    }
    let found = candidates.iter().any(|path| path.is_file());
    for path in candidates {
        if let Some(executable) = audited(path) {
            return Ok(executable);
        }
    }
    Err(if found {
        "app-server version not audited"
    } else {
        "app-server CLI not found"
    })
}

struct Scratch(PathBuf, Option<File>);
impl Scratch {
    fn new() -> Result<Self, FetchError> {
        let root = std::env::temp_dir();
        // A crash can leave only our scratch profile. No reparse point is followed.
        for entry in fs::read_dir(&root).map_err(|_| local_error())? {
            let entry = entry.map_err(|_| local_error())?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(SCRATCH_PREFIX)
            {
                if fs::symlink_metadata(entry.path())
                    .map_err(|_| local_error())?
                    .file_attributes()
                    & REPARSE_POINT
                    != 0
                {
                    continue;
                }
                use std::os::windows::fs::OpenOptionsExt;
                let lock = fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(entry.path().join("owner.lock"));
                match lock {
                    Ok(lock) => drop(lock),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(_) => continue,
                }
                remove_owned(&entry.path()).map_err(|_| local_error())?;
            }
        }
        let mut random = [0u8; 16];
        unsafe {
            windows::Win32::Security::Cryptography::BCryptGenRandom(
                windows::Win32::Security::Cryptography::BCRYPT_ALG_HANDLE::default(),
                &mut random,
                windows::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
            .ok()
            .map_err(|_| local_error())?;
        }
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = root.join(format!("{SCRATCH_PREFIX}{name}"));
        fs::create_dir(&path).map_err(|_| local_error())?;
        let mut scratch = Self(path, None);
        use std::os::windows::fs::OpenOptionsExt;
        scratch.1 = Some(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0)
                .open(scratch.0.join("owner.lock"))
                .map_err(|_| local_error())?,
        );
        fs::write(
            scratch.0.join("model-catalog.json"),
            include_bytes!("../tests/fixtures/codex-app-server/model-catalog.json"),
        )
        .map_err(|_| local_error())?;
        Ok(scratch)
    }
    fn cleanup(&mut self) -> Result<(), FetchError> {
        self.1.take();
        remove_owned(&self.0).map_err(|_| local_error())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn remove_owned(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_attributes() & REPARSE_POINT != 0 {
        return if metadata.is_dir() {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_owned(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn handle(value: &OwnedHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
unsafe fn own(value: HANDLE) -> OwnedHandle {
    OwnedHandle::from_raw_handle(value.0)
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let mut size = 0;
            let _ = InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                2,
                0,
                &mut size,
            );
            let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
            InitializeProcThreadAttributeList(list, 2, 0, &mut size)?;
            Ok(Self {
                _storage: storage,
                list,
            })
        }
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

struct Process {
    job: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
    input: File,
    output: Option<File>,
    stopped: bool,
    pending: Vec<u8>,
}
impl Process {
    fn spawn(
        executable: &Path,
        arguments: &[String],
        directory: &Path,
    ) -> Result<Self, FetchError> {
        unsafe {
            let job = own(CreateJobObjectW(None, None).map_err(|_| local_error())?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(|_| local_error())?;
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                bInheritHandle: true.into(),
                lpSecurityDescriptor: std::ptr::null_mut(),
            };
            let pipe = || -> Result<(OwnedHandle, OwnedHandle), FetchError> {
                let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
                CreatePipe(&mut read, &mut write, Some(&security), 64 * 1024)
                    .map_err(|_| local_error())?;
                Ok((own(read), own(write)))
            };
            let (child_input, input) = pipe()?;
            let (output, child_output) = pipe()?;
            windows::Win32::Foundation::SetHandleInformation(
                handle(&input),
                HANDLE_FLAG_INHERIT.0,
                Default::default(),
            )
            .map_err(|_| local_error())?;
            windows::Win32::Foundation::SetHandleInformation(
                handle(&output),
                HANDLE_FLAG_INHERIT.0,
                Default::default(),
            )
            .map_err(|_| local_error())?;
            let null = own(CreateFileW(
                windows::core::w!("NUL"),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                Some(&security),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(|_| local_error())?);
            let attributes = Attributes::new().map_err(|_| local_error())?;
            let inherited = [handle(&child_input), handle(&child_output), handle(&null)];
            let jobs = [handle(&job)];
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(inherited.as_ptr().cast()),
                std::mem::size_of_val(&inherited),
                None,
                None,
            )
            .map_err(|_| local_error())?;
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_ptr().cast()),
                std::mem::size_of_val(&jobs),
                None,
                None,
            )
            .map_err(|_| local_error())?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = inherited[0];
            startup.StartupInfo.hStdOutput = inherited[1];
            startup.StartupInfo.hStdError = inherited[2];
            startup.lpAttributeList = attributes.list;
            let executable_wide = wide(executable.as_os_str());
            let mut command = wide(std::ffi::OsStr::new(
                &std::iter::once(executable.to_string_lossy().into_owned())
                    .chain(arguments.iter().cloned())
                    .map(|argument| quote(&argument))
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
            let directory_wide = wide(directory.as_os_str());
            let mut environment = Vec::new();
            // Already sorted: CreateProcess expects a Unicode environment block.
            for key in [
                "APPDATA",
                "CODEX_HOME",
                "LOCALAPPDATA",
                "RUST_LOG",
                "SystemRoot",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "WINDIR",
            ] {
                let value = match key {
                    "SystemRoot" | "WINDIR" => std::env::var_os(key).ok_or_else(local_error)?,
                    "RUST_LOG" => "off".into(),
                    _ => directory.as_os_str().to_owned(),
                };
                environment.extend(std::ffi::OsStr::new(key).encode_wide());
                environment.push('=' as u16);
                environment.extend(value.encode_wide());
                environment.push(0);
            }
            environment.push(0);
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessW(
                PCWSTR(executable_wide.as_ptr()),
                PWSTR(command.as_mut_ptr()),
                None,
                None,
                true,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                Some(environment.as_ptr().cast()),
                PCWSTR(directory_wide.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            )
            .map_err(|_| local_error())?;
            let process = own(info.hProcess);
            drop(own(info.hThread));
            Ok(Self {
                job,
                process,
                pid: info.dwProcessId,
                input: File::from(input),
                output: Some(File::from(output)),
                stopped: false,
                pending: Vec::new(),
            })
        }
    }
    fn read(&mut self, deadline: Instant) -> Result<Value, FetchError> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let value = serde_json::from_slice(&self.pending[..end]).map_err(|_| invalid())?;
                self.pending.drain(..=end);
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(FetchError::new(FailureKind::Timeout));
            }
            let output = self.output.as_mut().ok_or_else(local_error)?;
            let mut available = 0;
            unsafe {
                PeekNamedPipe(
                    HANDLE(output.as_raw_handle()),
                    None,
                    0,
                    None,
                    Some(&mut available),
                    None,
                )
            }
            .map_err(|_| invalid())?;
            if available > 0 {
                let mut bytes = [0u8; 8192];
                let count = output
                    .read(&mut bytes[..(available as usize).min(8192)])
                    .map_err(|_| invalid())?;
                if count == 0 || self.pending.len() + count > MAX_PROVIDER_RESPONSE_BYTES {
                    return Err(invalid());
                }
                self.pending.extend_from_slice(&bytes[..count]);
            } else {
                // Event wait yields CPU without a helper thread or blocking pipe read.
                unsafe {
                    WaitForSingleObject(handle(&self.process), 5);
                }
            }
        }
    }
    fn stop(&mut self) -> Result<(), FetchError> {
        if self.stopped {
            return Ok(());
        }
        let system = std::env::var_os("SystemRoot").ok_or_else(local_error)?;
        let _ = Command::new(PathBuf::from(system).join("System32/taskkill.exe"))
            .args(["/T", "/F", "/PID", &self.pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .status();
        unsafe {
            TerminateJobObject(handle(&self.job), 1).map_err(|_| local_error())?;
            if WaitForSingleObject(handle(&self.process), 2000)
                != windows::Win32::Foundation::WAIT_OBJECT_0
            {
                return Err(local_error());
            }
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            QueryInformationJobObject(
                handle(&self.job),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                None,
            )
            .map_err(|_| local_error())?;
            if accounting.ActiveProcesses != 0 {
                return Err(local_error());
            }
        }
        self.stopped = true;
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn quote(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            slashes * if character == '"' { 2 } else { 1 },
        ));
        if character == '"' {
            result.push('\\');
        }
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

fn launch_arguments(directory: &Path) -> Vec<String> {
    let mut args = vec![
        "app-server".into(),
        "--stdio".into(),
        "--strict-config".into(),
    ];
    for setting in [
        "cli_auth_credentials_store=\"ephemeral\"".to_string(),
        "analytics.enabled=false".into(),
        "otel.exporter=\"none\"".into(),
        "otel.trace_exporter=\"none\"".into(),
        "otel.metrics_exporter=\"none\"".into(),
        "features.plugins=false".into(),
        "features.remote_control=false".into(),
        "features.runtime_metrics=false".into(),
        format!(
            "model_catalog_json={}",
            serde_json::to_string(&directory.join("model-catalog.json").to_string_lossy()).unwrap()
        ),
        format!(
            "chatgpt_base_url={}",
            serde_json::to_string(crate::network::CODEX_BACKEND_URL).unwrap()
        ),
    ] {
        args.extend(["-c".into(), setting]);
    }
    args
}

fn send(writer: &mut impl Write, bytes: &str) -> Result<(), FetchError> {
    if bytes.len() > MAX_REQUEST {
        return Err(invalid());
    }
    writer
        .write_all(bytes.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|_| local_error())
}

fn response(identifier: u64, message: Value) -> Result<Option<Value>, FetchError> {
    // Any server request is rejected by ending the cycle; no replacement token or arbitrary action.
    if message.get("method").is_some() && message.get("id").is_some() {
        return Err(FetchError::new(
            if message["method"] == "account/chatgptAuthTokens/refresh" {
                FailureKind::Authentication
            } else {
                FailureKind::UnexpectedResponse
            },
        ));
    }
    if message.get("id").is_some() {
        if message["id"].as_u64() != Some(identifier) {
            return Err(invalid());
        }
        if message.get("error").is_some() {
            return Err(invalid());
        }
        let mut message = message;
        return message
            .as_object_mut()
            .and_then(|object| object.remove("result"))
            .map(Some)
            .ok_or_else(invalid);
    }
    if message.get("method").and_then(Value::as_str).is_none() {
        return Err(invalid());
    }
    Ok(None)
}

fn rpc(
    process: &mut Process,
    deadline: Instant,
    id: u64,
    message: &str,
) -> Result<Value, FetchError> {
    send(&mut process.input, message)?;
    receive_response(process, deadline, id)
}

fn receive_response(
    process: &mut Process,
    deadline: Instant,
    id: u64,
) -> Result<Value, FetchError> {
    for _ in 0..MAX_FRAMES {
        let message = process.read(deadline)?;
        if let Some(result) = response(id, message)? {
            return Ok(result);
        }
    }
    Err(invalid())
}

fn external_login(
    process: &mut Process,
    deadline: Instant,
    request: &crate::codex::PreparedRequest,
) -> Result<Value, FetchError> {
    #[derive(Serialize)]
    struct Login<'a> {
        id: u64,
        method: &'static str,
        params: Tokens<'a>,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Tokens<'a> {
        r#type: &'static str,
        access_token: &'a str,
        chatgpt_account_id: &'a str,
        chatgpt_plan_type: &'static str,
    }
    // Borrow secret strings into the serializer; the single outgoing allocation
    // is wiped by SecretString. No serde_json::Value owns token copies.
    let outgoing = SecretString::new(
        serde_json::to_string(&Login {
            id: 2,
            method: "account/login/start",
            params: Tokens {
                r#type: "chatgptAuthTokens",
                access_token: request.access_token.expose(),
                chatgpt_account_id: request.account_id.expose(),
                // Do not import enterprise cloud policy into a quota reader.
                // Display plan always comes from the backend quota response.
                chatgpt_plan_type: "unknown",
            },
        })
        .map_err(|_| invalid())?,
    );
    if outgoing.expose().len() > MAX_REQUEST {
        return Err(invalid());
    }
    process
        .input
        .write_all(outgoing.expose().as_bytes())
        .and_then(|_| process.input.write_all(b"\n"))
        .and_then(|_| process.input.flush())
        .map_err(|_| local_error())?;
    receive_response(process, deadline, 2)
}

pub fn fetch(
    executable: &Executable,
    request: &crate::codex::PreparedRequest,
) -> Result<UsageSnapshot, FetchError> {
    fetch_at(&executable.path, request, None)
}

fn fetch_at(
    path: &Path,
    request: &crate::codex::PreparedRequest,
    scenario: Option<&str>,
) -> Result<UsageSnapshot, FetchError> {
    let mut scratch = Scratch::new()?;
    if let Some(scenario) = scenario {
        fs::write(scratch.0.join("scenario"), scenario).map_err(|_| local_error())?;
    }
    let deadline = Instant::now() + DEADLINE;
    let mut process = Process::spawn(path, &launch_arguments(&scratch.0), &scratch.0)?;
    let result = (|| {
        rpc(
            &mut process,
            deadline,
            1,
            concat!("{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"claudometer\",\"version\":\"", env!("CARGO_PKG_VERSION"), "\"},\"capabilities\":{\"experimentalApi\":true}}}"),
        )?;
        send(&mut process.input, "{\"method\":\"initialized\"}")?;
        let login = external_login(&mut process, deadline, request)?;
        if login["type"] != "chatgptAuthTokens" {
            return Err(invalid());
        }
        let limits = rpc(
            &mut process,
            deadline,
            3,
            "{\"id\":3,\"method\":\"account/rateLimits/read\",\"params\":{\"excludeResetCreditDetails\":true}}",
        )?;
        parse_limits(
            &limits,
            request.account_id.expose(),
            request.account().key.clone(),
            time::OffsetDateTime::now_utc().unix_timestamp(),
        )
    })();
    let stopped = process.stop();
    drop(process);
    let cleaned = scratch.cleanup();
    stopped?;
    cleaned?;
    result
}

fn parse_limits(
    value: &Value,
    account_id: &str,
    account: AccountKey,
    observed_at: i64,
) -> Result<UsageSnapshot, FetchError> {
    if value
        .get("accountId")
        .and_then(Value::as_str)
        .is_some_and(|id| id != account_id)
    {
        return Err(FetchError::new(FailureKind::Authentication));
    }
    let mut buckets = Vec::new();
    if let Some(map) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        if map.len() > MAX_LIMIT_ROWS {
            return Err(invalid());
        }
        buckets.extend(map.iter().map(|(id, bucket)| (id.as_str(), bucket)));
    } else if let Some(bucket) = value.get("rateLimits").filter(|value| value.is_object()) {
        buckets.push((
            bucket
                .get("limitId")
                .and_then(Value::as_str)
                .unwrap_or("codex"),
            bucket,
        ));
    }
    buckets.sort_by_key(|(id, _)| (*id != "codex", *id));
    let mut rows = Vec::new();
    let mut plan = None;
    for (id, bucket) in buckets {
        if bucket
            .get("limitId")
            .and_then(Value::as_str)
            .is_some_and(|own| own != id)
        {
            return Err(invalid());
        }
        if plan.is_none() {
            plan = bucket
                .get("planType")
                .and_then(Value::as_str)
                .map(crate::codex::plan_label)
                .map(bounded_text);
        }
        for (position, window) in ["primary", "secondary"].into_iter().filter_map(|position| {
            bucket
                .get(position)
                .filter(|value| value.is_object())
                .map(|window| (position, window))
        }) {
            let Some(percent) = window.get("usedPercent").and_then(Value::as_f64) else {
                continue;
            };
            let seconds = window
                .get("windowDurationMins")
                .and_then(Value::as_u64)
                .and_then(|minutes| minutes.checked_mul(60))
                .and_then(|seconds| u32::try_from(seconds).ok())
                .filter(|seconds| *seconds > 0);
            let (kind, label) = match seconds {
                Some(seconds) if seconds <= 86400 => (
                    LimitKind::Session,
                    if seconds % 3600 == 0 {
                        format!("Session ({}h)", seconds / 3600)
                    } else {
                        "Session".into()
                    },
                ),
                Some(604800) => (LimitKind::Weekly, "Weekly".into()),
                Some(seconds) => (
                    LimitKind::Other("window".into()),
                    format!("{}-day", seconds / 86400),
                ),
                None => (LimitKind::Other("unknown_window".into()), "Usage".into()),
            };
            let identity = format!(
                "{id}:{}",
                seconds.map_or_else(|| position.to_string(), |seconds| seconds.to_string())
            );
            let label = if id == "codex" {
                label
            } else {
                format!(
                    "{} · {label}",
                    bucket
                        .get("limitName")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                )
            };
            if let Some(row) = UsageLimit::from_adapter(
                identity,
                if id == "codex" {
                    kind
                } else {
                    LimitKind::Model
                },
                bounded_text(label),
                percent,
                None,
                window.get("resetsAt").and_then(Value::as_i64),
                seconds,
            ) {
                rows.push(row);
            }
        }
        if let Some(spend) = bucket
            .get("individualLimit")
            .filter(|value| value.is_object())
        {
            if let Some(left) = spend.get("remainingPercent").and_then(Value::as_f64) {
                if let Some(mut row) = UsageLimit::from_adapter(
                    format!("{id}:spend"),
                    LimitKind::ExtraUsage,
                    "Spend limit".into(),
                    100.0 - left,
                    None,
                    spend.get("resetsAt").and_then(Value::as_i64),
                    None,
                ) {
                    row.class = LimitClass::Spend;
                    row.severity = bucket
                        .get("spendControlReached")
                        .and_then(Value::as_bool)
                        .filter(|reached| *reached)
                        .map(|_| crate::provider::model::ProviderSeverity::Critical);
                    rows.push(row);
                }
            }
        }
        if rows.len() > MAX_LIMIT_ROWS {
            return Err(invalid());
        }
    }
    if rows.is_empty() {
        return Err(invalid());
    }
    if rows
        .iter()
        .enumerate()
        .any(|(index, row)| rows[..index].iter().any(|previous| previous.id == row.id))
    {
        return Err(invalid());
    }
    Ok(UsageSnapshot {
        provider: crate::provider::model::ProviderId::Codex,
        account,
        source: SourceProvenance::codex_app_server(),
        rows,
        plan,
        fetched_unix: observed_at,
        reset_credits_available: value
            .get("rateLimitResetCredits")
            .and_then(|credits| credits.get("availableCount"))
            .and_then(Value::as_u64)
            .and_then(|count| u32::try_from(count).ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader};
    use std::sync::OnceLock;
    fn read_frame(reader: &mut impl BufRead) -> Result<Value, FetchError> {
        let mut bytes = Vec::new();
        reader
            .take((MAX_PROVIDER_RESPONSE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > MAX_PROVIDER_RESPONSE_BYTES || !bytes.ends_with(b"\n") {
            return Err(invalid());
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }
    fn fake() -> &'static Path {
        static FAKE: OnceLock<PathBuf> = OnceLock::new();
        FAKE.get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let output = root.join("target/codex-fake-server.exe");
            let status = Command::new("rustc")
                .arg(root.join("tests/fixtures/codex-app-server/fake-server.rs"))
                .args(["--edition=2021", "-o"])
                .arg(&output)
                .status()
                .unwrap();
            assert!(status.success());
            output
        })
    }
    #[test]
    fn fixture_preserves_identity_dynamic_windows_spend_and_credits() {
        let value: Value = serde_json::from_slice(include_bytes!(
            "../tests/fixtures/codex-app-server/limits.json"
        ))
        .unwrap();
        let snapshot = parse_limits(
            &value,
            "synthetic-account",
            AccountKey::from_digest([2; 32]),
            1000,
        )
        .unwrap();
        assert_eq!(snapshot.rows.len(), 4);
        assert_eq!(snapshot.rows[0].kind, LimitKind::Weekly);
        assert_eq!(snapshot.rows[1].kind, LimitKind::Session);
        assert_eq!(snapshot.rows[1].resets_unix, None);
        assert_eq!(snapshot.rows[2].class, LimitClass::Spend);
        assert_eq!(snapshot.rows[3].kind, LimitKind::Model);
        assert_eq!(snapshot.reset_credits_available, Some(2));
        assert_eq!(snapshot.plan.as_deref(), Some("Pro 200"));
        assert!(parse_limits(
            &value,
            "other-account",
            AccountKey::from_digest([2; 32]),
            1000
        )
        .is_err());
        for (plan, label) in [
            ("prolite", "Pro 100"),
            ("pro", "Pro 200"),
            ("promax", "Pro 500"),
            ("self_serve_business_prolite", "Business Premium"),
        ] {
            assert_eq!(crate::codex::plan_label(plan), label);
        }
        let partial = serde_json::json!({"rateLimits":{"primary":{"usedPercent":12}}});
        let unknown = parse_limits(
            &partial,
            "synthetic-account",
            AccountKey::from_digest([2; 32]),
            1000,
        )
        .unwrap();
        assert!(matches!(unknown.rows[0].kind, LimitKind::Other(_)));
        assert!(unknown.rows[0].window_seconds.is_none());
    }
    #[test]
    fn protocol_refuses_refresh_actions_wrong_ids_and_raw_errors() {
        for value in [
            json!({"id":2,"result":{}}),
            json!({"id":1,"error":{"message":"private-token 401"}}),
            json!({"id":1}),
            json!({}),
        ] {
            assert!(response(1, value).is_err());
        }
        assert_eq!(
            response(
                1,
                json!({"id":9,"method":"account/chatgptAuthTokens/refresh"})
            )
            .err()
            .unwrap()
            .kind,
            FailureKind::Authentication
        );
        assert!(response(1, json!({"id":9,"method":"account/logout"})).is_err());
        assert!(response(1, json!({"method":"account/updated"}))
            .unwrap()
            .is_none());
        for data in [b"not-json\n".as_slice(), b"{\"id\":1}".as_slice()] {
            assert!(read_frame(&mut std::io::Cursor::new(data)).is_err());
        }
        assert!(read_frame(&mut std::io::Cursor::new(vec![
            b' ';
            MAX_PROVIDER_RESPONSE_BYTES
                + 1
        ]))
        .is_err());
    }
    #[test]
    fn fake_process_success_missing_malformed_refresh_and_tree_cleanup() {
        let mut request = crate::codex::PreparedRequest::synthetic();
        request.server = Some(Executable {
            path: fake().to_path_buf(),
            _image: File::open(fake()).unwrap(),
        });
        match crate::codex::fetch(request) {
            crate::provider::model::FetchOutcome::Ok(snapshot) => {
                assert_eq!(snapshot.source, SourceProvenance::codex_app_server())
            }
            _ => panic!("documented success must return without a direct compatibility request"),
        }
        for (scenario, expected) in [
            ("success", None),
            ("missing", Some(FailureKind::UnexpectedResponse)),
            ("malformed", Some(FailureKind::UnexpectedResponse)),
            ("refresh", Some(FailureKind::Authentication)),
        ] {
            let result = fetch_at(
                fake(),
                &crate::codex::PreparedRequest::synthetic(),
                Some(scenario),
            );
            assert_eq!(result.err().map(|error| error.kind), expected);
        }
        // A fake hung root reports its descendant PID before blocking. Kill the
        // job and prove both handles signal, with no sleep or provider network.
        let mut scratch = Scratch::new().unwrap();
        fs::write(scratch.0.join("scenario"), "hung").unwrap();
        let mut process =
            Process::spawn(fake(), &launch_arguments(&scratch.0), &scratch.0).unwrap();
        let mut output = BufReader::new(process.output.take().unwrap());
        send(
            &mut process.input,
            "{\"id\":3,\"method\":\"account/rateLimits/read\"}",
        )
        .unwrap();
        let notification = read_frame(&mut output).unwrap();
        let pid = notification["params"]["pid"].as_u64().unwrap() as u32;
        let descendant = unsafe { own(OpenProcess(PROCESS_SYNCHRONIZE, false, pid).unwrap()) };
        assert_eq!(
            rpc(
                &mut process,
                Instant::now(),
                4,
                "{\"id\":4,\"method\":\"account/read\",\"params\":{\"refreshToken\":false}}"
            )
            .err()
            .unwrap()
            .kind,
            FailureKind::Timeout
        );
        process.stop().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(handle(&descendant), 2000) },
            windows::Win32::Foundation::WAIT_OBJECT_0
        );
        drop(output);
        drop(process);
        scratch.cleanup().unwrap();
        assert!(!scratch.0.exists());
    }
    #[test]
    fn launch_pins_ephemeral_auth_and_disables_inherited_features() {
        let args = launch_arguments(Path::new("C:/synthetic profile"));
        for setting in [
            "cli_auth_credentials_store=\"ephemeral\"",
            "analytics.enabled=false",
            "features.plugins=false",
            "features.remote_control=false",
        ] {
            assert!(args.iter().any(|argument| argument == setting));
        }
        assert_eq!(quote("path\\"), "\"path\\\\\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert!(audited(fake().to_path_buf()).is_none());
    }
}
