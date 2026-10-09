# FRESH-01 verification — 2026-10-10

The flyout projects STATE-01 views, including pending worker preparation.
Updating… takes precedence, followed by Outdated at max(two poll intervals,
ten minutes), followed by the typed short error warning. Same-account values
remain visible after transient failures; authentication/account changes still
clear them. The original tray/parity behavior and accepted-success alerts remain
unchanged. No provider credential operation or automated live request occurred.

The footer displays the oldest shown observation, explicitly identifies cached
values, and shows Next update in Nm (rounded up to whole minutes). Click,
Tab/Enter/Space and UIA Invoke share the existing manual refresh dispatch.
While a displayed provider cools down, the footer says Retry at HH:MM, has no
Invoke pattern and fetches nothing; it waits until the latest active deadline.
The header refresh retains independent per-provider gates. The footer is focus
index 3; geometry is shared by rendering, hit testing, UIA and focus scrolling.
An eight-pixel gap separates its focus ring from the observation caption.

Evidence:

- Ordered fmt, Clippy, all 191 tests and x64 release build pass.
- Existing characterization retains the original stale cutoff and refresh
  gates. Focused injected-clock tests prove the deliberate flyout retention
  change, Updating during preparation/fetch, Outdated at ten minutes and three
  hours, transient warning, cooldown refusal and authentication clearing.
  Two-provider tests prove oldest observation/latest active cooldown and disabled
  Codex exclusion. Formatting tests cover minute boundaries, retry priority and
  expiry, age formatting, and footer geometry in loading/error/data views.
- ci/verify-freshness.ps1: 72 checks pass for loading, stale, cooldown, error,
  both, light, High Contrast and scrolled many-row views. UIA contains status,
  age and source, preserved rows and footer action; keyboard/Invoke/click paths
  pass. Empty isolated profiles stay empty, with no TCP or child process.
- 52 existing row checks, 19 pace/theme checks and 22 existing UIA checks pass.
  Reviewed captures show readable statuses and separate observation/action
  captions in dark/light/High Contrast; scrolled footer focus is visible.
- ci/verify-row-timer.ps1 observes two real 30-second ticks: Next update in 5m
  advances to 4m, UIA reads Updated 1m ago, and countdown progresses. Tray
  hide/reopen plus source checks prove timer removal and hidden repaint guard.
  Runtime observers wait for Windows ticks; unit tests never sleep or use HTTP.
- Both x64 trust-root variants pass demo safety, including registry and active
  scheme/AC/DC lid-policy invariance. Privacy, 122-package dependency policy
  and two-workflow policy pass. No crate, feature, profile or budget changed.
- Both architecture/trust-root variants pass original artifact gates. Size and
  final ten-minute controlled runtime results: ../performance/fresh-01.md.
  Raw local captures/JSON/artifacts remain under ignored target/fresh-01/.

Rollback: run the previous binary. No settings/cache schema changed, and no
credential, safety-journal or data migration action is necessary. The older
flyout resumes its original presentation; settings and cache remain compatible.
Narrator speech and real ARM64 runtime remain unverified; ARM64 proof is
cross-build and size only. Next queue item: A11Y-02, subject to its A11Y-01
prerequisite and the existing owner-deferred Narrator gate.

Implementation decisions: status stays on the existing line below the provider
name/plan; the footer action adds one Tab stop. A cooldown on either displayed
provider blocks the footer all-provider action until both can retry. The header
refresh continues to serve eligible providers independently. Stale demo uses
three-hour cached values so Outdated is truthful; Error keeps the no-data case.

The local commit includes code, tests, plan/status board, CHANGELOG, PRIVACY,
CLAUDE, architecture, size/runtime reports and this rollback note. No tag,
release, repository setting or remote publication is part of FRESH-01.


Final required ten-minute visible run passes: CPU 0.0033%, p95 private working
set 4.55 MiB, GDI 13, no TCP. The concurrent hidden-requested process acquired
flyout graphics and exceeded hidden memory; that sample is not claimed as
hidden baseline proof. See the performance report for evidence and limits.

The isolated final-binary 60-second check returns hidden 1.60 MiB p95,
10 GDI handles, 0.0000% CPU and no TCP; visible 4.56 MiB, 13 GDI and
0.0062% CPU. This is short diagnostic proof, distinct from the required
completed ten-minute visible acceptance measurement.

```text
Task: FRESH-01 — Updating, Outdated, next-update footer
Result: done locally
Changed: app, provider state accessor, gfx, main, accessibility, demo; freshness/timer scripts; plan, CHANGELOG, PRIVACY, CLAUDE, architecture, ledger, verification/performance reports
Gates: fmt ok | clippy ok | test ok (191 passed) | x64/ARM64 release builds ok
Runtime check: 72 freshness + 52 row + 19 pace + 22 existing UIA checks; reviewed themes/captures; timer progression/hide/reopen; ten-minute visible idle CPU 0.0033%, p95 private working set 4.55 MiB, GDI 13, no TCP
Size: x64 1,071,104 unprovisioned / 1,163,776 provisioned (0.00% vs ROW-01); ARM64 978,944 / 1,018,880 (+512 bytes)
Docs updated: plan, CHANGELOG, PRIVACY, ledger, CLAUDE, architecture, verification/performance
Deviations: extra footer Tab stop; latest displayed cooldown blocks the all-provider footer action; legacy tray cutoff retained; concurrent hidden measurements affected by UI automation, isolated short check passes
Follow-ups: existing Narrator/ARM64 runtime checks; A11Y-02 prerequisite remains A11Y-01
```
