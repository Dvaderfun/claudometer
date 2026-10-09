# PACE-01 verification — 2026-10-09

Implemented the roadmap's stateless pace table in `provider/pace.rs`. The pure
calculation receives normalized percentage/class/reset/window and injected
Unix time. It does not fetch, persist, alter provider values, or call alerts.
The display view computes it on each eligible visible repaint, using the
existing 30-second timer. The tray continues using original used-percent
severity and alerts continue consuming accepted provider observations.

The flyout shows critical/warning/normal pace colors, `Limit reached`, a local
clock run-out note or `At limit by reset`, and `~N% spare`. Over/Tight show an
even-pace tick. The full note is appended to the formatted row label and drawn
with its single-line caption format; UIA exposes that full label plus the pace
verdict. Long provider text retains existing ellipsis behavior. The default-on
`pace_colors_enabled` switch is reachable by mouse, Tab/Space/Enter, and UIA
Toggle. Turning it off constructs Level rows with the original severity and
without projection notes or ticks.

Evidence:

- Required formatting, Clippy, all 181 tests, and x64 release build pass.
  ARM64 release cross-build and original artifact gates pass as well.
- Added a characterization test before changing the row view. New tests cover
  p=90/90.01/99.99/100, 99.5%-used precedence, spare flooring/minimum, zero
  usage, missing reset/window, zero window, elapsed minimum and weekly 5%
  minimum, expired resets, future starts, Spend, extreme i64 epochs, run-out
  time/tick and 60-second margin, and spring/fall DST Unix weeks.
- Presentation test proves original snapshots/tray severity are unchanged,
  Level restoration, and full UIA note/verdict. Settings test exercises the
  real atomic writer and restart, missing/invalid defaults, and unknown-field
  preservation through older-reader writes. Existing settings/store failure,
  schema, and downgrade tests pass after persistence consolidation.
- `ci/verify-pace.ps1`: 19 focused UI checks pass for OnTrack/Tight/Over names
  in dark/light/High Contrast, Toggle state/pattern, keyboard/focus, and four
  captures. Screenshots reviewed: no overlap or clipped action; focus visible.
  All 22 existing UIA checks pass on the final binary.
- Demo safety passes on the final binary: no files, registry changes, child,
  TCP, active power-scheme or AC/DC lid-policy changes. Automated tests use
  synthetic data; no live provider request or credential change was performed.
- Privacy, dependency (122 packages), and workflow policies pass. No dependency,
  Windows feature, release profile, size ceiling, or comparison baseline changed.
- Sizes and ten-minute runtime results are in `../performance/pace-01.md` and
  the artifact ledger. Raw local output/screenshots are under ignored
  `target/pace-01/`; the reproduction scripts are committed.

Size recovery: initial provisioned x64 was 1,166,848 and failed the unchanged
1,164,134 cumulative gate. Consolidated the settings commit/boolean dispatch,
boolean encoding loop, reset/run-out formatter and accessible-name assembly.
Pace notes share the existing display-label string, eliminating a separate
allocated/cloned string per row. Final provisioned x64 is 1,163,776. Compiler
annotations that showed no benefit were removed; the store implementation is
unchanged. cargo-bloat debug-symbol analysis artifacts are not release sizes.

Deviation: `reset_format` is ROW-01's pending setting, so PACE-01 uses the current
local-clock format. ROW-01 must apply countdown to run-out and reset together.
The UI note shares the label font/span to fit the footprint budget. Narrator
and actual ARM64 runtime remain unverified; this work makes no new claim for
either. Demo mutations are guarded; Toggle-call success is not live persistence
proof (the atomic writer test supplies that evidence).

Rollback: disable the pace switch or use the previous binary. The setting is
additive in schema 1 and ignored/preserved by older readers. No cache/history
migration, credential action, or safety-journal edit is required. ADR 0008 records
decision D-06 and the row-format handoff.

```text
Task: PACE-01 — stateless flyout pace
Result: done locally
Changed: provider/pace, gfx, main, config, api formatting, demo, UIA; ci/verify-pace; plan, CHANGELOG, PRIVACY, CLAUDE, architecture, ADR, ledger, verification/performance
Gates: fmt ok | clippy ok | test ok (181 passed) | x64/ARM64 release build ok
Runtime check: demo safety; 22 UIA + 19 focused pace/theme checks; reviewed screenshots; isolated ten-minute hidden/visible run
Size: x64 1,071,616 unprovisioned / 1,163,776 provisioned (0.00% provisioned vs CODEX-01)
Docs updated: plan, CHANGELOG, PRIVACY, ledger, CLAUDE, architecture, ADR, verification/performance
Deviations: clock/countdown setting belongs to ROW-01; note shares label span
Follow-ups: ROW-01; existing Narrator/ARM64 human verification remains open
```
