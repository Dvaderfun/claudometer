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
| `auth.rs` | Claude account: identity from local files + explicit interactive browser sign-in (own console, cancellable) delegated to the resolved native `claude` executable |
| `api.rs` | Claude credentials read + usage fetch; shared display model (`UsageSnapshot`, `LimitRow`, `FetchOutcome`), time formatting |
| `codex.rs` | Codex (OpenAI) credentials read + usage fetch → same `UsageSnapshot` |
| `provider/model.rs` | stable provider/account/limit/request identities and typed completion envelope |
| `runtime_state.rs` | atomic optional `state.json` envelope, CNG install salt, and account-scoped receipt schema |
| `state_policy.rs` | fake-clock-testable debounce, stale, flyout-refresh, and 429 policy |
| `store.rs` | typed atomic JSON commit, verified `.bak` generation, corruption preservation, and failure injection |
| `demo.rs` | deterministic provider/view scenarios and guarded no-side-effect launch mode |
| `trayicon.rs` | CPU-rasterized ring/alert HICON (premultiplied DIB, no fonts) |
| `alerts.rs` | 75% toast alerts: WinRT toast pipeline, AUMID registration, per-window dedup |
| `updater.rs` | GitHub-Releases self-update: daily check, verified download, rename-swap handover |
| `config.rs` | cached validated `SettingsV1`, atomic persistence, legacy dual-read/write, and schema diagnostics |
| `util.rs` | theme/accent detection, autostart registry, caps-LED toggle, dark menus, acrylic |
| `vibecode.rs` | independent wake lock plus journaled Advanced lid override, conservative recovery, legacy one-shot restore, and lifecycle reconciliation |

## Rendering (`gfx::Surface`)

WARP D3D11 device → DXGI **composition** swapchain (premultiplied alpha) → `IDCompositionVisual` → window. D2D device context draws onto the swapchain buffer; DirectWrite for text. Window uses `WS_EX_NOREDIRECTIONBITMAP` so there's no GDI redirection surface at all.

Key decisions, with reasons:

- **WARP, not hardware** (v0.2.0): historical driver-comparison testing measured 57 MB with the hardware driver versus 7 MB with WARP because the hardware driver's user-mode heaps survived device release. Those are not current whole-process claims; the reproducible Foundation baseline is in `docs/performance/foundation-baseline.md`. The surface is ~330 px, CPU rasterization is microseconds, and DWM composes the swapchain on the GPU either way.
- **Surface dropped on hide, recreated on show** (~10 ms): the GPU stack *is* the app's RAM cost; windows are hidden 99% of the time.
- **Caches**: brushes keyed by `(dark, accent)`, text formats built once, single RT QI at creation. Zero per-draw allocations.
- **Flyout material — accent-policy acrylic** (`ACCENT_ENABLE_ACRYLICBLURBEHIND`, undocumented): `DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW` renders only its opaque fallback on borderless DComp popups (observed on build 28020, even with frame extension). Settings window is a titled window, where `DWMSBT_MAINWINDOW` (Mica) works through the documented path.
- **Tray icon on CPU** (`trayicon.rs`): 16–24 px ring with per-pixel AA math into a premultiplied DIB → `CreateIconIndirect`. No D2D needed for 256 pixels; no font dependency for the alert glyph.

## Data layer (`api.rs` + `codex.rs`)

Two independent providers, one worker thread each per poll (~1/min), both producing the same display-ready `UsageSnapshot`:

**Claude** (`api.rs::fetch`):

1. Read `%USERPROFILE%\.claude\.credentials.json`, or `$CLAUDE_CONFIG_DIR\.credentials.json` when configured — always read-only. The local `expiresAt` field is only a login-completion stamp, never authority for rejecting an otherwise usable token. Claudometer does not read or replay the refresh token; Claude Code is the sole session writer.
2. `GET api.anthropic.com/api/oauth/usage`, Bearer token, `anthropic-beta: oauth-2025-04-20`, via ureq + native-tls (schannel — OS cert store, no C deps). **Unofficial endpoint** — parsing is defensive, every field optional.
3. Prefer the `limits[]` array (kind/percent/severity/resets_at/scope); fall back to legacy `five_hour`/`seven_day`; append `extra_usage` if enabled.
4. Plan label comes from `GET /api/oauth/profile` (cached 1 h, falls back to `~/.claude.json` then to the last known value) — the credentials file's `subscriptionType` is stale after a plan change.

**Codex** (`codex.rs::fetch`):

1. Read `~/.codex/auth.json` (`CODEX_HOME` honored) — **read-only, never refreshed** (OpenAI rotates refresh tokens; an external refresh would invalidate the Codex CLI session). Expiry checked via the JWT `exp` claim (hand-rolled base64url, no verify). No file / no `tokens` (API-key-only install) → provider counts as absent and the section doesn't render at all.
2. `GET chatgpt.com/backend-api/wham/usage` — the same **unofficial endpoint** the Codex CLI's TUI polls — with `Authorization: Bearer` + `chatgpt-account-id` headers.
3. `rate_limit.primary_window`/`secondary_window` → rows; kind/label derived from `limit_window_seconds` (≤24 h → "Session (Nh)", 7 d → "Weekly"), **not** from window position — which window arrives as primary varies by plan. Severity is empty → percent thresholds color the bars.

Every provider HTTP body is read as at most 1 MiB plus one sentinel byte and rejected when oversized. Parsed output is bounded to 64 rows and 512 UTF-8 bytes per provider-controlled display string. Sanitized fixtures under `tests/fixtures/` cover normal, partial, unknown, malformed, missing-reset, weekly-primary, non-finite, and out-of-range shapes without live network access.

Resilience rules (in `main.rs`, per provider via `SLOTS`, with time decisions isolated in `state_policy.rs`):

- Credential parsing and secret-bearing request preparation run only on short-lived provider workers. A stable provider account ID is salted with the CNG-generated install salt and SHA-256; when none exists, an access-token fingerprint uses a process-only salt and is never persisted. Secret strings have no `Debug`/serialization surface and overwrite their buffers on drop.
- Every slot has a current opaque account, generation, reserved request ID, and account-bound completion/last-good state. Credential changes clear snapshot, plan, error, cooldown, debounce, and alerts before replacement work. A completion must match provider + generation + request + account before any side effect; obsolete work cannot clear a newer fetch flag.
- Claude's profile-plan cache is keyed by opaque account. Its local fallback plan and stable ID are captured from the same worker-local identity read, so a concurrent account switch cannot attach another account's plan.
- `last_good` snapshot survives failed fetches for up to 10 minutes — UI shows stale data + footer note; a provider with no data degrades to a dim note line in its own section; the whole-flyout error view exists only for the nothing-ever-fetched case.
- Every 429 starts a 60–900 s `cooldown_until` immediately (server `Retry-After` when useful, exponential fallback otherwise). Automatic and manual refreshes both honor it; there is no fast retry.
- 3 s debounce on refresh; `fetching` flag dedupes concurrent spawns. Characterization uses an injected fake clock and never sleeps.
- Fetch threads publish via mutexed statics + `PostMessageW(WM_DATA_READY)` — UI mutations stay on the UI thread.
- Codex enablement (`codex_active`) = settings toggle AND auth file present — checked per poll, so signing in/out of Codex shows/hides the section without restart.

## Alerts (`alerts.rs`)

One native toast per limit window that crosses **75%** (`WARN_AT`), evaluated on the UI thread on `WM_DATA_READY`. Each accepted completion posts its provider and request identity; alert evaluation selects only the matching newly accepted success. An unrelated provider completion cannot re-evaluate another provider's stored result, and stale/error-preserved snapshots remain ineligible.

- **Real WinRT toasts, unpackaged**: `ToastNotificationManager::CreateToastNotifierWithId` against an AUMID registered under `HKCU\Software\Classes\AppUserModelId\Claudometer` (`DisplayName` + `IconUri` → ico extracted to `%APPDATA%\Claudometer`). Gets Action Center persistence, Focus Assist / DND suppression, and a per-app toggle in Windows notification settings — none of which legacy balloons provide. `SetCurrentProcessExplicitAppUserModelID` ties the process to the AUMID at startup.
- **Account-scoped dedup**: provider + opaque account key + stable limit ID + threshold + reset instance. Percent climbing inside one window fires once; a real reset re-arms. Receipts live in sanitized `state.json`; the legacy `settings.json.alerted` map is dual-written only for downgrade compatibility and migrated once to the first proven account for each provider.
- Toast body: title + reset time + native `<progress>` bar pinned at the worst crossed limit. Click → `WM_TOAST_ACTIVATED` → flyout opens at the tray icon (`Shell_NotifyIconGetRect`). Shown `ToastNotification` objects are kept alive in a thread_local — the OS routes `Activated` through them.
- `NotificationSetting::DisabledForApplication/User` is honored: no balloon resurrection. The `NIF_INFO` balloon fallback fires only when the WinRT path itself errors.
- `claudometer.exe --test-alert` drives the whole pipeline with fake data; outcome written to `%APPDATA%\Claudometer\alert-test.txt` (exe has no console).

## Vibecode mode (`vibecode.rs`)

The flyout toggle controls only a wake lock through `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)`. It is per-*thread*, so it is armed and dropped on the UI thread and disappears with the process. Wake-lock failure/state is independent from the persistent lid transaction.

Ignoring lid close is a separate explicit **Advanced** Settings control. Before any system write, `power-override.v1.json` records schema, operation, exact scheme GUID, original/applied AC/DC values, `prepared` phase, time, and app version through `AtomicJsonStore`. The controller rechecks the active scheme, checks both writes and activation, reads back both values, then records `applied`. Only that verified state is displayed as active.

Recovery changes the journal to `restoring`, operates only on its recorded GUID, and restores a field only while it still equals Claudometer's applied value. A field already at the original is `restored`; any other value is `relinquished_external_change`. An inactive old scheme is never activated. If the journal scheme is still active it is reactivated even on a retry where stored originals were already present, closing the crash-before-activation window. The journal and `.bak` are deleted only after both fields reach terminal verified outcomes.

Startup recovers before applying a new override. `WM_QUERYENDSESSION`, `WM_ENDSESSION`, `WM_DESTROY`, power broadcasts, and the poll timer converge through the same idempotent recovery/reconcile path; wake lock is dropped first on exit even when persistent recovery remains. `--recover-vibecode` performs the same deterministic recovery without starting the UI, provider workers, alerts, or updater.

Legacy `settings.json.vibecode_lid` has no scheme GUID, so it blocks new overrides and remains preserved. Settings offers “Restore” with an explicit current-scheme explanation. That direct action creates a one-shot journal before applying the saved pair and clears the legacy field only after verified application; it is never guessed or silently discarded. See `docs/recovery/vibecode.md`.

## Updater (`updater.rs`)

Passive, transparent, user-initiated. When **Automatically check for updates** is enabled, `releases/latest` is checked once per day and once at launch (worker thread, silent failures, drafts/prereleases and non-semver tags skipped). The setting defaults off for genuinely new installs and migrates on for existing settings documents. Surfaces: the Settings toggle and About card ("Claudometer X.Y.Z · GitHub" → "Update vX.Y.Z available · Install"), plus an accent dot on the flyout gear. Deliberately **no** update toast — toasts are reserved for usage limits.

Versions without the Ed25519 trust root—including all published versions
through 0.7.3—cannot authenticate it retroactively and may not update
automatically to the first trust-root-enabled release. Existing users must
manually install that release after following the independent hash and
Authenticode checks in `docs/release-manifest-v1.md`. Builds without both a
production public key and a positive embedded release sequence reject every
manifest.

All fixed provider, updater, repository, and help destinations live in `network.rs`; direct GET construction also passes through that module. The CI privacy allowlist compares those constants and request sites with `PRIVACY.md`.

Install (only on click), all failure paths falling back to opening the release page:

1. Download the exact signed-manifest asset beside the current executable under a CNG-random, attempt-scoped candidate name. Bound its size, require `MZ`, and verify its in-process CNG SHA-256 before writing; then `sync_all` and verify VERSIONINFO and SHA-256 again from disk.
2. Persist `update-operation.v1.json` with the canonical, candidate, and backup names plus both executable hashes. Its atomic phases are `verified`, `current_moved`, `candidate_installed`, `candidate_ready`, and `committed`.
3. Move the running executable to the attempt-scoped backup and the candidate to the canonical name with same-directory, write-through renames, persisting the corresponding phase after each boundary. The backup is never deleted before `committed` is durable.
4. Handover by spawning the canonical executable with `--swap-wait`. Startup reconciles journal phase and actual hashes: a canonical candidate completes ready/commit, while any other pre-commit launch restores the verified backup. Recovery is idempotent even when the journal lags a completed rename; corrupt or path-tampered journals fail closed.

## State

- Cross-thread: `SLOTS[2]` (per-provider `state`, `last_good`, `last_fetch`, `cooldown_until` mutexes + `fetching` atomic); `POLL_SECS`, hwnds (atomics).
- UI-thread only: `UI` thread_local — surfaces, hover, keyboard focus, mouse-tracking flags.
- Persistent: `%APPDATA%\Claudometer\settings.json` is loaded once into validated `SettingsV1` and written through `AtomicJsonStore`. Canonical v1 preference keys are dual-written with the v0.7.x keys; unknown fields, legacy `alerted`, and legacy `vibecode_lid` remain preserved. Optional `%APPDATA%\Claudometer\state.json` contains only schema metadata, a 32-byte CNG-generated install salt, account/provider/limit/reset-scoped alert receipts, and one-time legacy-migration markers. Account changes atomically evict that provider's previous receipts; token-fingerprint identities never enter the file. Deleting `state.json` loses only cache/deduplication identity and safely generates a new salt. `%APPDATA%\Claudometer\icon.ico` (toast icon), HKCU Run key (autostart), HKCU AppUserModelId key (toast registration), and the LED kill switch under the Claude config directory (`CLAUDE_CONFIG_DIR`, otherwise `%USERPROFILE%\.claude`) remain separate. Account credentials remain owned by Claude Code/Codex; Claudometer adds no token store.

`store.rs` is the persistence primitive for the typed migrations that follow.
It serializes and validates completely in memory, writes a unique same-directory
temporary file, calls `sync_all`, and commits with `ReplaceFileW` (retaining one
verified `.bak`) or first-create `MoveFileExW(...WRITE_THROUGH)`. It validates
the committed target and backup, restores on validation failure, and moves a
malformed target to a timestamped `.corrupt` sibling before loading a verified
backup or requesting defaults. Typed errors contain only stage/error category,
never a path. Fault tests cover directory/temp creation, access denial, disk
full, write, flush/interruption, replace, validation, restoration, and corrupt
preservation. Settings now use it through `config.rs`; safety and runtime state
move to their own files in later slices.

## Deterministic demo mode

`claudometer.exe --demo=<scenario>` renders synthetic `claude-only`, `codex-only`, `both`, `loading`, `stale`, `cooldown`, `error`, or `neither` state. `--demo-hidden` keeps only its synthetic tray icon for hidden-state measurement. The demo branch runs before single-instance/update cleanup, toast registration, settings and credential reads, provider workers, or Vibecode initialization; refresh, settings, and wake-lock actions are guarded while it is active. Appearance and data are fixed, no provider endpoint is contacted, and Escape closes a visible demo.

`ci/verify-demo.ps1` runs the optimized binary with an isolated empty profile and verifies no profile files, Claudometer registry changes, child processes, TCP connections, or active-power-scheme changes.

## Known gaps

- No UI Automation provider — keyboard works, screen readers see nothing. The honest next step is `IRawElementProviderSimple`/fragment tree behind `WM_GETOBJECT` (~500 lines).
- Unofficial endpoint can change shape any day; failure mode is a visible parse error, not a crash.
- Unsigned exe (SmartScreen warning on first run).
