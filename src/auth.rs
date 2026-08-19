//! Claude account integration through the official Claude Code CLI.
//!
//! Claudometer is deliberately not an OAuth client: it never performs a token
//! exchange and never writes `.credentials.json`. Claude Code owns the browser
//! flow, the refresh-token rotation and the credential file. This module only
//!
//!   * reads the account identity Claude Code already cached on disk, and
//!   * hands the CLI its *own* refresh token so it can rotate credentials
//!     non-interactively (`CLAUDE_CODE_OAUTH_REFRESH_TOKEN` + `…_SCOPES`,
//!     the documented headless login path).
//!
//! Note `claude auth status` is *not* used for repair: it is a purely local
//! read that never touches the network, so it can never fix an expired token.

use serde::Deserialize;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const REPAIR_COOLDOWN: Duration = Duration::from_secs(5 * 60);

static BROKER: Mutex<()> = Mutex::new(());
static CONNECTION: Mutex<Option<ClaudeConnection>> = Mutex::new(None);
static INTERACTIVE_BUSY: AtomicBool = AtomicBool::new(false);
static LAST_REPAIR: Mutex<Option<Instant>> = Mutex::new(None);

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
    INTERACTIVE_BUSY.store(false, Ordering::SeqCst);
}

// ---------- identity (local reads, no process spawn) ----------

/// Account identity as Claude Code last cached it in `~/.claude.json`. Costs a
/// file read — `claude auth status` would cost a CLI launch for the same data.
pub fn query_status() -> ClaudeConnection {
    let Some(creds) = crate::api::credentials_summary() else {
        return ClaudeConnection::Disconnected;
    };
    let account = read_oauth_account();
    let email = account
        .as_ref()
        .and_then(|a| a.email_address.clone())
        .or_else(|| account.as_ref().and_then(|a| a.organization_name.clone()))
        .unwrap_or_else(|| "Connected account".into());
    let plan = local_plan().unwrap_or_default();
    if !creds.has_refresh_token && creds.expired {
        return ClaudeConnection::Problem(format!("{email} · sign-in expired"));
    }
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
    let home = std::env::var("USERPROFILE").ok()?;
    let raw = std::fs::read_to_string(std::path::Path::new(&home).join(".claude.json")).ok()?;
    serde_json::from_str::<ClaudeJson>(&raw).ok()?.oauth_account
}

/// Plan name from the profile Claude Code refreshes (`organizationType`).
/// `.credentials.json`'s `subscriptionType` is written once at login and goes
/// stale across plan changes — never read it for display.
pub fn local_plan() -> Option<String> {
    let ty = read_oauth_account()?.organization_type?;
    Some(crate::api::plan_label(ty.strip_prefix("claude_").unwrap_or(&ty)))
}

// ---------- repair ----------

/// Non-interactive credential repair. Hands Claude Code its own refresh token
/// so *it* performs the exchange and rewrites `.credentials.json`; Claudometer
/// only re-reads the result. Serialized and cooled down so a dead account never
/// spawns a CLI process on every poll. Returns true only when the access-token
/// expiry actually moved forward — a "successful" no-op must not trigger a
/// pointless retry of the usage request.
pub fn repair_session_if_due() -> bool {
    if INTERACTIVE_BUSY.load(Ordering::SeqCst) {
        return false;
    }
    let Some(creds) = crate::api::credentials_summary() else {
        return false;
    };
    let (Some(refresh_token), true) = (creds.refresh_token, !creds.scopes.is_empty()) else {
        return false;
    };
    {
        let Ok(mut last) = LAST_REPAIR.lock() else {
            return false;
        };
        if last.is_some_and(|t| t.elapsed() < REPAIR_COOLDOWN) {
            return false;
        }
        *last = Some(Instant::now());
    }

    let Ok(_guard) = BROKER.try_lock() else {
        return false;
    };
    let before = creds.expires_at;
    let ok = matches!(
        run_claude(
            &["auth", "login"],
            &[
                ("CLAUDE_CODE_OAUTH_REFRESH_TOKEN", refresh_token.as_str()),
                ("CLAUDE_CODE_OAUTH_SCOPES", creds.scopes.join(" ").as_str()),
            ],
            REFRESH_TIMEOUT,
        ),
        Ok(Run::Ok)
    );
    let advanced = ok
        && crate::api::credentials_summary().is_some_and(|c| c.expires_at > before);
    if advanced {
        store(query_status());
    }
    advanced
}

/// Let Claude Code own the complete browser login and token persistence flow.
pub fn login() -> ClaudeConnection {
    let Ok(_guard) = BROKER.lock() else {
        return ClaudeConnection::Problem("Couldn't start Claude sign-in".into());
    };
    match run_claude(&["auth", "login", "--claudeai"], &[], LOGIN_TIMEOUT) {
        Ok(Run::Ok) => query_status(),
        Ok(Run::CliMissing) => ClaudeConnection::CliUnavailable,
        Ok(Run::Failed) => ClaudeConnection::Problem("Claude sign-in wasn't completed".into()),
        Err(RunError::TimedOut) => ClaudeConnection::Problem("Claude sign-in timed out".into()),
        Err(RunError::Launch) => ClaudeConnection::CliUnavailable,
    }
}

fn store(connection: ClaudeConnection) {
    *CONNECTION.lock().unwrap() = Some(connection);
}

// ---------- CLI plumbing ----------

enum Run {
    Ok,
    Failed,
    CliMissing,
}

enum RunError {
    Launch,
    TimedOut,
}

/// `claude` on Windows is a `.cmd` shim, so it goes through cmd.exe. Args are
/// passed as a vector (no shell quoting) and both pipes are drained on reader
/// threads — `wait_with_output()` after the child exits would deadlock if the
/// CLI ever outran the 4 KB pipe buffer.
fn run_claude(args: &[&str], env: &[(&str, &str)], timeout: Duration) -> Result<Run, RunError> {
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/D", "/S", "/C", "claude"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|_| RunError::Launch)?;
    let stderr = drain(child.stderr.take());

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                kill(&mut child);
                return Err(RunError::TimedOut);
            }
            Err(_) => return Err(RunError::Launch),
        }
    };
    let stderr = stderr.join().unwrap_or_default();
    if cli_unavailable(&stderr, status.code()) {
        return Ok(Run::CliMissing);
    }
    Ok(if status.success() { Run::Ok } else { Run::Failed })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn cli_unavailable(stderr: &str, code: Option<i32>) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("is not recognized") || stderr.contains("command not found") || code == Some(9009)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_cli() {
        assert!(cli_unavailable(
            "'claude' is not recognized as an internal or external command",
            Some(9009)
        ));
        assert!(cli_unavailable("", Some(9009)));
        assert!(!cli_unavailable("Login failed: network error", Some(1)));
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
