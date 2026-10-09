# ROW-01 verification — 2026-10-10

Persistent `reset_format` (clock/countdown) and `quota_display` (used/left)
are additive in settings schema 1. Both Settings buttons support Tab,
Space/Enter and UIA Invoke. Clicking a row value or reset caption uses the
same action, changes every row, and adds no flyout Tab stops. Values and reset
labels sit below the unchanged used-percentage bar. Reset formatting also
applies to pace run-out text and the existing shared reset-format consumers.

Only Claude Session limits without a reset say Not started. The accessible
name adds “The session starts with your first message.” Codex and other kinds
retain the missing-reset behavior. The Claude-only demo now illustrates this
state; Both keeps the pace examples. Demo display choices are memory-only.

Evidence:

- Ordered fmt, Clippy, all 187 tests and x64 release build pass.
- Characterized clock copy before changing formatting. Tests cover clock
  today/weekday, invalid epochs, countdown zero/past/sub-minute/minute/24-hour
  and multi-day boundaries, Unix/DST weeks, matching run-out formatting,
  complement percentages, provider/kind missing-reset distinction, row hit
  bounds, atomic persistence/restart, invalid defaults, older-reader writes,
  unknown fields and failed-write disk/memory preservation.
- `ci/verify-rows.ps1`: 52 checks pass for dark/light/High Contrast, global
  value/reset clicks, weekly countdown, pace notes, Settings keyboard/Invoke,
  Not started explanation, Codex distinction, and scrolled-row shortcuts.
  Isolated demos write no profile, start no child and open no TCP connection.
- All 22 existing UIA checks and 19 pace/theme checks pass. Screenshots reviewed:
  value/reset labels readable, no overlap, Settings focus ring visible.
- `ci/verify-row-timer.ps1`: captures across 66 seconds show reset countdown
  from 2h 14m to 2h 13m and footer age from just now to 1m ago. Tray hide/reopen
  works. Source checks pin both visible timer registrations at 30,000 ms,
  `KillTimer` on hide, and the hidden-window repaint guard. The runtime observer
  waits for real ticks; deterministic unit tests never sleep or use live HTTP.
- Demo safety passes for both x64 trust-root variants, including registry,
  active scheme and AC/DC lid policies. Privacy, 122-package dependency policy
  and workflow policy pass. No crate/features/profile/budgets were changed.
- Both architecture/trust-root variants pass original artifact gates. Runtime
  measurements are in `../performance/row-01.md`; raw local artifacts and
  captures remain under ignored `target/row-01/`.

Size recovery consolidates UTF-16/DirectWrite draw alignment, font/brush
creation and Settings button rendering, live/demo render dispatch, and the
Settings view shared with UIA. Labels, controls, colors and cached-resource
ownership remain intact. Failed COM creation drops temporary owned resources
and leaves the previous cache intact. UIA account/help and lid recovery text
now follow the displayed Settings view rather than duplicate descriptions.
Unsupported sizing experiments were removed; no gate was raised.

Rollback: restore Clock/Used through Settings or use the previous binary.
Older readers preserve unknown schema-1 fields; no provider cache migration,
credential action or safety-journal edit is needed. Narrator speech and real
ARM64 runtime remain unverified. ARM64 evidence is cross-build/size only.

Follow-up: FRESH-01. This report supersedes the paused checkpoint.
No release, tag, remote repository setting or provider credential was
changed, and automated tests never called a live provider endpoint.

```text
Task: ROW-01 — reset format, Not started, click shortcuts
Result: done locally
Changed: config, api formatting, gfx, main, accessibility, demo; row/timer verification scripts; plan, CHANGELOG, PRIVACY, CLAUDE, architecture, performance ledger/reports
Gates: fmt ok | clippy ok | test ok (187 passed) | x64/ARM64 release build ok
Runtime check: demo safety, 52 row UI + 22 existing UIA + 19 pace checks, reviewed captures, timer hide/reopen/countdown, ten-minute controlled hidden/visible sample
Size: x64 1,071,104 unprovisioned / 1,163,776 provisioned (0.00% provisioned vs PACE-01)
Docs updated: plan, CHANGELOG, PRIVACY, ledger, CLAUDE, architecture, verification/performance
Deviations: size recovery consolidates existing renderer/Settings/UIA; Claude-only demo illustrates Not started
Follow-ups: FRESH-01; existing Narrator and real ARM64 runtime verification
```
