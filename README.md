# Claudometer

**Your Claude + Codex usage limits, live in the Windows 11 tray.**

A tiny native Windows app that shows how much of your Claude (and optionally OpenAI Codex) session and weekly limits you've used — as a colored ring in the taskbar corner and an acrylic flyout with the details. No Electron, no webview, no background bloat: the release is a single executable around 1.1 MiB, built with Rust + Win32 + Direct2D.

## What you get

- **Tray ring icon** — fills up as your 5-hour Claude session usage grows. Accent-colored while you're fine, amber at 85%, red at 100%.
- **Flyout on click** — every limit the API reports (session, weekly all-models, weekly per-model), each with a progress bar and its reset time. Acrylic blur, rounded corners, light/dark theme, your Windows accent color.
- **Codex too** — if you're signed into the [Codex CLI](https://developers.openai.com/codex/cli), a second section shows its session/weekly limits automatically. No setup; no Codex, no section. Toggle in settings.
- **Tooltip on hover** — quick numbers without clicking.
- **Account connection** — see the linked Claude account and plan in Settings; connect or switch it through Claude Code's official browser sign-in without copying OAuth secrets into Claudometer.
- **Settings window** (Mica) — Claude account, auto-refresh interval (30s / 1m / 2m / 5m), start with Windows, Codex section toggle, refresh, quit.
- **Keyboard**: Tab cycles controls (visible focus ring), Space/Enter activates, ←/→ changes the refresh interval, Esc closes.
- Survives Explorer restarts, per-monitor DPI aware, respects `Retry-After` on rate limits, keeps showing cached data through network blips.
- Frugal by design: the reproducible no-network baseline is 1.53 MiB hidden and 4.45 MiB with the two-provider flyout visible (p95 private working set), with less than 0.002% idle CPU. See the [measurement method and raw runs](docs/performance/foundation-baseline.md).

> Accessibility: UI Automation names, Invoke/Toggle, and keyboard focus are implemented. Narrator verification, additional patterns, and dynamic announcements remain incomplete.

## Install

1. Download the matching versioned x64 or ARM64 executable from [Releases](https://github.com/Dvaderfun/claudometer/releases). ARM64 runtime verification remains incomplete.
2. Follow the release notes to verify SHA-256 and GitHub provenance, then run it. A ring appears in the tray overflow (`^` near the clock) — drag it onto the taskbar to pin it.
3. Optional: right-click the icon → **Start with Windows**.

> **Manual update from 0.7.x:** this first trust-root release cannot be installed by the old updater. Windows executables remain unsigned, so SmartScreen may warn. Verify the release before choosing to run it, or build from source. Subsequent updates require authenticated manifests.

## Requirements

- Windows 11 (22H2 or later for the full visual effects)
- [Claude Code](https://claude.com/claude-code) installed and signed in (Pro / Max / Team subscription — the app reads limits, it can't create them)
- Optional: [Codex CLI](https://developers.openai.com/codex/cli) signed in with a ChatGPT plan for the Codex section

## How it works (and what it touches)

- Reads the OAuth access token from `%USERPROFILE%\.claude\.credentials.json`, or `$CLAUDE_CONFIG_DIR\.credentials.json` when configured (and, if present, `%USERPROFILE%\.codex\auth.json`) — **read-only**. Claudometer never writes either file and never receives or rotates a refresh token.
- Uses the installed Claude Code CLI only for an explicit Connect/Reconnect. It opens `claude auth login` in a console window so the browser flow's fallback code can be pasted if needed; click the card again to cancel. Current native and npm installs are resolved to the real `claude.exe`, avoiding a CMD shim. Claude Code alone owns token refresh and durable credential storage.
- Calls the Anthropic usage/profile APIs and, when Codex is signed in, the ChatGPT usage API. Automatic GitHub update checks default off for new installs; enable them in Settings. Existing settings retain enabled checks. There is no telemetry or analytics; the complete network, file, registry, process, notification, and power inventory is in [PRIVACY.md](PRIVACY.md).
- Settings and runtime state live under `%APPDATA%\Claudometer`.

⚠️ Both usage endpoints are **unofficial**. Anthropic/OpenAI can change them any time, at which point the flyout will tell you it can't parse the response until this app is updated.

If the server rejects Claude's access token, open Claude Code once so its credential owner can refresh the session, then refresh Claudometer. Use **Reconnect** only if Claude Code itself asks you to sign in. Codex credentials are also read-only; open Codex if that sign-in expires.

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
