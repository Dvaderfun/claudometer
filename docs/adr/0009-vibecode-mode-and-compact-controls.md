# ADR 0009: Clear Vibecode mode and compact support controls

- Status: Accepted; owner requested 2026-10-10
- Scope: UI-CLARITY-01, follow-up to FRESH-01

The owner requested one flyout Vibecode control that keeps work running even
when the lid closes, a compact freshness footer/Diagnostics, and an explanation
of the optional Codex source. This supersedes the earlier two-independent-
controls presentation; the underlying journal/restore safety contract remains.

Vibecode mode explicitly enables the existing wake request and journaled
AC/DC lid Do nothing override. Verified success requires both. Enabling failure
restores the previous wake state; turning off drops wake before restoring lid.
Previous independent preferences are preserved at startup, never silently
upgraded to combined protection. Existing recovery/legacy restore actions remain
in Settings; its normal power row mirrors Vibecode mode. No credential or
provider behavior changes. Power API return values must be checked.

The flyout names the mode and describes lid behavior. Detailed accessible help
explains restoration and the limits of this setting (manual Sleep/shutdown,
network loss and battery exhaustion). It never promises a CLI turn completes.
The demonstration toggle changes memory only and touches no OS power policy.

Freshness is one row: Updated at left, next update/retry at right. Its existing
manual refresh, cooldown, keyboard and UIA contracts remain. Diagnostics shows
a small Copy support card; the sanitized complete
snapshot remains copied and in UIA HelpText. The Codex switch explains that it
uses the installed CLI for extra usage details with slower checks. Existing
hash/source/isolation gates and default-off behavior remain.

Rollback: previous binary reads the same wake/lid preferences and journal.
Disable Vibecode or exit normally first so lid restoration is verified. Never
hand-edit or remove a live safety journal. No schema or dependency change.
