# Claudometer: state-of-the-art roadmap

**Status:** Proposed implementation plan
**Baseline:** `v0.7.3` / commit `5b735ce`
**Plan date:** 2026-09-03
**Primary target:** a trustworthy, accessible, signed `v1.0` for Windows 11 x64 and ARM64

## 1. Executive decision

Claudometer will compete on trust, native Windows quality, and footprint—not on provider count.

The product thesis is:

> The smallest trustworthy native Windows quota monitor for AI coding tools.

The current strengths remain load-bearing:

- Native Win32 + Direct2D/DirectComposition; no WebView or managed runtime.
- One small process, no service, daemon, local server, or resident helper.
- Provider credentials stay owned by the official CLIs.
- Fast glanceable tray UI with low memory and CPU use.
- Honest freshness, stale-data, and rate-limit behavior.

The route to state of the art is therefore:

1. Make every persistent or system-changing operation recoverable.
2. Isolate all state by provider and account.
3. Prefer documented local provider surfaces and label compatibility fallbacks.
4. Make failures diagnosable without collecting telemetry or secrets.
5. Make the custom native UI accessible and adaptive.
6. Make installation, updating, and removal signed and boring.
7. Add actionable pacing only after the foundation is proven.

Provider breadth, dashboards, and themes do not compensate for weak safety or trust. No new provider is authorized before `v1.0`.

## 2. How to use this plan

- Treat every task ID as a separate issue and, normally, a separate reviewable pull request.
- Keep every pull request runnable and independently releasable.
- Add characterization tests before structural changes.
- A milestone is complete only when every acceptance criterion is demonstrated.
- Do not begin a later milestone to work around an incomplete earlier invariant.
- If a task forces a product or security tradeoff, record an ADR under `docs/adr/` before implementation.
- Check a task box only after code, tests, documentation, and rollback behavior land together.

### 2.1 Start here

The first implementation slice is intentionally small:

1. Ship PR 0: disable new persistent lid-policy mutations while retaining the wake lock and all recovery data.
2. Ship PR 1: format the tree and make format, Clippy, and tests required for branches and tags.
3. Add current Claude/Codex fixtures and fake-clock state characterization.
4. Measure and record the current binary/memory/startup/network baseline.
5. Land the distribution/channel ADR early, even though installer work comes later.

Stop that slice when the emergency risk is contained and Foundation acceptance is green. Do not start the architecture extraction in the same batch.

## 3. Product contract

Every release must preserve these invariants.

### 3.1 Privacy and credentials

- Claude Code and Codex remain the sole owners of login, refresh-token rotation, and durable credentials.
- Claudometer never writes provider credential files and never stores refresh tokens, access tokens, browser cookies, or user-entered API keys.
- Secret material remains worker-local, is never included in `Debug`, and is never serialized or logged.
- Every displayed snapshot belongs to an opaque account identity that matches the current credential identity.
- No telemetry, cloud sync, prompt collection, response-body logging, or automatic crash upload.
- Every network destination, file, registry key, child process, and system-setting mutation is documented with its trigger and user control.

### 3.2 Native footprint

- No Electron, Tauri, WebView, .NET runtime, Node runtime, Java runtime, or browser shell.
- No Tokio or general asynchronous runtime unless an ADR proves that the native thread/message model cannot meet a concrete requirement.
- No persistent helper, Windows service, scheduled task, or loopback server.
- No new dependency without measured binary-size, startup, security, maintenance, and license impact.

### 3.3 Data integrity

- Every quota has a provider, account, stable limit identity, source, observation time, and freshness state.
- Cached data is loaded only after its account identity is matched locally.
- Obsolete asynchronous results cannot update UI, cache, alerts, or cooldowns.
- A failed write leaves the previous valid file intact.
- A failed system mutation leaves either verified original state or a durable actionable recovery journal.
- Alerts consume accepted fresh-fetch events, never inferred freshness from the currently rendered view.

### 3.4 Valid installation states

All of these are first-class, tested states:

- Claude only.
- Codex only.
- Claude and Codex.
- Neither provider installed or signed in.
- Portable installation.
- Per-user installed application.

## 4. Provisional performance budgets

Milestone `BASE-01` will measure and freeze the reference hardware and exact methodology. Until then, use these budgets:

| Metric | Budget |
|---|---:|
| Current unsigned release executable | 815,104 bytes |
| Signed executable soft target | at most 1.0 MiB |
| Signed executable hard ceiling | 1.25 MiB; exceed only through an approved ADR |
| Hidden private working set after 60 seconds | at most 10 MiB |
| Visible two-provider flyout private working set | at most 15 MiB |
| Idle CPU, excluding refresh work | below 0.1% over five minutes |
| Tray readiness on reference Windows 11 VM | at most 500 ms p95 |
| Statusline bridge duration | at most 50 ms p95 |
| Persistent quota-history cap after `v1.0` | at most 1 MiB |

Additional rules:

- Report the executable, installer, memory, startup, and idle-CPU measurements separately.
- A provider refresh may launch a deadline-bound child, but no child may remain afterward.
- Opening or repainting a window must not itself create a network request unless the selected freshness policy says the data is due.
- Investigate any executable growth greater than 10% in one pull request, even when still under the hard ceiling.

Initial defensive bounds, adjustable only with fixture evidence:

| Input/state | Bound |
|---|---:|
| Provider HTTP response body | 1 MiB |
| Update manifest | 64 KiB |
| Application/update download | 16 MiB, read as limit + 1 byte |
| Normalized quota rows per provider | 64 |
| Provider-controlled display string | 512 UTF-8 bytes |
| Retained diagnostic files | 3 files × 256 KiB |
| Update readiness timeout | 15 seconds |
| Pacing limits retained | 32 current-account limit identities |
| Pacing samples used per limit | 96 downsampled points |

## 5. Target architecture

Keep the current single-process Win32 architecture. Move policy and state out of window procedures so it can be tested without creating windows.

```text
Win32 messages
      │
      ▼
App (UI-thread owner) ─────► View models ─────► flyout / settings / tray / alerts
      │
      ├────► Provider controller ─────► short-lived source workers
      │                                      ├── documented local source
      │                                      └── compatibility endpoint
      │
      └────► Typed stores
              ├── settings
              ├── sanitized runtime cache
              ├── power-operation journal
              └── update-operation journal
```

### 5.1 State ownership

- The UI thread exclusively mutates `App` and provider state.
- Workers receive immutable inputs and send typed `AppEvent` values through `std::sync::mpsc`.
- `PostMessageW` only wakes the UI thread; the UI thread drains the event queue.
- Every request and event carries `provider`, `generation`, `request_id`, and `account_key`.
- Events that no longer match current state are discarded without any side effect.
- Rendering consumes derived view models, never provider JSON or transport errors.

### 5.2 Intended module boundaries

| Module | Responsibility |
|---|---|
| `main.rs` | Process bootstrap, argument dispatch, window registration, message loop |
| `app.rs` | UI-thread-owned `App`, commands, timers, window-event coordination |
| `provider/model.rs` | Provider/source IDs, normalized limits, account keys, snapshots, typed errors |
| `provider/state.rs` | Pure reducer, freshness policy, retry policy, display derivation |
| `provider/mod.rs` | Static provider catalog and source-order policy |
| `provider/claude.rs` | Claude credentials, source adapters, parsers |
| `provider/codex.rs` | Codex credentials, app-server client, compatibility parser |
| `poller.rs` | Worker spawning and typed event delivery; no provider-specific policy |
| `config.rs` | Typed settings, validation, defaults, migration |
| `store.rs` | Atomic JSON writes, replacement, corruption preservation |
| `cache.rs` | Sanitized account-bound snapshots, alert receipts, retry deadlines |
| `timefmt.rs` | Target-instant local-time conversion and reset labels |
| `diagnostics.rs` | Stable error codes, bounded redacted log, support snapshot |
| `alerts.rs` | Fresh-event-only alert policy and WinRT delivery |
| `vibecode.rs` | Wake lock and its independent power-operation transaction |
| `updater.rs` | Manifest verification and its independent update transaction |
| `gfx.rs` | Existing renderer until accessibility/layout tests allow a safe split |
| `trayicon.rs` | CPU tray icon generation |
| `util.rs` | Small Windows helpers only; split opportunistically, not as a rewrite |

Do not create a cross-platform abstraction. Windows is the product platform, not an implementation detail.

### 5.3 Normalized provider model

The exact names may change during implementation, but the model must express the following contract:

```rust
enum ProviderId {
    Claude,
    Codex,
}

enum SourceId {
    ClaudeStatusline,
    ClaudeOAuthCompatibility,
    CodexAppServer,
    CodexWhamCompatibility,
}

enum SourceSupport {
    Documented,
    Compatibility,
}

struct UsageSnapshot {
    provider: ProviderId,
    account: AccountKey,
    source: SourceProvenance,
    plan: Option<String>,
    limits: Vec<UsageLimit>,
    observed_at_unix: i64,
}

struct SourceProvenance {
    id: SourceId,
    support: SourceSupport,
}

struct UsageLimit {
    id: LimitId,
    kind: LimitKind,
    label: String,
    class: LimitClass,
    utilization: Percent,
    provider_severity_hint: Option<ProviderSeverity>,
    resets_at_unix: Option<i64>,
}

enum LimitClass {
    Quota,
    Spend,
}

enum LimitKind {
    Session,
    Weekly,
    Model,
    ExtraUsage,
    Other(String),
}
```

Required behavior:

- `Percent` rejects non-finite input and clamps once at the adapter boundary.
- Formatted reset text does not live in the domain model; the UI formats the target instant using the offset applicable at that instant.
- Alert identity uses provider + account + stable limit ID + threshold + reset instance, never a display label.
- Spend/extra usage is typed rather than excluded through string comparisons.
- User-facing `DisplaySeverity` is derived in policy/view state from utilization, configured thresholds, and any provider hint; it is not frozen into cached snapshots.
- Errors contain a stable safe category, source, retry hint, and user message. Raw response bodies are not retained.
- Existing direct endpoints are labeled `Compatibility`, never `Documented`.
- A source fallback occurs only when the source is unsupported, unavailable, or explicitly disabled. It must not evade an authentication error or 429 by immediately hitting another source.

### 5.4 Provider reducer

Use one UI-owned state machine per provider:

```rust
enum ProviderPhase {
    Disabled,
    Unavailable(UnavailableReason),
    Idle,
    Fetching { request_id: u64 },
    Ready,
    Backoff { until: Instant, error: FetchError },
    Failed(FetchError),
}
```

| Event | Required result |
|---|---|
| Provider disabled | Increment generation, clear displayed data, enter `Disabled` |
| Credential/account changes | Increment generation; clear old snapshot, plan, error, cooldown, and debounce |
| Matching cache loaded | Attach as explicitly cached data |
| Refresh requested | Start only if enabled, available, not fetching, and allowed by debounce/backoff |
| Fetch succeeds | Accept only matching generation/request/account; clear errors and streak; persist sanitized snapshot |
| Fetch returns 429 | Preserve same-account last-good data; enter explicit backoff |
| Other transient failure | Preserve same-account data only for the configured stale window |
| Authentication fails | Clear account-bound data and enter `Unavailable` |
| Obsolete worker completes | Drop it; no render, alert, cache, or cooldown change |
| Cooldown expires | Return to `Idle`; the next explicit/scheduled command may fetch |

Manual refresh during cooldown must display the next allowed retry time rather than silently doing nothing. Any non-429 result resets the consecutive 429 streak.

### 5.5 Persistence ownership

Use separate files because their failure and deletion semantics differ:

| File | Contents | Safe to delete? |
|---|---|---|
| `settings.json` | User preferences only | Yes; defaults return |
| `state.json` | Sanitized snapshots, alert receipts, retry deadline, install salt | Yes; cached state is lost |
| `power-override.v1.json` | Safety-critical Vibecode transaction | No while present; recover first |
| `update-operation.v1.json` | Update swap transaction | No while present; recover first |
| `diagnostics.log` | Bounded redacted operational events | Yes |

All JSON writes go through `AtomicJsonStore`:

1. Validate and serialize completely in memory.
2. Create a unique temporary file beside the target.
3. Write and `sync_all` the temporary file.
4. Validate the existing target and retain one verified last-known-good `.bak` generation.
5. Atomically replace with `ReplaceFileW` using the backup generation, or first-create with `MoveFileExW(...WRITE_THROUGH)`.
6. Validate the committed file before reporting success.
7. Restore the verified backup if post-commit validation fails.
8. Return a typed error without modifying in-memory state if the operation fails.
9. Preserve malformed input as a timestamped `.corrupt` copy before loading the verified backup or creating defaults.

Safety journals are never stored inside ordinary settings and are never deleted until the corresponding restoration or commit is verified.

## 6. Milestone overview

| Milestone | Theme | Exit result |
|---|---|---|
| Foundation | Baseline and required gates | Reproducible evidence before structural work |
| `v0.8` | Trustworthy state and system safety | No cross-account data and no unrecoverable lid mutation |
| `v0.9` | Authenticated, crash-safe updates | No unauthenticated executable can be installed |
| `v0.10` | Supported sources and diagnostics | Documented sources preferred; fallbacks and failures are visible |
| `v0.11` | Accessible, adaptive first run | Screen-reader, High Contrast, scaling, and small-screen support |
| `v0.12` | Actionable tray and alerts | Correct Codex-only mode and user-selected signals |
| `v1.0` | Signed distribution | Signed x64/ARM64 install, update, rollback, and uninstall |
| `v1.1` | Bounded local pacing | Honest answer to “will I run out before reset?” |
| `v1.2` | One gated provider | At most one provider that passes the strict source/privacy gate |

### 6.1 Rollback contract by milestone

| Milestone | Kill switch / rollback | Compatibility and proof |
|---|---|---|
| Foundation | Revert workflow-only changes only through a reviewed emergency change; publish nothing while required gates are unavailable | No user data changes; previous binary remains valid |
| `v0.8` | Persistent lid override defaults disabled until its suite passes; unresolved recovery journal blocks re-apply | Dual-read/write legacy settings for two releases; previous binary can read legacy keys; `state.json` is safe to delete, power journal is not |
| `v0.9` | Disable automatic update checks and direct users to manual signed downloads | Last-known-good executable retained until commit; every journal phase has an idempotent recovery test |
| `v0.10` | Per-provider source selector can return to the compatibility source; disable a broken adapter without disabling the provider | `state.json` remains optional and backward-compatible; deleting it loses only cache/deduplication |
| `v0.11` | Roll back to the previous release binary; do not perform irreversible data migration in UI work | New UI settings are additive and ignored safely by older binaries; demo/UI matrix proves old and new layouts from the same view model |
| `v0.12` | Reset tray/alert choices to documented defaults or roll back the binary | Settings remain additive; old binaries ignore unknown keys; alert receipt schema remains readable |
| `v1.0` | Keep the immediately previous signed installer available as an explicit rollback release authorized by signed policy | Managed installs never self-modify; install/update/uninstall smoke tests prove both architectures and both channels |
| `v1.1` | Disable history and delete its bounded file | History is optional and never required to render live quota |
| `v1.2` | Disable the new provider independently | No other provider state, icon, alert, or history changes when the adapter is disabled |

Abort a rollout immediately if any of these occurs:

- Cross-account data is rendered, cached, alerted, or logged.
- A safety/update journal cannot converge under a tested recovery path.
- An unsigned/untrusted executable reaches an execution boundary.
- A migration prevents the last released binary from starting or safely ignoring new state.
- The hard executable/memory budget is exceeded without an approved ADR.
- A critical Narrator/keyboard regression or inaccessible recovery action is discovered.

## 7. Foundation: baseline and required gates

**Goal:** make every later change measurable and prevent another release from bypassing tests.

### Tasks

- [x] **BASE-01 — Freeze the baseline.**
  - Record executable size, cold tray readiness, hidden/open memory, idle CPU, GDI handles, and network requests.
  - Define the reference Windows 11 x64 machine/VM, sample duration, commands, and acceptable variance.
  - Use at least five cold-start runs; report median and p95. Measure idle CPU/memory for ten minutes after a 60-second warm-up.
  - Repeat the baseline twice on separate runs; investigate more than 10% disagreement before accepting it.
  - Reconcile the conflicting memory and binary-size claims in `README.md`, `CLAUDE.md`, and `ARCHITECTURE.md`.

- [x] **BASE-02 — Add parser characterization fixtures.**
  - Add sanitized Claude and Codex response fixtures for normal, partial, unknown-field, malformed, missing-reset, weekly-as-primary, non-finite, and out-of-range cases.
  - No live network calls in automated tests.
  - Enforce the bounds in Section 4 and test limit, limit + 1, and truncated input.

- [x] **BASE-03 — Add state-policy characterization tests.**
  - Freeze current debounce, stale-data, 429, manual-refresh, provider-isolation, and alert behavior before refactoring it.
  - Use an injected clock; tests must not sleep.

- [x] **BASE-04 — Add deterministic demo mode.**
  - Add a test-only/demo argument that renders representative Claude-only, Codex-only, dual-provider, loading, stale, cooldown, and error states without reading credentials, touching power policy, or making network calls.
  - Make it usable for screenshots and later UI Automation tests.

- [ ] **CI-01 — Add one required source gate.**
  - `cargo fmt --all -- --check`
  - `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --locked --workspace --all-targets --all-features`
  - Release builds for supported architectures.
  - RustSec/OSV advisory check plus dependency source/license policy.
  - Binary-size comparison against the checked baseline.

- [ ] **CI-02 — Make CI reproducible.**
  - Add `rust-toolchain.toml` with an explicit supported toolchain.
  - Build with `--locked`.
  - Use minimal workflow permissions.
  - Pin every GitHub Action to a full commit SHA with a version comment.
  - Add dependency review for lockfile changes.
  - Configure branch/tag rulesets and a protected release environment so required checks cannot be bypassed by a direct tag push.
  - Enable immutable releases as soon as the repository setting is available; otherwise document the equivalent no-edit/no-tag-reuse policy.

- [ ] **DIST-00 — Resolve long-lead distribution decisions now.**
  - Record an ADR selecting installer technology and defining install channels before updater implementation.
  - Default channels: portable builds use journaled self-swap; managed installs use a signed-installer/package-manager handoff and never self-modify.
  - Select the Authenticode provider, certificate/publisher identity, key custodian, timestamp service, protected environment, and approval owner.
  - Confirm native ARM64 build/test capacity and Winget package ownership.
  - Record silent install/update/uninstall commands and the app-to-uninstaller recovery handshake.
  - Treat unavailable signing credentials, ARM64 test capacity, or package ownership as explicit blockers rather than deferring their discovery to `v1.0`.

- [ ] **REL-01 — Gate releases through the same workflow.**
  - Verify tag, `Cargo.toml`, VERSIONINFO, changelog heading, and artifact names agree.
  - Build once and test/publish that exact artifact rather than rebuilding it later; once signing lands, sign that same tested artifact.
  - Publish draft releases first and smoke-test downloaded assets before making them public.

### Foundation acceptance

- The current code is formatted and every required gate is green.
- Pull requests and tags cannot bypass the source gate.
- Installer/channel/signing decisions have named owners and concrete unblock conditions.
- Parser and state tests fail if current behavior changes unintentionally.
- Demo mode performs no external write, registry mutation, credential read, power change, or network call.
- Performance results are reproducible enough to detect a 10% regression.

## 8. `v0.8`: trustworthy state and system safety

**Goal:** repair the current correctness and recovery risks before adding capabilities.

### 8.1 Typed, atomic settings

- [x] **STORE-01 — Implement `AtomicJsonStore`.**
  - Same-directory temporary file, full write, `sync_all`, atomic replace, typed errors.
  - Fault-inject create, write, flush, replace, disk-full, access-denied, and interrupted-operation failures.

- [x] **CFG-01 — Introduce `SettingsV1`.**
  - Preserve current defaults and unversioned keys.
  - Add `schema_version` and validation.
  - Preserve unknown fields during the compatibility period.
  - A future schema is not overwritten; settings become read-only with a diagnostic explanation.
  - Avoid repeated disk reads from render paths; keep validated settings in the cached config runtime, then move ownership into `App` in `APP-01` (which depends on this task).

- [x] **CFG-02 — Use expand–migrate–contract.**
  - Continue reading legacy unversioned files indefinitely.
  - Dual-read/dual-write moved fields for at least two releases.
  - Preserve malformed files as `.corrupt`.
  - Prove downgrade compatibility before deleting legacy writes.

### 8.2 Minimal identity and runtime-state contracts

- [x] **MODEL-00 — Introduce the minimum stable identity types.**
  - Add `ProviderId`, opaque `AccountKey`, stable `LimitId`, request generation/ID, and a typed fetch-completion envelope.
  - Keep the existing display model and renderer unchanged; the full provider-domain extraction remains in `MODEL-01`.
  - Use these types immediately for account isolation and alert identity so no temporary string-key scheme or second migration is required.

- [x] **STATE-00 — Create the first version of `state.json`.**
  - Initially store the install salt and define the account-scoped alert-receipt envelope through `AtomicJsonStore`; populate/migrate receipts only after `AUTH-01/02` can derive the account key.
  - Define the forward-compatible envelope now; `CACHE-01` extends it with sanitized snapshots and retry state later.
  - Keep dual-read compatibility with legacy `settings.json.alerted` for at least two releases.
  - Deleting `state.json` remains safe and only loses cache/deduplication state.

### 8.3 Vibecode containment and transaction

- [x] **VIBE-00 — Contain the current unsafe behavior immediately.**
  - Separate wake lock from persistent lid-policy override.
  - Ship this as emergency PR 0: disable new persistent lid-policy mutations until the journaled implementation passes its fault-injection suite.
  - Preserve any legacy recovery values and show a recovery notice; never clear or guess them.
  - Do not report the lid override as active unless persistence, both writes, activation if needed, and read-back verification succeeded.
  - Put the persistent lid override behind an explicit Advanced explanation until the transaction work is complete.

- [x] **VIBE-01 — Add `power-override.v1.json`.**
  - Store schema, exact power-scheme GUID, original AC/DC values, applied AC/DC values, phase, timestamp, and app version.
  - Phases: `prepared`, `applied`, and `restoring`.
  - Persist `prepared` before the first OS mutation.

- [x] **VIBE-02 — Implement an injectable power transaction.**
  - Read active scheme and originals.
  - Persist `prepared`; if it fails, perform zero power writes.
  - Recheck that the active scheme did not change.
  - Write AC and DC to that exact GUID; check every return value.
  - Apply only when that scheme is still active.
  - Read back and verify before recording the persistent override as `applied`.
  - Keep the wake lock independently operable; a lid-override failure must not falsely report that the wake lock failed or vice versa.
  - Roll back partial failures immediately and retain the journal unless rollback is verified.

- [x] **VIBE-03 — Make restoration conservative and idempotent.**
  - Restore the journal’s GUID, never an arbitrary current scheme.
  - Restore a field only if it still equals Claudometer’s applied value, so an external user change is not overwritten.
  - Record each field as `restored` or `relinquished_external_change`; either is a resolved terminal outcome.
  - Never force an inactive old scheme to become active.
  - Delete the journal only after every field is verified restored or explicitly relinquished because of an external change.
  - Drop the wake lock even if persistent recovery remains outstanding.

- [x] **VIBE-04 — Recover across lifecycle failures.**
  - On startup, recover any existing journal before re-arming the current scheme.
  - If recovery fails, do not apply a new override; show an actionable recovery state.
  - Handle `WM_QUERYENDSESSION`, `WM_ENDSESSION`, and `WM_DESTROY` idempotently.
  - Reconcile an active-scheme change by restoring the old transaction before applying a new one.
  - Add `--recover-vibecode` for deterministic support and uninstall use.

- [x] **VIBE-05 — Handle the legacy migration honestly.**
  - Legacy `vibecode_lid` has no scheme GUID; exact automatic recovery is impossible.
  - Disable the persistent override during migration, preserve the legacy pair, and offer “Restore these values to the current scheme.”
  - Never silently assume the current scheme owns the legacy values and never silently discard them.

### 8.4 Account and asynchronous-result isolation

- [x] **AUTH-01 — Add an opaque `AccountKey`.**
  - Prefer a stable provider-owned account identifier; salt/hash it with Windows CNG.
  - Fall back to an in-memory access-token fingerprint only when no stable identity exists.
  - Persist no token, email, organization name, username, home path, or raw account ID.

- [x] **AUTH-02 — Make all provider state account-bound.**
  - Plan cache, current result, last-good result, error, cooldown, debounce, alerts, and later history all carry the account key.
  - A credential/account change immediately increments generation and clears data from the previous identity.
  - An old worker result is discarded even when it finishes successfully after a switch.
  - Login completion invalidates state before starting the replacement fetch.

- [x] **AUTH-03 — Make credential parsing tolerant but explicit.**
  - Do not require `expiresAt` when it is not authoritative.
  - Give Codex credential reads the same atomic-replacement retry discipline as Claude.
  - Distinguish missing, temporarily unreadable, malformed, and unsupported credential shapes.

### 8.5 Correctness fixes

- [x] **ALERT-01 — Deliver provider-specific fresh events.**
  - Include provider/request identity in completion events.
  - Evaluate alerts only for the newly accepted successful result.
  - A completion for one provider can never reclassify another provider’s stored result as fresh.

- [x] **ALERT-02 — Fix missing-reset deduplication.**
  - With no reset timestamp, re-arm only after an observed below-threshold state; do not invent a reset boundary from wall-clock time.
  - Scope receipts by account, provider, stable limit ID, threshold, and reset instance.

- [x] **CAPS-01 — Replace the Caps LED boolean with a real state.**
  - States: unavailable, installed-disabled, installed-enabled, and error.
  - Check that the script and relevant hook configuration exist.
  - Honor `CLAUDE_CONFIG_DIR` consistently.
  - Return and display write/launch errors.
  - Never show enabled on a clean installation.

- [x] **TIME-01 — Format reset time at the target instant.**
  - Use a timezone-aware Windows conversion for the reset timestamp.
  - Test weekly resets crossing both DST boundaries.

- [x] **POLL-01 — Define one freshness rule.**
  - Opening the flyout does not bypass the chosen refresh interval after 15 seconds.
  - Manual refresh during cooldown shows its next eligible time.
  - Non-429 results reset the consecutive 429 streak.
  - No immediate source fallback after a 429 or authentication failure.

### 8.6 Accurate privacy contract

- [x] **PRIV-01 — Add `PRIVACY.md`.**
  - Network table: destination, trigger, transmitted data, frequency, and control.
  - Local file table: reads, writes, retention, and deletion behavior.
  - Registry keys, child processes, toast registration, update files, power changes, and Caps marker.
  - State explicitly that bearer tokens are held in memory and sent only to the corresponding provider.
  - State explicitly that no telemetry, analytics, response logging, or crash upload exists.

- [x] **PRIV-02 — Add update-check control and enforcement.**
  - Preserve automatic checks for migrated existing installations, but default them off for genuinely new installs until the Welcome flow can disclose and offer the choice.
  - Provide the control in Settings immediately; `ONBOARD-01` later presents the same choice during first run rather than creating a second onboarding flow.
  - Centralize network destination constants.
  - Add a CI allowlist test so a new URL/request site requires a privacy update.

### `v0.8` acceptance

- Switching or signing out clears visible old-account data before the next fetch.
- A failed first fetch after a switch cannot restore the previous account’s snapshot, plan, alert receipt, or cooldown.
- Every injected Vibecode failure leaves verified original values or a durable actionable journal.
- No Vibecode failure reports success after partial application.
- Corrupt/interrupted settings preserve the previous valid file and produce a visible diagnostic.
- Missing-reset alerts can re-arm safely without duplicate spam.
- Network/file/registry/system behavior in documentation matches an instrumented clean-VM run.
- No new provider, dashboard, or visual redesign lands in this milestone.

## 9. `v0.9`: authenticated, crash-safe updates

**Goal:** ensure Claudometer cannot execute an unauthenticated release and can recover from interruption at every mutation boundary.

### 9.1 Immediate containment

- [ ] **UPD-00 — Fail closed on current release metadata.**
  - Implement the channel contract from `DIST-00`: portable may self-swap; a managed install must hand off to its signed installer/package manager and must never rename its managed executable in place.
  - Require the checksum asset rather than treating it as optional.
  - Build fixed repository/tag/asset URLs and reject unexpected schemes or hosts.
  - Restrict redirects to documented GitHub release hosts.
  - Hash in-process through Windows CNG; remove PATH-resolved `certutil`.
  - Read at most `MAX + 1` bytes and reject oversize downloads rather than silently truncating them.
  - Do not automatically open a browser after failure; show an explicit release-page action.

### 9.2 Signed manifest

- [ ] **UPD-01 — Define and verify a signed release manifest.**
  - Fields: schema, channel, monotonically increasing release sequence, version, tag, issued-at, policy expiry, architecture, exact asset name, exact size, SHA-256, and minimum updater version.
  - Sign the exact manifest bytes with an offline/protected Ed25519 key.
  - Embed only the release public key in the application.
  - Use a maintained verifier; no handwritten cryptography.
  - Support key rotation only through a manifest cross-signed by an already trusted key.
  - Accept only a newer sequence/version on the selected channel. A downgrade requires an explicit separately signed rollback authorization naming the exact target and expiry.
  - Reject replayed/older manifests, missing or malformed timestamps, expired policy, wrong channel, architecture, version, host, key, size, or hash before filesystem mutation.

- [ ] **UPD-02 — Record the bootstrap decision.**
  - The current updater cannot securely establish a new embedded trust root.
  - Existing users must manually install and verify the first trust-root release, or explicitly accept a documented one-time bootstrap.
  - This limitation must be visible in release notes; do not pretend code can retroactively authenticate the old channel.

### 9.3 Crash-safe handover

- [ ] **UPD-03 — Add `update-operation.v1.json`.**
  - Phases: `verified`, `current_moved`, `candidate_installed`, `candidate_ready`, and `committed`.
  - Use unique candidate/backup names per attempt.
  - Never delete the last verified executable before commit.

- [ ] **UPD-04 — Add readiness and rollback.**
  - Verify before swap and again at the canonical path.
  - On spawn failure, reverse the rename and restart the old binary.
  - The old binary/watchdog creates a unique attempt nonce and waits for readiness bound to that nonce, expected candidate PID, version, and hash.
  - Ignore stale/spoofed readiness events and require readiness only after normal non-destructive initialization.
  - Defer irreversible configuration/system migrations until update commit; candidate startup before commit may perform only backward-compatible reads/writes.
  - Candidate crash or readiness timeout automatically restores the last-known-good executable.
  - Delete backups only after readiness and journal commit, preferably on the following healthy launch.
  - Startup recovery is idempotent for every journal phase.

### 9.4 Release supply chain

- [ ] **REL-02 — Produce verifiable release evidence.**
  - Build once with `--locked` after all required checks.
  - Generate the manifest and checksum from that exact artifact.
  - Generate an SBOM and GitHub artifact attestation.
  - Upload to a draft release and smoke-test the downloaded assets.
  - Publish immutable releases; never edit/reuse an existing version or tag.
  - Protect `main`, release tags, and the signing environment.

### `v0.9` acceptance

- The updater cannot execute a release without a valid embedded-trust-root signature.
- Replayed manifests, unsigned downgrades, stale readiness events, and wrong-channel assets are rejected.
- Crash/failure injection after every write, rename, spawn, readiness, and cleanup boundary converges to one verified launchable version.
- At least one last-known-good executable remains until commit.
- Repeated startup recovery is idempotent.
- No release publishes without source gates, version consistency, manifest verification, smoke tests, SBOM, and provenance.

## 10. `v0.10`: supported sources and diagnostics

**Goal:** prefer documented provider-owned data surfaces, disclose compatibility fallbacks, and make failures supportable.

### 10.1 Normalize state before changing sources

- [ ] **MODEL-01 — Extract the normalized domain model.**
  - Expand the minimal identities from `MODEL-00` into the complete normalized model and move shared types out of `api.rs` without changing behavior.
  - Convert parsers, alerts, tray, and renderer to typed IDs/classes.
  - Keep rendering and network behavior unchanged in this pull request.

- [ ] **STATE-01 — Implement the pure provider reducer.**
  - Inject the clock.
  - Reproduce and test debounce, stale expiry, cooldown, account invalidation, provider disable, and obsolete completion.
  - Derive loading, fresh, refreshing-with-data, cached, stale-with-error, cooldown, unavailable, and failed views.

- [ ] **APP-01 — Move provider state to the UI thread.**
  - Introduce the event queue.
  - Remove the current mutex/atomic `SLOTS` cluster only after behavior-parity tests pass.
  - Keep Win32 message procedures thin and preserve current window behavior.

- [ ] **CACHE-01 — Add sanitized `state.json`.**
  - Extend the `STATE-00` envelope with bounded normalized snapshots, retry deadline, and source choice; retain its alert receipts and install salt.
  - Load a snapshot only after its account key matches current local identity.
  - Label restored values `cached`; never present them as freshly fetched.
  - Bound provider count, rows, strings, timestamps, and age.

### 10.2 Source provenance

- [ ] **SRC-00 — Add source/freshness metadata everywhere.**
  - Compact states: `Documented · fresh`, `Documented · cached`, `Compatibility · fresh`, `Compatibility · cached`, and `Unavailable`.
  - Show compact source/age in each provider section and full detail in diagnostics.
  - Do not change source priority until provenance is visible and tested.

### 10.3 Codex documented source

- [ ] **CODEX-01 — Add Codex app-server support.**
  - Prefer documented `codex app-server` RPC `account/rateLimits/read`.
  - Request account state without forcing token refresh.
  - Parse dynamic `rateLimitsByLimitId`, plan, reset metadata, spend-control state, and reset-credit availability.
  - Reset credits are display-only; Claudometer never consumes them.
  - Apply a strict startup/request deadline and terminate the child/process tree afterward.
  - Retain `wham/usage` only as a separately labeled compatibility fallback.

- [ ] **CODEX-02 — Gate the default source on measured behavior.**
  - Documented-source success causes no direct ChatGPT request in that cycle.
  - Default eligibility requires at most 2 seconds p95 on the reference VM and no leaked child.
  - If it misses that budget, keep it opt-in until a persistent process can be justified without breaking the product contract.

### 10.4 Claude documented local signal

- [ ] **CLAUDE-01 — Add an opt-in statusline bridge.**
  - `claudometer.exe --claude-statusline` reads statusline JSON on stdin.
  - Store only normalized rate-limit fields; never store transcript path, session name, cwd, prompt, or cost payload.
  - Print one useful one-line statusline and return within the performance budget.
  - Throttle unchanged writes to at most once per 30 seconds.
  - Deliver a fresh sample to the running app within two seconds.
  - Tag each sample with the locally derived credential revision/account key and current app generation; the running app accepts only an exact current match.
  - Quarantine samples while login/account transition is in progress and test an old Claude session emitting after sign-out or account switch.
  - If the documented payload cannot be proven to belong to the current account, keep it as statusline/diagnostic output and do not use it for tray, cache, history, or alerts.

- [ ] **CLAUDE-02 — Preserve user configuration.**
  - Offer one-click installation only when no existing `statusLine` exists.
  - Never overwrite or attempt to chain an arbitrary existing command.
  - Remove the integration only when the setting still exactly matches Claudometer’s owned value.
  - If a recent bridge sample does not exist, use the OAuth endpoint only when compatibility fallback is enabled.

### 10.5 Diagnostics

- [ ] **DIAG-01 — Add stable operational diagnostics.**
  - Version, architecture, Windows build, and install channel.
  - Provider detected/authenticated state.
  - Selected source and fallback reason.
  - Last attempt, success, freshness, cooldown, and next retry.
  - Stable error category and code; never raw response body.
  - Update/recovery state and relevant non-secret settings.

- [ ] **DIAG-02 — Add bounded local logging and support commands.**
  - Rotating redacted log bounded to the 3 × 256 KiB limit in Section 4.
  - `--diagnose`, `--version`, and “Copy diagnostics.”
  - Redact bearer/refresh tokens, raw account IDs, email, username, home path, account hash, and response body.
  - Rendering/config/registry failures must become visible diagnostics rather than silent no-ops.

### `v0.10` acceptance

- Codex app-server success performs no direct compatibility request and leaves no process behind.
- Existing Claude statusline configuration remains byte-for-byte unchanged.
- Source adapters have fixture coverage for null/missing/extra/malformed data, timeouts, and fallback decisions.
- No two sources poll one provider unnecessarily in the same cycle.
- Compatibility failures retain honest same-account cached data with source and freshness labels.
- A redaction corpus proves diagnostics contain no token, account identifier, PII, home path, or response body.
- Deleting `state.json` or diagnostics affects only cached display/support information.

## 11. `v0.11`: accessible, adaptive first run

**Goal:** make every surface understandable and operable with keyboard, Narrator, High Contrast, text scaling, and small screens.

### 11.1 Accessibility

- [ ] **A11Y-01 — Expose a UI Automation fragment tree through `WM_GETOBJECT`.**
  - Buttons implement Invoke.
  - Switches implement Toggle.
  - Interval and metric choices expose Selection.
  - Quota bars expose read-only RangeValue.
  - Accessible names include provider, window, used/remaining value, reset, source, and freshness.
  - Dynamic changes raise targeted property events; the ticking age footer must not cause repeated announcements.

- [ ] **A11Y-02 — Add non-color semantics.**
  - Warning, critical, stale, unavailable, and source state are available in text/UIA, not color alone.
  - Focus is always visible.
  - Contrast is computed for text placed on the Windows accent color.

### 11.2 Adaptive layout and rendering

- [ ] **LAYOUT-01 — Bound all windows to the monitor work area.**
  - Add scrolling to Settings and long flyouts.
  - Keep all controls reachable at 1280×720 and 1366×768 from 100% through 225% scaling.
  - Support DPI changes while a window is open.
  - Preserve keyboard focus across polling, resizing, and rerendering.

- [ ] **LAYOUT-02 — Honor text scaling and High Contrast.**
  - Derive text metrics and layout from Windows text-scale settings.
  - Use system High Contrast colors and disable acrylic when required.
  - Validate High Contrast Black and White.

- [ ] **RENDER-01 — Recover from device/render failure.**
  - Classify D2D/DXGI recreate-target/device-loss failures.
  - Drop and recreate a broken surface once.
  - If recreation fails, show a minimal native diagnostic surface/action rather than a blank resident process.
  - Log only the safe HRESULT/category.

### 11.3 First run and single instance

- [ ] **ONBOARD-01 — Add one compact Welcome page.**
  - Explain what Claudometer monitors and touches.
  - Show Claude/Codex as Ready, Sign in required, CLI unavailable, or Unsupported.
  - Explain documented versus compatibility sources.
  - Offer the Claude bridge only under its safe installation rules.
  - Do not enable autostart, history, or provider-setting changes without direct user action.
  - One Done action; “Run setup again” remains available.

- [ ] **ONBOARD-02 — Make second launch useful.**
  - Signal the existing instance to open its flyout or Settings instead of exiting silently.
  - First launch explains tray overflow/pinning without requiring a second failed launch.

- [ ] **UI-TEST-01 — Add deterministic UI and accessibility proof.**
  - Demo-state screenshots for no provider, Claude only, Codex only, both, stale, cooldown, error, and long dynamic rows.
  - UI Automation smoke tests for discoverability, names, roles, values, focus order, and invocation.
  - Accessibility Insights FastPass as a release checklist item.

### `v0.11` acceptance

- Narrator can discover, read, and operate every control without a mouse.
- A quota row announces provider, window, value, reset, source, and freshness.
- Tab order matches visual order; Space/Enter/arrows/Escape behave consistently.
- Refreshing never resets focus.
- Every control remains reachable at the required resolutions/scales through reflow or scrolling.
- High Contrast and text scaling remain readable and preserve state distinctions.
- Device loss cannot leave a permanently blank flyout/settings window.
- Welcome performs no browser launch, integration write, or system mutation without a direct action.

## 12. `v0.12`: actionable tray and alerts

**Goal:** make the one glance answer the question the user actually cares about.

### Tasks

- [ ] **TRAY-01 — Support correct provider selection.**
  - Modes: `Auto: highest used visible quota` and any currently available provider/window.
  - Claude-only, Codex-only, both, and neither all behave correctly.
  - An unavailable explicit selection temporarily falls back to Auto but retains the preference.
  - Auto tie-breaking is deterministic.

- [ ] **TRAY-02 — Add used versus remaining display.**
  - The display can invert, but severity and alerts always use normalized used percentage.
  - Stale values retain the ring only with explicit stale text in tooltip/UIA.
  - The alert icon appears only when no selected/fallback value is usable.

- [ ] **ALERT-03 — Add constrained alert controls.**
  - One threshold: Off, 50%, 75%, or 90%; default 75%.
  - Optional reset notification.
  - Windows Do Not Disturb remains the quiet-hours mechanism.
  - No custom scheduler, sound system, webhook, or multi-rule engine.

- [ ] **ALERT-04 — Add visible testing and timing.**
  - “Send test notification” with visible success/failure.
  - Show cooldown and next retry rather than silently ignoring refresh.
  - Reset alerts require an observed previous window; first observation is never a reset.

- [ ] **TRAY-03 — Make tooltips deterministic and bounded.**
  - Prioritize selected metric, error/freshness, then other providers.
  - Respect Windows tooltip limits without cutting the most actionable information.
  - Include source/freshness only in compact form.

### `v0.12` acceptance

- Deterministic tests cover Claude-only, Codex-only, both, neither, provider error, disabled provider, disappearing window, and explicit fallback.
- Metric and used/remaining changes update immediately and survive restart.
- Threshold boundary, reset drift, missing reset, account switch, stale result, and restart deduplication are tested.
- Reset notifications fire once and never from stale or first-observed data.
- Test-notification failure produces a useful diagnostic.
- No extra tray icon or resident process is introduced.

## 13. `v1.0`: signed distribution

**Goal:** make install, update, rollback, and removal verifiable and uneventful.

### Tasks

- [ ] **SIGN-01 — Authenticode-sign every Windows artifact.**
  - Sign x64/ARM64 executables and installers through a protected, approval-gated signing environment.
  - Timestamp signatures.
  - Verify with `WinVerifyTrust` in release smoke tests and before update application.
  - Pin the expected publisher/signing identity in updater policy.
  - Manifest signature remains the updater’s independent artifact-integrity trust root.
  - Authenticode-sign first, then hash the immutable signed bytes and sign the release manifest; never mutate an artifact after its manifest is produced.

- [ ] **DIST-01 — Produce native x64 and ARM64 artifacts.**
  - Portable signed executable for each architecture.
  - Signed per-user installer requiring no administrator privileges.
  - Architecture-aware manifest and update selection.

- [ ] **DIST-02 — Publish through Winget.**
  - Proposed ID: `Dvaderfun.Claudometer`.
  - Verify clean install, launch, upgrade, repair, and uninstall for x64 and ARM64.
  - Keep installed and portable update paths distinct so package state cannot drift from an in-place swap.

- [ ] **DIST-03 — Make uninstall system-safe.**
  - Ask the running app to restore/recover Vibecode and exit before removal.
  - Remove binaries, updater debris, Run entry, AUMID registration, toast icon, and Claudometer-owned bridge configuration.
  - Never remove unrelated Claude/Codex configuration.
  - Offer an explicit retain/remove choice for ordinary settings and optional history.
  - If a safety journal cannot be resolved, stop uninstall and show the recovery action.

- [ ] **DOCS-01 — Complete the public product surface.**
  - `README.md`: screenshot/GIF, accurate footprint, quick start, source/freshness explanation, uninstall, verification, and troubleshooting.
  - `SECURITY.md`: supported versions, private-reporting route, credential/update threat model.
  - `PRIVACY.md`: authoritative data/system-effects inventory.
  - `CONTRIBUTING.md`: setup, tests, architecture, fixture/redaction rules.
  - Release checklist, source compatibility matrix, and recovery guide.
  - GitHub topics, homepage, social preview, issue/PR templates, and support links.

### `v1.0` acceptance

- `Get-AuthenticodeSignature` and `WinVerifyTrust` report the expected valid publisher for every executable and installer.
- The updater rejects unsigned, wrong-publisher, wrong-manifest, wrong-host, wrong-version, or wrong-architecture artifacts.
- Winget install/upgrade/uninstall passes on clean Windows 11 x64 and ARM64 environments.
- Uninstall removes all selected Claudometer-owned state and restores every recoverable system effect.
- Tag, Cargo version, VERSIONINFO, installer, manifest, and artifact names match.
- Release jobs cannot bypass source, security, architecture, size, signature, provenance, and smoke-test gates.
- The installed runtime still satisfies the performance budgets.
- No critical accessibility or recovery defect remains open.

## 14. `v1.1`: bounded local pacing

**Goal:** answer exactly one new question: “At my current pace, will I hit this quota before it resets?”

### Storage

- [ ] **HIST-01 — Add opt-in normalized sample history.**
  - Fresh accepted samples only.
  - Partition by account, provider, stable limit ID, and reset instance.
  - At most one point per five minutes unless value/reset identity changes.
  - Eight-day retention and hard 1 MiB cap.
  - Retain at most 32 current-account limit identities and compact each to at most 96 representative points.
  - Evict oldest reset instances first; never evict or merge data across account identities.
  - Atomic compaction and corruption recovery.
  - Enable, disable, and clear actions; no history file while disabled.

### Forecast

- [ ] **PACE-01 — Add conservative exhaustion projection.**
  - Analyze only the current reset instance.
  - Ignore stale points, decreases, reset transitions, and implausible samples.
  - Downsample to at most 96 points before fitting; use bounded Theil–Sen or an ADR-approved linear-time robust fit.
  - Require at least four points spanning 20 minutes and at least 1% change.
  - Show only `Need more data`, `On pace for reset`, or `May reach 100% around …, before reset`.
  - Always say “at current pace”; never present a forecast as a guarantee.

### `v1.1` acceptance

- No history crosses account identities.
- Clearing history removes it atomically.
- A simulated year of 30-second polling remains under 1 MiB.
- Tests cover steady, bursty, flat, decreasing, reset, DST, stale, corrupt, and insufficient-evidence cases.
- Exhaustion time appears only when it is earlier than reset.
- The flyout adds at most one compact pace line per qualifying row; there is no analytics window.
- Compared with history disabled, history adds at most 1 MiB private memory, 0.01 CPU percentage point while idle, and remains inside the executable budget.
- Worst-case tests cover 32 limits × 96 points, repeated account switches, and windows longer than the eight-day retention horizon; pacing is unavailable when retained data cannot support the window.

### Explicit `v1.1` non-goals

- Token or dollar accounting.
- Per-project/session/model attribution.
- Charts, exports, reports, cloud backup, or indefinite retention.
- Machine-learning predictions.
- Reimplementation of `ccusage`.

## 15. `v1.2`: one gated provider

**Goal:** add at most one provider only if it strengthens the product without weakening its trust model.

### Qualification gate

A candidate must have all of the following:

- A documented machine-readable quota API or official local CLI IPC.
- Provider-reported used amount and reset identity/time.
- Reuse of the provider’s existing login.
- No copied, persisted, refreshed, or user-entered provider secret.
- No browser scraping, cookie extraction, or terminal-output scraping.
- No WebView or resident helper.
- Clean mapping to the normalized source/account/freshness model.
- Legal, sanitized contract fixtures that can live in the repository.
- Independent failure behavior and compliance with the executable/runtime budgets.

Gemini/Antigravity and Copilot are candidates only after their documented surfaces satisfy this gate. If nobody passes, `v1.2` ships reliability and polish instead of a provider.

### `v1.2` acceptance

- An ADR records the qualification evidence before implementation.
- New-provider-only, new+Claude, new+Codex, and all-three states work independently.
- One provider’s failure cannot change another provider’s data, alerts, icon, cooldown, or history.
- The adapter passes the same source, account-isolation, redaction, timeout, fixture, and accessibility contracts.
- Supported providers remain capped at three throughout `v1.x`.

## 16. Test strategy

### 16.1 Unit and property tests

- Provider parsers: complete, partial, unknown, malformed, oversized, and hostile-but-valid payloads.
- `Percent`, time conversion, source selection, stable IDs, tooltip truncation, and view derivation.
- Reducer sequences with fake time and arbitrary event order.
- Configuration validation and every schema migration.
- Alert crossing, reset, drift, missing reset, account switch, and restart behavior.
- Fuzz parsers, manifest parsing, Base64/JWT hint parsing, and string/row limits for no-panics and bounded allocation.

### 16.2 Fault-injection tests

- Filesystem: create/write/flush/replace/delete failure and simulated interruption.
- Vibecode: fail/crash after every journal and power-API transition.
- Updater: fail/crash after every download, verification, journal, rename, spawn, readiness, and cleanup transition.
- Child process: missing CLI, bad protocol, malformed JSON-RPC, timeout, hung descendant, and forced termination.
- Renderer: surface creation, resize, EndDraw, Present, and device-loss failure.

### 16.3 Contract and integration tests

- No automated test calls a live provider endpoint.
- Use sanitized committed fixtures and fake HTTP/app-server processes.
- Verify fallback policy without contacting the fallback source unnecessarily.
- Verify diagnostics and persisted state contain none of the redaction corpus.
- Add an explicit manual live-source smoke command for maintainers, disabled in CI.

### 16.4 Windows UI matrix

- Windows 11 supported builds, x64 and ARM64.
- 1280×720 and 1366×768 minimum work areas.
- 100%, 125%, 150%, 200%, and 225% display/text scaling.
- Light, dark, High Contrast Black, and High Contrast White.
- Primary/secondary monitors, different DPI per monitor, taskbar edges, Explorer restart.
- Keyboard-only and Narrator workflows.
- Notifications enabled/disabled, Do Not Disturb, and toast activation.

### 16.5 Release evidence

Every release records:

- Source commit and toolchain.
- Test/lint/format/security results.
- Executable and installer hashes, signatures, SBOM, and provenance.
- Executable size, startup, hidden/open memory, idle CPU, and GDI handles.
- UI Automation/accessibility result.
- Clean install, update, rollback, and uninstall result.

## 17. Pull-request sequence

This is the preferred implementation order. Do not combine adjacent rows merely because they touch similar files.

| PR | Scope | Depends on |
|---:|---|---|
| 0 | Emergency `VIBE-00` containment: disable new persistent lid mutations and preserve recovery data | — |
| 1 | Format baseline, unified source gate, and repository rulesets | 0 |
| 2 | Parser/state fixtures and injected clock | 1 |
| 3 | Deterministic no-side-effect demo mode and performance baseline | 1 |
| 4 | Distribution/channel/signing ADR and long-lead procurement | 1 |
| 5 | `AtomicJsonStore`, verified backup generation, and failure injection | 2 |
| 6 | Typed settings and compatibility migration | 5 |
| 7 | Minimal provider/account/limit identities and initial `state.json` envelope | 5–6 |
| 8 | Vibecode journal, recovery, and lifecycle handling | 0, 5–6 |
| 9 | Account generation isolation and plan-cache fix | 2, 6–7 |
| 10 | Fresh-event alerts, missing-reset handling, Caps state, DST, and poll policy | 2, 6–9 |
| 11 | Accurate privacy inventory, update preference, and network allowlist | 6–10 |
| 12 | Channel-aware updater containment and in-process hash | 4–5 |
| 13 | Signed manifest, replay/downgrade policy, and key handling | 4, 12 |
| 14 | Update journal, nonce-bound readiness, rollback, and startup recovery | 5, 13 |
| 15 | Complete normalized provider model | 2, 7, 9 |
| 16 | Pure provider reducer and sanitized runtime-cache extension | 5–7, 15 |
| 17 | UI-owned `App` state and worker event queue | 16 |
| 18 | Source provenance and redacted diagnostics | 11, 15–17 |
| 19 | Codex app-server adapter | 18 |
| 20 | Optional account-correlated Claude statusline bridge | 18 |
| 21 | Render recovery and bounded adaptive layout | 3, 17 |
| 22 | UI Automation, High Contrast, and text scaling | 21 |
| 23 | Welcome and useful second-launch behavior | 18, 22 |
| 24 | Tray metric, used/remaining, alert controls, and retry countdown | 16–23 |
| 25 | Authenticode, x64/ARM64 installer, Winget, and uninstall recovery | 4, 8, 14, 23–24 |
| 26 | Public docs, release evidence, and `v1.0` hardening | all `v1.0` work |
| 27 | Opt-in bounded history and conservative pacing | `v1.0` |
| 28 | One-provider qualification ADR and implementation, or reliability work | `v1.1` |

## 18. Decisions to record before implementation

Use these defaults unless an ADR accepts a different tradeoff.

| ADR | Default decision |
|---|---|
| Update trust | Embedded Ed25519 manifest trust root plus Authenticode before `v1.0` |
| Existing-user trust bootstrap | Manual verification/install of the first trust-root release |
| Installer | Signed per-user native installer plus portable executables and Winget |
| Update channels | Portable uses journaled self-swap; managed install uses signed-installer/package-manager handoff and never self-modifies |
| Runtime concurrency | UI-owned state + short-lived std threads + message wakeup; no Tokio |
| Claude documented source | Explicit opt-in statusline bridge only when no existing statusline is configured |
| Codex documented source | Deadline-bound app-server first; compatibility endpoint fallback |
| Statusline coexistence | Never overwrite, execute, or chain arbitrary existing statusline commands; unprovable account binding makes a sample display-ineligible |
| History storage | Bounded native JSON sample store; no SQLite through `v1.x` |
| Vibecode | Wake lock is separate; persistent lid override is Advanced and journaled |
| OS scope | Windows 11 x64/ARM64 through `v1.x`; Windows 10 only after explicit demand/testing |
| Provider scope | Claude + Codex through `v1.0`; at most one additional provider in `v1.x` |

## 19. Explicit non-goals through `v1.x`

- Cross-platform UI rewrite.
- Electron, Tauri, WebView, WinUI migration, or managed runtime.
- Runtime provider plugin SDK or provider marketplace.
- Dozens of providers.
- Browser cookie extraction or web-page scraping.
- Owning provider login/token refresh.
- User-entered API keys.
- Consuming Codex reset credits or buying provider credits.
- Multiple simultaneously active accounts per provider.
- Full token/cost analytics, project dashboards, or transcript indexing.
- Cloud sync, remote dashboard, team administration, email, webhook, or mobile alerts.
- Internal quiet-hours scheduler that duplicates Windows Do Not Disturb.
- Arbitrary theme, font, color, or alert-rule editors.
- More power-management features.

## 20. Post-`v1.x` candidates

Consider these only after the v1 product contract and performance budgets are demonstrated in real releases:

- Multiple account profiles with strict partitioning and explicit switching.
- Optional `ccusage --json` interoperability instead of duplicating its analytics engine.
- A stable machine-readable `claudometer --json` snapshot for statuslines and scripts.
- Windows 10 fallback visuals.
- A fourth provider only if the provider cap is deliberately revisited in an ADR.

## 21. Definition of done for `v1.0`

`v1.0` is complete when all of the following are true:

- No known P0/P1 correctness, safety, security, accessibility, install, update, or uninstall issue remains open.
- Old-account data cannot appear after an identity change, including late worker completion and failed first fetch.
- Vibecode failure always ends in verified original state or a durable visible recovery state.
- No unauthenticated executable can be installed by the updater.
- Update interruption at every mutation boundary recovers automatically.
- Claude/Codex documented sources are preferred where eligible; compatibility paths are labeled and controllable.
- Every failure has a stable redacted diagnostic without telemetry or secret leakage.
- Narrator, keyboard, High Contrast, text scaling, DPI changes, and minimum work areas pass the UI matrix.
- Claude-only, Codex-only, both, and neither are complete experiences.
- Signed x64/ARM64 portable and per-user installations can install, update, roll back, and uninstall cleanly.
- All release evidence is published and performance budgets remain satisfied.
- Public documentation accurately describes every network request and persistent/system side effect.

## 22. Reference implementations and primary contracts

Borrow ideas, not scope:

- [CodexBar](https://github.com/steipete/CodexBar): source provenance, provider contracts, diagnostics, and documentation structure.
- [Win-CodexBar](https://github.com/nesszer/Win-CodexBar): Windows installation, Winget, localization, and support diagnostics; do not copy its WebView/provider breadth.
- [WhereMyTokens](https://github.com/jeongwookie/WhereMyTokens): account-bound normalized state, Claude statusline-first behavior, and privacy-safe fallback descriptions.
- [RateTray](https://github.com/nowrap/rate-tray): Codex app-server integration, explicit thresholds, persistent last-good data, and publishing/security documentation.
- [ccusage](https://github.com/ccusage/ccusage): optional future JSON interoperability and bounded analytics ideas.
- [Claude Code statusline contract](https://code.claude.com/docs/en/statusline): documented Claude rate-limit fields.
- [Codex app-server contract](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md): documented Codex account/rate-limit RPC.
- [Microsoft WinVerifyTrust](https://learn.microsoft.com/en-us/windows/win32/api/wintrust/nf-wintrust-winverifytrust): Authenticode verification.
- [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations): release provenance.

The stopping rule is simple: if a proposed feature does not improve trust, reliability, accessibility, actionability, or distribution while staying inside the native footprint contract, it is not part of this roadmap.
