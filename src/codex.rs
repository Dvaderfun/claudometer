//! Fetches Codex usage from the same endpoint the Codex CLI's TUI polls
//! (`wham/usage`). Read-only: never refreshes or rewrites the OAuth token —
//! OpenAI rotates refresh tokens, so an external refresh would invalidate the
//! user's Codex CLI session. Expired = tell user to open Codex.

use std::fs::File;

use serde::Deserialize;

use crate::api::{
    bounded_text, plain, prettify, read_bounded, read_json_with_retry, CredentialReadError,
    FetchErr, PreparationFailure, PreparationFailureKind, MAX_LIMIT_ROWS,
};
use crate::provider::error::{request_error, FailureKind, FetchError};
use crate::provider::model::{
    derive_account_context, AccountContext, AccountKey, FetchOutcome, LimitKind, ProviderId,
    SecretString, SourceProvenance, UsageLimit, UsageSnapshot,
};

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
        Err(error) => FetchOutcome::Failure(error),
    }
}

fn fetch_inner(request: PreparedRequest) -> Result<UsageSnapshot, FetchErr> {
    // Expiry lives in the JWT `exp` claim (auth.json has no expires field).
    // Unparseable claim = skip the check and let the server decide.
    if let Some(exp) = jwt_exp(request.access_token.expose()) {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if now > exp {
            return Err(FetchError::new(FailureKind::Authentication));
        }
    }

    let tls = native_tls::TlsConnector::new().map_err(|_| FetchError::new(FailureKind::Offline))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let authorization = SecretString::new(format!("Bearer {}", request.access_token.expose()));
    let resp = crate::network::get(&agent, crate::network::CODEX_USAGE_URL)
        .set("Authorization", authorization.expose())
        .set("chatgpt-account-id", request.account_id.expose())
        .set(
            "User-Agent",
            concat!("claudometer/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| request_error(error, time::OffsetDateTime::now_utc().unix_timestamp()))?;

    let body = read_bounded(resp.into_reader())?;
    parse_usage_json(
        &body,
        time::OffsetDateTime::now_utc().unix_timestamp(),
        request.account.key,
    )
}

fn parse_usage_json(
    body: &[u8],
    observed_at_unix: i64,
    account: AccountKey,
) -> Result<UsageSnapshot, FetchErr> {
    let parsed: UsageResp =
        serde_json::from_slice(body).map_err(|_| plain("Unexpected API response shape."))?;

    let mut rows: Vec<UsageLimit> = Vec::new();
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
        provider: ProviderId::Codex,
        account,
        source: SourceProvenance::compatibility(ProviderId::Codex),
        rows,
        plan: (!plan.is_empty()).then(|| bounded_text(plan)),
        fetched_unix: observed_at_unix,
    })
}

fn push_row(rows: &mut Vec<UsageLimit>, w: &Window, fallback_kind: &str) {
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
    let typed_kind = match w.limit_window_seconds {
        Some(s) if s > 0 && s <= 24 * 3600 => LimitKind::Session,
        Some(s) if s > 24 * 3600 => LimitKind::Weekly,
        _ => LimitKind::Other("unknown_window".into()),
    };
    let duration = w
        .limit_window_seconds
        .and_then(|s| u32::try_from(s).ok())
        .filter(|s| *s > 0);
    if let Some(row) = UsageLimit::from_adapter(
        kind.into(),
        typed_kind,
        bounded_text(label),
        pct,
        None,
        w.reset_at,
        duration,
    ) {
        rows.push(row);
    }
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
        parse_usage_json(fixture(name), OBSERVED_AT, AccountKey::from_digest([2; 32]))
    }

    #[test]
    fn weekly_only_and_missing_duration_display_are_characterized() {
        let weekly = parse_fixture("weekly-primary").unwrap();
        assert_eq!(weekly.rows.len(), 1);
        assert_eq!(weekly.rows[0].label, "Weekly");
        let unknown = parse_usage_json(
            br#"{"rate_limit":{"primary_window":{"used_percent":12}}}"#,
            OBSERVED_AT,
            AccountKey::from_digest([2; 32]),
        )
        .unwrap();
        assert_eq!(unknown.rows[0].label, "Session");
        assert_eq!(unknown.rows[0].resets_unix, None);
        assert_eq!(unknown.rows[0].window_seconds, None);
        assert!(matches!(unknown.rows[0].kind, LimitKind::Other(_)));
    }

    #[test]
    fn parses_sanitized_codex_fixture_matrix() {
        let normal = parse_fixture("normal").unwrap();
        assert_eq!(normal.rows.len(), 2);
        assert_eq!(normal.rows[0].kind, LimitKind::Session);
        assert_eq!(normal.rows[1].kind, LimitKind::Weekly);
        assert_eq!(normal.fetched_unix, OBSERVED_AT);
        assert_eq!(normal.provider, ProviderId::Codex);
        assert!(normal.account == AccountKey::from_digest([2; 32]));
        assert_eq!(
            normal.source.support,
            crate::provider::model::SourceSupport::Compatibility
        );
        assert_eq!(normal.rows[0].window_seconds, Some(18_000));
        assert_eq!(normal.rows[1].window_seconds, Some(604_800));

        let partial = parse_fixture("partial").unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.rows[0].kind, LimitKind::Weekly);

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
        assert_eq!(weekly.rows[0].label, "Weekly");

        let out_of_range = parse_fixture("out-of-range").unwrap();
        assert_eq!(out_of_range.rows[0].percent.get(), 0.0);
        assert_eq!(out_of_range.rows[1].percent.get(), 100.0);
    }
}
