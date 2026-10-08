# CLAUDE.md

Claudometer — Claude + Codex usage limits in the Windows 11 tray. Native Win32 Rust, no webview. See `ARCHITECTURE.md` for the full design; this file is the working knowledge for a session.

## Commands

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo build --locked --release --target x86_64-pc-windows-msvc
```

- **Kill before rebuild** — running instance locks the exe: `Stop-Process -Name claudometer -Force`
- Single instance enforced via named mutex; second launch exits silently.
- Toolchain: stable-msvc + VS Build Tools (C++ workload). `build.rs` needs `rc.exe` (comes with Build Tools).

## Verify changes (no UI clicking needed)

Drive the flyout programmatically: find the hidden window by class `Claudometer.Main` (EnumWindows by pid — `FindWindowW` is flaky), then
`PostMessageW(hwnd, 0x8001 /* WM_APP+1 */, coords, 0x400 /* NIN_SELECT */)` toggles the flyout. Measure RAM with `(Get-Process claudometer).PrivateMemorySize64`.

Toast alerts: `.\target\release\claudometer.exe --test-alert` fires the whole pipeline with fake data; read `%APPDATA%\Claudometer\alert-test.txt` ("ok" or the error). Delivered toasts are queryable from Windows PowerShell 5.1 (not pwsh): `[Windows.UI.Notifications.ToastNotificationManager]::History.GetHistory('Claudometer')` after loading the WinRT type.

Foundation runtime baseline on the reference x64 machine: **1.53 MiB hidden and 4.45 MiB with the two-provider flyout visible (p95 private working set), below 0.002% idle CPU, 10/13 GDI handles, and 50.387 ms median / 76.551 ms p95 tray readiness in the slower 50-start run**. The REL-02 x64 baseline is 1,010,688 bytes unprovisioned and 1,097,728 bytes with the release trust root provisioned. WIP-00 UIA measures 1,045,504 / 1,132,544 bytes, using a synthetic public trust root for the latter (hard ceiling 1,310,720); per-slice sizes are in `docs/performance/artifact-ledger.md`. The controlled runtime mode excludes provider refresh/TLS work; methodology and both ten-minute runs are in `docs/performance/foundation-baseline.md`. Investigate a greater-than-10% per-slice regression.

SIZE-01 selects release `opt-level = "z"`: current x64 **947,712 bytes unprovisioned / 1,058,304 bytes with a synthetic public trust root**, ARM64 **892,928 / 940,544 bytes**. The final x64 50-start run measures **47.774 ms median / 58.141 ms p95**. The unsigned provisioned soft target is **1,245,184 bytes**, reserving 65,536 bytes inside the unchanged hard ceiling for signing and contingency. The audit and milestone allowances are in `docs/performance/size-01.md` and the ledger. cargo-bloat forces MSVC PDB/debug symbols; its analysis artifact is separate from normal stripped release measurements. Do not infer ten-minute idle CPU compliance from the timing script's one-second ancillary sample.

## Hard-won gotchas (do not re-learn these)

- **windows crate is pinned to 0.58.** API churns between minors. Known holes: `NIN_SELECT`/`NIN_KEYSELECT`/`WM_MOUSELEAVE` not exported (local consts in `main.rs`); COM methods vanish silently if a param type's cargo feature is off — `CreateSolidColorBrush` needs `Foundation_Numerics`.
- **D3D device must stay `D3D_DRIVER_TYPE_WARP`.** Historical v0.2 hardware-driver testing measured 57 MB versus 7 MB for WARP; those figures compare driver choices, not the current Foundation process baseline. WARP renders the ~330px surface in microseconds; DWM still composes on GPU.
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
- **Vibecode has two independent controls.** The flyout toggle is only the per-thread wake lock. The Advanced lid override may write power policy only after `power-override.v1.json` is durably `prepared`; every write/activation/read-back is checked, and recovery operates on the exact recorded GUID without overwriting external changes or activating an inactive scheme. Drop wake first on every exit path. Never delete a live/corrupt journal manually. Legacy `vibecode_lid` values are applied to the current scheme only after the explicit Restore action and are cleared only after verification.
- Alert dedup is per window instance (`resets_unix`), persisted in settings.json — never key on the formatted `reset_text` ("resets 18:59" recurs daily) and never alert from stale/error-preserved snapshots.
- **Updater swap relies on Windows allowing a *rename* of the running exe** (delete/overwrite are forbidden): exe → `.old`, new → exe, spawn `--swap-wait`, quit. `--swap-wait` waits on the single-instance mutex (WAIT_ABANDONED = old died = proceed) then deletes the `.old`. Downloaded exe is verified via VERSIONINFO == tag + `certutil -hashfile` vs the `.sha256` release asset — keep release.yml attaching that asset or hash verification silently stops.

## Conventions

- `fmt_caption` word-wraps on purpose (footer notes rely on it). Single-line captions inside fixed-height cards must use `fmt_caption_1` (NO_WRAP + ellipsis trimming) or they overflow their card.
- Fluent tokens hand-translated in `gfx.rs::Palette` — 4px spacing grid, Body 14 / Caption 12, colors documented next to values. New UI goes through `BrushCache`/cached formats, no per-draw allocations.
- Every user-visible action needs a keyboard path (Tab/Space/Enter/arrows) + focus ring. The committed UIA tree passes local Invoke/Toggle client checks; Narrator speech and the remaining A11Y-01 patterns/events are unverified or incomplete — don't claim screen-reader support.
- Provider state lives in `app.rs`'s UI-thread-only `APP`; graphics state stays in `main.rs`'s `UI`. Workers publish only non-secret `AppEvent`s through `poller.rs`, wait for a generation/account-bound UI ticket, and retain credentials locally. Release APP/UI borrows before alert handling, rendering, or releasing a worker.
- Errors: stale data beats error UI. Never wipe `LAST_GOOD` on a failed fetch.
- Runtime snapshots restore only after worker preparation matches a persistent opaque account and selected source. Restored data stays cached until accepted success; it never alerts or suppresses the first poll except for a persisted 429 deadline. Keep cache fields additive inside state schema 1 so old receipt writers preserve them.
- Provider errors come from typed categories in `provider/error.rs`; never classify by message or show a transport/response string. `claudeAiOauth.scopes` is optional for legacy credentials; if present without `user:profile`, preparation must stop before usage/profile requests. Native tooltip text buffers stay owned by UI for the tooltip lifetime.
- Diagnostics projects only fixed operational fields; never add raw error strings, account hashes, plan/limit labels, paths, or credential material to the snapshot. Clipboard writes require explicit Copy diagnostics; demos must leave clipboard unchanged.
- Diagnostic logs accept only fixed event codes; raw messages are never logged. Support `--diagnose`/`--version` must exit before normal startup and use read-only config/state loading, so corrupt files stay untouched. GUI-subsystem stdout uses inherited handles/parent-console attachment; verify commands through redirected output.

## Release process

Human-only; agents prepare notes and stop (see `plans/state-of-the-art-v1.md` §7).

1. Bump `Cargo.toml` version, update `CHANGELOG.md`.
2. Commit, push, wait for `build` workflow green.
3. `git tag vX.Y.Z && git push origin vX.Y.Z`, then approve the `release` environment. `release.yml` signs the manifest, attests, creates a draft with generated notes, smoke-tests the downloaded assets, and **publishes automatically** (a failed run deletes its draft). It needs `CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX`, `CLAUDOMETER_RELEASE_SEQUENCE`, and `CLAUDOMETER_RELEASE_ADMIN_READ_TOKEN` provisioned.
4. Optionally replace the notes: `gh release edit vX.Y.Z --notes-file <notes>`. Tags and assets are immutable; never reuse a version.
