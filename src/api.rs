//! Fetches usage limits from the same endpoint Claude Code's `/usage` uses.
//! Read-only: never refreshes or rewrites the OAuth token (refresh rotation
//! could invalidate the Claude Code session).

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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
    expires_at: i64,
    subscription_type: Option<String>,
}

// ---------- API response ----------

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
        /// server Retry-After (seconds) on 429 — honored exactly
        retry_after: Option<u64>,
    },
}

pub fn fetch() -> FetchOutcome {
    match fetch_inner() {
        Ok(s) => FetchOutcome::Ok(s),
        Err((msg, retry_after)) => FetchOutcome::Err { msg, retry_after },
    }
}

type FetchErr = (String, Option<u64>);

fn plain(msg: impl Into<String>) -> FetchErr {
    (msg.into(), None)
}

fn fetch_inner() -> Result<UsageSnapshot, FetchErr> {
    let home = std::env::var("USERPROFILE").map_err(|_| plain("USERPROFILE not set"))?;
    let path = std::path::Path::new(&home).join(".claude").join(".credentials.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| plain("No Claude sign-in found.\nSign in with Claude Code first."))?;
    let creds: CredsFile =
        serde_json::from_str(&raw).map_err(|_| plain("Credentials file unreadable."))?;

    let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1000;
    if now_ms > creds.oauth.expires_at {
        return Err(plain("Sign-in expired.\nOpen Claude Code once to refresh it."));
    }

    let tls = native_tls::TlsConnector::new().map_err(|_| plain("TLS init failed"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {}", creds.oauth.access_token))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(401, _) => {
                plain("Sign-in rejected (401).\nOpen Claude Code once to refresh it.")
            }
            ureq::Error::Status(429, resp) => {
                let retry_after = resp
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                ("Rate limited by the API.".to_string(), retry_after)
            }
            ureq::Error::Status(code, _) => plain(format!("Anthropic API error {code}.")),
            _ => plain("Network error.\nCheck your connection."),
        })?;

    let body = resp.into_string().map_err(|_| plain("Bad API response."))?;
    let parsed: UsageResp =
        serde_json::from_str(&body).map_err(|_| plain("Unexpected API response shape."))?;

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
            rows.push(LimitRow {
                kind,
                label,
                percent: pct,
                severity: l.severity.clone().unwrap_or_default(),
                reset_text: l.resets_at.as_deref().map(fmt_reset).unwrap_or_default(),
            });
        }
    }

    // Fallback for older response shapes without `limits`
    if rows.is_empty() {
        if let Some(b) = &parsed.five_hour {
            if let Some(u) = b.utilization {
                rows.push(LimitRow {
                    kind: "session".into(),
                    label: "Session (5h)".into(),
                    percent: u,
                    severity: String::new(),
                    reset_text: b.resets_at.as_deref().map(fmt_reset).unwrap_or_default(),
                });
            }
        }
        if let Some(b) = &parsed.seven_day {
            if let Some(u) = b.utilization {
                rows.push(LimitRow {
                    kind: "weekly_all".into(),
                    label: "Weekly · all models".into(),
                    percent: u,
                    severity: String::new(),
                    reset_text: b.resets_at.as_deref().map(fmt_reset).unwrap_or_default(),
                });
            }
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
            });
        }
    }

    if rows.is_empty() {
        return Err(plain("API returned no limit data."));
    }

    let plan = match creds.oauth.subscription_type.as_deref() {
        Some("max") => "Max plan".to_string(),
        Some("pro") => "Pro plan".to_string(),
        Some("team") => "Team plan".to_string(),
        Some(other) => format!("{} plan", prettify(other)),
        None => String::new(),
    };

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

fn prettify(s: &str) -> String {
    let mut out = s.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

/// "resets 18:59" if today (local), otherwise "resets Sat 19:59"
fn fmt_reset(iso: &str) -> String {
    let Ok(dt) = OffsetDateTime::parse(iso, &Rfc3339) else {
        return String::new();
    };
    let local = dt.to_offset(local_offset());
    let today = OffsetDateTime::now_utc().to_offset(local_offset()).date();
    if local.date() == today {
        format!("resets {:02}:{:02}", local.hour(), local.minute())
    } else {
        let wd = &local.date().weekday().to_string()[..3];
        format!("resets {} {:02}:{:02}", wd, local.hour(), local.minute())
    }
}
