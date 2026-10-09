//! Fetches usage limits from the same endpoint Claude Code's `/usage` uses.
//! Read-only: Claudometer never exchanges refresh tokens or writes credentials.
//! Claude Code alone owns its rotating OAuth session; this module trusts the
//! API response instead of treating the local `expiresAt` hint as authoritative.

use std::fs::File;
use std::io::{BufReader, Read};

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::Time::{
    SystemTimeToTzSpecificLocalTimeEx, DYNAMIC_TIME_ZONE_INFORMATION,
};

use crate::provider::error::{request_error, FailureKind, FetchError};
use crate::provider::model::{
    derive_account_context, AccountContext, AccountKey, FetchOutcome, LimitKind, ProviderId,
    ProviderSeverity, SecretString, SourceProvenance, UsageLimit, UsageSnapshot,
};

/// Authoritative plan/identity. `.credentials.json`'s `subscriptionType` is
/// written once at login and survives plan changes unchanged, so it reports
/// "max" long after a downgrade — this endpoint is the only source that moves.
/// The plan changes at most monthly; one profile round-trip per hour is plenty.
const PLAN_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

pub(crate) const MAX_PROVIDER_RESPONSE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_LIMIT_ROWS: usize = 64;
pub(crate) const MAX_DISPLAY_BYTES: usize = 512;

struct PlanCache {
    account: AccountKey,
    plan: String,
    observed_at: std::time::Instant,
}

static PLAN_CACHE: std::sync::Mutex<Option<PlanCache>> = std::sync::Mutex::new(None);

// ---------- credentials ----------

#[derive(Deserialize)]
struct CredsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Oauth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Oauth {
    access_token: SecretString,
    expires_at: Option<i64>,
    scopes: Option<Vec<String>>,
}

pub struct PreparedRequest {
    access_token: SecretString,
    account: AccountContext,
    local_plan: Option<String>,
}

impl PreparedRequest {
    pub fn account(&self) -> &AccountContext {
        &self.account
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparationFailureKind {
    Missing,
    TemporarilyUnreadable,
    Malformed,
    Unsupported,
    IdentityUnavailable,
    UsageScope,
}

pub struct PreparationFailure {
    pub kind: PreparationFailureKind,
    pub message: &'static str,
}

impl PreparationFailure {
    pub fn invalidates_account(&self) -> bool {
        self.kind != PreparationFailureKind::TemporarilyUnreadable
    }

    pub fn error(&self) -> FetchError {
        FetchError::new(match self.kind {
            PreparationFailureKind::Missing | PreparationFailureKind::Unsupported => {
                FailureKind::MissingCredentials
            }
            PreparationFailureKind::UsageScope => FailureKind::UsageScope,
            PreparationFailureKind::TemporarilyUnreadable
            | PreparationFailureKind::IdentityUnavailable => FailureKind::Transient,
            PreparationFailureKind::Malformed => FailureKind::UnexpectedResponse,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialReadError {
    Missing,
    TemporarilyUnreadable,
    Malformed,
}

pub(crate) fn read_json_with_retry<T, R>(
    mut open: impl FnMut() -> std::io::Result<R>,
    mut wait: impl FnMut(),
) -> Result<T, CredentialReadError>
where
    T: serde::de::DeserializeOwned,
    R: Read,
{
    let mut file_seen = false;
    let mut malformed = false;
    for attempt in 0..3 {
        match open() {
            Ok(reader) => {
                file_seen = true;
                if let Ok(value) = serde_json::from_reader(BufReader::new(reader)) {
                    return Ok(value);
                }
                malformed = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => file_seen = true,
        }
        if attempt < 2 {
            wait();
        }
    }
    if malformed {
        Err(CredentialReadError::Malformed)
    } else if file_seen {
        Err(CredentialReadError::TemporarilyUnreadable)
    } else {
        Err(CredentialReadError::Missing)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CredentialsStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    expires_at: i64,
}

pub fn credentials_available() -> bool {
    read_credentials().is_ok()
}

pub fn credentials_stamp() -> Option<CredentialsStamp> {
    let path = credentials_path()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let expires_at = read_credentials()
        .ok()?
        .oauth
        .expires_at
        .unwrap_or_default();
    Some(CredentialsStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
        expires_at,
    })
}

fn claude_config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".claude"))
        })
}

fn credentials_path() -> Option<std::path::PathBuf> {
    Some(claude_config_dir()?.join(".credentials.json"))
}

/// Claude Code places `.claude.json` inside a custom config directory, but at
/// the profile root for the default configuration.
pub fn claude_state_path() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Some(std::path::PathBuf::from(dir).join(".claude.json"));
    }
    std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".claude.json"))
}

#[derive(Deserialize)]
struct ClaudeIdentityFile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<ClaudeIdentity>,
}

#[derive(Deserialize)]
struct ClaudeIdentity {
    #[serde(rename = "accountUuid")]
    account_uuid: Option<String>,
    #[serde(rename = "organizationUuid")]
    organization_uuid: Option<String>,
    #[serde(rename = "organizationType")]
    organization_type: Option<String>,
}

struct LocalIdentity {
    stable_identifier: Option<SecretString>,
    plan: Option<String>,
}

fn local_identity() -> LocalIdentity {
    let account = claude_state_path()
        .and_then(|path| File::open(path).ok())
        .and_then(|file| {
            serde_json::from_reader::<_, ClaudeIdentityFile>(BufReader::new(file)).ok()
        })
        .and_then(|identity| identity.oauth_account);
    let Some(account) = account else {
        return LocalIdentity {
            stable_identifier: None,
            plan: None,
        };
    };
    let stable_identifier = account
        .account_uuid
        .or(account.organization_uuid)
        .filter(|value| !value.is_empty())
        .map(SecretString::new);
    let plan = account
        .organization_type
        .map(|kind| plan_label(kind.strip_prefix("claude_").unwrap_or(&kind)));
    LocalIdentity {
        stable_identifier,
        plan,
    }
}

// ---------- API response ----------

#[derive(Deserialize)]
struct ProfileResp {
    account: Option<ProfileAccount>,
    organization: Option<ProfileOrg>,
}

#[derive(Deserialize)]
struct ProfileAccount {
    has_claude_max: Option<bool>,
    has_claude_pro: Option<bool>,
}

#[derive(Deserialize)]
struct ProfileOrg {
    organization_type: Option<String>,
}

#[derive(Deserialize)]
struct UsageResp {
    limits: Option<Vec<ApiLimit>>,
    five_hour: Option<Bucket>,
    seven_day: Option<Bucket>,
    extra_usage: Option<ExtraUsage>,
}

#[derive(Deserialize)]
struct ApiLimit {
    kind: Option<String>,
    percent: Option<f64>,
    severity: Option<String>,
    resets_at: Option<String>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<ScopeModel>,
}

#[derive(Deserialize)]
struct ScopeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Bucket {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ExtraUsage {
    is_enabled: Option<bool>,
    utilization: Option<f64>,
}

pub fn prepare() -> Result<PreparedRequest, PreparationFailure> {
    let credentials = read_credentials()?;
    validate_usage_scope(&credentials.oauth)?;
    if credentials.oauth.access_token.is_empty() {
        return Err(PreparationFailure {
            kind: PreparationFailureKind::Malformed,
            message: "Claude credentials are malformed.",
        });
    }
    let access_token = credentials.oauth.access_token;
    let local_identity = local_identity();
    let install_salt = crate::runtime_state::install_salt();
    let account = derive_account_context(
        install_salt.as_ref().map(|salt| salt.as_bytes()),
        ProviderId::Claude,
        local_identity.stable_identifier.as_ref(),
        &access_token,
    )
    .map_err(|_| PreparationFailure {
        kind: PreparationFailureKind::IdentityUnavailable,
        message: "Claude account identity is unavailable.",
    })?;
    Ok(PreparedRequest {
        access_token,
        account,
        local_plan: local_identity.plan,
    })
}

fn validate_usage_scope(oauth: &Oauth) -> Result<(), PreparationFailure> {
    if oauth
        .scopes
        .as_ref()
        .is_some_and(|scopes| !scopes.iter().any(|scope| scope == "user:profile"))
    {
        return Err(PreparationFailure {
            kind: PreparationFailureKind::UsageScope,
            message: "Sign in again for live usage",
        });
    }
    Ok(())
}

pub fn fetch(request: PreparedRequest) -> FetchOutcome {
    match fetch_inner(request) {
        Ok(s) => FetchOutcome::Ok(s),
        Err(error) => FetchOutcome::Failure(error),
    }
}

pub(crate) type FetchErr = FetchError;

pub(crate) fn plain(_msg: &'static str) -> FetchErr {
    FetchError::new(FailureKind::UnexpectedResponse)
}

fn read_credentials() -> Result<CredsFile, PreparationFailure> {
    let path = credentials_path().ok_or(PreparationFailure {
        kind: PreparationFailureKind::Unsupported,
        message: "Claude config path unavailable.",
    })?;
    match read_json_with_retry(
        || File::open(&path),
        || std::thread::sleep(std::time::Duration::from_millis(25)),
    ) {
        Ok(credentials) => Ok(credentials),
        Err(CredentialReadError::Malformed) => Err(PreparationFailure {
            kind: PreparationFailureKind::Malformed,
            message: "Claude credentials are malformed.",
        }),
        Err(CredentialReadError::TemporarilyUnreadable) => Err(PreparationFailure {
            kind: PreparationFailureKind::TemporarilyUnreadable,
            message: "Claude credentials are temporarily unavailable.",
        }),
        Err(CredentialReadError::Missing) => Err(PreparationFailure {
            kind: PreparationFailureKind::Missing,
            message: "Not signed in.\nOpen Settings to connect.",
        }),
    }
}

fn fetch_inner(request: PreparedRequest) -> Result<UsageSnapshot, FetchErr> {
    let tls = native_tls::TlsConnector::new().map_err(|_| FetchError::new(FailureKind::Offline))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let authorization = SecretString::new(format!("Bearer {}", request.access_token.expose()));
    let response = || -> Result<ureq::Response, FetchError> {
        crate::network::get(&agent, crate::network::ANTHROPIC_USAGE_URL)
            .set("Authorization", authorization.expose())
            .set("anthropic-beta", "oauth-2025-04-20")
            .set(
                "User-Agent",
                concat!("claudometer/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(|error| request_error(error, OffsetDateTime::now_utc().unix_timestamp()))
    };
    let resp = response()?;

    let body = read_bounded(resp.into_reader())?;

    let plan = resolve_plan(
        &agent,
        &request.access_token,
        &request.account,
        request.local_plan,
    );
    parse_usage_json(
        &body,
        plan,
        OffsetDateTime::now_utc().unix_timestamp(),
        request.account.key,
    )
}

/// Plan name for the flyout header. Hits `/oauth/profile` at most once an hour
/// and falls back to the profile Claude Code caches in `~/.claude.json`, so a
/// profile outage shows a slightly stale plan rather than none.
fn resolve_plan(
    agent: &ureq::Agent,
    access_token: &SecretString,
    account: &AccountContext,
    local_plan: Option<String>,
) -> String {
    let now = std::time::Instant::now();
    if let Ok(cache) = PLAN_CACHE.lock() {
        if let Some(plan) = cached_plan(&cache, &account.key, now) {
            return plan;
        }
    }
    let Some(plan) = fetch_plan(agent, access_token).or(local_plan) else {
        // Keep only this account's known plan rather than leaking another
        // account's header while the profile source is unavailable.
        return PLAN_CACHE
            .lock()
            .ok()
            .and_then(|cache| cached_plan(&cache, &account.key, now))
            .unwrap_or_default();
    };
    if let Ok(mut cache) = PLAN_CACHE.lock() {
        *cache = Some(PlanCache {
            account: account.key.clone(),
            plan: plan.clone(),
            observed_at: now,
        });
    }
    plan
}

fn cached_plan(
    cache: &Option<PlanCache>,
    account: &AccountKey,
    now: std::time::Instant,
) -> Option<String> {
    cache
        .as_ref()
        .filter(|entry| {
            &entry.account == account && now.saturating_duration_since(entry.observed_at) < PLAN_TTL
        })
        .map(|entry| entry.plan.clone())
}

fn fetch_plan(agent: &ureq::Agent, access_token: &SecretString) -> Option<String> {
    let authorization = SecretString::new(format!("Bearer {}", access_token.expose()));
    let response = crate::network::get(agent, crate::network::ANTHROPIC_PROFILE_URL)
        .set("Authorization", authorization.expose())
        .set("anthropic-beta", "oauth-2025-04-20")
        .set(
            "User-Agent",
            concat!("claudometer/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .ok()?;
    let body = read_bounded(response.into_reader()).ok()?;
    plan_from_profile(&serde_json::from_slice::<ProfileResp>(&body).ok()?)
}

fn plan_from_profile(profile: &ProfileResp) -> Option<String> {
    // organization_type is "claude_pro" / "claude_max" / "claude_team"…; the
    // account booleans are the fallback for org shapes that don't set it.
    if let Some(ty) = profile
        .organization
        .as_ref()
        .and_then(|o| o.organization_type.as_deref())
    {
        return Some(plan_label(ty.strip_prefix("claude_").unwrap_or(ty)));
    }
    let account = profile.account.as_ref()?;
    match (account.has_claude_max, account.has_claude_pro) {
        (Some(true), _) => Some("Max".to_string()),
        (_, Some(true)) => Some("Pro".to_string()),
        _ => None,
    }
}

/// "pro" -> "Pro", "max_5x" -> "Max 5x". Shared with `auth.rs`.
pub fn plan_label(raw: &str) -> String {
    match raw {
        "max" => "Max".to_string(),
        "pro" => "Pro".to_string(),
        "team" => "Team".to_string(),
        other => prettify(other),
    }
}

fn parse_usage_json(
    body: &[u8],
    plan: String,
    observed_at_unix: i64,
    account: AccountKey,
) -> Result<UsageSnapshot, FetchErr> {
    let parsed: UsageResp =
        serde_json::from_slice(body).map_err(|_| plain("Unexpected API response shape."))?;
    parse_usage(parsed, plan, observed_at_unix, account)
}

fn parse_usage(
    parsed: UsageResp,
    plan: String,
    observed_at_unix: i64,
    account: AccountKey,
) -> Result<UsageSnapshot, FetchErr> {
    let mut rows: Vec<UsageLimit> = Vec::new();

    if let Some(limits) = &parsed.limits {
        for l in limits {
            if rows.len() == MAX_LIMIT_ROWS {
                break;
            }
            let Some(pct) = l.percent else { continue };
            let kind = bounded_text(l.kind.clone().unwrap_or_default());
            let label = match kind.as_str() {
                "session" => "Session (5h)".to_string(),
                "weekly_all" => "Weekly · all models".to_string(),
                "weekly_scoped" => {
                    let model = l
                        .scope
                        .as_ref()
                        .and_then(|s| s.model.as_ref())
                        .and_then(|m| m.display_name.clone())
                        .unwrap_or_else(|| "model".to_string());
                    bounded_text(format!("Weekly · {model}"))
                }
                other => prettify(other),
            };
            let reset_dt = l
                .resets_at
                .as_deref()
                .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok());
            let (typed_kind, duration) = match kind.as_str() {
                "session" => (LimitKind::Session, Some(5 * 3600)),
                "weekly_all" => (LimitKind::Weekly, Some(7 * 86400)),
                "weekly_scoped" => (LimitKind::Model, Some(7 * 86400)),
                "extra" => (LimitKind::ExtraUsage, None),
                _ => (LimitKind::Other(kind.clone()), None),
            };
            if let Some(row) = UsageLimit::from_adapter(
                kind,
                typed_kind,
                label,
                pct,
                l.severity.as_deref().and_then(ProviderSeverity::from_hint),
                reset_dt.map(|d| d.unix_timestamp()),
                duration,
            ) {
                rows.push(row);
            }
        }
    }

    // Fallback for older response shapes without `limits`
    if rows.is_empty() {
        for (b, kind, label) in [
            (&parsed.five_hour, "session", "Session (5h)"),
            (&parsed.seven_day, "weekly_all", "Weekly · all models"),
        ] {
            let Some(b) = b else { continue };
            let Some(u) = b.utilization else { continue };
            let reset_dt = b
                .resets_at
                .as_deref()
                .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok());
            let (typed_kind, duration) = if kind == "session" {
                (LimitKind::Session, 5 * 3600)
            } else {
                (LimitKind::Weekly, 7 * 86400)
            };
            if let Some(row) = UsageLimit::from_adapter(
                kind.into(),
                typed_kind,
                label.into(),
                u,
                None,
                reset_dt.map(|d| d.unix_timestamp()),
                Some(duration),
            ) {
                rows.push(row);
            }
        }
    }

    if rows.len() < MAX_LIMIT_ROWS {
        if let Some(x) = &parsed.extra_usage.filter(|x| x.is_enabled == Some(true)) {
            if let Some(row) = UsageLimit::from_adapter(
                "extra".into(),
                LimitKind::ExtraUsage,
                "Extra usage".into(),
                x.utilization.unwrap_or(0.0),
                None,
                None,
                None,
            ) {
                rows.push(row);
            }
        }
    }

    if rows.is_empty() {
        return Err(plain("API returned no limit data."));
    }

    Ok(UsageSnapshot {
        provider: ProviderId::Claude,
        account,
        source: SourceProvenance::compatibility(ProviderId::Claude),
        rows,
        plan: (!plan.is_empty()).then(|| bounded_text(plan)),
        fetched_unix: observed_at_unix,
        reset_credits_available: None,
    })
}

/// "12:56" local time for a unix timestamp — the absolute half of the
/// relative+absolute footer label.
pub fn fmt_unix_hhmm(unix: i64) -> String {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(unix) else {
        return String::new();
    };
    let Some(local) = windows_local_time(dt, None) else {
        return String::new();
    };
    fmt_hhmm(&local)
}

pub(crate) fn prettify(s: &str) -> String {
    let mut out = s.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    bounded_text(out)
}

pub(crate) fn bounded_text(mut value: String) -> String {
    if value.len() > MAX_DISPLAY_BYTES {
        let mut end = MAX_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

pub(crate) fn read_bounded(reader: impl Read) -> Result<Vec<u8>, FetchErr> {
    let mut body = Vec::with_capacity(MAX_PROVIDER_RESPONSE_BYTES.min(16 * 1024));
    reader
        .take((MAX_PROVIDER_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| crate::provider::error::io_error(&error))?;
    if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(plain("API response exceeded the 1 MiB safety limit."));
    }
    Ok(body)
}

/// Same formatting for unix-seconds reset stamps (Codex API shape).
pub(crate) fn fmt_reset_unix(unix: i64) -> String {
    use crate::provider::state::Clock;
    fmt_event_unix(
        unix,
        "resets",
        crate::provider::state::SystemClock.read().unix_seconds,
        crate::config::settings().reset_format,
    )
}

pub(crate) fn fmt_limit_unix(unix: i64, now: i64) -> String {
    fmt_event_unix(unix, "Limit", now, crate::config::settings().reset_format)
}

pub(crate) fn fmt_event_unix(
    unix: i64,
    verb: &str,
    now: i64,
    format: crate::config::ResetFormat,
) -> String {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(unix) else {
        return String::new();
    };
    if format == crate::config::ResetFormat::Countdown {
        let minutes = unix.saturating_sub(now).max(0) / 60;
        let (major, minor, units) = if minutes >= 1440 {
            (minutes / 1440, minutes % 1440 / 60, ("d", "h"))
        } else {
            (minutes / 60, minutes % 60, ("h", "m"))
        };
        let mut text = verb.to_string();
        text.push_str(" in ");
        text.push_str(&format!("{major}{} {minor}{}", units.0, units.1));
        return text;
    }
    fmt_event_dt(dt, verb)
}

/// Local clock today, otherwise weekday and clock.
fn fmt_event_dt(dt: OffsetDateTime, verb: &str) -> String {
    let Some(local) = windows_local_time(dt, None) else {
        return String::new();
    };
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(time: *mut SYSTEMTIME);
    }
    let mut today = SYSTEMTIME::default();
    unsafe { GetLocalTime(&mut today) };
    fmt_clock(&local, &today, verb)
}

fn fmt_clock(local: &SYSTEMTIME, today: &SYSTEMTIME, verb: &str) -> String {
    let day = if (local.wYear, local.wMonth, local.wDay) == (today.wYear, today.wMonth, today.wDay)
    {
        ""
    } else {
        const WEEKDAYS: [&str; 7] = ["Sun ", "Mon ", "Tue ", "Wed ", "Thu ", "Fri ", "Sat "];
        WEEKDAYS
            .get(usize::from(local.wDayOfWeek))
            .copied()
            .unwrap_or("")
    };
    let mut text = verb.to_string();
    text.push(' ');
    text.push_str(day);
    text.push_str(&fmt_hhmm(local));
    text
}

#[inline(never)]
fn fmt_hhmm(local: &SYSTEMTIME) -> String {
    format!("{:02}:{:02}", local.wHour, local.wMinute)
}

fn windows_local_time(
    utc: OffsetDateTime,
    timezone: Option<&DYNAMIC_TIME_ZONE_INFORMATION>,
) -> Option<SYSTEMTIME> {
    let utc = utc.to_offset(UtcOffset::UTC);
    let utc = SYSTEMTIME {
        wYear: u16::try_from(utc.year()).ok()?,
        wMonth: utc.month() as u16,
        wDay: u16::from(utc.day()),
        wHour: u16::from(utc.hour()),
        wMinute: u16::from(utc.minute()),
        wSecond: u16::from(utc.second()),
        wMilliseconds: u16::try_from(utc.nanosecond() / 1_000_000).ok()?,
        ..SYSTEMTIME::default()
    };
    let mut local = SYSTEMTIME::default();
    unsafe {
        SystemTimeToTzSpecificLocalTimeEx(
            timezone.map(std::ptr::from_ref),
            std::ptr::from_ref(&utc),
            std::ptr::from_mut(&mut local),
        )
        .ok()?;
    }
    Some(local)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::io::Cursor;

    use super::*;

    #[test]
    fn reset_clock_preserves_today_and_weekday_copy() {
        let local = SYSTEMTIME {
            wYear: 2026,
            wMonth: 10,
            wDay: 9,
            wDayOfWeek: 5,
            wHour: 18,
            wMinute: 59,
            ..Default::default()
        };
        assert_eq!(fmt_clock(&local, &local, "resets"), "resets 18:59");
        let today = SYSTEMTIME { wDay: 8, ..local };
        assert_eq!(fmt_clock(&local, &today, "resets"), "resets Fri 18:59");
        assert_eq!(
            fmt_event_unix(i64::MAX, "resets", 0, crate::config::ResetFormat::Clock),
            ""
        );
    }

    #[test]
    fn countdown_boundaries_and_pace_notes_use_unix_time() {
        use crate::config::ResetFormat::Countdown;
        let now = time::macros::datetime!(2026-03-08 09:00 UTC).unix_timestamp();
        for (seconds, expected) in [
            (0, "0h 0m"),
            (-1, "0h 0m"),
            (59, "0h 0m"),
            (60, "0h 1m"),
            (12300, "3h 25m"),
            (86399, "23h 59m"),
            (86400, "1d 0h"),
            (187200, "2d 4h"),
        ] {
            assert_eq!(
                fmt_event_unix(now + seconds, "resets", now, Countdown),
                format!("resets in {expected}")
            );
            assert_eq!(
                fmt_event_unix(now + seconds, "Limit", now, Countdown),
                format!("Limit in {expected}")
            );
        }
        assert_eq!(
            fmt_event_unix(now + 604800, "resets", now, Countdown),
            "resets in 7d 0h"
        );
        assert_eq!(fmt_event_unix(i64::MAX, "resets", now, Countdown), "");
        assert_eq!(
            fmt_event_unix(now, "resets", i64::MAX, Countdown),
            "resets in 0h 0m"
        );
    }

    fn pacific_timezone() -> DYNAMIC_TIME_ZONE_INFORMATION {
        for index in 0.. {
            let mut timezone = DYNAMIC_TIME_ZONE_INFORMATION::default();
            let status = unsafe {
                windows::Win32::System::Time::EnumDynamicTimeZoneInformation(
                    index,
                    std::ptr::from_mut(&mut timezone),
                )
            };
            if status == windows::Win32::Foundation::ERROR_NO_MORE_ITEMS.0 {
                break;
            }
            assert_eq!(status, windows::Win32::Foundation::ERROR_SUCCESS.0);
            let key_len = timezone
                .TimeZoneKeyName
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(timezone.TimeZoneKeyName.len());
            if String::from_utf16_lossy(&timezone.TimeZoneKeyName[..key_len])
                == "Pacific Standard Time"
            {
                return timezone;
            }
        }
        panic!("Windows Pacific Standard Time definition is unavailable")
    }

    #[test]
    fn weekly_resets_use_target_offset_across_spring_dst() {
        let timezone = pacific_timezone();
        let before = windows_local_time(
            time::macros::datetime!(2026-03-07 09:00 UTC),
            Some(&timezone),
        )
        .unwrap();
        let after = windows_local_time(
            time::macros::datetime!(2026-03-14 09:00 UTC),
            Some(&timezone),
        )
        .unwrap();

        assert_eq!((before.wMonth, before.wDay, before.wHour), (3, 7, 1));
        assert_eq!((after.wMonth, after.wDay, after.wHour), (3, 14, 2));
    }

    #[test]
    fn windows_conversion_normalizes_rfc3339_offsets_to_utc() {
        let timezone = pacific_timezone();
        let timestamp = OffsetDateTime::parse("2026-03-07T01:00:00-08:00", &Rfc3339).unwrap();
        let local = windows_local_time(timestamp, Some(&timezone)).unwrap();

        assert_eq!((local.wMonth, local.wDay, local.wHour), (3, 7, 1));
    }

    #[test]
    fn weekly_resets_use_target_offset_across_fall_dst() {
        let timezone = pacific_timezone();
        let before = windows_local_time(
            time::macros::datetime!(2026-10-31 09:00 UTC),
            Some(&timezone),
        )
        .unwrap();
        let after = windows_local_time(
            time::macros::datetime!(2026-11-07 09:00 UTC),
            Some(&timezone),
        )
        .unwrap();

        assert_eq!((before.wMonth, before.wDay, before.wHour), (10, 31, 2));
        assert_eq!((after.wMonth, after.wDay, after.wHour), (11, 7, 1));
    }

    const OBSERVED_AT: i64 = 1_788_400_000;

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            "normal" => include_bytes!("../tests/fixtures/claude/normal.json"),
            "partial" => include_bytes!("../tests/fixtures/claude/partial.json"),
            "unknown-fields" => {
                include_bytes!("../tests/fixtures/claude/unknown-fields.json")
            }
            "malformed" => include_bytes!("../tests/fixtures/claude/malformed.json"),
            "missing-reset" => include_bytes!("../tests/fixtures/claude/missing-reset.json"),
            "weekly-primary" => {
                include_bytes!("../tests/fixtures/claude/weekly-primary.json")
            }
            "non-finite" => include_bytes!("../tests/fixtures/claude/non-finite.json"),
            "out-of-range" => include_bytes!("../tests/fixtures/claude/out-of-range.json"),
            _ => panic!("unknown fixture"),
        }
    }

    fn parse_fixture(name: &str) -> Result<UsageSnapshot, FetchErr> {
        parse_usage_json(
            fixture(name),
            "Fixture plan".to_string(),
            OBSERVED_AT,
            AccountKey::from_digest([1; 32]),
        )
    }

    #[test]
    fn parses_sanitized_claude_fixture_matrix() {
        let normal = parse_fixture("normal").unwrap();
        assert_eq!(normal.rows.len(), 3);
        assert_eq!(normal.rows[0].kind, LimitKind::Session);
        assert_eq!(normal.rows[0].percent.get(), 42.5);
        assert_eq!(normal.fetched_unix, OBSERVED_AT);
        assert_eq!(normal.provider, ProviderId::Claude);
        assert!(normal.account == AccountKey::from_digest([1; 32]));
        assert_eq!(
            normal.source.support,
            crate::provider::model::SourceSupport::Compatibility
        );
        assert_eq!(normal.rows[0].window_seconds, Some(18_000));
        assert_eq!(normal.rows[1].window_seconds, Some(604_800));
        assert_eq!(
            normal.rows[2].class,
            crate::provider::model::LimitClass::Spend
        );

        let partial = parse_fixture("partial").unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.rows[0].label, "Weekly · model");

        assert_eq!(parse_fixture("unknown-fields").unwrap().rows.len(), 1);
        assert!(parse_fixture("malformed").is_err());
        assert!(parse_fixture("non-finite").is_err());

        let missing_reset = parse_fixture("missing-reset").unwrap();
        assert_eq!(missing_reset.rows[0].resets_unix, None);
        assert!(crate::gfx::LimitRow::from(missing_reset.rows[0].clone())
            .reset_text
            .is_empty());

        let weekly = parse_fixture("weekly-primary").unwrap();
        assert_eq!(weekly.rows[0].kind, LimitKind::Weekly);

        let out_of_range = parse_fixture("out-of-range").unwrap();
        assert_eq!(out_of_range.rows[0].percent.get(), 0.0);
        assert_eq!(out_of_range.rows[1].percent.get(), 100.0);
    }

    #[test]
    fn enforces_response_size_boundary_and_truncation() {
        let at_limit = vec![b'x'; MAX_PROVIDER_RESPONSE_BYTES];
        assert_eq!(
            read_bounded(Cursor::new(&at_limit)).unwrap().len(),
            MAX_PROVIDER_RESPONSE_BYTES
        );

        let over_limit = vec![b'x'; MAX_PROVIDER_RESPONSE_BYTES + 1];
        assert!(read_bounded(Cursor::new(over_limit)).is_err());

        let normal = fixture("normal");
        let closing_brace = normal
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .unwrap();
        assert!(parse_usage_json(
            &normal[..closing_brace],
            String::new(),
            OBSERVED_AT,
            AccountKey::from_digest([1; 32])
        )
        .is_err());
    }

    #[test]
    fn bounds_rows_and_utf8_display_strings() {
        let limits: Vec<_> = (0..(MAX_LIMIT_ROWS + 1))
            .map(|index| {
                serde_json::json!({
                    "kind": format!("unknown_{index}"),
                    "percent": index,
                })
            })
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({ "limits": limits })).unwrap();
        assert_eq!(
            parse_usage_json(
                &body,
                String::new(),
                OBSERVED_AT,
                AccountKey::from_digest([1; 32])
            )
            .unwrap()
            .rows
            .len(),
            MAX_LIMIT_ROWS
        );

        let bounded = bounded_text("é".repeat(MAX_DISPLAY_BYTES));
        assert!(bounded.len() <= MAX_DISPLAY_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn claude_credentials_do_not_require_expiry_hint() {
        let credentials: CredsFile =
            serde_json::from_str(r#"{"claudeAiOauth":{"accessToken":"worker-only-token"}}"#)
                .unwrap();
        assert_eq!(credentials.oauth.expires_at, None);
    }

    #[test]
    fn inference_only_credentials_cannot_reach_request_preparation() {
        for scopes in [
            None,
            Some("[]"),
            Some("[\"user:inference\"]"),
            Some("[\"user:profile\",\"user:inference\"]"),
        ] {
            let field = scopes
                .map(|scopes| format!(",\"scopes\":{scopes}"))
                .unwrap_or_default();
            let json =
                format!("{{\"claudeAiOauth\":{{\"accessToken\":\"synthetic-token\"{field}}}}}");
            let credentials: CredsFile = serde_json::from_str(&json).unwrap();
            let allowed = scopes.is_none() || scopes.unwrap().contains("user:profile");
            assert_eq!(validate_usage_scope(&credentials.oauth).is_ok(), allowed);
            if !allowed {
                let error = validate_usage_scope(&credentials.oauth).err().unwrap();
                assert_eq!(error.kind, PreparationFailureKind::UsageScope);
                assert_eq!(error.error().code(), "usage_scope_missing");
            }
        }
    }

    #[test]
    fn malformed_response_error_never_contains_the_body() {
        let error = parse_usage_json(
            b"synthetic-private-response",
            String::new(),
            1000,
            AccountKey::from_digest([1; 32]),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, FailureKind::UnexpectedResponse);
        assert!(!format!("{error:?}").contains("synthetic-private"));
    }

    #[test]
    fn plan_cache_never_crosses_account_keys() {
        let now = std::time::Instant::now();
        let account_a = AccountKey::from_digest([1; 32]);
        let account_b = AccountKey::from_digest([2; 32]);
        let cache = Some(PlanCache {
            account: account_a.clone(),
            plan: "Account A plan".to_string(),
            observed_at: now,
        });
        assert_eq!(
            cached_plan(&cache, &account_a, now),
            Some("Account A plan".to_string())
        );
        assert_eq!(cached_plan(&cache, &account_b, now), None);
        assert_eq!(cached_plan(&cache, &account_a, now + PLAN_TTL), None);
    }

    #[test]
    fn credential_retry_distinguishes_atomic_handoff_failure_modes_without_sleeping() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Probe {
            value: u32,
        }

        let mut attempts = VecDeque::from([
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            Ok(Cursor::new(br#"{"value":7}"#.to_vec())),
        ]);
        let waits = Cell::new(0);
        let parsed: Probe = read_json_with_retry(
            || attempts.pop_front().unwrap(),
            || waits.set(waits.get() + 1),
        )
        .unwrap();
        assert_eq!(parsed.value, 7);
        assert_eq!(waits.get(), 1);

        let missing = read_json_with_retry::<Probe, Cursor<Vec<u8>>>(
            || Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            || {},
        );
        assert_eq!(missing.unwrap_err(), CredentialReadError::Missing);

        let unreadable = read_json_with_retry::<Probe, Cursor<Vec<u8>>>(
            || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            || {},
        );
        assert_eq!(
            unreadable.unwrap_err(),
            CredentialReadError::TemporarilyUnreadable
        );

        let malformed = read_json_with_retry::<Probe, Cursor<Vec<u8>>>(
            || Ok(Cursor::new(b"{malformed".to_vec())),
            || {},
        );
        assert_eq!(malformed.unwrap_err(), CredentialReadError::Malformed);
    }
}
