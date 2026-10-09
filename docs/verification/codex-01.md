# CODEX-01: isolated documented source (2026-10-09)

Result: implemented locally on `feat/codex-isolated-app-server`, under the
owner-approved exceptions in ADR 0007. `Codex app-server` in Settings is opt-in;
Compatibility is the default because CODEX-02 measured p95 above two seconds.

`codex_server.rs` selects exact audited native x64 0.159.1/0.160.0 hashes,
holds the image open without write/delete sharing, rejects machine Codex
configuration, and launches only native bytes. ARM64/other versions use
Compatibility. No Node/npm/batch launcher or managed login is executed.

The child receives access token/account ID only through private stdin,
ephemeral external auth, and local plan hint `unknown`. The hint prevents
enterprise cloud configuration from overriding the quota reader's policy;
display plan comes from the quota response. Cleared environment, fresh scratch
profile, static synthetic model catalog, disabled plugins/remote control/OTEL/
analytics/runtime metrics, and pre-creation kill-on-close Job Object remain
binding. Token JSON is serialized from borrowed strings into a wiped outgoing
buffer, never a token-owning generic JSON tree. The refresh token is never read
or passed. Any child request ends the cycle; no replacement credentials.

The worker polls its private stdout pipe with bounded reads and a ten-second
deadline, without an extra reader thread. Maximum frame is 1 MiB, notifications
64 per RPC, outgoing request 16 KiB. Wrong IDs, malformed frames, and raw RPC
errors never enter diagnostics. Cleanup calls absolute System32 `taskkill /T`,
terminates the job, verifies root exit and zero active children, then removes
owned scratch contents without following reparse points. Unlocked crash debris
is removed before later normal polls; another live worker's owner lock prevents
deletion. No provider-owned file is modified.

Selected source/fallback travels through the account/generation-bound UI
preparation handoff and matches cache restoration. Obsolete preparation cannot
change source. A source transition clears previous data before loading matching
cache. App-server failure ends the cycle and never starts a direct Compatibility
request. Dynamic model buckets, duration-based quota kinds, Spend limits, plan
labels, and display-only reset-credit count normalize into schema-1 snapshots.

Evidence:

- Required fmt/Clippy/174 tests/x64 release build pass. The fixture matrix covers
  dynamic model limits, weekly-as-primary, missing reset, Spend class, plan,
  reset-credit count, wrong account, wrong RPC ID, malformed/oversized/incomplete
  frames, server action/refresh refusal, missing fields, and fake success/error.
- A standalone std-only fake process is built with rustc in `target/` by tests;
  it never uses credentials or provider network. A hung descendant reports its
  PID before blocking; job termination signals both root and descendant handles.
  Timeout uses an already-expired injected deadline, with no test sleep.
- Real native CLI synthetic probes pass on 0.159.1 and 0.160.0. The original
  business hint fetched/cached cloud policy; `--unknown-plan-hint` eliminates
  that request/cache for a synthetic business token while limits still succeed.
  Account discovery remains read-only and documented. Probe startup and all
  fake endpoints are isolated; no production credential is used in these tests.
- Manual live samples use the existing default `.codex` account because this
  agent session's custom `CODEX_HOME` has no auth file. Twenty timed samples
  plus initial smoke succeed; credential SHA-256 stays unchanged. Final ten
  samples: 1.7695 s median, 2.198 s p95. No child/scratch remains.
- Demo safety passes; existing 22 UIA checks and the new source toggle's
  discoverability/focus/Toggle/keyboard checks pass. Screenshots reviewed.
  Demo actions are guarded; config round-trip tests prove persistence.
- Privacy/dependency/workflow policies pass. No crate/version added; Windows
  JobObjects/Pipes features added. Fixed Retry-After format moves to a compile-
  time macro to recover measured executable headroom; date/error tests pass.

Known limits: upstream maps HTTP failures to generic RPC `-32603`. Raw message
classification is forbidden, so those failures use `response_invalid`, not an
invented 401/429 category. Local JWT expiry/external refresh are authentication
errors; transport deadline is timeout. Typed upstream error metadata is a future
compatibility improvement. Exact-hash gating intentionally requires a fresh
audit after Codex upgrades. Real ARM64 runtime and Narrator remain unverified.

Rollback: turn the source toggle off, or revert this commit. New settings and
reset-credit fields are additive under schema 1; previous binaries ignore them.
Cache from a different source is not restored; no credential/data migration.

```text
Task: CODEX-01 — isolated Codex app-server adapter
Result: done locally; opt-in
Changed: codex_server/codex/poller/app/config/model/main/gfx/UIA/error; ADR, plan, CHANGELOG, PRIVACY, architecture, ledger, verification
Gates: fmt ok | clippy ok | test ok (174 passed) | release build ok
Runtime check: fake protocol/tree lifecycle; real CLI synthetic isolation; manual live read-only samples; demo safety/22 UIA/source toggle
Size: x64 1,071,104 unprovisioned / 1,163,776 provisioned (+1.61% vs local R0); ARM64 1,017,856 provisioned; original gates unchanged
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, ADR, architecture, ledger, verification
Deviations: only audited x64 native images eligible; unknown local plan hint suppresses cloud policy; generic RPC errors stay generic
Follow-ups: audit new CLI versions/ARM64; upstream typed status/retry metadata; default-source latency improvement
```
