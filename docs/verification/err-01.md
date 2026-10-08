# ERR-01 — actionable provider errors (2026-10-09)

`provider/error.rs` owns failure kinds, stable codes, fixed short text, and
provider-specific recovery details. Adapters emit `FetchOutcome::Failure`;
the old string variant is cfg(test)-only for existing parity tests. Error
codes derive from a finite category instead of transport or response text.

Both adapters map 401/403 to Authentication, 429 to RateLimited with numeric
or HTTP-date Retry-After, IO timeout to Timeout, connection failure to Offline,
and parse/unknown HTTP failure to UnexpectedResponse. Error response bodies,
status strings, and transport messages are discarded. Body reads preserve
the 1 MiB limit and classify read timeouts separately. Profile-plan fallback
and request destinations remain unchanged.

Local read-only boolean inspection confirmed `claudeAiOauth.scopes` is an
array in the owner's credential and contains `user:profile`; no credential
values were printed or saved. The optional field preserves legacy credentials.
If present without `user:profile`, validation rejects before identity/request
preparation. It never starts login, refresh, token exchange, or credential writes.

Accepted authentication failure clears account-bound snapshot/cache/alerts;
scope failure clears previous data. Transient errors retain same-account values
with existing age policy. Retry time derives from the attempt/cooldown deadline
and does not slide forward on repaint. Short status appears immediately below
the provider header while data remains, preserving plan/buttons and quota rows.
Full recovery detail is in UIA HelpText and a native flyout tooltip. The UI owns
the UTF-16 tooltip buffer. Native TTTOOLINFO uses the supported V2 size; the full
struct size created the window but rejected registration during verification.

## Evidence

- Baseline 151 tests passed; final fmt, clippy, **161 tests**, x64 release build.
- Table-driven tests pin every §5.3 row's short/detail/code. Fixtures cover
  HTTP failures, numeric/date Retry-After, timeout/connect distinction, bodies
  excluded from Debug/copy, inference-only scopes, auth clearing, transient
  preservation, stable retry time, and matching renderer/UIA status geometry.
- No test calls live providers or modifies credentials. Existing parser
  fixture files and compatibility requests remain unchanged.
- Demo error now uses deterministic Offline copy. Client reads exact HelpText;
  native TTM_GETTOOLCOUNT/TTM_GETTEXTW proves one registered tooltip with the
  exact recovery text. The test allocates temporary memory only in its own
  synthetic demo process; tool screenshots omit transient tooltip windows.
  Failed assertions due to JSON apostrophe escaping were corrected by parsing
  JSON. Failed native registration was corrected before final proof.
- Existing 22 UIA checks pass; error/both/Settings screenshots reviewed.
  Local evidence: ignored `target/err-01/`.
- Demo safety passes; privacy/dependency/workflow and artifact checks pass.
  No new crate, endpoint, durable field, or package version change.
- x64: 1,038,336 unprovisioned (+3.95%) / 1,148,928 provisioned (+3.55%),
  +39,424 bytes each vs CACHE-01. Provisioned ceiling headroom 161,792 bytes;
  soft target/CI budgets remain unchanged. Further v0.10 slices need size care.

Copy diagnostics is introduced by DIAG-01; ERR-01 supplies the exact instruction
and code now. No screen-reader speech or ARM64 runtime claim. Rollback: prior
binary/revert; state schema 1 and cache/receipt formats remain compatible.
Never modify provider credentials or unresolved safety/update journals.

## §0.7 handoff

```text
Task: ERR-01 — actionable errors
Result: done
Changed: provider/error/model/state, api/codex, app/main/gfx/accessibility/demo, test-only state_reference; plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Gates: fmt ok | clippy ok | test ok (161 passed) | release build ok
Runtime check: demo safety, 22 UIA checks, error UIA detail, native tooltip registration/text, screenshot review
Size: x64 1,038,336 unprovisioned (+3.95%) / 1,148,928 provisioned (+3.55%) vs CACHE-01
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Deviations from plan: status below header preserves existing layout; Copy diagnostics action belongs to DIAG-01
Follow-ups: DIAG-01 next; continued footprint attention; Narrator remains deferred
```
