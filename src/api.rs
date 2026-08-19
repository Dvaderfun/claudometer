//! Fetches usage limits from the same endpoint Claude Code's `/usage` uses.
//! Also home of the provider-agnostic display model (`UsageSnapshot`,
//! `LimitRow`, `FetchOutcome`) shared with `codex.rs`.
//! Claudometer never performs a token exchange and never writes credentials.
//! When the access token is near expiry or rejected, `auth.rs` asks the official
//! Claude Code CLI to rotate its own credentials, and this module re-reads them.

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::auth;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Authoritative plan/identity. `.credentials.json`'s `subscriptionType` is
/// written once at login and survives plan changes unchanged, so it reports
/// "max" long after a downgrade — this endpoint is the only source that moves.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// The plan changes at most monthly; one profile round-trip per hour is plenty.
const PLAN_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

static PLAN_CACHE: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

// ---------- credentials ----------

#[derive(Deserialize)]
struct CredsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Oauth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Oauth {
    access_token: String,
    /// Read-only: handed straight to the Claude Code CLI for its own rotation.
    refresh_token: Option<String>,
    expires_at: i64,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Credential facts `auth.rs` needs without giving it the access token.
pub struct CredsSummary {
    pub refresh_token: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at: i64,
    pub has_refresh_token: bool,
    pub expired: bool,
}

pub fn credentials_summary() -> Option<CredsSummary> {
    let oauth = read_credentials().ok()?.oauth;
    Some(CredsSummary {
        has_refresh_token: oauth.refresh_token.is_some(),
        expired: OffsetDateTime::now_utc().unix_timestamp() * 1000 > oauth.expires_at,
        refresh_token: oauth.refresh_token,
        scopes: oauth.scopes,
        expires_at: oauth.expires_at,
    })
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

pub fn fetch() -> FetchOutcome {
    match fetch_inner() {
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

fn read_credentials() -> Result<CredsFile, FetchErr> {
    let home = std::env::var("USERPROFILE").map_err(|_| plain("USERPROFILE not set"))?;
    let path = std::path::Path::new(&home).join(".claude").join(".credentials.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| plain("Not signed in.\nOpen Settings to connect."))?;
    serde_json::from_str(&raw).map_err(|_| plain("Credentials file unreadable."))
}

fn fetch_inner() -> Result<UsageSnapshot, FetchErr> {
    let mut creds = read_credentials()?;

    let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let mut repaired = false;
    // Give Claude Code time to rotate the access token before it expires. The
    // broker is cooled down in auth.rs, so this stays cheap during an outage.
    if creds.oauth.expires_at <= now_ms + 5 * 60 * 1000 {
        repaired = auth::repair_session_if_due();
        if repaired {
            creds = read_credentials()?;
        }
    }
    if now_ms > creds.oauth.expires_at {
        return Err(plain(
            "Sign-in expired.\nOpen Settings to reconnect.",
        ));
    }

    let tls = native_tls::TlsConnector::new().map_err(|_| plain("TLS init failed"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let request = |oauth: &Oauth| -> Result<ureq::Response, RequestFailure> {
        agent
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {}", oauth.access_token))
            .set("anthropic-beta", "oauth-2025-04-20")
            .set("User-Agent", concat!("claudometer/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(compact_request_error)
    };
    let resp = match request(&creds.oauth) {
        Ok(resp) => resp,
        Err(RequestFailure::Auth) if !repaired => {
            // Server authority beats the local expiry stamp. Ask the official
            // broker once, then retry only with credentials re-read from disk.
            if auth::repair_session_if_due() {
                creds = read_credentials()?;
                request(&creds.oauth).map_err(RequestFailure::into_fetch_error)?
            } else {
                return Err(plain(
                    "Sign-in rejected.\nOpen Settings to reconnect.",
                ));
            }
        }
        Err(e) => return Err(e.into_fetch_error()),
    };

    let body = resp.into_string().map_err(|_| plain("Bad API response."))?;
    let parsed: UsageResp =
        serde_json::from_str(&body).map_err(|_| plain("Unexpected API response shape."))?;

    let plan = resolve_plan(&agent, &creds.oauth.access_token);
    parse_usage(parsed, plan)
}

/// Plan name for the flyout header. Hits `/oauth/profile` at most once an hour
/// and falls back to the profile Claude Code caches in `~/.claude.json`, so a
/// profile outage shows a slightly stale plan rather than none.
fn resolve_plan(agent: &ureq::Agent, access_token: &str) -> String {
    if let Ok(cache) = PLAN_CACHE.lock() {
        if let Some((plan, at)) = cache.as_ref() {
            if at.elapsed() < PLAN_TTL {
                return plan.clone();
            }
        }
    }
    let Some(plan) = fetch_plan(agent, access_token).or_else(crate::auth::local_plan) else {
        // Keep showing the last known plan rather than blanking the header.
        return PLAN_CACHE
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|(p, _)| p.clone()))
            .unwrap_or_default();
    };
    if let Ok(mut cache) = PLAN_CACHE.lock() {
        *cache = Some((plan.clone(), std::time::Instant::now()));
    }
    plan
}

fn fetch_plan(agent: &ureq::Agent, access_token: &str) -> Option<String> {
    let body = agent
        .get(PROFILE_URL)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", concat!("claudometer/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    plan_from_profile(&serde_json::from_str::<ProfileResp>(&body).ok()?)
}

fn plan_from_profile(profile: &ProfileResp) -> Option<String> {
    // organization_type is "claude_pro" / "claude_max" / "claude_team"…; the
    // account booleans are the fallback for org shapes that don't set it.
    if let Some(ty) = profile.organization.as_ref().and_then(|o| o.organization_type.as_deref()) {
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
            Self::Auth => plain(
                "Sign-in rejected.\nOpen Settings to reconnect.",
            ),
            Self::Other(err) => err,
        }
    }
}

fn compact_request_error(e: ureq::Error) -> RequestFailure {
    match e {
        ureq::Error::Status(401, _) => RequestFailure::Auth,
        ureq::Error::Status(429, resp) => {
            let retry_after = resp
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok());
            RequestFailure::Other((
                "Rate limited by the API.".to_string(),
                retry_after,
                true,
            ))
        }
        ureq::Error::Status(code, _) => {
            RequestFailure::Other(plain(format!("Anthropic API error {code}.")))
        }
        _ => RequestFailure::Other(plain("Network error.\nCheck your connection.")),
    }
}

fn parse_usage(parsed: UsageResp, plan: String) -> Result<UsageSnapshot, FetchErr> {
    let mut rows: Vec<LimitRow> = Vec::new();

    if let Some(limits) = &parsed.limits {
        for l in limits {
            let Some(pct) = l.percent else { continue };
            let kind = l.kind.clone().unwrap_or_default();
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
                    format!("Weekly · {model}")
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
                percent: pct,
                severity: l.severity.clone().unwrap_or_default(),
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
                percent: u,
                severity: String::new(),
                reset_text: reset_dt.map(fmt_reset_dt).unwrap_or_default(),
                resets_unix: reset_dt.map(|d| d.unix_timestamp()),
            });
        }
    }

    if let Some(x) = &parsed.extra_usage {
        if x.is_enabled == Some(true) {
            rows.push(LimitRow {
                kind: "extra".into(),
                label: "Extra usage".into(),
                percent: x.utilization.unwrap_or(0.0),
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
        plan,
        fetched_unix: OffsetDateTime::now_utc().unix_timestamp(),
    })
}

/// "12:56" local time for a unix timestamp — the absolute half of the
/// relative+absolute footer label.
pub fn fmt_unix_hhmm(unix: i64) -> String {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(unix) else {
        return String::new();
    };
    let local = dt.to_offset(local_offset());
    format!("{:02}:{:02}", local.hour(), local.minute())
}

pub(crate) fn prettify(s: &str) -> String {
    let mut out = s.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
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
    let local = dt.to_offset(local_offset());
    let today = OffsetDateTime::now_utc().to_offset(local_offset()).date();
    if local.date() == today {
        format!("resets {:02}:{:02}", local.hour(), local.minute())
    } else {
        let wd = &local.date().weekday().to_string()[..3];
        format!("resets {} {:02}:{:02}", wd, local.hour(), local.minute())
    }
}
