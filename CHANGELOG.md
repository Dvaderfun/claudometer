# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Flyout and Settings expose a UI Automation fragment tree with named controls, button Invoke, switch Toggle, keyboard focus events, and readable quota labels. Local UIA client checks pass; Narrator speech remains unverified, and Selection/RangeValue patterns and targeted property events remain A11Y-01 follow-ups. See [WIP-00 verification](docs/verification/wip-00.md).

### Security
- Release tags now build once with `--locked`, publish signed manifests and an SPDX SBOM, create GitHub provenance/SBOM attestations, verify downloaded draft assets, smoke-test the downloaded x64 executable, and publish only with immutable releases enabled.
- **Manual trust-root bootstrap required.** Versions without an embedded Ed25519 release key—including all published versions through 0.7.3—cannot authenticate a trust-root-enabled update. They will not update automatically across that boundary. Existing users must manually download and independently verify the first trust-root-enabled release using its published hash and Authenticode identity before installing it. Later releases can use signed-manifest updates. See the [bootstrap verification procedure](docs/release-manifest-v1.md#trust-root-bootstrap).
- Portable self-updates now use an atomic `update-operation.v1.json` journal, attempt-unique candidate and backup names, and hash-reconciled startup recovery. The previous verified executable remains available until the candidate commit is durable.

## [0.7.3] — 2026-08-29

### Fixed
- **Recurring Claude logouts.** Claudometer no longer replays Claude Code's rotating refresh token through a second `claude auth login` process. Polling is now strictly read-only and Claude Code remains the only process that refreshes or writes its OAuth session, avoiding cross-process refresh-token races.
- **Reconnect detoured through CMD.** The app now prefers the recommended native `%USERPROFILE%\.local\bin\claude.exe` and resolves the real `claude.exe` embedded behind current official npm shims. CMD is only a compatibility fallback for legacy batch-only installs.
- Claude credentials and cached identity now honor `CLAUDE_CONFIG_DIR`. A local `expiresAt` value of `0` or another stale hint no longer causes a false logout; the API response is authoritative.
- Credential reads retry briefly across Claude Code's atomic file replacement instead of flashing a false "not signed in" state.
- A 429 now starts a real 60–900 second cooldown immediately. Manual refresh respects the cooldown, and the removed fast retry can no longer extend server throttling. Stale data is retained for at most 10 minutes.
- Non-finite and out-of-range percentages from either unofficial usage API are clamped before reaching labels, alerts, or D2D geometry.

## [0.7.2] — 2026-08-19

### Fixed
- **Slow console on Reconnect.** Sign-in ran `claude --version` first to check the CLI was installed, which is a full launch of a ~285 MB binary (~1 s measured) before the real launch even started. The check now resolves `claude` on `PATH` in-process and only falls back to launching it to confirm a negative. Click-to-console is ~0.8 s.
- The account card's status line no longer wraps out of the card; it stays on one line and ellipsizes.

## [0.7.1] — 2026-08-19

### Fixed
- **Reconnect hung forever.** The Claude browser sign-in ends on a hosted callback page that shows a code the user pastes back into the CLI — there is no loopback listener — but Claudometer launched `claude auth login` with a hidden window and no stdin, so it could never be completed and simply sat at "Waiting for Claude sign-in…" until the 10-minute timeout. Sign-in now runs in its own console window with working input.
- Sign-in is cancellable: clicking the account card while it runs abandons the attempt and closes the console instead of being ignored.
- Cancelling or timing out now terminates the whole process tree; previously only the `cmd.exe` shim was killed, orphaning `claude.exe` and its console window.
- A missing Claude Code CLI is detected before sign-in starts, so no empty console window opens.

## [0.7.0] — 2026-08-19

### Added
- **First-class Claude account connection** in Settings: linked email/plan status, Connect/Reconnect browser flow, and install guidance when Claude Code is missing. The official Claude Code CLI remains the credential owner; Claudometer never performs a token exchange and never writes a credentials file.
- **Automatic sign-in renewal.** Within 5 minutes of access-token expiry, or after an authoritative 401, Claudometer hands the Claude Code CLI its own refresh token so the CLI renews and rewrites its credentials; usage is then retried with credentials re-read from disk. Renewal is single-flight with a 5-minute cooldown. Reconnecting by hand is now only needed for a revoked or long-dead session.

### Fixed
- **Wrong plan in the flyout header.** The plan came from `.credentials.json`'s `subscriptionType`, which is written once at sign-in and never updated — a Max→Pro change still showed "Max" indefinitely. The plan now comes from the account profile endpoint (cached for an hour, with an offline fallback).
- Claude credential "repair" previously ran `claude auth status`, which is a purely local read with no network call: it could never renew a token, yet launched a CLI process every 5 minutes while an account was expired.
- Footer notes no longer read "Claude: Claude account needs attention" — the section title already names the provider.

### Changed
- The Settings account card reads the identity Claude Code caches on disk instead of launching the CLI, so opening Settings (and app startup) no longer spawns a process.

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
