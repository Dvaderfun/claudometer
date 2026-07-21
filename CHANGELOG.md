# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions: [SemVer](https://semver.org/).

## [0.5.0] — 2026-07-21

### Added
- **Native Windows 11 toast alerts at 75% usage** for every limit window (Claude session/weekly/per-model, Codex session/weekly). Real WinRT toasts via a registry-registered AUMID — no packaging: they persist in Action Center, respect Focus Assist / Do-not-disturb and the per-app switch in Windows notification settings, and show a native progress bar pinned at the worst limit. Clicking the toast opens the flyout at the tray icon.
- One alert per window *instance*: dedup keys on the raw `resets_at` and is persisted, so neither 1-minute polling nor an app restart repeats an alert; the window rolling over re-arms it. Stale (error-preserved) data never alerts — only fresh fetches.
- "Alert at 75% usage" toggle in settings (default on). Legacy balloon fallback only if the WinRT path errors; a user-disabled app notification setting is honored, not worked around.
- `claudometer.exe --test-alert` — fires a fake alert end-to-end (registration → XML → Show), outcome written to `%APPDATA%\Claudometer\alert-test.txt`.
- Settings cards got leading Segoe Fluent Icons glyphs (Windows 11 Settings row style).
- **Built-in updater** (About card in settings): checks GitHub Releases once a day (and at launch) in the background; when a newer version exists the card flips to "Update vX.Y.Z available — Install" and the flyout gear gets a quiet accent dot. Install downloads next to the exe, verifies (PE magic, VERSIONINFO == tag, SHA256 via certutil against the release's `.sha256` asset), rename-swaps the running exe, and relaunches — with rollback on failure and "open the release page" as the universal fallback. No update toasts, no nagging, nothing happens without a click.
- "GitHub" button on the About card opens the project page; release workflow now also attaches `claudometer.exe.sha256`.

## [0.3.0] — 2026-07-21

### Added
- **Codex (OpenAI) usage** as a second flyout section: 5-hour session + weekly bars from the same `wham/usage` endpoint the Codex CLI polls. Auto-detected from `~/.codex/auth.json` (or `CODEX_HOME`) — zero configuration; installs without Codex look exactly like before. Strictly read-only: the OAuth token is never refreshed (rotation would invalidate the Codex CLI session).
- Per-provider resilience: independent last-good snapshots, 429 cooldowns, and error notes — a Codex failure degrades to one dim line in its section, never touching Claude data (and vice versa).
- "Show Codex usage" toggle in settings; tray tooltip gains a Codex line.

### Changed
- Plan name ("Max", "Plus") moved from the footer to each section header.
- Settings footer now lists both data sources.

## [0.2.0] — 2026-07-21

### Changed
- **~10x lower RAM**: WARP software D3D device instead of hardware (HW driver heaps cost ~40 MB private and survive device release); the whole render stack is now dropped when a window hides and recreated on show. Measured: 57 → 7 MB with the flyout open, 5.5 MB idle.
- Brushes, text formats, and the render-target cast are cached — zero per-draw allocations. Brush cache re-keys on theme/accent change.
- `WM_SETTINGCHANGE` handling filtered to `ImmersiveColorSet` — wallpaper changes and misc SPI broadcasts no longer rebuild the tray icon.
- The relative-time repaint timer runs only while the flyout is visible.

### Added
- Full keyboard navigation with Fluent focus rings: Tab/Shift+Tab cycles controls, Space/Enter activates, ←/→ changes the refresh interval, Esc closes.
- Embedded exe icon + version info; settings window icon.
- GitHub Actions CI: build + clippy (`-D warnings`) on push/PR, automatic release with exe on version tags.

### Known gaps
- No UI Automation (screen reader) support yet — UI is keyboard-operable but not announced.

## [0.1.0] — 2026-07-20

### Added
- Tray ring icon showing 5-hour session usage (accent → amber ≥85% → red ≥100%), tooltip with quick numbers.
- Acrylic flyout: every reported limit with progress bars and reset times; refresh + settings buttons; relative "Updated just now / Xm ago" footer that ticks.
- Mica settings window: Caps Lock LED toggle, start with Windows, auto-refresh interval (30s/1m/2m/5m), refresh, quit.
- Resilience: last-good snapshot survives errors, `Retry-After` honored on 429, refresh debounce, explorer-restart recovery, per-monitor DPI, light/dark/accent theming.
- `extras/caps-led.ps1` — Caps Lock LED as a Claude Code status light (driver-level LED control, real Caps Lock state untouched).
