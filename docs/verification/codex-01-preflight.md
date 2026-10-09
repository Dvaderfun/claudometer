# CODEX-01 / CODEX-02 preflight (2026-10-09)

Historical preflight result: blocked. Superseded by owner-approved ADR 0007
and the implemented opt-in adapter in [CODEX-01 evidence](codex-01.md).
The investigation below remains as evidence of why managed auth is forbidden.
The owner requested both tasks. Repository invariants require stopping when an
implementation would refresh, exchange, or write provider credentials.

## Evidence

The active PATH installation's npm metadata reports Codex 0.159.1; a second
global installation reports 0.160.0. Initial preflight read package metadata
and native `app-server --help` only. The follow-up below launches both native
executables against a local fake backend with synthetic tokens and isolated
profiles. No real provider credentials were read, copied, or written by the
follow-up, and no live provider limits request was made.

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

## External-token follow-up (2026-10-09)

The owner requested CODEX-01/02 again. Investigation proceeds without changing
the secret boundary: §1.2 says secret material stays worker-local and is never
serialized. An external-token adapter would send the real access token to a
child through JSON stdio, so even a successfully isolated design needs an
explicit, narrow exception. No such exception is assumed here.

[`ci/probe-codex-contract.py`](../../ci/probe-codex-contract.py) accepts an
explicit native `codex.exe` and tests personal success, business success, and
personal 401. It uses a fresh temporary `CODEX_HOME`/profile each time, clears
inherited environment except Windows runtime/temp paths, disables analytics,
OTEL exporters, plugins, remote control, and runtime metrics, selects ephemeral
credential storage, and supplies a synthetic static catalog to suppress model
discovery. It refuses machines with system Codex config/requirements files,
which could override the fake backend. Every request targets a loopback fake
backend; no production token is loaded. The script itself is a manual audit
tool, not a resident component or part of normal startup.

Run on Windows with Python, from the repository root:

```powershell
python ci/probe-codex-contract.py --exe C:\path\to\native\codex.exe --report target\codex-contract-report.json
```

Both installed versions produced the same observations:

| Synthetic case | Backend requests observed | Result |
|---|---|---|
| Personal success | `wham/accounts/check`, `wham/usage` | limits success, no token/ID persistence |
| Business success | `wham/config/bundle`, `wham/accounts/check`, `wham/usage` | limits success, raw account/user IDs persisted in cloud bundle cache |
| Personal 401 | `wham/accounts/check`, `wham/usage` | limits fail with generic JSON-RPC code `-32603`; no refresh request observed |

Every case leaves `auth.json` absent and the synthetic access token absent
from files. The child exits and its stdout reader stops after `taskkill /T /F`.
The fake account-discovery response deliberately contains no matching workspace,
so login-completion `success` is false even though the later limits read can
succeed. This does not establish successful live login/routing behavior.
No child tree was observed after the six completed probes; this is not the
hung-descendant fault suite required for a production adapter.

The probe waits for `account/login/completed`, not only the login RPC result:
the latter is sent before notification-triggered account discovery/plugin work.
Killing as soon as the limits result arrives can conceal requests racing with
that result. Reusing the scratch home can conceal cloud-bundle requests through
the cache. A clean profile per sample is therefore essential.

Pinned-source evidence explaining the results:

- [`from_external_access_token`, line 1732](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L1732)
  supplies no refresh token; external auth commits to process-local Ephemeral
  storage ([line 3055](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/login/src/auth/manager.rs#L3055)).
- [`login_chatgpt_auth_tokens`, line 811](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/app-server/src/request_processors/account_processor.rs#L811)
  sends the RPC result before `send_login_success_notifications`, which reads
  account state and can refresh plugin caches.
- [`read_account`, line 190](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/app-server/src/request_processors/account_processor/workspace_routing.rs#L190)
  calls `get_accounts_check` during workspace discovery. Its routing response
  can influence the backend; this needs separate destination/identity review.
- [`cloud_config_eligible_auth`, line 50](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/cloud-config/src/service.rs#L50)
  admits business/education/enterprise auth. External login replaces the cloud
  loader and synchronizes configuration; that loader starts background work
  ([line 44](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/cloud-config/src/bundle_loader.rs#L44)).
- [`ModelsRefreshWorker`, line 33](https://github.com/openai/codex/blob/8e68a98ef03cdde76d2e6800791ebdf1b3b95b24/codex-rs/app-server/src/models_refresh_worker.rs#L33)
  starts an Online catalog read immediately; the probe's static fixture avoids
  that dependency. This alone does not disable account/cloud discovery.

Consequently, `CODEX-01` still lacks an eligible production adapter and
`CODEX-02` has no real p95/default-source evidence. Additional lifecycle side
effects would need explicit product/privacy decisions and isolation proof;
turning off telemetry or withholding refresh responses alone is insufficient.
The audit does not change `PRIVACY.md`'s shipped runtime inventory.

## Unblock conditions

Keep compatibility polling until an official read-only path is demonstrated,
or an isolated external-token design satisfies the explicit secret/side-effect
decisions and isolation proof above. The
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

The older DIAG-02 provisioned x64 measurement was 1,163,776 bytes, leaving 358
bytes under the original cumulative CI regression gate. R0 removed optional
Ed25519 tables: its local x64 measurement is 1,145,344 bytes, leaving 18,790
bytes; the exact published 0.9.1 x64 is 1,142,784 bytes. Any adapter must still
fit the original gate; audit scripts/docs add no executable bytes.

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

## Follow-up handoff

```text
Task: CODEX-01 / CODEX-02 — isolated external-token investigation
Result: partial investigation; runtime tasks blocked
Changed: ci/probe-codex-contract.py, synthetic model catalog, plan, CHANGELOG, CLAUDE, preflight evidence
Gates: fmt ok | clippy ok | test ok (168 passed) | release build ok
Runtime check: six local fake-backend cases across native 0.159.1/0.160.0; demo safety; x64 artifact check
Size: x64 1,053,184 bytes unprovisioned (unchanged); no new provisioned measurement
Docs updated: plan, CHANGELOG, CLAUDE, verification; PRIVACY/ledger unchanged because shipped behavior/artifact size unchanged
Deviations from plan: no production adapter or live p95 due to secret-serialization rule and extra upstream account/cloud lifecycle
Follow-ups: narrow private-stdio exception decision; control account/cloud configuration and ID persistence; typed error mapping; footprint; eligible-adapter lifecycle tests and real CODEX-02 measurements
Rollback: revert the probe/docs commit; no runtime schema or provider data migration
```
