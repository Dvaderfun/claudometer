# DIAG-01 — operational snapshot (2026-10-09)

`diagnostics.rs` projects fixed local operational fields. Version/architecture,
Windows build, install channel, update/power/store status, enabled providers,
detected/sign-in state, selected source/support/fallback reason, reserved attempt
and accepted-success timestamps, observation/age/freshness, cooldown/next retry,
stable error code, and the six non-secret settings appear in Settings.

Provider metadata is UI-owned. Unknown detection before preparation is explicit;
obsolete events cannot change attempt/success metadata. Cache observation time
is distinct from success in this process. Detection means a usable local
credential was prepared, not a launched CLI probe. Compatibility source is
currently selected; no invented app-server attempt/fallback is claimed.

The snapshot excludes account key/digest, raw ID, email, username, paths,
credential contents, plan/limit strings, and response/transport text. Windows
build uses read-only RtlGetVersion; channel classification reuses updater reads;
recovery checks presence of the adjacent update journal without loading/mutating
it. No log, new persisted settings, CLI command, or endpoint is introduced.

Settings adds Diagnostics after existing cards, retaining the initial window
height with scrolling. Keyboard focus scrolls to the card header; Invoke,
Tab/Enter/Space and click execute Copy. UIA HelpText exposes the exact export.
Normal explicit copy writes CF_UNICODETEXT using a moveable global allocation;
Windows owns the allocation only after successful SetClipboardData. Errors free
untransferred memory/close clipboard and show Retry; success shows Copied.
Demo invokes the same action/UI but suppresses the clipboard write.

## Evidence

- Baseline 161 tests passed. Final fmt/clippy/**165 tests**/x64 release build
  pass, including required-field/excluded-field, unknown/disabled/cache/error,
  obsolete timestamp, and scroll-geometry tests.
- Deterministic Settings demo exposes all required fields in UIA HelpText;
  Copy Invoke and Enter update the button to Copied. Clipboard content remains
  byte-for-byte unchanged in demo. Reviewed screenshots preserve the existing
  initial Settings size and show readable Diagnostics content/header/scroll.
  Local evidence: ignored `target/diag-01/`.
- Actual clipboard allocation/ownership path is reviewed; demo intentionally
  does not exercise SetClipboardData against the user's clipboard. No live
  provider endpoint or credential mutation is used by tests.
- Demo safety, existing UIA client checks, privacy/dependency/workflow and
  x64 artifact checks pass. Windows features DataExchange/Memory added to
  existing windows 0.58 bindings; no new crate or package-version change.
  All 22 existing UIA client checks pass alongside the new diagnostics check.
- x64: 1,048,576 unprovisioned (+0.99%) / 1,159,168 provisioned (+0.89%),
  +10,240 bytes each versus ERR-01. Hard-ceiling headroom 151,552 bytes.

Rollback: prior binary/revert. No state/settings schema migration, diagnostics
file, registry mutation, service, or helper. Clipboard retention is owned by
Windows/user settings, as documented in PRIVACY. Do not change provider
credentials or unresolved recovery journals.

## §0.7 handoff

```text
Task: DIAG-01 — operational diagnostics snapshot
Result: done
Changed: diagnostics, app, updater, main, gfx, accessibility, demo; Cargo Windows features; plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Gates: fmt ok | clippy ok | test ok (165 passed) | release build ok
Runtime check: Settings Diagnostics fields/Copy Invoke/keyboard, demo clipboard guard, UIA HelpText and screenshot review; demo safety
Size: x64 1,048,576 unprovisioned (+0.99%) / 1,159,168 provisioned (+0.89%) vs ERR-01
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Deviations from plan: detection uses prepared credential state rather than CLI probe; raw labels/identifiers excluded by construction; demo suppresses clipboard writes
Follow-ups: DIAG-02 next; continued size care; Narrator remains deferred
```
