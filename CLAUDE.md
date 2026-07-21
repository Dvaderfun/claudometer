# CLAUDE.md

Claudometer — Claude usage limits in the Windows 11 tray. Native Win32 Rust, no webview. See `ARCHITECTURE.md` for the full design; this file is the working knowledge for a session.

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

Expected budgets: **~3 MB fresh, ~7 MB flyout open, ~5.5 MB after close, ~0.02% avg CPU, GDI count stable (~10)**. A regression here is a bug.

## Hard-won gotchas (do not re-learn these)

- **windows crate is pinned to 0.58.** API churns between minors. Known holes: `NIN_SELECT`/`NIN_KEYSELECT`/`WM_MOUSELEAVE` not exported (local consts in `main.rs`); COM methods vanish silently if a param type's cargo feature is off — `CreateSolidColorBrush` needs `Foundation_Numerics`.
- **D3D device must stay `D3D_DRIVER_TYPE_WARP`.** Hardware device = ~40 MB of driver user-mode heaps that survive device release. WARP renders the ~330px surface in microseconds; DWM still composes on GPU. Measured: 57 MB vs 7 MB.
- **Flyout acrylic = undocumented accent policy** (`SetWindowCompositionAttribute`, `util::apply_acrylic`). `DWMWA_SYSTEMBACKDROP_TYPE` renders only its opaque fallback on borderless `WS_EX_NOREDIRECTIONBITMAP` popups — don't "modernize" back to it without testing on real hardware.
- **`LoadIconW` id-1 pointer:** clippy suggests `std::ptr::dangling::<u16>()` — that's address 2, wrong resource id. The `#[allow]` there is load-bearing.
- **Never refresh the OAuth token.** `api.rs` reads `~/.claude/.credentials.json` strictly read-only. Refresh-token rotation would invalidate the user's Claude Code session. Expired = tell user to open Claude Code.
- Tray icon must be re-added on the `TaskbarCreated` broadcast (explorer restart) — already handled, keep it.

## Conventions

- Fluent tokens hand-translated in `gfx.rs::Palette` — 4px spacing grid, Body 14 / Caption 12, colors documented next to values. New UI goes through `BrushCache`/cached formats, no per-draw allocations.
- Every user-visible action needs a keyboard path (Tab/Space/Enter/arrows) + focus ring. UIA is a known gap — don't claim screen-reader support.
- All fetch-state statics live in `main.rs` top; UI-thread-only state in the `UI` thread_local.
- Errors: stale data beats error UI. Never wipe `LAST_GOOD` on a failed fetch.

## Release process

1. Bump `Cargo.toml` version, update `CHANGELOG.md`.
2. Commit, push, wait for `build` workflow green.
3. `git tag vX.Y.Z && git push --tags` — `release.yml` builds and attaches the exe.
4. `gh release edit vX.Y.Z --notes-file <notes>` if custom notes wanted.
