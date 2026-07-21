//! Fetches Codex usage from the same endpoint the Codex CLI's TUI polls
//! (`wham/usage`). Read-only: never refreshes or rewrites the OAuth token —
//! OpenAI rotates refresh tokens, so an external refresh would invalidate the
//! user's Codex CLI session. Expired = tell user to open Codex.

use serde::Deserialize;

use crate::api::{fmt_reset_unix, plain, prettify, FetchErr, FetchOutcome, LimitRow, UsageSnapshot};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

// ---------- credentials (~/.codex/auth.json) ----------

#[derive(Deserialize)]
struct AuthFile {
    tokens: Option<Tokens>,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

fn auth_path() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        if !home.is_empty() {
            return Some(std::path::Path::new(&home).join("auth.json"));
        }
    }
    let home = std::env::var("USERPROFILE").ok()?;
    Some(std::path::Path::new(&home).join(".codex").join("auth.json"))
}

fn read_tokens() -> Option<(String, String)> {
    let raw = std::fs::read_to_string(auth_path()?).ok()?;
    let auth: AuthFile = serde_json::from_str(&raw).ok()?;
    let t = auth.tokens?;
    match (t.access_token, t.account_id) {
        (Some(a), Some(id)) if !a.is_empty() && !id.is_empty() => Some((a, id)),
        _ => None,
    }
}

/// ChatGPT-login Codex sign-in present? API-key-only installs have no usage
/// limits to show and count as absent.
pub fn available() -> bool {
    read_tokens().is_some()
}

// ---------- API response ----------

#[derive(Deserialize)]
struct UsageResp {
    plan_type: Option<String>,
    rate_limit: Option<RateLimit>,
}

#[derive(Deserialize)]
struct RateLimit {
    primary_window: Option<Window>,
    secondary_window: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

pub fn fetch() -> FetchOutcome {
    match fetch_inner() {
        Ok(s) => FetchOutcome::Ok(s),
        Err((msg, retry_after)) => FetchOutcome::Err { msg, retry_after },
    }
}

fn fetch_inner() -> Result<UsageSnapshot, FetchErr> {
    let (token, account_id) =
        read_tokens().ok_or_else(|| plain("No Codex sign-in — run codex once."))?;

    // Expiry lives in the JWT `exp` claim (auth.json has no expires field).
    // Unparseable claim = skip the check and let the server decide.
    if let Some(exp) = jwt_exp(&token) {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if now > exp {
            return Err(plain("Sign-in expired — open Codex to refresh."));
        }
    }

    let tls = native_tls::TlsConnector::new().map_err(|_| plain("TLS init failed"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("chatgpt-account-id", &account_id)
        .set("User-Agent", concat!("claudometer/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => {
                plain("Sign-in expired — open Codex to refresh.")
            }
            ureq::Error::Status(429, resp) => {
                let retry_after = resp
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                ("Rate limited by the API.".to_string(), retry_after)
            }
            ureq::Error::Status(code, _) => plain(format!("OpenAI API error {code}.")),
            _ => plain("Network error."),
        })?;

    let body = resp.into_string().map_err(|_| plain("Bad API response."))?;
    let parsed: UsageResp =
        serde_json::from_str(&body).map_err(|_| plain("Unexpected API response shape."))?;

    let mut rows: Vec<LimitRow> = Vec::new();
    if let Some(rl) = &parsed.rate_limit {
        // kind/label come from the window duration — which window arrives as
        // primary vs secondary varies by plan (observed: weekly-only accounts
        // get the 168 h window as primary)
        if let Some(w) = &rl.primary_window {
            push_row(&mut rows, w, "session");
        }
        if let Some(w) = &rl.secondary_window {
            push_row(&mut rows, w, "weekly_all");
        }
    }
    if rows.is_empty() {
        return Err(plain("API returned no limit data."));
    }

    let plan = parsed
        .plan_type
        .as_deref()
        .map(prettify)
        .unwrap_or_default();

    Ok(UsageSnapshot {
        rows,
        plan,
        fetched_unix: time::OffsetDateTime::now_utc().unix_timestamp(),
    })
}

fn push_row(rows: &mut Vec<LimitRow>, w: &Window, fallback_kind: &str) {
    let Some(pct) = w.used_percent else { return };
    let (kind, label) = match w.limit_window_seconds {
        Some(s) if s > 0 && s <= 24 * 3600 => {
            let label = if s % 3600 == 0 {
                format!("Session ({}h)", s / 3600)
            } else {
                "Session".to_string()
            };
            ("session", label)
        }
        Some(s) if s > 24 * 3600 => {
            let days = s / 86400;
            let label = if days == 7 {
                "Weekly".to_string()
            } else {
                format!("{days}-day")
            };
            ("weekly_all", label)
        }
        _ => (
            fallback_kind,
            if fallback_kind == "session" { "Session" } else { "Weekly" }.to_string(),
        ),
    };
    rows.push(LimitRow {
        kind: kind.into(),
        label,
        percent: pct,
        severity: String::new(), // no severity field — percent thresholds apply
        reset_text: w.reset_at.map(fmt_reset_unix).unwrap_or_default(),
        resets_unix: w.reset_at,
    });
}

// ---------- JWT expiry (no verification, just the claim) ----------

fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp")?.as_i64()
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4 + 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => continue,
            _ => return None,
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}
