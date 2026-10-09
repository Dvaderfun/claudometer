# ADR 0008: Stateless pace in the flyout

- Status: Accepted
- Date: 2026-10-09
- Roadmap: PACE-01; decision D-06

Quota-window pace uses the provider's used percentage, reset epoch, window
duration, and the current Unix time. It projects only after at least 15 minutes
and 5% of the window have elapsed. Missing or invalid timing, zero usage, and
Spend rows retain the existing used-percentage severity. The limit-reached
verdict takes precedence at 99.5%, as specified by the roadmap.

Pace affects only flyout bar color, a short note, an even-pace tick, and the
accessible name. Tray selection, tray severity, alert thresholds, and stored
snapshots continue using the provider's original values. Projection is described
as “at current pace”; it is not a guarantee. No history or new polling is added.

`pace_colors_enabled` defaults on. Turning it off restores the Level view,
including the original severity, and removes projection notes/ticks. This
preference is additive in settings schema 1 and persists through the existing
atomic writer. Previous binaries preserve unknown fields.

Run-out notes use the existing local clock formatter. ROW-01 owns the planned
`reset_format` setting and will apply countdown formatting to both reset and
run-out notes. PACE-01 introduces neither an unused setting nor row shortcuts.

Rollback: disable the pace setting or revert the binary. No data migration,
history deletion, credential action, or safety-journal edit is required.
