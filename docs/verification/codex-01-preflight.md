# CODEX-01 / CODEX-02 preflight (2026-10-09)

Result: blocked on the credential contract; no adapter or source-default change.
The owner requested both tasks. Repository invariants require stopping when an
implementation would refresh, exchange, or write provider credentials.

## Evidence

The active PATH installation's npm metadata reports Codex 0.159.1; a second
global installation reports 0.160.0. Only package metadata and native
`app-server --help` were read/run. No provider credentials were copied or
written, and no live app-server limits request was made.

The [official app-server documentation](https://developers.openai.com/codex/app-server)
describes managed ChatGPT auth as automatically refreshing/persisting tokens.
`account/read` with `refreshToken: false` disables a forced refresh; it is not
a global read-only authentication policy.

Source tag `rust-v0.159.1` resolves to commit
`8e68a98ef03cdde76d2e6800791ebdf1b3b95b24`. Its request path is:

1. [`get_account_rate_limits_response`, line 1115](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/app-server/src/request_processors/account_processor.rs#L1115)
   calls `auth_with_http_client_factory()` before backend requests.
2. [`auth_with_http_client_factory`, line 2411](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L2411)
   calls `auth()`.
3. [`auth`, line 2393](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L2393)
   calls `refresh_token()` when proactive refresh is due.
4. [`should_refresh_proactively`, line 3004](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L3004)
   checks JWT expiry within five minutes, with an older-refresh fallback.
5. [`refresh_and_persist_chatgpt_token`, line 3092](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L3092)
   exchanges the refresh token and persists replacement tokens.

Reading fresh credentials before launching a managed app-server does not remove
this capability or prove all startup/request paths read-only. A deadline or a
process-tree kill also cannot undo a completed token exchange.

## Unblock conditions

Keep compatibility polling until an official read-only path is demonstrated,
or the owner authorizes investigating an isolated external-token design. The
documented experimental `chatgptAuthTokens` mode accepts a token over stdio and
delegates refresh requests to the host. It is documented for hosts that own the
auth lifecycle, so it is not accepted here as a drop-in read-only replacement.

Any candidate must prove that it cannot load managed credentials at startup,
cannot exchange or persist tokens, refuses every refresh request, does not
inherit telemetry/plugins or unapproved network destinations, preserves the
prepared account identity, and leaves no child after success or failure.
Approval to investigate is not approval to refresh, exchange, or write tokens.
Automated tests must use synthetic tokens and fake processes/endpoints.

CODEX-02 stays unchecked until an eligible adapter exists and real reference
measurements prove the deadline, <= 2 s p95, no leaked child, and no direct
compatibility request in an app-server-success cycle. Fake-process latency is
not evidence for promoting the installed CLI to the default.

The last provisioned x64 measurement is 1,163,776 bytes, leaving 358 bytes under
the original cumulative CI regression gate. Footprint reduction remains a
prerequisite; this documentation change adds no executable bytes.

## Release preparation

Existing work can be released independently of these incomplete tasks once the
R0 signing/provisioning/evidence requirements are satisfied. The 0.9.0 draft
now includes the committed state/cache/error/diagnostic work and corrects its
outdated SIZE-01 size claim. Version remains the owner's choice; no version,
release workflow, production policy, tag, secret, or repository setting changed.

Rollback: revert these documentation changes; no runtime or schema change.

## Local verification

The existing source passes fmt, Clippy with warnings denied, all 168 tests,
and the x64 release build. The x64 unprovisioned executable remains 1,053,184
bytes. The read-only live release-infrastructure audit passes main/tag
protection, approval-gated release environment, and immutable-release checks.
These checks do not prove production signing or secret provisioning. The
current release workflow still has no Authenticode integration.
Demo safety and the x64 artifact gate pass; demo execution creates no provider
connections, child processes, files, registry changes, or power-policy changes.

## §0.7 handoff

```text
Task: CODEX-01 / CODEX-02 — documented-source preflight
Result: blocked (managed app-server violates the credential invariant)
Changed: plan, CHANGELOG, proposed release notes, preflight evidence
Gates: fmt ok | clippy ok | test ok (168 passed) | release build ok
Runtime check: native app-server help only; isolated demo safety passes; no live limits read
Size: x64 1,053,184 bytes unprovisioned (unchanged)
Docs updated: plan, CHANGELOG, release notes, verification
Deviations from plan: no adapter/default change because pinned upstream source proves proactive token exchange/persistence
Follow-ups: owner decision on isolated external-token investigation; size headroom; human signing/provisioning and ARM64 release evidence
```
