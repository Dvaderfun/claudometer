//! Fetches Codex usage from the same endpoint the Codex CLI's TUI polls
//! (`wham/usage`). Read-only: never refreshes or rewrites the OAuth token —
//! OpenAI rotates refresh tokens, so an external refresh would invalidate the
//! user's Codex CLI session. Expired = tell user to open Codex.

use std::fs::File;

use serde::Deserialize;

use crate::api::{
    bounded_text, clamp_percent, fmt_reset_unix, plain, prettify, read_bounded,
    read_json_with_retry, CredentialReadError, FetchErr, FetchOutcome, LimitRow,
    PreparationFailure, PreparationFailureKind, UsageSnapshot, MAX_LIMIT_ROWS,
};
use crate::provider::model::{derive_account_context, AccountContext, ProviderId, SecretString};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

// ---------- credentials (~/.codex/auth.json) ----------

#[derive(Deserialize)]
struct AuthFile {
    tokens: Option<Tokens>,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: Option<SecretString>,
    account_id: Option<SecretString>,
}

pub struct PreparedRequest {
    access_token: SecretString,
    account_id: SecretString,
    account: AccountContext,
}

impl PreparedRequest {
    pub fn account(&self) -> &AccountContext {
        &self.account
    }
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

pub fn prepare() -> Result<PreparedRequest, PreparationFailure> {
    let path = auth_path().ok_or(PreparationFailure {
        kind: PreparationFailureKind::Unsupported,
        message: "Codex config path unavailable.",
    })?;
    let auth: AuthFile = match read_json_with_retry(
        || File::open(&path),
        || std::thread::sleep(std::time::Duration::from_millis(25)),
    ) {
        Ok(auth) => auth,
        Err(CredentialReadError::Malformed) => {
            return Err(PreparationFailure {
                kind: PreparationFailureKind::Malformed,
                message: "Codex credentials are malformed.",
            });
        }
        Err(CredentialReadError::TemporarilyUnreadable) => {
            return Err(PreparationFailure {
                kind: PreparationFailureKind::TemporarilyUnreadable,
                message: "Codex credentials are temporarily unavailable.",
            });
        }
        Err(CredentialReadError::Missing) => {
            return Err(PreparationFailure {
                kind: PreparationFailureKind::Missing,
                message: "No Codex sign-in — run codex once.",
            });
        }
    };
    let Some(tokens) = auth.tokens else {
        return Err(PreparationFailure {
            kind: PreparationFailureKind::Unsupported,
            message: "Codex has no ChatGPT quota sign-in.",
        });
    };
    let (Some(token), Some(account_id)) = (tokens.access_token, tokens.account_id) else {
        return Err(PreparationFailure {
            kind: PreparationFailureKind::Unsupported,
            message: "Codex has no ChatGPT quota sign-in.",
        });
    };
    if token.is_empty() || account_id.is_empty() {
        return Err(PreparationFailure {
            kind: PreparationFailureKind::Malformed,
            message: "Codex credentials are malformed.",
        });
    }
    let access_token = token;
    let install_salt = crate::runtime_state::install_salt();
    let account = derive_account_context(
        install_salt.as_ref().map(|salt| salt.as_bytes()),
        ProviderId::Codex,
        Some(&account_id),
        &access_token,
    )
    .map_err(|_| PreparationFailure {
        kind: PreparationFailureKind::IdentityUnavailable,
        message: "Codex account identity is unavailable.",
    })?;
    Ok(PreparedRequest {
        access_token,
        account_id,
        account,
    })
}

/// ChatGPT-login Codex sign-in present? API-key-only installs have no usage
/// limits to show and count as absent.
pub fn available() -> bool {
    auth_path().is_some_and(|path| path.is_file())
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

fn fetch_inner(request: PreparedRequest) -> Result<UsageSnapshot, FetchErr> {
    // Expiry lives in the JWT `exp` claim (auth.json has no expires field).
    // Unparseable claim = skip the check and let the server decide.
    if let Some(exp) = jwt_exp(request.access_token.expose()) {
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
    let authorization = SecretString::new(format!("Bearer {}", request.access_token.expose()));
    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", authorization.expose())
        .set("chatgpt-account-id", request.account_id.expose())
        .set(
            "User-Agent",
            concat!("claudometer/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => plain("Sign-in expired — open Codex to refresh."),
            ureq::Error::Status(429, resp) => {
                let retry_after = resp
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                ("Rate limited by the API.".to_string(), retry_after, true)
            }
            ureq::Error::Status(code, _) => plain(format!("OpenAI API error {code}.")),
            _ => plain("Network error."),
        })?;

    let body = read_bounded(resp.into_reader())?;
    parse_usage_json(&body, time::OffsetDateTime::now_utc().unix_timestamp())
}

fn parse_usage_json(body: &[u8], observed_at_unix: i64) -> Result<UsageSnapshot, FetchErr> {
    let parsed: UsageResp =
        serde_json::from_slice(body).map_err(|_| plain("Unexpected API response shape."))?;

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
        plan: bounded_text(plan),
        fetched_unix: observed_at_unix,
    })
}

fn push_row(rows: &mut Vec<LimitRow>, w: &Window, fallback_kind: &str) {
    if rows.len() == MAX_LIMIT_ROWS {
        return;
    }
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
            if fallback_kind == "session" {
                "Session"
            } else {
                "Weekly"
            }
            .to_string(),
        ),
    };
    rows.push(LimitRow {
        kind: kind.into(),
        label: bounded_text(label),
        percent: clamp_percent(pct),
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

#[cfg(test)]
mod tests {
    use super::*;

    const OBSERVED_AT: i64 = 1_788_400_000;

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            "normal" => include_bytes!("../tests/fixtures/codex/normal.json"),
            "partial" => include_bytes!("../tests/fixtures/codex/partial.json"),
            "unknown-fields" => include_bytes!("../tests/fixtures/codex/unknown-fields.json"),
            "malformed" => include_bytes!("../tests/fixtures/codex/malformed.json"),
            "missing-reset" => include_bytes!("../tests/fixtures/codex/missing-reset.json"),
            "weekly-primary" => {
                include_bytes!("../tests/fixtures/codex/weekly-primary.json")
            }
            "non-finite" => include_bytes!("../tests/fixtures/codex/non-finite.json"),
            "out-of-range" => include_bytes!("../tests/fixtures/codex/out-of-range.json"),
            _ => panic!("unknown fixture"),
        }
    }

    fn parse_fixture(name: &str) -> Result<UsageSnapshot, FetchErr> {
        parse_usage_json(fixture(name), OBSERVED_AT)
    }

    #[test]
    fn parses_sanitized_codex_fixture_matrix() {
        let normal = parse_fixture("normal").unwrap();
        assert_eq!(normal.rows.len(), 2);
        assert_eq!(normal.rows[0].kind, "session");
        assert_eq!(normal.rows[1].kind, "weekly_all");
        assert_eq!(normal.fetched_unix, OBSERVED_AT);

        let partial = parse_fixture("partial").unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.rows[0].kind, "weekly_all");

        assert_eq!(parse_fixture("unknown-fields").unwrap().rows.len(), 1);
        assert!(parse_fixture("malformed").is_err());
        assert!(parse_fixture("non-finite").is_err());

        let missing_reset = parse_fixture("missing-reset").unwrap();
        assert_eq!(missing_reset.rows[0].resets_unix, None);
        assert!(missing_reset.rows[0].reset_text.is_empty());

        let weekly = parse_fixture("weekly-primary").unwrap();
        assert_eq!(weekly.rows[0].kind, "weekly_all");
        assert_eq!(weekly.rows[0].label, "Weekly");

        let out_of_range = parse_fixture("out-of-range").unwrap();
        assert_eq!(out_of_range.rows[0].percent, 0.0);
        assert_eq!(out_of_range.rows[1].percent, 100.0);
    }
}
