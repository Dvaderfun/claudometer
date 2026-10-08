use std::error::Error as _;

use super::model::ProviderId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Transient,
    RateLimited,
    Authentication,
    MissingCredentials,
    UsageScope,
    Timeout,
    Offline,
    UnexpectedResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FailureKind,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl FetchError {
    pub fn new(kind: FailureKind) -> Self {
        let message = match kind {
            FailureKind::MissingCredentials => "Not signed in",
            FailureKind::Authentication => "Sign-in expired",
            FailureKind::UsageScope => "Sign in again for live usage",
            FailureKind::RateLimited => "Paused by provider",
            FailureKind::Timeout => "Timed out",
            FailureKind::Offline => "Offline",
            FailureKind::UnexpectedResponse | FailureKind::Transient => "Couldn't read usage",
        };
        Self {
            kind,
            message: message.to_string(),
            retry_after: None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            FailureKind::MissingCredentials => "credentials_missing",
            FailureKind::Authentication => "sign_in_expired",
            FailureKind::UsageScope => "usage_scope_missing",
            FailureKind::RateLimited => "provider_rate_limited",
            FailureKind::Timeout => "request_timeout",
            FailureKind::Offline => "connection_failed",
            FailureKind::UnexpectedResponse => "response_invalid",
            FailureKind::Transient => "local_unavailable",
        }
    }

    pub fn detail(&self, provider: ProviderId, retry_time: Option<&str>) -> String {
        let name = if provider == ProviderId::Claude {
            "Claude"
        } else {
            "Codex"
        };
        match self.kind {
            FailureKind::MissingCredentials if provider == ProviderId::Claude => "Open Claude Code and sign in.".to_string(),
            FailureKind::MissingCredentials => "Run codex and sign in.".to_string(),
            FailureKind::Authentication if provider == ProviderId::Claude => "Open Claude Code once; it renews the sign-in automatically.".to_string(),
            FailureKind::Authentication => "Run codex once; it renews the sign-in automatically.".to_string(),
            FailureKind::UsageScope => "This login can run Claude but cannot read usage limits (for example, a token from claude setup-token). Run claude and sign in with your Claude account.".to_string(),
            FailureKind::RateLimited => format!("{name} is limiting usage checks. Retrying at {}.", retry_time.unwrap_or("the next allowed time")),
            FailureKind::Timeout => format!("No response within 10 seconds. Retrying at {}.", retry_time.unwrap_or("the next update")),
            FailureKind::Offline => format!("Can't reach {}. Showing the last values.", if provider == ProviderId::Claude { "api.anthropic.com" } else { "chatgpt.com" }),
            FailureKind::UnexpectedResponse => format!("The usage format changed. Code {}. Copy diagnostics to report it.", self.code()),
            FailureKind::Transient => format!("Usage check unavailable. Code {}. Retrying at {}.", self.code(), retry_time.unwrap_or("the next update")),
        }
    }
}

pub(crate) fn io_error(error: &std::io::Error) -> FetchError {
    FetchError::new(
        if matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            FailureKind::Timeout
        } else {
            FailureKind::Offline
        },
    )
}

pub(crate) fn request_error(error: ureq::Error, now_unix: i64) -> FetchError {
    match error {
        ureq::Error::Status(401 | 403, _) => FetchError::new(FailureKind::Authentication),
        ureq::Error::Status(429, response) => {
            let mut error = FetchError::new(FailureKind::RateLimited);
            error.retry_after = response
                .header("retry-after")
                .and_then(|value| retry_after(value, now_unix));
            error
        }
        ureq::Error::Status(_, _) => FetchError::new(FailureKind::UnexpectedResponse),
        ureq::Error::Transport(transport) => {
            let mut source = transport.source();
            while let Some(error) = source {
                if let Some(io) = error.downcast_ref::<std::io::Error>() {
                    return io_error(io);
                }
                source = error.source();
            }
            FetchError::new(FailureKind::Offline)
        }
    }
}

fn retry_after(value: &str, now_unix: i64) -> Option<u64> {
    let value = value.trim();
    value.parse().ok().or_else(|| {
        let format = time::format_description::parse_borrowed::<2>(
            "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT",
        )
        .ok()?;
        let timestamp = time::PrimitiveDateTime::parse(value, &format)
            .ok()?
            .assume_utc()
            .unix_timestamp();
        Some(timestamp.saturating_sub(now_unix).max(0) as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_roadmap_error_has_exact_short_detail_and_stable_code() {
        for (provider, kind, short, detail, code) in [
            (ProviderId::Claude, FailureKind::MissingCredentials, "Not signed in", "Open Claude Code and sign in.", "credentials_missing"),
            (ProviderId::Claude, FailureKind::Authentication, "Sign-in expired", "Open Claude Code once; it renews the sign-in automatically.", "sign_in_expired"),
            (ProviderId::Claude, FailureKind::UsageScope, "Sign in again for live usage", "This login can run Claude but cannot read usage limits (for example, a token from claude setup-token). Run claude and sign in with your Claude account.", "usage_scope_missing"),
            (ProviderId::Claude, FailureKind::RateLimited, "Paused by provider", "Claude is limiting usage checks. Retrying at 18:42.", "provider_rate_limited"),
            (ProviderId::Claude, FailureKind::Timeout, "Timed out", "No response within 10 seconds. Retrying at 18:42.", "request_timeout"),
            (ProviderId::Claude, FailureKind::Offline, "Offline", "Can't reach api.anthropic.com. Showing the last values.", "connection_failed"),
            (ProviderId::Codex, FailureKind::MissingCredentials, "Not signed in", "Run codex and sign in.", "credentials_missing"),
            (ProviderId::Codex, FailureKind::Authentication, "Sign-in expired", "Run codex once; it renews the sign-in automatically.", "sign_in_expired"),
            (ProviderId::Codex, FailureKind::UnexpectedResponse, "Couldn't read usage", "The usage format changed. Code response_invalid. Copy diagnostics to report it.", "response_invalid"),
        ] {
            let error = FetchError::new(kind);
            assert_eq!(error.message, short);
            assert_eq!(error.detail(provider, Some("18:42")), detail);
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn status_errors_never_read_or_expose_response_bodies() {
        for (status, expected) in [
            (401, FailureKind::Authentication),
            (403, FailureKind::Authentication),
            (429, FailureKind::RateLimited),
            (500, FailureKind::UnexpectedResponse),
        ] {
            let response = ureq::Response::new(
                status,
                "synthetic-private-status",
                "synthetic-private-body bearer secret",
            )
            .unwrap();
            let error = request_error(ureq::Error::Status(status, response), 1000);
            assert_eq!(error.kind, expected);
            let rendered = format!("{error:?} {}", error.detail(ProviderId::Claude, None));
            assert!(!rendered.contains("synthetic-private"));
            assert!(!rendered.contains("bearer secret"));
        }
    }

    #[test]
    fn request_timeouts_and_connect_errors_are_distinct_without_network() {
        for (io, expected) in [
            (std::io::ErrorKind::TimedOut, FailureKind::Timeout),
            (std::io::ErrorKind::WouldBlock, FailureKind::Timeout),
            (std::io::ErrorKind::ConnectionRefused, FailureKind::Offline),
        ] {
            let error = request_error(
                std::io::Error::new(io, "private-provider-error").into(),
                1000,
            );
            assert_eq!(error.kind, expected);
            assert!(!format!("{error:?}").contains("private-provider-error"));
        }
    }

    #[test]
    fn retry_after_supports_seconds_and_http_dates() {
        assert_eq!(retry_after(" 120 ", 0), Some(120));
        let now = time::macros::datetime!(2026-10-09 18:40 UTC).unix_timestamp();
        assert_eq!(retry_after("Fri, 09 Oct 2026 18:42:00 GMT", now), Some(120));
        assert_eq!(retry_after("Fri, 09 Oct 2026 18:39:00 GMT", now), Some(0));
        assert_eq!(retry_after("invalid", now), None);
        let response: ureq::Response =
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\n\r\nprivate"
                .parse()
                .unwrap();
        assert_eq!(
            request_error(ureq::Error::Status(429, response), now).retry_after,
            Some(120)
        );
    }
}
