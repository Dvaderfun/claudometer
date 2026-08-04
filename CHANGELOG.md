# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions: [SemVer](https://semver.org/).

## [0.6.0] — 2026-08-04

### Added
- **Vibecode mode** — a toggle row in the flyout that keeps the machine working with the lid shut. It holds a `SetThreadExecutionState` wake lock (no idle sleep, no display timeout) and forces the active power scheme's lid-close action to *Do nothing* on both AC and DC. Turning it off restores the exact indices that were there before; they're saved to settings.json before the first override, so a crash or a kill doesn't strand the setting — quitting restores it too, and the mode itself is remembered and re-armed on the next launch. Keyboard: Tab to the row, Space/Enter to toggle. Machines with no lid setting just get the wake lock.

## [0.5.2] — 2026-07-23

### Fixed
- Claude section could stay stale for many minutes (session bar frozen at an old value) while the footer still said "Updated just now". Root cause: the usage endpoint's burst limiter is shared with Claude Code's own polling, so a fixed 60 s poll phase can collide with it for many consecutive ticks — it answers 429 with `Retry-After: 0` ("fine again in a moment"), but the app waited a full poll interval and collided again. A rate-limited fetch now retries once after a 2–5 s jitter to de-phase; sustained 429s back off exponentially (120 s → 600 s), and an explicit server `Retry-After` is honored up to 15 min (was capped at 5).
- Two-section footer timestamp now shows the *oldest* data on screen — a fresh Codex fetch no longer masks stale Claude rows.
- Manual refresh (flyout button/Enter, tray menu, settings card) bypasses an active 429 cooldown instead of silently doing nothing.
- Claude requests now identify themselves with a `claudometer/<version>` User-Agent, matching the Codex requests (was the ureq default).

## [0.5.1] — 2026-07-21

### Fixed
- 75% alert re-fired on every poll while a window stayed over the threshold: the API recomputes an in-flight window's `resets_at` (observed drifting ±1 min between polls) and exact-epoch dedup saw each drift as a new window. Epochs within 30 minutes now count as the same window instance.

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
