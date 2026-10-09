# UI-CLARITY-01 — 2026-10-10

Owner follow-up to FRESH-01; accepted product change in ADR 0009.

Vibecode mode combines the existing wake request and journaled AC/DC lid-close
protection. The flyout and normal Settings power row share it. Successful mode
requires both; failed enable restores prior wake state; disable drops wake
before lid restoration even when settings saving fails. Return values from
SetThreadExecutionState are checked. Startup preserves older independent
preferences. Recovery/legacy restore actions remain visible in Settings.
Captions distinguish enabled, wake-only, lid-only and recovery/failure states.
No promise is made about manual Sleep, shutdown, network or battery exhaustion.

The active mode has an accent icon/border and toggle. Demo Toggle changes a
memory-only flag, allowing visual/keyboard/UIA proof with no real wake request,
power-policy change, journal or settings write. Real power operations were not
used as an automated test fixture; existing exhaustive fake controller/journal
recovery tests remain green. No actual lid-close sleep test is claimed.

Freshness footer is one row: Updated at left, Next in Nm / Retry at HH:MM at
right. Full observation/action text stays in UIA, keyboard and shared bounds.
Diagnostics is 128 DIP rather than 720 DIP, with Copy support copy; full
sanitized export and UIA HelpText retain the existing report. The optional
Use Codex CLI for usage card explains extra quotas and slower checks in the
visible caption, and approximately two seconds/direct fallback in UIA help.
The existing source selection, credential/isolation/hash gates remain intact.
Pace row UIA describes the even-pace marker; projection/tray/alerts are unchanged.

Validation:

- Ordered fmt/Clippy/192 tests/x64 release pass. Fake combined-mode tests cover
  wake enable failure, lid enable failure and prior wake rollback, existing
  lid protection, disabling despite wake/lid failure, and wake-first ordering.
- 59 focused UI clarity checks pass, including demo on/off via keyboard/Invoke,
  Settings mirrored mode, full diagnostics retention/Copy, Codex explanatory
  help, themes and cooldown. 72 freshness, 52 row, 19 pace and 22 existing UIA
  checks pass. A concurrent UI batch lost foreground focus once; the isolated
  rerun passes. Reviewed dark/light/High Contrast and support/source captures.
- Runtime timer observes two real 30-second ticks, Next in 5m to 4m and Updated
  1m ago. Hide/reopen and hidden repaint guards remain. Both x64 trust-root
  variants pass demo safety (files/registry/power/TCP/child invariance).
- Privacy/dependency/workflow policy pass. No crate, feature, schema, release
  version or footprint baseline changed. Both architecture/trust-root builds
  pass original size gates. x64 1,071,104 unprovisioned / 1,163,264 provisioned.
- Initial provisioned candidate 1,165,824 failed cumulative size. Compact shared
  action copy and the existing footer schedule reused by Settings remove the
  old manual-notice state and duplicated caption path. Provider fetch gates,
  cooldown reducer and identity/alert contracts remain; all parity tests pass.
- Short isolated final-binary footprint check is recorded in the artifact
  ledger. This is not a replacement ten-minute measurement; FRESH-01 supplies
  the prior ten-minute baseline. Narrator/actual ARM64 runtime remain unverified.

Rollback: disable mode or exit normally to restore lid settings, then use the
previous binary. Same two preferences and unchanged journal/schema remain
compatible; never delete/edit a live or corrupt safety journal. Previous binary
returns to independent wake/lid presentation. No provider credential, live
provider endpoint test, release, tag or remote repository setting changed.


Final short footprint result: hidden/visible p95 private working set
1.60/4.14 MiB, CPU 0.0000/0.0000%, GDI 10/13, no TCP. Five-start readiness
median/p95 49.147/80.892 ms. ARM64 unprovisioned/provisioned 979,456 /
1,018,880 bytes. Both architectures and trust-root variants pass original gates.

```text
Task: UI-CLARITY-01 — unified Vibecode mode and compact controls
Result: done locally
Changed: vibecode, main, gfx, accessibility, app schedule presentation, demo; UI/freshness/timer scripts; ADR 0009, plan, CHANGELOG, PRIVACY, architecture, CLAUDE, ledger, verification
Gates: fmt ok | clippy ok | test ok (192 passed) | x64/ARM64 release build ok
Runtime check: 59 clarity + 72 freshness + 52 row + 19 pace + 22 UIA checks, timer, both x64 demo-safety modes, reviewed themes/captures; short isolated footprint 1.60/4.14 MiB, GDI 10/13, no TCP
Size: x64 1,071,104 / 1,163,264 bytes (provisioned -0.044% vs FRESH-01); ARM64 979,456 / 1,018,880
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, architecture, ADR, ledger, verification
Deviations: owner-requested combined mode supersedes independent-control presentation; compact Next in Nm copy; Settings reuses footer schedule instead of manual-notice state
Follow-ups: existing Narrator, real ARM64 and physical lid-close verification; no live provider/credential operation
```
