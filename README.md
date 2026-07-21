# Claudometer

**Your Claude usage limits, live in the Windows 11 tray.**

A tiny native Windows app that shows how much of your Claude session and weekly limits you've used — as a colored ring in the taskbar corner and an acrylic flyout with the details. No Electron, no webview, no background bloat: a single ~500 KB executable built with Rust + Win32 + Direct2D.

## What you get

- **Tray ring icon** — fills up as your 5-hour session usage grows. Accent-colored while you're fine, amber at 85%, red at 100%.
- **Flyout on click** — every limit the API reports (session, weekly all-models, weekly per-model), each with a progress bar and its reset time. Acrylic blur, rounded corners, light/dark theme, your Windows accent color.
- **Tooltip on hover** — quick numbers without clicking.
- **Settings window** (Mica) — auto-refresh interval (30s / 1m / 2m / 5m), start with Windows, refresh, quit.
- **Keyboard**: Tab cycles controls (visible focus ring), Space/Enter activates, ←/→ changes the refresh interval, Esc closes.
- Survives Explorer restarts, per-monitor DPI aware, respects `Retry-After` on rate limits, keeps showing cached data through network blips.
- Frugal by design: ~20 MB RAM when idle (GPU resources are released whenever the flyout closes), ~0.02% CPU.

> Known gap: screen-reader (UI Automation) support is not implemented yet — the UI is fully keyboard-operable, but not announced to narrators.

## Install

1. Grab `claudometer.exe` from [Releases](../../releases).
2. Run it. A ring appears in the tray overflow (`^` near the clock) — drag it onto the taskbar to pin it.
3. Optional: right-click the icon → **Start with Windows**.

> **SmartScreen note:** the exe is unsigned, so Windows may warn on first run. "More info" → "Run anyway", or build from source below.

## Requirements

- Windows 11 (22H2 or later for the full visual effects)
- [Claude Code](https://claude.com/claude-code) installed and signed in (Pro / Max / Team subscription — the app reads limits, it can't create them)

## How it works (and what it touches)

- Reads the OAuth access token from `%USERPROFILE%\.claude\.credentials.json` — **read-only**, the file is never modified and the token is never refreshed or stored anywhere else.
- Calls `https://api.anthropic.com/api/oauth/usage` — the same endpoint Claude Code's `/usage` command uses. Nothing else is contacted; no telemetry, no analytics.
- Settings live in `%APPDATA%\Claudometer\settings.json`.

⚠️ The usage endpoint is **unofficial**. Anthropic can change it any time, at which point the flyout will tell you it can't parse the response until this app is updated.

If your sign-in expires, the flyout says so — open Claude Code once and it refreshes the token itself.

## Build from source

```powershell
# needs: rustup (stable-msvc) + Visual Studio Build Tools (C++ workload)
git clone https://github.com/Dvaderfun/claudometer
cd claudometer
cargo build --release
.\target\release\claudometer.exe
```

## Bonus: Caps Lock LED as Claude Code status light

`extras/caps-led.ps1` turns your keyboard's Caps Lock LED into a Claude Code status indicator — flashing while Claude works, solid when it's done and waiting for you. It drives the LED at the keyboard-driver level (`IOCTL_KEYBOARD_SET_INDICATORS`), so **your actual Caps Lock state never changes** — typing is unaffected.

Setup:

1. Copy `extras/caps-led.ps1` to `%USERPROFILE%\.claude\hooks\caps-led.ps1`.
2. Add to `%USERPROFILE%\.claude\settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"%USERPROFILE%\\.claude\\hooks\\caps-led.ps1\" work" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"%USERPROFILE%\\.claude\\hooks\\caps-led.ps1\" done" } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"%USERPROFILE%\\.claude\\hooks\\caps-led.ps1\" attention" } ] }
    ],
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"%USERPROFILE%\\.claude\\hooks\\caps-led.ps1\" end" } ] }
    ]
  }
}
```

3. Restart Claude Code. Flashing = working, rapid burst = waiting for your permission, solid = done, off = session closed.

The Claudometer settings window has a toggle to disable it without touching the hooks.

Patterns: `work` (slow flash) · `attention` (rapid burst) · `done` (solid) · `end` (off). Test manually with `caps-led.ps1 blink`.

## License

[MIT](LICENSE)
