//! Claude account integration through the official Claude Code credential.
//!
//! Normal operation is process-free and read-only: Claudometer reads the
//! access token Claude Code already stored and calls the usage API directly.
//! Claude Code alone owns refresh-token rotation and credential writes. A CLI
//! process is launched only when the user explicitly asks to connect again.

use serde::Deserialize;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The browser sign-in ends on a *hosted* callback page that shows a code the
/// user pastes back into the CLI — there is no loopback listener. So the login
/// child needs a real console with real stdin; a hidden, stdin-less child can
/// only sit at "Paste code here if prompted >" until it times out.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

static BROKER: Mutex<()> = Mutex::new(());
static CONNECTION: Mutex<Option<ClaudeConnection>> = Mutex::new(None);
static INTERACTIVE_BUSY: AtomicBool = AtomicBool::new(false);
static CANCEL_LOGIN: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub enum ClaudeConnection {
    Connected { email: String, plan: String },
    Disconnected,
    CliUnavailable,
    Problem(String),
}

#[derive(Clone)]
pub struct Snapshot {
    pub connection: Option<ClaudeConnection>,
    pub busy: bool,
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        connection: CONNECTION.lock().unwrap().clone(),
        busy: INTERACTIVE_BUSY.load(Ordering::SeqCst),
    }
}

/// Reserve the account broker for a user-visible status/login operation.
pub fn begin_interactive() -> bool {
    INTERACTIVE_BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

pub fn finish_interactive(connection: ClaudeConnection) {
    store(connection);
    CANCEL_LOGIN.store(false, Ordering::SeqCst);
    INTERACTIVE_BUSY.store(false, Ordering::SeqCst);
}

/// Sign-in happens in a console the user drives, so it can outlive their
/// interest in it. Asking again while it runs abandons it instead of being
/// ignored — otherwise the card reads "waiting" for the full timeout.
pub fn cancel_interactive() {
    CANCEL_LOGIN.store(true, Ordering::SeqCst);
}

// ---------- identity (local reads, no process spawn) ----------

/// Account identity as Claude Code last cached it in `~/.claude.json`. Costs a
/// file read — `claude auth status` would cost a CLI launch for the same data.
pub fn query_status() -> ClaudeConnection {
    if !crate::api::credentials_available() {
        return ClaudeConnection::Disconnected;
    }
    let account = read_oauth_account();
    let email = account
        .as_ref()
        .and_then(|a| a.email_address.clone())
        .or_else(|| account.as_ref().and_then(|a| a.organization_name.clone()))
        .unwrap_or_else(|| "Connected account".into());
    let plan = local_plan().unwrap_or_default();
    ClaudeConnection::Connected { email, plan }
}

#[derive(Deserialize)]
struct ClaudeJson {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(Deserialize)]
struct OauthAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    #[serde(rename = "organizationName")]
    organization_name: Option<String>,
    #[serde(rename = "organizationType")]
    organization_type: Option<String>,
}

fn read_oauth_account() -> Option<OauthAccount> {
    let raw = std::fs::read_to_string(crate::api::claude_state_path()?).ok()?;
    serde_json::from_str::<ClaudeJson>(&raw).ok()?.oauth_account
}

/// Plan name from the profile Claude Code refreshes (`organizationType`).
/// `.credentials.json`'s `subscriptionType` is written once at login and goes
/// stale across plan changes — never read it for display.
pub fn local_plan() -> Option<String> {
    let ty = read_oauth_account()?.organization_type?;
    Some(crate::api::plan_label(ty.strip_prefix("claude_").unwrap_or(&ty)))
}

/// Let Claude Code own the complete browser login and token persistence flow,
/// in its own console window so the user can read the fallback URL and paste
/// the callback code. Completion is detected from the credentials file rather
/// than from process exit: the console may linger after the CLI is done, and
/// its exit code says nothing about whether credentials were actually written.
pub fn login() -> ClaudeConnection {
    let Ok(_guard) = BROKER.lock() else {
        return ClaudeConnection::Problem("Couldn't start Claude sign-in".into());
    };
    // Check first — opening an empty console for a missing CLI helps nobody.
    if !cli_present() {
        return ClaudeConnection::CliUnavailable;
    }
    let before = credentials_stamp();
    let Some(program) = find_claude() else {
        return ClaudeConnection::CliUnavailable;
    };
    let mut command = program.command(&["auth", "login"]);
    let mut child = match command.creation_flags(CREATE_NEW_CONSOLE).spawn() {
        Ok(child) => child,
        Err(_) => return ClaudeConnection::CliUnavailable,
    };

    let started = Instant::now();
    loop {
        if credentials_stamp().is_some_and(|now| Some(now) != before) {
            // Credentials changed: sign-in landed. Leave the console alone —
            // it closes itself once the CLI prints its result.
            return query_status();
        }
        if CANCEL_LOGIN.load(Ordering::SeqCst) {
            kill(&mut child);
            return query_status();
        }
        match child.try_wait() {
            // A successful CLI exit is authoritative even if a filesystem has
            // coarse timestamps and the credential stamp did not visibly move.
            Ok(Some(status)) if status.success() => return query_status(),
            Ok(Some(_)) => return ClaudeConnection::Problem("Sign-in wasn't completed".into()),
            Ok(None) if started.elapsed() < LOGIN_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(400));
            }
            Ok(None) => {
                kill(&mut child);
                return ClaudeConnection::Problem("Sign-in timed out".into());
            }
            Err(_) => return ClaudeConnection::Problem("Sign-in wasn't completed".into()),
        }
    }
}

/// File identity is a more reliable login marker than `expiresAt`: current
/// Claude Code regressions have produced valid freshly-written credentials
/// whose expiry field is zero.
fn credentials_stamp() -> Option<crate::api::CredentialsStamp> {
    crate::api::credentials_stamp()
}

fn cli_present() -> bool {
    find_claude().is_some()
}

#[derive(Clone, Debug)]
enum ClaudeProgram {
    Executable(PathBuf),
    Batch(PathBuf),
}

impl ClaudeProgram {
    fn command(&self, args: &[&str]) -> Command {
        match self {
            Self::Executable(path) => {
                let mut command = Command::new(path);
                command.args(args);
                command
            }
            Self::Batch(path) => {
                let mut command = Command::new("cmd.exe");
                command.args(["/D", "/S", "/C", path.to_string_lossy().as_ref()]);
                command.args(args);
                command
            }
        }
    }
}

/// Prefer the recommended native install, then a real executable on PATH. The
/// current official npm shim contains `node_modules/.../bin/claude.exe`; launch
/// that payload directly so an explicit login does not detour through CMD.
fn find_claude() -> Option<ClaudeProgram> {
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let native = PathBuf::from(home).join(".local/bin/claude.exe");
        if native.is_file() {
            return Some(ClaudeProgram::Executable(native));
        }
    }

    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    for dir in &dirs {
        for name in ["claude.exe", "claude.com"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(ClaudeProgram::Executable(candidate));
            }
        }
    }
    for dir in &dirs {
        for name in ["claude.cmd", "claude.bat"] {
            let shim = dir.join(name);
            if !shim.is_file() {
                continue;
            }
            if let Some(embedded) = embedded_npm_executable(&shim) {
                return Some(ClaudeProgram::Executable(embedded));
            }
            return Some(ClaudeProgram::Batch(shim));
        }
    }
    None
}

fn embedded_npm_executable(shim: &Path) -> Option<PathBuf> {
    let candidate = shim
        .parent()?
        .join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
    candidate.is_file().then_some(candidate)
}

fn store(connection: ClaudeConnection) {
    *CONNECTION.lock().unwrap() = Some(connection);
}

/// `Child::kill` terminates only the `cmd.exe` shim — `claude.exe` and its
/// console window would survive as orphans. Take the whole tree down.
fn kill(child: &mut Child) {
    let _ = Command::new("taskkill.exe")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_official_npm_shim_to_embedded_executable() {
        let root = std::env::temp_dir().join(format!("claudometer-auth-test-{}", std::process::id()));
        let bin = root.join("node_modules/@anthropic-ai/claude-code/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = root.join("claude.cmd");
        let executable = bin.join("claude.exe");
        std::fs::write(&shim, "@echo off").unwrap();
        std::fs::write(&executable, []).unwrap();

        assert_eq!(embedded_npm_executable(&shim), Some(executable));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plan_label_strips_claude_prefix() {
        assert_eq!(
            crate::api::plan_label("claude_pro".strip_prefix("claude_").unwrap()),
            "Pro"
        );
        assert_eq!(crate::api::plan_label("max"), "Max");
        assert_eq!(crate::api::plan_label("enterprise"), "Enterprise");
    }
}
