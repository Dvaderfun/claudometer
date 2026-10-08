# WIP-00 verification — 2026-10-08

Result: **blocked on human Narrator verification**. The implementation is
committed in `0ed4ea9`, followed by plan revision `0fdfd19`, on
`feat/a11y-uia-and-plan-v2`. The verification changes document those commits;
they do not modify product code or mark WIP-00/A11Y-01 complete.

## Gates and builds

All commands ran separately, in §0.3 order, with full output inspected:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo build --locked --release --target x86_64-pc-windows-msvc
cargo build --locked --release --target aarch64-pc-windows-msvc
```

All passed; 115 tests passed with no failures or ignored tests. A running
Claudometer was stopped before builds. The unprovisioned build warning is the
expected fail-closed updater behavior. Both architectures were also built
with the synthetic public trust root documented in the
[artifact ledger](../performance/artifact-ledger.md); no production provisioning
occurred. ARM64 was cross-compiled, not run on ARM64 hardware.

`ci/check-artifact.ps1` passed for each architecture in each build mode.
`ci/check-privacy.ps1` passed for six destinations and four request sites;
`ci/check-dependencies.ps1` passed for 122 registry packages;
`ci/check-workflows.ps1` passed for two workflows.

`ci/verify-demo.ps1` passed on both x64 build modes: no profile writes,
Claudometer registry changes, child processes, TCP connections, active-scheme
changes, or AC/DC lid-policy changes were observed by that script.

## UIA and visual evidence

Windows 11 Pro Insider Preview x64 build 28020, PowerShell 7.6.6, Windows SDK
10.0.26100.0 x64 `inspect.exe`, and `winapp` 0.5.0 were used. Demo launches used
only `--demo both` and `--demo settings`; no live provider was queried.

The flyout root `UsageFlyout` exposes Refresh, Settings, KeepAwake, and four
quota text elements. Quota names include provider, window, used percentage,
and reset text. The Settings root `SettingsWindow` exposes eleven app controls
with nonempty AutomationIds. The ordinary window chrome is separate.

All 22 batched client checks passed:

| Scenario | Checks |
|---|---|
| `both` | Invoke Refresh and Settings; Toggle KeepAwake; read its Off state |
| `settings` | Invoke ClaudeAccount, RefreshInterval, RefreshNow, About, Quit; Toggle and read CapsStatusLight, Autostart, ShowCodex, UsageAlerts, UpdateChecks, LidOverride; keyboard focus |

`inspect.exe` displayed the focused flyout Refresh button as `UIA_Button`,
AutomationId `Refresh`, with keyboard focus and matching screen bounds.
It displayed the Settings account as `Claude account, Reconnect`, AutomationId
`ClaudeAccount`, with keyboard focus and help text `Connected account · Max`.
Screenshots of both windows were reviewed: text and controls were visible,
without clipped rows or overlap in these observed layouts. This is not the
multi-monitor/DPI acceptance suite for LAYOUT-01.

Local raw evidence is under ignored `target/wip-00/`: `check-uia.ps1`,
`uia-results.json`, `both-tree.json`, `settings-tree.json`,
`settings-focus.json`, demo screenshots, and `inspect-refresh.png` /
`inspect-settings.png`. The helper script is local verification scaffolding,
not a new source gate.

Demo Invoke/Toggle calls exercise the COM patterns and UI-thread dispatch but
intentionally make no setting, credential, browser, or power changes. In
`--demo both`, Settings Invoke hides the flyout but does not open Settings;
the separate `--demo settings` scenario covers that tree. Successful pattern
calls therefore do not demonstrate live mutation or announcements of changed
values.

## Remaining acceptance and rollback

Narrator was **not verified**. This session cannot listen to its speech; the
human must check every control on both demo scenarios, including names,
roles, states, Tab/Shift+Tab navigation, Enter/Space, and unwanted repeated
announcements. WIP-00 stays unchecked until that result is recorded.

A11Y-01 additionally needs Selection for interval/metric choices, read-only
RangeValue for quota bars (currently Text), names containing freshness/pace
when those view states exist, and targeted property events (currently only
focus events are raised). Runtime memory/CPU remeasurement remains due when
A11Y-01 lands. No test failure was hidden or weakened.

Rollback: use the pre-UIA binary built from `89241c1`, or revert `0ed4ea9`
through a reviewed branch. UIA work introduces no irreversible data migration;
leave all credential files and recovery journals untouched.

## §0.7 handoff

```text
Task: WIP-00 — verify committed UI Automation work
Result: blocked
Changed: plans/state-of-the-art-v1.md, docs/performance/artifact-ledger.md, docs/verification/wip-00.md, CHANGELOG.md, ARCHITECTURE.md, CLAUDE.md
Gates: fmt ok | clippy ok | test ok (115 passed) | release build ok (x64 and ARM64, both trust-root modes)
Runtime check: demo safety passed in both x64 modes; 22 UIA checks passed; inspect.exe confirmed focused controls in flyout and Settings; screenshots reviewed; Narrator speech unverified
Size: x64 1,132,544 bytes (+3.17% vs REL-02 provisioned); unprovisioned 1,045,504 (+3.44% vs matching REL-02 row). ARM64 1,029,120 provisioned / 978,432 unprovisioned
Docs updated: plan, CHANGELOG, ledger, verification report, ARCHITECTURE, CLAUDE.md; PRIVACY unchanged because no product side effect changed
Deviations from plan: UIA was already committed, so no stash/recommit; parent rebuild has small differences from historical REL-02 sizes; WIP-00 remains unchecked and review stays draft pending Narrator
Follow-ups: human Narrator listening check; remaining A11Y-01 patterns/names/property events; SIZE-01 then REL-03 after WIP-00 completion
```

The required upstream command succeeded:
`git branch -u origin/feat/a11y-uia-and-plan-v2`.
