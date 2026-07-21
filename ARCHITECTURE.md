# Architecture

Single-process, single-UI-thread Win32 app. Three windows, one worker thread per fetch, everything else message-driven.

```
┌ tray icon (Shell_NotifyIconW, VERSION_4) ─ ring = session %
│        │ NIN_SELECT / WM_CONTEXTMENU (coords in wParam)
▼        ▼
Claudometer.Main (hidden WS_POPUP)          ← owns tray, timers, broadcasts
│  WM_TRAY (WM_APP+1)  → toggle flyout / context menu
│  WM_DATA_READY (+2)  → update tray, alert check, re-render visible flyout
│  WM_TOAST_ACTIVATED (+3) → open flyout at tray icon (posted by toast click)
│  WM_UPDATE (+4)       → wparam 0: repaint update state · wparam 1: quit for handover
│  TIMER_POLL          → spawn_fetch()          (30s–5m, user setting)
│  TIMER_TICK          → repaint "Updated Xm ago" (only while flyout visible)
│  WM_SETTINGCHANGE    → only if lParam == "ImmersiveColorSet"
│  TaskbarCreated      → re-add tray icon (explorer restarted)
│
├─ Claudometer.Flyout (WS_POPUP + TOOLWINDOW + TOPMOST + NOREDIRECTIONBITMAP)
│    acrylic via accent policy, DWM round corners, hides on deactivate/Esc
│
└─ Claudometer.Settings (overlapped, fixed size, NOREDIRECTIONBITMAP)
     Mica: DwmExtendFrameIntoClientArea(-1) + DWMSBT_MAINWINDOW
```

## Modules

| File | Owns |
|---|---|
| `main.rs` | windows, wndprocs, tray, menu, timers, per-provider fetch orchestration (`SLOTS`), hit-testing, keyboard nav, all statics |
| `gfx.rs` | `Surface` (D3D/DXGI/DComp/D2D stack), all drawing, layout constants, Fluent palette, brush/format caches |
| `api.rs` | Claude credentials read + usage fetch; shared display model (`UsageSnapshot`, `LimitRow`, `FetchOutcome`), time formatting |
| `codex.rs` | Codex (OpenAI) credentials read + usage fetch → same `UsageSnapshot` |
| `trayicon.rs` | CPU-rasterized ring/alert HICON (premultiplied DIB, no fonts) |
| `alerts.rs` | 75% toast alerts: WinRT toast pipeline, AUMID registration, per-window dedup |
| `updater.rs` | GitHub-Releases self-update: daily check, verified download, rename-swap handover |
| `util.rs` | theme/accent detection, autostart registry, poll-interval config, caps-LED toggle, dark menus, acrylic |

## Rendering (`gfx::Surface`)

WARP D3D11 device → DXGI **composition** swapchain (premultiplied alpha) → `IDCompositionVisual` → window. D2D device context draws onto the swapchain buffer; DirectWrite for text. Window uses `WS_EX_NOREDIRECTIONBITMAP` so there's no GDI redirection surface at all.

Key decisions, with reasons:

- **WARP, not hardware** (v0.2.0): the HW driver's user-mode heaps cost ~40 MB private and are not returned on device release (measured 57 MB open *and* after close). WARP: 7 MB open, 5.5 MB after close. Surface is ~330 px — CPU rasterization is microseconds, and DWM composes the swapchain on the GPU either way.
- **Surface dropped on hide, recreated on show** (~10 ms): the GPU stack *is* the app's RAM cost; windows are hidden 99% of the time.
- **Caches**: brushes keyed by `(dark, accent)`, text formats built once, single RT QI at creation. Zero per-draw allocations.
- **Flyout material — accent-policy acrylic** (`ACCENT_ENABLE_ACRYLICBLURBEHIND`, undocumented): `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` renders only its opaque fallback on borderless DComp popups (observed on build 28020, even with frame extension). Settings window is a titled window, where `DWMSBT_MAINWINDOW` (Mica) works through the documented path.
- **Tray icon on CPU** (`trayicon.rs`): 16–24 px ring with per-pixel AA math into a premultiplied DIB → `CreateIconIndirect`. No D2D needed for 256 pixels; no font dependency for the alert glyph.

## Data layer (`api.rs` + `codex.rs`)

Two independent providers, one worker thread each per poll (~1/min), both producing the same display-ready `UsageSnapshot`:

**Claude** (`api.rs::fetch`):

1. Read `%USERPROFILE%\.claude\.credentials.json` — **read-only, never refreshed** (refresh rotation would kill the user's Claude Code session). Expired → friendly error.
2. `GET api.anthropic.com/api/oauth/usage`, Bearer token, `anthropic-beta: oauth-2025-04-20`, via ureq + native-tls (schannel — OS cert store, no C deps). **Unofficial endpoint** — parsing is defensive, every field optional.
3. Prefer the `limits[]` array (kind/percent/severity/resets_at/scope); fall back to legacy `five_hour`/`seven_day`; append `extra_usage` if enabled.

**Codex** (`codex.rs::fetch`):

1. Read `~/.codex/auth.json` (`CODEX_HOME` honored) — **read-only, never refreshed** (OpenAI rotates refresh tokens; an external refresh would invalidate the Codex CLI session). Expiry checked via the JWT `exp` claim (hand-rolled base64url, no verify). No file / no `tokens` (API-key-only install) → provider counts as absent and the section doesn't render at all.
2. `GET chatgpt.com/backend-api/wham/usage` — the same **unofficial endpoint** the Codex CLI's TUI polls — with `Authorization: Bearer` + `chatgpt-account-id` headers.
3. `rate_limit.primary_window`/`secondary_window` → rows; kind/label derived from `limit_window_seconds` (≤24 h → "Session (Nh)", 7 d → "Weekly"), **not** from window position — which window arrives as primary varies by plan. Severity is empty → percent thresholds color the bars.

Resilience rules (in `main.rs`, per provider via `SLOTS`):

- `last_good` snapshot survives failed fetches — UI shows stale data + footer note; a provider with no data degrades to a dim note line in its own section; the whole-flyout error view exists only for the nothing-ever-fetched case.
- 429 `Retry-After` honored exactly (capped 300 s) via `cooldown_until`; no extra client backoff on top (the poll interval is the floor).
- 3 s debounce on refresh; `fetching` flag dedupes concurrent spawns.
- Fetch threads publish via mutexed statics + `PostMessageW(WM_DATA_READY)` — UI mutations stay on the UI thread.
- Codex enablement (`codex_active`) = settings toggle AND auth file present — checked per poll, so signing in/out of Codex shows/hides the section without restart.

## Alerts (`alerts.rs`)

One native toast per limit window that crosses **75%** (`WARN_AT`), evaluated on the UI thread on every `WM_DATA_READY` — but only from a *fresh* `FetchOutcome::Ok`; stale/error-preserved data never alerts.

- **Real WinRT toasts, unpackaged**: `ToastNotificationManager::CreateToastNotifierWithId` against an AUMID registered under `HKCU\Software\Classes\AppUserModelId\Claudometer` (`DisplayName` + `IconUri` → ico extracted to `%APPDATA%\Claudometer`). Gets Action Center persistence, Focus Assist / DND suppression, and a per-app toggle in Windows notification settings — none of which legacy balloons provide. `SetCurrentProcessExplicitAppUserModelID` ties the process to the AUMID at startup.
- **Dedup keyed on the window instance**: `provider.kind.label → resets_unix`. Percent climbing inside one window fires once; the window rolling over (new `resets_at`) re-arms. Persisted in settings.json (`alerted`) so restarts stay quiet mid-window.
- Toast body: title + reset time + native `<progress>` bar pinned at the worst crossed limit. Click → `WM_TOAST_ACTIVATED` → flyout opens at the tray icon (`Shell_NotifyIconGetRect`). Shown `ToastNotification` objects are kept alive in a thread_local — the OS routes `Activated` through them.
- `NotificationSetting::DisabledForApplication/User` is honored: no balloon resurrection. The `NIF_INFO` balloon fallback fires only when the WinRT path itself errors.
- `claudometer.exe --test-alert` drives the whole pipeline with fake data; outcome written to `%APPDATA%\Claudometer\alert-test.txt` (exe has no console).

## Updater (`updater.rs`)

Passive, transparent, user-initiated. Check: `releases/latest` once per day and once at launch (worker thread, silent failures, drafts/prereleases and non-semver tags skipped). Surfaces: the settings About card ("Claudometer X.Y.Z · GitHub" → "Update vX.Y.Z available · Install") and an accent dot on the flyout gear. Deliberately **no** update toast — toasts are reserved for usage limits.

Install (only on click), all failure paths falling back to opening the release page:

1. Download the `claudometer.exe` asset to `claudometer.new.exe` **next to the current exe** (same volume → atomic renames; also proves the folder is writable).
2. Verify: length sanity → `MZ` magic → VERSIONINFO version == release tag → SHA256 via `certutil` (ships with Windows, zero crypto deps) against the release's `.sha256` asset when present (release.yml attaches it since 0.5.0).
3. The rename swap: running exe → `claudometer.old.exe`, new → `claudometer.exe` (Windows allows renaming a mapped exe, not deleting it), rollback if the second rename fails.
4. Handover: spawn `claudometer.exe --swap-wait`, post `WM_UPDATE(1)`, old instance quits. The new instance sees the busy single-instance mutex, `WaitForSingleObject`s on it (abandoned-mutex on old-process death counts as acquired), then deletes the `.old`. Startup always tries the `.old` cleanup, covering crashed updates.

## State

- Cross-thread: `SLOTS[2]` (per-provider `state`, `last_good`, `last_fetch`, `cooldown_until` mutexes + `fetching` atomic); `POLL_SECS`, hwnds (atomics).
- UI-thread only: `UI` thread_local — surfaces, hover, keyboard focus, mouse-tracking flags.
- Persistent: `%APPDATA%\Claudometer\settings.json` (poll interval, Codex toggle, alerts toggle, `alerted` dedup map), `%APPDATA%\Claudometer\icon.ico` (toast icon), HKCU Run key (autostart), HKCU AppUserModelId key (toast registration), `~/.claude/hooks/caps-led.disabled` (LED kill switch).

## Known gaps

- No UI Automation provider — keyboard works, screen readers see nothing. The honest next step is `IRawElementProviderSimple`/fragment tree behind `WM_GETOBJECT` (~500 lines).
- Unofficial endpoint can change shape any day; failure mode is a visible parse error, not a crash.
- Unsigned exe (SmartScreen warning on first run).
