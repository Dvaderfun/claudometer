//! Fetches usage limits from the same endpoint Claude Code's `/usage` uses.
//! Also home of the provider-agnostic display model (`UsageSnapshot`,
//! `LimitRow`, `FetchOutcome`) shared with `codex.rs`.
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

use crate::provider::model::{
    derive_account_context, AccountContext, AccountKey, ProviderId, SecretString,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Authoritative plan/identity. `.credentials.json`'s `subscriptionType` is
/// written once at login and survives plan changes unchanged, so it reports
/// "max" long after a downgrade — this endpoint is the only source that moves.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
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
}

pub struct PreparationFailure {
    pub kind: PreparationFailureKind,
    pub message: &'static str,
}

impl PreparationFailure {
    pub fn invalidates_account(&self) -> bool {
        self.kind != PreparationFailureKind::TemporarilyUnreadable
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

// ---------- display-ready model ----------

#[derive(Clone)]
pub struct LimitRow {
    pub kind: String,
    pub label: String,
    pub percent: f64,
    pub severity: String,
    /// e.g. "resets 18:59" / "resets Sat 19:59", empty when unknown
    pub reset_text: String,
    /// raw reset stamp (unix seconds) — identifies the window *instance*,
    /// which is what alert dedup keys on
    pub resets_unix: Option<i64>,
}

#[derive(Clone)]
pub struct UsageSnapshot {
    pub rows: Vec<LimitRow>,
    pub plan: String,
    /// unix seconds of the fetch — rendered as a ticking relative label
    pub fetched_unix: i64,
}

#[derive(Clone)]
pub enum FetchOutcome {
    Ok(UsageSnapshot),
    Err {
        msg: String,
        /// server Retry-After (seconds) on 429
        retry_after: Option<u64>,
        /// true only for HTTP 429 — drives the jittered retry + backoff
        rate_limited: bool,
    },
}

pub fn prepare() -> Result<PreparedRequest, PreparationFailure> {
    let credentials = read_credentials()?;
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

pub fn fetch(request: PreparedRequest) -> FetchOutcome {
    match fetch_inner(request) {
        Ok(s) => FetchOutcome::Ok(s),
        Err((msg, retry_after, rate_limited)) => FetchOutcome::Err {
            msg,
            retry_after,
            rate_limited,
        },
    }
}

pub(crate) type FetchErr = (String, Option<u64>, bool);

pub(crate) fn plain(msg: impl Into<String>) -> FetchErr {
    (msg.into(), None, false)
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
    let tls = native_tls::TlsConnector::new().map_err(|_| plain("TLS init failed"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let authorization = SecretString::new(format!("Bearer {}", request.access_token.expose()));
    let response = || -> Result<ureq::Response, RequestFailure> {
        agent
            .get(USAGE_URL)
            .set("Authorization", authorization.expose())
            .set("anthropic-beta", "oauth-2025-04-20")
            .set(
                "User-Agent",
                concat!("claudometer/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(compact_request_error)
    };
    let resp = response().map_err(RequestFailure::into_fetch_error)?;

    let body = read_bounded(resp.into_reader())?;

    let plan = resolve_plan(
        &agent,
        &request.access_token,
        &request.account,
        request.local_plan,
    );
    parse_usage_json(&body, plan, OffsetDateTime::now_utc().unix_timestamp())
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
    let response = agent
        .get(PROFILE_URL)
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

enum RequestFailure {
    Auth,
    Other(FetchErr),
}

impl RequestFailure {
    fn into_fetch_error(self) -> FetchErr {
        match self {
            Self::Auth => {
                plain("Claude Code needs to refresh sign-in.\nOpen Claude Code once, then refresh.")
            }
            Self::Other(err) => err,
        }
    }
}

fn compact_request_error(e: ureq::Error) -> RequestFailure {
    match e {
        ureq::Error::Status(401 | 403, _) => RequestFailure::Auth,
        ureq::Error::Status(429, resp) => {
            let retry_after = resp
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok());
            RequestFailure::Other(("Rate limited by the API.".to_string(), retry_after, true))
        }
        ureq::Error::Status(code, _) => {
            RequestFailure::Other(plain(format!("Anthropic API error {code}.")))
        }
        _ => RequestFailure::Other(plain("Network error.\nCheck your connection.")),
    }
}

fn parse_usage_json(
    body: &[u8],
    plan: String,
    observed_at_unix: i64,
) -> Result<UsageSnapshot, FetchErr> {
    let parsed: UsageResp =
        serde_json::from_slice(body).map_err(|_| plain("Unexpected API response shape."))?;
    parse_usage(parsed, plan, observed_at_unix)
}

fn parse_usage(
    parsed: UsageResp,
    plan: String,
    observed_at_unix: i64,
) -> Result<UsageSnapshot, FetchErr> {
    let mut rows: Vec<LimitRow> = Vec::new();

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
            rows.push(LimitRow {
                kind,
                label,
                percent: clamp_percent(pct),
                severity: bounded_text(l.severity.clone().unwrap_or_default()),
                reset_text: reset_dt.map(fmt_reset_dt).unwrap_or_default(),
                resets_unix: reset_dt.map(|d| d.unix_timestamp()),
            });
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
            rows.push(LimitRow {
                kind: kind.into(),
                label: label.into(),
                percent: clamp_percent(u),
                severity: String::new(),
                reset_text: reset_dt.map(fmt_reset_dt).unwrap_or_default(),
                resets_unix: reset_dt.map(|d| d.unix_timestamp()),
            });
        }
    }

    if rows.len() < MAX_LIMIT_ROWS {
        if let Some(x) = &parsed.extra_usage.filter(|x| x.is_enabled == Some(true)) {
            rows.push(LimitRow {
                kind: "extra".into(),
                label: "Extra usage".into(),
                percent: clamp_percent(x.utilization.unwrap_or(0.0)),
                severity: String::new(),
                reset_text: String::new(),
                resets_unix: None,
            });
        }
    }

    if rows.is_empty() {
        return Err(plain("API returned no limit data."));
    }

    Ok(UsageSnapshot {
        rows,
        plan: bounded_text(plan),
        fetched_unix: observed_at_unix,
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
    format!("{:02}:{:02}", local.wHour, local.wMinute)
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
        .map_err(|_| plain("Bad API response."))?;
    if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(plain("API response exceeded the 1 MiB safety limit."));
    }
    Ok(body)
}

/// API percentages are untrusted floats. Keep NaN/infinity and out-of-range
/// values out of text formatting, alert comparisons, and D2D geometry.
pub(crate) fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Same formatting for unix-seconds reset stamps (Codex API shape).
pub(crate) fn fmt_reset_unix(unix: i64) -> String {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(unix) else {
        return String::new();
    };
    fmt_reset_dt(dt)
}

/// "resets 18:59" if today (local), otherwise "resets Sat 19:59"
fn fmt_reset_dt(dt: OffsetDateTime) -> String {
    let Some(local) = windows_local_time(dt, None) else {
        return String::new();
    };
    let today = windows_local_time(OffsetDateTime::now_utc(), None);
    if today.is_some_and(|today| {
        (local.wYear, local.wMonth, local.wDay) == (today.wYear, today.wMonth, today.wDay)
    }) {
        format!("resets {:02}:{:02}", local.wHour, local.wMinute)
    } else {
        const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let weekday = WEEKDAYS
            .get(usize::from(local.wDayOfWeek))
            .copied()
            .unwrap_or("");
        format!("resets {} {:02}:{:02}", weekday, local.wHour, local.wMinute)
    }
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
        parse_usage_json(fixture(name), "Fixture plan".to_string(), OBSERVED_AT)
    }

    #[test]
    fn sanitizes_untrusted_percentages() {
        assert_eq!(clamp_percent(f64::NAN), 0.0);
        assert_eq!(clamp_percent(f64::INFINITY), 0.0);
        assert_eq!(clamp_percent(-3.0), 0.0);
        assert_eq!(clamp_percent(42.5), 42.5);
        assert_eq!(clamp_percent(104.0), 100.0);
    }

    #[test]
    fn parses_sanitized_claude_fixture_matrix() {
        let normal = parse_fixture("normal").unwrap();
        assert_eq!(normal.rows.len(), 3);
        assert_eq!(normal.rows[0].kind, "session");
        assert_eq!(normal.rows[0].percent, 42.5);
        assert_eq!(normal.fetched_unix, OBSERVED_AT);

        let partial = parse_fixture("partial").unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.rows[0].label, "Weekly · model");

        assert_eq!(parse_fixture("unknown-fields").unwrap().rows.len(), 1);
        assert!(parse_fixture("malformed").is_err());
        assert!(parse_fixture("non-finite").is_err());

        let missing_reset = parse_fixture("missing-reset").unwrap();
        assert_eq!(missing_reset.rows[0].resets_unix, None);
        assert!(missing_reset.rows[0].reset_text.is_empty());

        let weekly = parse_fixture("weekly-primary").unwrap();
        assert_eq!(weekly.rows[0].kind, "weekly_all");

        let out_of_range = parse_fixture("out-of-range").unwrap();
        assert_eq!(out_of_range.rows[0].percent, 0.0);
        assert_eq!(out_of_range.rows[1].percent, 100.0);
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
        assert!(parse_usage_json(&normal[..closing_brace], String::new(), OBSERVED_AT).is_err());
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
            parse_usage_json(&body, String::new(), OBSERVED_AT)
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
