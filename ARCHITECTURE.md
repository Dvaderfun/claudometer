# Architecture

Single-process, single-UI-thread Win32 app. Three windows, one worker thread per fetch, everything else message-driven.

```
┌ tray icon (Shell_NotifyIconW, VERSION_4) ─ ring = session %
│        │ NIN_SELECT / WM_CONTEXTMENU (coords in wParam)
▼        ▼
Claudometer.Main (hidden WS_POPUP)          ← owns tray, timers, broadcasts
│  WM_TRAY (WM_APP+1)  → toggle flyout / context menu
│  WM_DATA_READY (+2)  → update tray, re-render visible flyout
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
| `main.rs` | windows, wndprocs, tray, menu, timers, fetch orchestration, hit-testing, keyboard nav, all statics |
| `gfx.rs` | `Surface` (D3D/DXGI/DComp/D2D stack), all drawing, layout constants, Fluent palette, brush/format caches |
| `api.rs` | credentials read, usage fetch, response parsing → display-ready `UsageSnapshot`, time formatting |
| `trayicon.rs` | CPU-rasterized ring/alert HICON (premultiplied DIB, no fonts) |
| `util.rs` | theme/accent detection, autostart registry, poll-interval config, caps-LED toggle, dark menus, acrylic |

## Rendering (`gfx::Surface`)

WARP D3D11 device → DXGI **composition** swapchain (premultiplied alpha) → `IDCompositionVisual` → window. D2D device context draws onto the swapchain buffer; DirectWrite for text. Window uses `WS_EX_NOREDIRECTIONBITMAP` so there's no GDI redirection surface at all.

Key decisions, with reasons:

- **WARP, not hardware** (v0.2.0): the HW driver's user-mode heaps cost ~40 MB private and are not returned on device release (measured 57 MB open *and* after close). WARP: 7 MB open, 5.5 MB after close. Surface is ~330 px — CPU rasterization is microseconds, and DWM composes the swapchain on the GPU either way.
- **Surface dropped on hide, recreated on show** (~10 ms): the GPU stack *is* the app's RAM cost; windows are hidden 99% of the time.
- **Caches**: brushes keyed by `(dark, accent)`, text formats built once, single RT QI at creation. Zero per-draw allocations.
- **Flyout material — accent-policy acrylic** (`ACCENT_ENABLE_ACRYLICBLURBEHIND`, undocumented): `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` renders only its opaque fallback on borderless DComp popups (observed on build 28020, even with frame extension). Settings window is a titled window, where `DWMSBT_MAINWINDOW` (Mica) works through the documented path.
- **Tray icon on CPU** (`trayicon.rs`): 16–24 px ring with per-pixel AA math into a premultiplied DIB → `CreateIconIndirect`. No D2D needed for 256 pixels; no font dependency for the alert glyph.

## Data layer (`api.rs`)

`fetch()` (worker thread, ~1/min):

1. Read `%USERPROFILE%\.claude\.credentials.json` — **read-only, never refreshed** (refresh rotation would kill the user's Claude Code session). Expired → friendly error.
2. `GET api.anthropic.com/api/oauth/usage`, Bearer token, `anthropic-beta: oauth-2025-04-20`, via ureq + native-tls (schannel — OS cert store, no C deps). **Unofficial endpoint** — parsing is defensive, every field optional.
3. Prefer the `limits[]` array (kind/percent/severity/resets_at/scope); fall back to legacy `five_hour`/`seven_day`; append `extra_usage` if enabled.
4. Produce display-ready `UsageSnapshot` (labels, formatted reset times, unix fetch stamp).

Resilience rules (in `main.rs`):

- `LAST_GOOD` snapshot survives failed fetches — UI shows stale data + footer note; the error view exists only for the never-fetched case.
- 429 `Retry-After` honored exactly (capped 300 s) via `COOLDOWN_UNTIL`; no extra client backoff on top (the poll interval is the floor).
- 3 s debounce on refresh; `FETCHING` flag dedupes concurrent spawns.
- Fetch thread publishes via mutexed statics + `PostMessageW(WM_DATA_READY)` — UI mutations stay on the UI thread.

## State

- Cross-thread: `STATE`, `LAST_GOOD`, `LAST_FETCH`, `COOLDOWN_UNTIL` (mutexes); `FETCHING`, `POLL_SECS`, hwnds (atomics).
- UI-thread only: `UI` thread_local — surfaces, hover, keyboard focus, mouse-tracking flags.
- Persistent: `%APPDATA%\Claudometer\settings.json` (poll interval), HKCU Run key (autostart), `~/.claude/hooks/caps-led.disabled` (LED kill switch).

## Known gaps

- No UI Automation provider — keyboard works, screen readers see nothing. The honest next step is `IRawElementProviderSimple`/fragment tree behind `WM_GETOBJECT` (~500 lines).
- Unofficial endpoint can change shape any day; failure mode is a visible parse error, not a crash.
- Unsigned exe (SmartScreen warning on first run).
