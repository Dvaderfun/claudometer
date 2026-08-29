# CLAUDE.md

Claudometer — Claude + Codex usage limits in the Windows 11 tray. Native Win32 Rust, no webview. See `ARCHITECTURE.md` for the full design; this file is the working knowledge for a session.

## Commands

```powershell
cargo build --release          # output: target/release/claudometer.exe (~640 KB)
cargo clippy --release         # CI gates on -D warnings — keep zero warnings
.\target\release\claudometer.exe
```

- **Kill before rebuild** — running instance locks the exe: `Stop-Process -Name claudometer -Force`
- Single instance enforced via named mutex; second launch exits silently.
- Toolchain: stable-msvc + VS Build Tools (C++ workload). `build.rs` needs `rc.exe` (comes with Build Tools).

## Verify changes (no UI clicking needed)

Drive the flyout programmatically: find the hidden window by class `Claudometer.Main` (EnumWindows by pid — `FindWindowW` is flaky), then
`PostMessageW(hwnd, 0x8001 /* WM_APP+1 */, coords, 0x400 /* NIN_SELECT */)` toggles the flyout. Measure RAM with `(Get-Process claudometer).PrivateMemorySize64`.

Toast alerts: `.\target\release\claudometer.exe --test-alert` fires the whole pipeline with fake data; read `%APPDATA%\Claudometer\alert-test.txt` ("ok" or the error). Delivered toasts are queryable from Windows PowerShell 5.1 (not pwsh): `[Windows.UI.Notifications.ToastNotificationManager]::History.GetHistory('Claudometer')` after loading the WinRT type.

Expected budgets: **~3.5 MB fresh, ~9 MB flyout open (two sections), ~7–8.5 MB after close (updater's TLS session adds ~1), ~0.02% avg CPU, GDI count stable (~18 once settings was opened)**. A regression here is a bug.

## Hard-won gotchas (do not re-learn these)

- **windows crate is pinned to 0.58.** API churns between minors. Known holes: `NIN_SELECT`/`NIN_KEYSELECT`/`WM_MOUSELEAVE` not exported (local consts in `main.rs`); COM methods vanish silently if a param type's cargo feature is off — `CreateSolidColorBrush` needs `Foundation_Numerics`.
- **D3D device must stay `D3D_DRIVER_TYPE_WARP`.** Hardware device = ~40 MB of driver user-mode heaps that survive device release. WARP renders the ~330px surface in microseconds; DWM still composes on GPU. Measured: 57 MB vs 7 MB.
- **Flyout acrylic = undocumented accent policy** (`SetWindowCompositionAttribute`, `util::apply_acrylic`). `DWMWA_SYSTEMBACKDROP_TYPE` renders only its opaque fallback on borderless `WS_EX_NOREDIRECTIONBITMAP` popups — don't "modernize" back to it without testing on real hardware.
- **`LoadIconW` id-1 pointer:** clippy suggests `std::ptr::dangling::<u16>()` — that's address 2, wrong resource id. The `#[allow]` there is load-bearing.
- **Never perform or broker the token exchange.** `api.rs` and `codex.rs` keep both credential files strictly read-only. Claude Code alone reads its refresh token, rotates it, and rewrites `.credentials.json`; a second refresh-token user can invalidate active CLI sessions. On 401/403, tell the user to open Claude Code once. Only an explicit Connect/Reconnect may run `claude auth login`.
- **Interactive `claude auth login` needs a real console.** The normal browser callback can finish automatically, but WSL/SSH/browser failures show a code that must be pasted at `Paste code here if prompted >`. `auth.rs` uses `CREATE_NEW_CONSOLE` with inherited stdio and detects success from credential file identity (mtime + length + `expiresAt`) or a successful process exit. Do not use `expiresAt` alone: affected Claude Code releases have written `0` after a successful login.
- **Never probe the CLI by launching it on a hot path.** `claude --version` costs ~1 s. `auth.rs::find_claude` prefers `%USERPROFILE%\.local\bin\claude.exe`, then a PATH executable, then the real executable behind the official npm shim; a legacy batch file is the last fallback.
- **Killing a CLI child needs `taskkill /T`.** A CLI executable can spawn descendants; terminating only the immediate child may orphan the console process tree.
- **Honor `CLAUDE_CONFIG_DIR`.** Credentials live at `$CLAUDE_CONFIG_DIR\.credentials.json`, and the cached account state at `$CLAUDE_CONFIG_DIR\.claude.json`; only the default profile uses `%USERPROFILE%\.claude\.credentials.json` + `%USERPROFILE%\.claude.json`.
- **`claude auth status` cannot repair anything.** Its handler is a purely local read (token source + cached profile) with no network call, so it never refreshes an expired token — an earlier version "auto-repaired" with it and was a no-op that spawned a 285 MB process every 5 min. It is also not an identity source worth a process launch: `auth.rs` reads the same data straight from `~/.claude.json` (`oauthAccount`).
- **Plan name never comes from `.credentials.json`.** `subscriptionType` (and `rateLimitTier`) are written once at login and survive plan changes unchanged — a Max→Pro downgrade still reports `"max"` forever. Authoritative source is `GET /api/oauth/usage`'s sibling `GET /api/oauth/profile` (`organization.organization_type`, `account.has_claude_pro/max`), cached 1 h in `api.rs`; `~/.claude.json` `oauthAccount.organizationType` is the offline fallback.
- **Codex windows aren't positional.** `wham/usage` may deliver the weekly (168 h) window as `primary_window`; kind/label must derive from `limit_window_seconds`, never from primary/secondary position.
- Tray icon must be re-added on the `TaskbarCreated` broadcast (explorer restart) — already handled, keep it.
- **Toasts need the AUMID registry key AND a live `ToastNotification` object.** Unpackaged exes toast via `HKCU\Software\Classes\AppUserModelId\Claudometer` (`alerts::init`); the OS routes the `Activated` (click) event through the shown `ToastNotification` — `alerts.rs` keeps recent ones in a thread_local on purpose. `IconUri` must be a real file on disk (ico extracted to `%APPDATA%\Claudometer`), not an exe resource path.
- **Vibecode mode must always be able to put the lid setting back.** The original (AC, DC) lid-close indices go into settings.json *before* the first override and are only cleared when the user turns the mode off — arming again never overwrites them (that would save our own "Do nothing" as the original). Quit restores the system state but keeps the flag, so the next launch re-arms. The wake lock is per-*thread*: arm/drop it on the UI thread only.
- Alert dedup is per window instance (`resets_unix`), persisted in settings.json — never key on the formatted `reset_text` ("resets 18:59" recurs daily) and never alert from stale/error-preserved snapshots.
- **Updater swap relies on Windows allowing a *rename* of the running exe** (delete/overwrite are forbidden): exe → `.old`, new → exe, spawn `--swap-wait`, quit. `--swap-wait` waits on the single-instance mutex (WAIT_ABANDONED = old died = proceed) then deletes the `.old`. Downloaded exe is verified via VERSIONINFO == tag + `certutil -hashfile` vs the `.sha256` release asset — keep release.yml attaching that asset or hash verification silently stops.

## Conventions

- `fmt_caption` word-wraps on purpose (footer notes rely on it). Single-line captions inside fixed-height cards must use `fmt_caption_1` (NO_WRAP + ellipsis trimming) or they overflow their card.
- Fluent tokens hand-translated in `gfx.rs::Palette` — 4px spacing grid, Body 14 / Caption 12, colors documented next to values. New UI goes through `BrushCache`/cached formats, no per-draw allocations.
- Every user-visible action needs a keyboard path (Tab/Space/Enter/arrows) + focus ring. UIA is a known gap — don't claim screen-reader support.
- All fetch-state statics live in `main.rs` top; UI-thread-only state in the `UI` thread_local.
- Errors: stale data beats error UI. Never wipe `LAST_GOOD` on a failed fetch.

## Release process

1. Bump `Cargo.toml` version, update `CHANGELOG.md`.
2. Commit, push, wait for `build` workflow green.
3. `git tag vX.Y.Z && git push --tags` — `release.yml` builds and attaches the exe.
4. `gh release edit vX.Y.Z --notes-file <notes>` if custom notes wanted.
