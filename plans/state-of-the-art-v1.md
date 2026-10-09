# Claudometer: state-of-the-art roadmap

**Revision:** 2 (2026-10-08). Revision 1 (2026-09-03, baseline `v0.7.3` / `5b735ce`) is in git history.
**Executor:** an autonomous coding agent working task by task. Read §0 before touching code.
**Primary target:** a trustworthy, accessible, glanceable, signed `v1.0` for Windows 11 x64 and ARM64.

---

## 0. Executor brief (read first, every session)

### 0.1 What to read before the first task

1. `CLAUDE.md` — working knowledge. Its “Hard-won gotchas” section is **binding**; treat each bullet as a test you must not break.
2. `ARCHITECTURE.md` — current design.
3. `PRIVACY.md` — the authoritative inventory of every network destination, file, registry key, child process, and system change. Any change to those must update it in the same commit.
4. `docs/adr/0001-distribution-signing-and-channels.md` — distribution decisions.
5. This plan: §2 (status board) tells you what to do next; the task's own section tells you how.

When the plan and the code disagree about a **fact** (a file name, an existing behavior), the code wins: adapt and note it in your report. When they disagree about an **invariant** (§1.2), the plan wins: stop and report.

### 0.2 Environment

- Windows 11, PowerShell 7 (`pwsh`). Rust `1.97.1` pinned by `rust-toolchain.toml`, MSVC toolchain, `windows` crate pinned to `0.58`.
- A running Claudometer locks the executable. Before every build: `Stop-Process -Name claudometer -Force -ErrorAction SilentlyContinue`.
- Run each gate command on its own and read its full output before running the next one.

### 0.3 Required gates (run in this order for every task)

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo build --locked --release --target x86_64-pc-windows-msvc
```

Then, for any task that touches UI or runtime behavior:

```powershell
./ci/verify-demo.ps1 -ExePath .\target\x86_64-pc-windows-msvc\release\claudometer.exe   # demo mode performs no side effects
.\target\x86_64-pc-windows-msvc\release\claudometer.exe --demo both   # also: claude-only, codex-only, neither, loading, stale, cooldown, error, many, settings
```

Demo flags: `--demo <scenario>`, `--demo-light`, `--demo-contrast`, `--demo-hidden`, `--demo-ready-event=<name>`.
Drive the flyout without clicking: see “Verify changes” in `CLAUDE.md`.
For any task that changes code size, record x64 bytes of the release build in `docs/performance/artifact-ledger.md` (§3.1).

### 0.4 Per-task loop

1. Pick the first unchecked task in the §2.3 queue whose dependencies (§15) are all done.
2. Read the files the task names. Use `codegraph explore "<symbols>"` when `.codegraph/` exists; otherwise search.
3. If the task changes existing behavior, first add a characterization test that pins today's behavior, then change it deliberately.
4. Implement the smallest change that meets the task's acceptance list. Match the surrounding code style and comment density.
5. Run all §0.3 gates. Fix failures; never weaken a test or a gate to pass.
6. Verify at runtime when the task touches UI, tray, alerts, timers, or processes.
7. Update, in the same commit: this plan's checkbox and §2 board, `CHANGELOG.md` under `## Unreleased`, `PRIVACY.md` if any side effect changed, the size ledger if size changed, and `CLAUDE.md` if you learned a new non-obvious gotcha.
8. Commit with a Conventional Commit message scoped like the history (`feat(ui): …`, `fix(core): …`, `docs: …`). One task per commit unless the task says otherwise.
9. Write the handoff report (§0.7).

A task is **done** only when code, tests, docs, and the rollback note (§6) land together and every acceptance bullet is demonstrated, not assumed.

### 0.5 Never do these

- Never write, refresh, rotate, or exchange provider credentials. Never touch `.credentials.json` or Codex `auth.json` except to read. On 401/403, tell the user to open the CLI.
- Never call a live provider endpoint from an automated test.
- Never add Tokio, a WebView, a managed runtime, a loopback server, a service, or a scheduled task.
- Never add a crate without recording its x64 size delta, license, and maintenance status in the commit message.
- Never delete or hand-edit `power-override.v1.json` or `update-operation.v1.json`.
- Tags, releases, repository settings/rulesets, and secrets/variables are human-only by default (§7). The owner explicitly authorized agent setup and publication of 0.9.1 on 2026-10-09 (ADR 0006); this does not authorize future releases or provider credential changes.
- Never bump `Cargo.toml` `version` unless the task explicitly says so.
- Never use `git push --force`, `--no-verify`, or interactive git commands.
- Never “modernize” anything `CLAUDE.md` calls load-bearing (acrylic policy, WARP device, `LoadIconW` allow, `CREATE_NEW_CONSOLE` login).

### 0.6 Stop and ask the human when

- A task is blocked on an external action (signing certificate, secrets, Winget ownership, ARM64 hardware).
- A product decision is needed that §16 does not settle.
- The x64 release executable would exceed the 1.25 MiB hard ceiling, or a single task adds more than 10%.
- A test cannot be made deterministic without sleeping or network access.
- Real-hardware behavior contradicts the plan (for example, a Windows API renders differently on the DirectComposition flyout).

### 0.7 Handoff report (end of every task)

```text
Task: <ID> — <title>
Result: done | partial | blocked
Changed: <files>
Gates: fmt ok | clippy ok | test ok (N passed) | release build ok
Runtime check: <what you ran and what you saw>
Size: x64 <bytes> (<±%> vs previous ledger row)
Docs updated: <plan, CHANGELOG, PRIVACY, ledger, CLAUDE.md>
Deviations from plan: <none | list with reason>
Follow-ups: <none | list>
```

---

## 1. Product thesis and contract

> The smallest trustworthy native Windows quota monitor for AI coding tools.

Claudometer competes on trust, native Windows quality, footprint, and **one-glance answers**, not on provider count. Provider breadth, dashboards, and themes never compensate for weak safety or trust. The stopping rule: if a feature does not improve trust, reliability, accessibility, actionability, or distribution while staying inside the footprint contract, it is not on this roadmap.

### 1.1 Load-bearing strengths

- Native Win32 + Direct2D/DirectComposition. No WebView or managed runtime.
- One small process. No service, daemon, local server, or resident helper.
- Provider credentials stay owned by the official CLIs.
- Fast, glanceable tray UI with low memory and CPU use.
- Honest freshness, stale-data, and rate-limit behavior.

### 1.2 Invariants every release preserves

**Privacy and credentials**

- Claude Code and Codex alone own login, refresh-token rotation, and durable credentials.
- Claudometer never stores refresh tokens, access tokens, browser cookies, or user-entered API keys.
- Secret material stays worker-local, is never in `Debug` output, and is never serialized or logged.
- Every displayed snapshot belongs to an opaque account identity that matches the current credential identity.
- No telemetry, analytics, cloud sync, prompt collection, response-body logging, or automatic crash upload.
- Every network destination, file, registry key, child process, and system mutation is documented in `PRIVACY.md` with its trigger and user control. `ci/check-privacy.ps1` enforces the network allowlist.

**Native footprint**

- No Electron, Tauri, WebView, .NET, Node, Java, or browser shell.
- No async runtime unless an ADR proves the thread/message model cannot meet a concrete requirement.
- No persistent helper, service, scheduled task, or loopback server.
- No continuous animation. Visible timers repaint at most once per 30 seconds; hidden windows never repaint on a timer.

**Data integrity**

- Every quota has a provider, account, stable limit identity, source, observation time, and freshness state.
- Cached data loads only after its account identity matches locally.
- Obsolete asynchronous results never update UI, cache, alerts, or cooldowns.
- A failed write leaves the previous valid file intact.
- A failed system mutation leaves verified original state or a durable, actionable recovery journal.
- Alerts consume accepted fresh-fetch events, never freshness inferred from the rendered view.
- Stale data beats error UI: never wipe last-good data on a failed fetch (except on account change or authentication failure).

**Valid installation states (all first-class and tested)**

Claude only · Codex only · both · neither installed or signed in · portable · per-user installed.

---

## 2. Status board (2026-10-09)

### 2.1 Milestones

| Milestone | Theme | State |
|---|---|---|
| Foundation | Baseline and required gates | **Done**; SIZE-01 profile audit complete on `chore/size-audit` |
| `v0.8` | Trustworthy state and system safety | **Shipped in 0.9.1** |
| `v0.9` | Authenticated, crash-safe updates | **Shipped in 0.9.1** |
| R0 | Ship the trust-root release | **Shipped 0.9.1 on 2026-10-09**; signed manifests/provenance verified, Windows signing deferred to v1.0 (ADR 0006) |
| `v0.10` | State core, diagnostics, Codex documented source | **In progress** (model/state/cache/errors/diagnostics committed on `main`; CODEX-01 blocked on the app-server credential contract) |
| `v0.11` | Accessible, adaptive first run | **In progress** (UIA committed; Narrator deferred by owner) |
| `v0.12` | Glanceable status, tray, and alerts | Not started |
| `v1.0` | Signed distribution | Not started |
| `v1.1` | History-refined pacing | **Gated** on field evidence |
| `v1.2` | One gated provider | Not started |

`Cargo.toml` says `0.9.1`. R0 is published with all work since `5b735ce`, including the completed state/cache/error/diagnostic slices. v0.9.0 remains an immutable failed/unreleased tag.

### 2.2 Facts an executor needs now

- WIP-00 measured x64 at **1,132,544 bytes** with a synthetic public trust root, above the former 1.0 MiB soft target and **178,176 bytes** below the hard ceiling. ARM64 was **1,029,120 bytes**. Growth versus the matching REL-02 rows was +3.17% / +3.34%; those historical rows remain in the ledger. Measure every task (§3.1).
  - SIZE-01 selects `opt-level = "z"`: provisioned x64 **1,058,304 bytes**, ARM64 **940,544 bytes**; unprovisioned **947,712 / 892,928 bytes**. The size-reducing change updates CI comparison baselines, preserves the 1.25 MiB ceiling, and records a **1,245,184-byte unsigned provisioned soft target** and milestone allowances in the ledger.
- Commit `0ed4ea9` on `feat/a11y-uia-and-plan-v2` contains the A11Y-01 implementation in `src/accessibility.rs`, `src/main.rs`, `src/gfx.rs`, `src/util.rs`, `src/demo.rs`, `Cargo.toml`, and `Cargo.lock` (UIA fragment tree via `WM_GETOBJECT`, new `windows` features `implement`, `Win32_UI_Accessibility`, `Win32_System_Ole`, `Win32_System_Variant`, plus a direct `windows-core` dependency). WIP-00 passed all §0.3 gates, both architecture builds, demo safety, and 22 UIA client checks on 2026-10-08; Narrator speech remains unverified. See `docs/verification/wip-00.md`.
- HTTP requests already use a 10-second `ureq` timeout (`api.rs`, `codex.rs`).
- Existing runtime modules: `accessibility`, `alerts`, `api`, `app`, `auth`, `codex`, `config`, `demo`, `diagnostics`, `gfx`, `main`, `network`, `poller`, `provider/{mod,error,model,state}`, `release_manifest`, `runtime_state`, `store`, `trayicon`, `updater`, `util`, `vibecode`. `state_reference` is test-only legacy parity code.
- Demo scenarios: `claude-only`, `codex-only`, `both`, `loading`, `stale`, `cooldown`, `error`, `neither`, `settings`, `many`.
- Codex-family agents auto-load `AGENTS.md`, which points here and to `CLAUDE.md`. Read both explicitly either way.

### 2.3 Execution queue

Work strictly top to bottom, skipping only tasks whose dependencies are not done.

1. **WIP-00** — verify the committed accessibility work (§9.1); **Narrator verification deferred by the owner on 2026-10-08**. WIP-00 and A11Y-01 remain unchecked; the owner explicitly authorized proceeding to SIZE-01 and release-note preparation without this check.
2. **SIZE-01** — **done**: size audit, measured `z` profile, and headroom plan (§3.2) on `chore/size-audit`, branched from PR #2.
3. **REL-03** — **done (notes only)**: proposed `docs/release-notes/0.9.1.md`, expanded Unreleased changelog, local release-input check, and live protection audit (§7). Human publishes.
4. **MODEL-01 done** → **STATE-01 done** → **APP-01 done** → **CACHE-01 done** (§8.1).
5. **ERR-01 done** — actionable error states (§8.2).
6. **DIAG-01 done** → **DIAG-02 done** (§8.3).
7. **CODEX-01 blocked** → **CODEX-02** (§8.4): installed Codex 0.159.1 can proactively refresh and persist managed credentials during a limits read. `refreshToken: false` does not disable this path. Resolve the credential contract before implementing or measuring live app-server polling; see `docs/verification/codex-01-preflight.md`.
8. **PACE-01** → **ROW-01** → **FRESH-01** (§10.1). If the v0.10 tail is blocked, PACE-01 and ROW-01 may start once MODEL-01 is done, and FRESH-01 once APP-01 is done.
9. **A11Y-02**, **LAYOUT-01**, **LAYOUT-02**, **RENDER-01** (§9).
10. **ONBOARD-01**, **ONBOARD-02**, **KEY-01**, **UI-TEST-01** (§9).
11. **TRAY-01**, **TRAY-02**, **TRAY-03**, **ALERT-03**, **ALERT-04**, **PRIV-03** (§10).
12. `v1.0` tasks (§11), most of which are human-gated.

---

## 3. Budgets

### 3.1 Measured baseline and budgets

Methodology: `docs/performance/foundation-baseline.md`, script `ci/measure-baseline.ps1`. CI enforces size through `ci/check-artifact.ps1` and `ci/release-budgets.json`.

| Metric | Measured (Foundation) | Budget |
|---|---:|---:|
| x64 release executable, provisioned trust root | 1,097,728 bytes | Hard ceiling **1,310,720 bytes (1.25 MiB)**; see §3.2 |
| ARM64 release executable, provisioned trust root | 995,840 bytes | Hard ceiling 1.25 MiB |
| Hidden private working set, p95 | 1.53 MiB | **≤ 2.5 MiB** |
| Visible two-provider flyout private working set, p95 | 4.45 MiB | **≤ 6.0 MiB** |
| Idle CPU, excluding refresh work | < 0.002% | **≤ 0.01%** over ten minutes |
| GDI handles hidden / visible | 10 / 13 | ≤ 16 / ≤ 24 |
| Tray readiness, 50 starts | 50.4 ms median, 76.6 ms p95 | **≤ 150 ms p95** |
| Codex app-server read (CODEX-02) | — | ≤ 2 s p95, hard kill at 10 s |

Rules:

- A single task that grows the executable more than 10% must explain the growth in the ledger before merge.
- Opening or repainting a window never triggers a network request unless the freshness policy says data is due.
- A provider refresh may launch a deadline-bound child process, but no child may remain afterward.
- Re-measure memory and CPU after A11Y-01, APP-01, and PACE-01/FRESH-01 land.

### 3.2 SIZE-01 — size audit and headroom plan

- [x] **SIZE-01**
  - Build x64 and ARM64 with and without the A11Y WIP; record both in the ledger.
  - Produce a size breakdown (for example `cargo bloat --release --crates` run locally, not added as a dependency) and list the five largest contributors.
  - Evaluate, with measurements, at least: `opt-level = "z"` versus `"s"` (also re-measure startup and the 50-start p95), trimming unused `windows` features, and feature-gating heavy `ed25519-dalek` options.
  - Decide a new soft target that fits the remaining roadmap. Record the decision and the expected per-milestone allowance in the ledger. The 1.25 MiB hard ceiling does not move without an ADR.
  - Acceptance: ledger has per-crate numbers, a chosen profile, and a per-milestone allowance; `ci/release-budgets.json` is updated only if a size-reducing change lands.
  - Completed 2026-10-08 on `chore/size-audit`: chose `z` with fat LTO; measured both architectures and both trust-root modes, compared the existing with/without-UIA builds, evaluated Windows feature trimming and Ed25519 `fast`, and recorded raw crate/startup data and milestone allowances. All §0.3 gates, artifact/policy checks, demo safety, and 22 UIA checks passed. Narrator remains deferred, not completed. Rollback: restore `opt-level = "s"` and the previous CI comparison baselines together; no data migration. See `docs/performance/size-01.md` and the ledger.

### 3.3 Defensive bounds (adjust only with fixture evidence)

| Input/state | Bound |
|---|---:|
| Provider HTTP response body | 1 MiB |
| Update manifest | 64 KiB |
| Application/update download | 16 MiB, read as limit + 1 byte |
| Normalized quota rows per provider | 64 |
| Provider-controlled display string | 512 UTF-8 bytes |
| Retained diagnostic files | 3 files × 256 KiB |
| Update readiness timeout | 15 seconds |
| HTTP request total timeout | 10 seconds |
| Codex app-server request deadline | 10 seconds, then kill the process tree |
| History limits retained (v1.1 only) | 32 current-account limit identities × 96 points |

---

## 4. Target architecture

Keep the single-process Win32 architecture. Move policy and state out of window procedures so it can be tested without windows.

```text
Win32 messages
      │
      ▼
App (UI-thread owner) ─────► View models ─────► flyout / settings / tray / alerts / UIA
      │
      ├────► Provider controller ─────► short-lived source workers
      │                                      ├── documented local source
      │                                      └── compatibility endpoint
      │
      └────► Typed stores
              ├── settings.json
              ├── state.json (sanitized runtime cache)
              ├── power-override.v1.json
              └── update-operation.v1.json
```

### 4.1 State ownership

- The UI thread exclusively mutates `App` and provider state.
- Workers receive immutable inputs and send typed `AppEvent` values through `std::sync::mpsc`.
- `PostMessageW` only wakes the UI thread; the UI thread drains the queue.
- Every request and event carries `provider`, `generation`, `request_id`, and `account_key`.
- Events that no longer match current state are dropped with no side effect.
- Rendering and UIA consume derived view models, never provider JSON or transport errors.

### 4.2 Module boundaries

`exists` means the file is on `main` today; `planned` means the named task creates it.

| Module | Responsibility | State |
|---|---|---|
| `main.rs` | Bootstrap, argument dispatch, window registration, message loop | exists (too large; shrink in APP-01) |
| `app.rs` | UI-thread `App`, commands, timers, window-event coordination | planned (APP-01) |
| `provider/model.rs` | Provider/source IDs, normalized limits, account keys, snapshots, typed errors | exists (minimal; completed in MODEL-01) |
| `provider/state.rs` | Pure reducer, freshness, retry, display derivation | planned (STATE-01); absorb `state_policy.rs` |
| `provider/pace.rs` | Pure pace verdict | planned (PACE-01) |
| `provider/mod.rs` | Static provider catalog and source order | exists |
| `api.rs`, `codex.rs` | Provider adapters and parsers (move under `provider/` only when touched anyway) | exists |
| `poller.rs` | Worker spawning and typed event delivery; no provider policy | planned (APP-01) |
| `config.rs` | Typed settings, validation, defaults, migration | exists |
| `store.rs` | Atomic JSON writes, replacement, corruption preservation | exists |
| `runtime_state.rs` | `state.json` envelope, receipts, install salt | exists; extended in CACHE-01 |
| `diagnostics.rs` | Stable error codes, bounded redacted log, support snapshot | planned (DIAG-01) |
| `alerts.rs` | Fresh-event-only alert policy and WinRT delivery | exists |
| `vibecode.rs` | Wake lock and power-operation transaction | exists |
| `updater.rs`, `release_manifest.rs` | Manifest verification and update transaction | exists |
| `accessibility.rs` | UIA providers derived from the same view/geometry as rendering | exists (uncommitted) |
| `gfx.rs` | Renderer | exists |
| `trayicon.rs` | CPU tray icon generation | exists |
| `network.rs` | Central network destination constants | exists |
| `util.rs` | Small Windows helpers only | exists |

Do not create a cross-platform abstraction. Windows is the product platform.

### 4.3 Normalized provider model (MODEL-01 target)

Names may change; the contract may not.

```rust
enum ProviderId { Claude, Codex }

enum SourceId { ClaudeOAuthCompatibility, CodexAppServer, CodexWhamCompatibility }

enum SourceSupport { Documented, Compatibility }

struct UsageSnapshot {
    provider: ProviderId,
    account: AccountKey,
    source: SourceProvenance,
    plan: Option<String>,
    limits: Vec<UsageLimit>,
    observed_at_unix: i64,
}

struct SourceProvenance { id: SourceId, support: SourceSupport }

struct UsageLimit {
    id: LimitId,
    kind: LimitKind,
    label: String,
    class: LimitClass,
    utilization: Percent,
    provider_severity_hint: Option<ProviderSeverity>,
    resets_at_unix: Option<i64>,
    window_seconds: Option<u32>,   // needed by PACE-01; Claude: derived from kind, Codex: limit_window_seconds
}

enum LimitClass { Quota, Spend }

enum LimitKind { Session, Weekly, Model, ExtraUsage, Other(String) }
```

Required behavior:

- `Percent` rejects non-finite input and clamps once at the adapter boundary.
- Formatted reset text never lives in the domain model. The UI formats the target instant using the offset in force at that instant.
- Alert identity = provider + account + stable limit ID + threshold + reset instance, never a display label.
- Spend/extra usage is typed, not excluded by string comparison.
- `DisplaySeverity` and the pace verdict are derived in view state, never frozen into cached snapshots.
- Errors carry a stable safe category, source, retry hint, and user message (§5.3). Raw response bodies are never retained.
- Direct endpoints are labeled `Compatibility`, never `Documented`.
- Fallback to another source happens only when a source is unsupported, unavailable, or disabled — never to evade a 401/403 or 429.
- Window kind comes from duration (`limit_window_seconds`), never from `primary`/`secondary` position.

### 4.4 Provider reducer (STATE-01 target)

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
| Credential/account changes | Increment generation; clear snapshot, plan, error, cooldown, debounce |
| Matching cache loaded | Attach as cached data with its true age; it is never “fresh” |
| Refresh requested | Start only if enabled, available, not fetching, and allowed by debounce/backoff |
| Fetch succeeds | Accept only matching generation/request/account; clear errors and 429 streak; persist sanitized snapshot |
| Fetch returns 429 | Keep same-account last-good data; enter explicit backoff; persist the retry deadline |
| Other transient failure | Keep same-account data; show it as Outdated once the age rule in §5.2 triggers |
| Authentication fails | Clear account-bound data; enter `Unavailable` |
| Obsolete worker completes | Drop it: no render, alert, cache, or cooldown change |
| Cooldown expires | Return to `Idle`; the next scheduled or explicit command may fetch |

Manual refresh during cooldown shows the next allowed time instead of silently doing nothing. Any non-429 result resets the consecutive-429 streak.

### 4.5 Persistence ownership

| File | Contents | Safe to delete? |
|---|---|---|
| `settings.json` | User preferences only | Yes; defaults return |
| `state.json` | Sanitized snapshots, alert receipts, retry deadline, install salt | Yes; cache and dedup state are lost |
| `power-override.v1.json` | Vibecode transaction | **No** while present; recover first |
| `update-operation.v1.json` | Update swap transaction | **No** while present; recover first |
| `diagnostics.log` (+2 rotations) | Bounded redacted operational events | Yes |

All JSON writes go through `AtomicJsonStore` (`store.rs`): full in-memory serialize, same-directory temp file, `sync_all`, verified `.bak` generation, `ReplaceFileW` (or `MoveFileExW(...WRITE_THROUGH)` on first create), post-commit validation, restore on failure, `.corrupt` preservation of malformed input.

---

## 5. UI rules and copy (single source of truth)

All user-visible strings below are exact. Reuse an existing string in the code when it already says the same thing; otherwise use these. Every state must be available as text through UIA, never as color alone.

### 5.1 Quota row anatomy

```text
Session                                   ~3% spare      ← name + optional pace note
[██████████████░░░░░░│░░░░░░]                            ← bar + optional even-pace tick
48% used                          resets in 3h 25m       ← value + reset label
```

- **Value:** `48% used` or `52% left` per the `quota_display` setting (TRAY-02). Severity and alerts always use used %.
- **Reset label:** `resets in 3h 25m` (countdown) or `resets 18:59` / `resets Tue 18:59` (clock) per the `reset_format` setting (ROW-01). Countdown over 24 h: `resets in 2d 4h`.
- **Not started:** a Claude session limit with no reset timestamp reads `Not started`; UIA adds “The session starts with your first message.” Codex never infers this state.
- **Pace note and color:** §10.1 PACE-01.

### 5.2 Provider header and footer

- Header: provider name, plan badge, then at most one status token:
  - `Updating…` while a fetch is in flight (static text, no spinner animation).
  - `Outdated` when the last successful fetch is older than `max(2 × poll interval, 10 minutes)`. Tooltip/UIA: `Last updated 3h ago`.
  - A warning glyph plus the short error text from §5.3 when the last attempt failed.
- Footer: the existing `Updated …` caption plus `Next update in 4m`. Activating the footer action (click, Enter, Space) refreshes all providers now. During cooldown it reads `Retry at 18:42` and does not fetch.
- Source provenance (`Documented`/`Compatibility`) is **not** shown in the flyout. It appears in diagnostics, Settings → About/Diagnostics, and the provider's UIA description.

### 5.3 Error states (ERR-01)

| Condition | Short text (header) | Detail (tooltip, UIA, diagnostics) | Action |
|---|---|---|---|
| Claude credentials missing | `Not signed in` | `Open Claude Code and sign in.` | Existing Connect |
| Claude 401/403 | `Sign-in expired` | `Open Claude Code once; it renews the sign-in automatically.` | Existing Reconnect |
| Claude credential lacks `user:profile` scope | `Sign in again for live usage` | `This login can run Claude but cannot read usage limits (for example, a token from claude setup-token). Run claude and sign in with your Claude account.` | Existing Reconnect |
| HTTP 429 | `Paused by provider` | `<Provider> is limiting usage checks. Retrying at 18:42.` | none |
| Timeout | `Timed out` | `No response within 10 seconds. Retrying at 18:42.` | none |
| No network | `Offline` | `Can't reach <host>. Showing the last values.` | none |
| Codex CLI or auth missing | `Not signed in` | `Run codex and sign in.` | none |
| Codex 401/403 | `Sign-in expired` | `Run codex once; it renews the sign-in automatically.` | none |
| Unexpected response | `Couldn't read usage` | `The usage format changed. Code <stable code>. Copy diagnostics to report it.` | Copy diagnostics |

---

## 6. Rollback contract

| Milestone | Kill switch / rollback | Compatibility proof |
|---|---|---|
| `v0.8` | Persistent lid override defaults disabled until its suite passes; an unresolved journal blocks re-apply | Dual-read/write legacy settings for two releases; `state.json` safe to delete; power journal is not |
| `v0.9` | Disable automatic update checks; direct users to manual signed downloads | Last-known-good executable kept until commit; every journal phase has an idempotent recovery test |
| `v0.10` | Per-provider source selector returns to the compatibility source; a broken adapter can be disabled without disabling the provider | `state.json` optional and backward compatible |
| `v0.11` | Roll back the binary; no irreversible migration in UI work | New settings are additive; older binaries ignore them |
| `v0.12` | Reset tray/alert/display choices to defaults or roll back the binary | Settings additive; alert receipt schema stays readable |
| `v1.0` | Keep the previous signed installer as an explicit rollback release authorized by signed policy | Managed installs never self-modify; install/update/uninstall smoke tests on both architectures |
| `v1.1` | Disable history and delete its bounded file | Live quota never depends on history |
| `v1.2` | Disable the new provider independently | No other provider's state, icon, alert, or history changes |

Abort a rollout immediately if: cross-account data is rendered, cached, alerted, or logged; a safety/update journal cannot converge; an unauthenticated executable reaches an execution boundary (Windows Authenticode additionally required from v1.0; ADR 0006); a migration stops the previous release from starting; the hard size or memory budget is exceeded without an ADR; or a critical Narrator/keyboard regression appears.

---

## 7. R0: ship the trust-root release

Owner decision 2026-10-09 (ADR 0006) supersedes the historical human-only/Authenticode prerequisites below for 0.9.1: agent setup and publication are authorized, Windows signing stays SIGN-01/v1.0, and bootstrap uses hashes plus GitHub source/workflow provenance. Narrator and real ARM64 runtime remain disclosed as unverified. All existing source, artifact, signature, manifest, and repository-protection gates remain required.

Everything in `v0.8` and `v0.9` shipped in the immutable 0.9.1 release, published 2026-10-09T00:09:00Z. Both published architectures, signatures, hashes, SBOM and GitHub attestations verify; downloaded x64 demo safety passes. See `docs/verification/r0-0.9.0.md`. The first authenticated release is also the trust-root bootstrap (UPD-02): existing users must install it manually and verify it independently; the old updater cannot authenticate it.

- [x] **REL-03 — Prepare the trust-root release (agent prepares; human publishes).**
  - Prepare notes after WIP-00 local verification. The owner deferred Narrator on 2026-10-08 and authorized proceeding; notes must disclose incomplete A11Y-01 and do not authorize publishing or claim screen-reader support.
  - Draft `CHANGELOG.md` for the next version covering all `v0.8` and `v0.9` work. Lead with the manual-install requirement and the verification steps from `docs/release-manifest-v1.md`.
  - Draft release notes at `docs/release-notes/<version>.md` with: what changed for users, the manual install and verification procedure, the Vibecode legacy-recovery notice, and the update-check default for new installs.
  - Run `ci/check-release.ps1` and `ci/check-release-infrastructure.ps1` locally where they can run without secrets; report which checks need the release environment.
  - Write a human checklist in the handoff report and stop. Human-only steps, in this order:
    1. Generate and store the offline Ed25519 release key; set `CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX` and `CLAUDOMETER_RELEASE_SEQUENCE`; provision `CLAUDOMETER_RELEASE_ADMIN_READ_TOKEN` in the `release` environment.
    2. Choose the version (recommended `0.9.1`), bump `Cargo.toml`, commit, and wait for `build` to go green.
    3. Push the tag and approve the `release` environment. The workflow smoke-tests and publishes automatically; then apply the drafted notes with `gh release edit <tag> --notes-file docs/release-notes/<version>.md`.
    4. Record the dated provisioning entry in `docs/release-manifest-v1.md`.
  - Completed 2026-10-08: proposed 0.9.1 notes and Unreleased changelog cover v0.8/v0.9, manual verification, Vibecode recovery, update consent, and deferred A11Y. fmt/clippy/115 tests/x64 build pass; `ci/check-release.ps1` passes on actual v0.7.3 isolated assets and live `ci/check-release-infrastructure.ps1` passes. No version bump or publication. **Publishing prerequisite:** current release workflow signs Ed25519 manifests but has no Authenticode signing/verification step required by the bootstrap contract. The human checklist records signing integration, private/public policy provisioning, real ARM64 runtime, version PR, immutable tag, and approval. Rollback: revert the notes; no runtime/data change. See `docs/release-notes/0.9.1.md` for §0.7 handoff.

---

## 8. `v0.10`: state core, diagnostics, Codex documented source

**Goal:** one testable state machine per provider, actionable errors, supportable diagnostics, and the documented Codex source.

### 8.1 State core

- [x] **MODEL-01 — Complete the normalized domain model.**
  - Expand `provider/model.rs` to §4.3, including `window_seconds`. Move shared types out of `api.rs`/`codex.rs`.
  - Convert parsers, alerts, tray, renderer, and UIA to typed IDs and classes.
  - No rendering or network behavior change in this commit; existing fixtures must pass unchanged.
  - Acceptance: no string comparison decides limit class or kind; fixtures cover weekly-as-primary, weekly-only (no invented Session row), missing reset, non-finite, and out-of-range values.
  - Completed 2026-10-08 on `feat/model-01-normalized-model`: shared snapshots/outcomes/limits moved to provider model with account/provider/source, typed kind/class/severity, validated Percent, stable LimitId, and window duration. Formatted resets stay in the rendering view, not snapshots. Both compatibility sources retain their requests; Codex missing-duration captions remain unchanged but typed kind is Unknown/Other instead of inferred from position. Fixture files are unchanged; 118 tests, all §0.3 gates, demo safety, 22 UIA checks, screenshot review, and provisioned x64/ARM64 artifact checks pass. Error category expansion remains ERR-01; raw strings are parsed only at adapter boundaries. Rollback: revert the binary/code; no store migration. See `docs/verification/model-01.md` and the artifact ledger.

- [x] **STATE-01 — Pure provider reducer.**
  - Implement §4.4 in `provider/state.rs` with an injected clock; absorb `state_policy.rs`.
  - Derive view states: loading, fresh, updating-with-data, cached, outdated, cooldown, unavailable, failed.
  - Acceptance: table-driven tests for every §4.4 row plus arbitrary event orders; no test sleeps.
  - Completed 2026-10-09 on `feat/state-01-provider-reducer`: pure event reducer, identity-checked fetch tickets, explicit accepted-success/retry effects, and all eight derived view states. Policy/helpers and characterization tests moved from `state_policy.rs`; the current SLOTS shell still owns runtime state until APP-01. The §4.4 table, boundary tests, and 12,288 generated event steps pass with an injected clock. All §0.3 gates pass (131 tests); demo safety and both x64 trust-root artifact checks pass at unchanged sizes (950,784 / 1,061,376 bytes). Snapshot/retry persistence is an effect contract for CACHE-01. ERR-01 supplies stable adapter error categories later. Rollback: revert the binary/code; no durable schema change. See `docs/verification/state-01.md`.

- [x] **APP-01 — Move provider state to the UI thread.**
  - Add `app.rs` and `poller.rs`; introduce the `AppEvent` queue; keep window procedures thin.
  - Remove the mutex/atomic `SLOTS` cluster only after behavior-parity tests pass.
  - Acceptance: all demo scenarios render identically (compare screenshots before/after); `main.rs` shrinks; memory/CPU re-measured within budget.
  - Completed 2026-10-09 on `feat/state-01-provider-reducer`: UI-owned reducers/preparation in `app.rs`, worker-local credentials and AppEvent queue in `poller.rs`; SLOTS removed from runtime. `main.rs` shrinks from 2,958 to 2,125 lines. Legacy slot code is cfg(test)-only for direct parity proof. All §0.3 gates pass (140 tests), ten demo captures are pixel-identical, 22 UIA checks pass, demo safety and x64 artifact checks pass. Ten-minute isolated demo run: hidden/visible p95 1.61/4.75 MiB, idle CPU 0.0000/0.0045%, GDI 10/13; ten-start tray p95 80.454 ms. x64 964,096 unprovisioned / 1,074,688 provisioned (+1.40/+1.25%). Rollback: prior binary/revert; no schema change. See `docs/verification/app-01.md` and `docs/performance/app-01.md`. Interrupted runtime/UIA attempts are documented, not treated as passing evidence.

- [x] **CACHE-01 — Sanitized runtime cache.**
  - Extend the `state.json` envelope (`runtime_state.rs`) with bounded normalized snapshots, the persisted 429 retry deadline, and the selected source.
  - Load a snapshot only after its account key matches the current local identity.
  - A snapshot restored at launch shows instantly with its true age and is **never fresh**: the first poll after launch always fetches unless a persisted retry deadline is still in the future.
  - Bound provider count, rows, strings, timestamps, and age (drop snapshots older than 8 days).
  - Acceptance: tests for account mismatch, restart inside a 429 window, deleted/corrupt `state.json`, and a cached window whose reset already passed (drop that limit's value).
  - Completed 2026-10-09: additive `provider_cache` version 1 in state schema 1, normalized snapshots/source and accepted retry deadlines, persistent identity/source matching during worker preparation, cached labels/true age, and first-request cooldown enforcement. 151 tests pass including restart/account/source/expiry/bounds, old-reader preservation, and failed atomic writes. All §0.3 gates, demo safety, 22 UIA checks, and x64 artifact gates pass. Rollback: prior binary/revert; optional cache fields are ignored/preserved by older readers. See `docs/verification/cache-01.md`; stable HTTP authentication/error categories remain ERR-01.
  - Size: x64 998,912 unprovisioned / 1,109,504 provisioned (+3.61/+3.24% vs APP-01), within unchanged hard/regression gates. Cumulative v0.10 work exceeds its initial SIZE-01 planning allowance; remaining slices must address footprint without raising the ceiling.

### 8.2 Actionable errors

- [x] **ERR-01 — Map every failure to a §5.3 state.**
  - Add a stable error category and code to `FetchError`; map 401/403, 429 (with `Retry-After`), timeout, connect failure, and parse failure.
  - Detect an inference-only Claude credential before any request: if the credential's `scopes` array exists and lacks `user:profile`, enter the `Sign in again for live usage` state and make no usage request. Confirm the field name against a real credential file without logging values.
  - Acceptance: one test per §5.3 row; UIA and tooltip expose the detail text; no raw response body appears in any string.
  - Completed 2026-10-09: shared typed provider failures with stable category/code and §5.3 short/detail text; HTTP 401/403, 429 numeric/date Retry-After, timeout/connect/parse failures mapped without exposing bodies/transport strings. Real local credential confirms `claudeAiOauth.scopes` shape through boolean-only inspection; present scopes lacking `user:profile` reject preparation before any request. Accepted auth failures clear account data/cache/alerts, transient values remain. Status is below the provider header to preserve name/plan/button layout. UIA HelpText and native tooltip registration/text verified with deterministic Offline demo; 161 tests and §0.3 gates pass. Copy diagnostics itself remains DIAG-01's action. Rollback: prior binary/revert; state schema unchanged. See `docs/verification/err-01.md`.

### 8.3 Diagnostics

- [x] **DIAG-01 — Operational diagnostics snapshot.**
  - Version, architecture, Windows build, install channel, provider detected/authenticated state, selected source and fallback reason, last attempt/success, freshness, cooldown, next retry, stable error code, update/recovery state, non-secret settings.
  - Show it in Settings (Diagnostics section) with a `Copy diagnostics` action.
  - Completed 2026-10-09: fixed-field local snapshot and scrollable Settings Diagnostics card with Copy/Invoke/Tab/Enter/Space. Includes Windows build, install/source/support/fallback, detection/auth state, timestamps/freshness/retry/error, settings/update/power/store states. No credentials/account keys/paths/provider labels; no log or network probe. 165 tests and §0.3 gates pass; deterministic UIA copy/keyboard/privacy/demo-clipboard checks and screenshots verified. Rollback: prior binary/revert; no durable schema change. See `docs/verification/diag-01.md`; DIAG-02 adds the log/CLI commands separately.

- [x] **DIAG-02 — Bounded redacted log and support commands.**
  - `diagnostics.log`, rotating at 3 × 256 KiB.
  - `--diagnose` prints the snapshot to stdout; `--version` prints the version.
  - Redact bearer/refresh tokens, raw account IDs, email, username, home path, account hash, and response bodies.
  - Rendering, config, and registry failures become visible diagnostics instead of silent no-ops.
  - Acceptance: a redaction corpus test proves no token, identifier, PII, home path, or body reaches the log or snapshot.
  - Completed 2026-10-09: local timestamp/fixed-code log with three 256 KiB generations and repeated-failure suppression; unknown fields replaced wholesale, no raw messages. Support commands exit before tray/startup and use read-only settings/state and dedicated process-local identity preparation without execute/UI. Rendering/config/registry/provider/update/clipboard failures report locally. 168 tests pass including rotation/write failure and redaction corpus for tokens, IDs, email/user/path/hash/body; support-command stdout and corrupt-file unchanged tests pass. Demo safety/diagnostics UI/22 UIA and required gates pass. First size gate failure resolved through smaller formatting/IO without changing budgets. Rollback: prior binary/revert; optional log deletion loses support history only. See `docs/verification/diag-02.md`.
  - Size: x64 1,053,184 unprovisioned / 1,163,776 provisioned (+0.44/+0.40% vs DIAG-01). Original cumulative CI margin is 358 bytes; CODEX-01 must first recover size headroom. Hard ceiling/profile/baselines remain unchanged.

### 8.4 Codex documented source

- [ ] **CODEX-01 — Codex app-server adapter.**
  - Preflight 2026-10-09: the installed 0.159.1 source calls `auth_with_http_client_factory()` → `auth()` → proactive refresh → token persistence during `account/rateLimits/read`. This conflicts with §1.2 and the binding credential gotcha; the managed adapter remains unimplemented. The documented experimental external-token mode requires a separate design decision and isolation proof before use. CODEX-02 cannot measure/promote an unsafe adapter. Evidence and unblock conditions: `docs/verification/codex-01-preflight.md`.
  - Prefer the documented `codex app-server` JSON-RPC `account/rateLimits/read`. Request account state without forcing a token refresh.
  - Parse dynamic `rateLimitsByLimitId`, plan, reset metadata, spend-control state, and reset-credit availability (display only; never consume credits).
  - Model-specific limits (for example the `additional_rate_limits` entries in the compatibility payload) become `LimitKind::Model` rows using duration-based classification; omit them when absent.
  - Plan display names: `prolite` → `Pro 100`, `pro` → `Pro 200`, `promax` → `Pro 500`, `self_serve_business_prolite` → `Business Premium`; unknown identifiers are title-cased. (Source: openusage provider docs, 2026-10; re-verify against a fixture.)
  - Enforce the §3.3 deadline and terminate the process tree with `taskkill /T` afterward.
  - Keep `wham/usage` as the separately labeled compatibility fallback.
  - Acceptance: fake app-server fixtures for success, missing fields, malformed JSON-RPC, timeout, hung descendant; no leaked process.

- [ ] **CODEX-02 — Gate the default source on measured behavior.**
  - App-server success makes no `chatgpt.com` request in that cycle.
  - It becomes the default only at ≤ 2 s p95 on the reference machine with no leaked child; otherwise it stays opt-in.

### `v0.10` acceptance

- Every §4.4 transition and every §5.3 error state is tested.
- Codex app-server success makes no compatibility request and leaves no process behind.
- No two sources poll one provider in the same cycle.
- Cached values are labeled by age and never treated as fresh after restart.
- The redaction corpus passes.
- Deleting `state.json` or `diagnostics.log` loses only cache/support information.

---

## 9. `v0.11`: accessible, adaptive first run

**Goal:** every surface is operable with keyboard and Narrator, readable in High Contrast and at large text sizes, and usable on small screens.

### 9.1 Accessibility

- [ ] **WIP-00 — Verify the committed A11Y-01 work.**
  - Run all §0.3 gates on the working tree. Record x64/ARM64 size with and without the change.
  - Verify with Accessibility Insights or `inspect.exe`: the flyout and Settings expose a tree; every button Invokes; every switch Toggles; Narrator reads each control.
  - The implementation is already committed as `0ed4ea9`; do not stash or rewrite it. Record verification on its existing branch. Mark done only after the gates, size review, and Narrator check pass, then continue A11Y-01 from what is missing.
  - 2026-10-08: fmt, clippy, 115 tests, x64/ARM64 builds, both trust-root build modes, artifact checks, demo safety, and all 22 UIA client checks pass. `inspect.exe` confirms focused controls on both windows. **Blocked:** Narrator announcements require a human listening check (§0.6); checkbox stays open. Rollback: use the pre-UIA binary from `89241c1`, with no data migration or journal edits. Evidence: `docs/verification/wip-00.md` and the artifact ledger.
  - Owner decision, 2026-10-08: defer Narrator and proceed to SIZE-01/REL-03 because accessibility is lower priority. This authorizes the queue dependency exception, not a completed Narrator result; WIP-00 and A11Y-01 stay unchecked.

- [ ] **A11Y-01 — Complete the UI Automation fragment tree.**
  - Buttons: Invoke. Switches: Toggle. Interval and metric choices: Selection. Quota bars: read-only RangeValue.
  - Accessible names include provider, window, used/left value, reset, pace verdict, and freshness.
  - Dynamic changes raise targeted property events; the ticking `Updated …` and `Next update in …` captions must not cause repeated announcements.
  - Remaining after WIP-00 inspection: Selection for interval/metric choices, read-only RangeValue for quota bars (currently Text), richer names with freshness/pace, targeted property events (currently focus events only), and Narrator verification. Demo actions are intentionally guarded; successful Invoke/Toggle calls do not prove live setting mutations or state-change announcements.

- [ ] **A11Y-02 — Non-color semantics.**
  - Warning, critical, stale, unavailable, and pace states are available as text and through UIA.
  - Focus is always visible. Compute contrast for text drawn on the Windows accent color.

### 9.2 Adaptive layout and rendering

- [ ] **LAYOUT-01 — Bound all windows to the monitor work area.**
  - Scroll Settings and long flyouts. All controls reachable at 1280×720 and 1366×768 from 100% to 225% scaling.
  - Handle `WM_DPICHANGED` while open. Preserve keyboard focus across polling, resizing, and re-rendering.

- [ ] **LAYOUT-02 — Text scaling and High Contrast.**
  - Derive text metrics from the Windows text-scale setting.
  - Use system High Contrast colors and disable acrylic when High Contrast is on. Validate High Contrast Black and White (`--demo-contrast`).

- [ ] **RENDER-01 — Recover from device or render failure.**
  - Classify D2D/DXGI recreate-target and device-loss failures; recreate once.
  - If recreation fails, show a minimal native diagnostic surface instead of a blank resident process. Log only the HRESULT category.

### 9.3 First run, second launch, and shortcut

- [ ] **ONBOARD-01 — Inline first-run card (replaces the separate Welcome page).**
  - Show a card at the top of the flyout on genuinely new installs only (the same new-install detection that chose the `update_checks_enabled` default). Persist `welcome_dismissed`.
  - Contents: one sentence on what Claudometer monitors and what it never touches; each provider as `Ready`, `Sign in required`, or `CLI not found`; the `Check for updates automatically` switch (bound to `update_checks_enabled`); a one-line hint about pinning the icon from the tray overflow; a dismiss button (`✕`, keyboard reachable, UIA name “Dismiss welcome”).
  - Nothing is enabled, written, or launched without a direct action. No browser launch.
  - Settings gets `Show welcome again`.

- [ ] **ONBOARD-02 — Useful second launch.**
  - A second launch signals the running instance (registered window message or `WM_COPYDATA` to the `Claudometer.Main` window) to open the flyout, instead of exiting silently.

- [ ] **KEY-01 — Global shortcut to toggle the flyout.**
  - Setting `global_shortcut`: `Off` (default), `Ctrl+Alt+U`, `Ctrl+Shift+Alt+U`.
  - `RegisterHotKey` on the main window with `MOD_NOREPEAT`; `WM_HOTKEY` toggles the flyout and puts keyboard focus on its first control.
  - If registration fails, show `This shortcut is used by another app` in Settings and leave the setting effectively off.
  - Unregister on change and on exit. Document it in `PRIVACY.md` only if it adds a registry or system effect (it should not).

- [ ] **UI-TEST-01 — Deterministic UI and accessibility proof.**
  - Demo screenshots for every scenario in §2.2, light and dark and High Contrast, at 100% and 200%.
  - UIA smoke tests for discoverability, names, roles, values, focus order, and invocation.
  - Accessibility Insights FastPass as a release checklist item.

### `v0.11` acceptance

- Narrator can discover, read, and operate every control without a mouse.
- A quota row announces provider, window, value, reset, pace, and freshness.
- Tab order matches visual order; Space/Enter/arrows/Escape behave consistently; refresh never resets focus.
- Every control is reachable at the required resolutions and scales.
- High Contrast and text scaling keep all state distinctions readable.
- Device loss cannot leave a permanently blank window.
- The first-run card performs no write, launch, or system change without a direct action.

---

## 10. `v0.12`: glanceable status, tray, and alerts

**Goal:** one glance answers “am I going to run out before the reset, and is this number current?”

### 10.1 Glanceable rows

- [ ] **PACE-01 — Stateless pace verdict.** Needs no history file.
  - Pure function in `provider/pace.rs`: `fn pace(limit: &UsageLimit, now_unix: i64) -> Pace`.
  - Inputs: used `u` (0–100), reset `R`, window length `W` seconds, now `t`. Window start `S = R − W`; elapsed `e = t − S`.
  - **Projectable** only if: class is `Quota`; `R` and `W` are known; `R > t`; `S ≤ t`; `u > 0`; and `e ≥ max(0.05 × W, 900)` seconds.
  - Projection at reset: `p = u × W / e`.
  - Verdicts, checked in this order:

    | Verdict | Condition | Bar color | Note next to the limit name |
    |---|---|---|---|
    | `LimitReached` | `u ≥ 99.5` | critical | `Limit reached` |
    | `Over` | projectable and `p ≥ 100` | critical | `Limit in 3h 5m` when the run-out time `t + (100 − u) × e / u` is more than 60 s before `R`; otherwise `At limit by reset` |
    | `Tight` | projectable and `90 < p < 100` | warning | `~N% spare` with `N = max(1, floor(100 − p))` |
    | `OnTrack` | projectable and `p ≤ 90` | normal | none |
    | `Level` | not projectable | the existing used-% severity, unchanged | none |

  - `Tight` and `Over` also draw an even-pace tick on the bar at fraction `e / W`.
  - Run-out and reset times follow the `reset_format` setting.
  - Setting `pace_colors_enabled` (default on): when off, every row uses `Level`.
  - Scope: pace changes **only** flyout bar color, the note, and the UIA name. The tray icon and alerts keep using used-% thresholds (decision D-06).
  - Acceptance: table tests for every verdict boundary (`p` = 90, 90.01, 99.99, 100), `u = 0`, missing reset, missing window, `e` just below and at the minimum, `R ≤ t`, clock skew (`S > t`), a Spend-class limit, and DST weeks (pure Unix arithmetic must make DST irrelevant; prove it).

- [ ] **ROW-01 — Reset format, “Not started”, and click shortcuts.**
  - Setting `reset_format`: `clock` (default; today's behavior) or `countdown`. Formats per §5.1.
  - Claude session with no reset timestamp shows `Not started` (§5.1).
  - Mouse shortcuts: clicking a row's value flips `quota_display`; clicking a reset label flips `reset_format`. Both apply everywhere and persist. The keyboard path for both is the Settings control (no extra Tab stops per row).
  - Countdown text repaints at most once per 30 s, only while the flyout is visible.

- [ ] **FRESH-01 — Updating, Outdated, and next-update footer.**
  - Implement the §5.2 header tokens and footer from the STATE-01 view states.
  - `Next update in 4m` is minute-granular; the footer action refreshes now or shows `Retry at …` during cooldown.
  - Acceptance: demo scenarios `loading`, `stale`, `cooldown`, and `error` show the right token; UIA exposes each; idle CPU stays within budget with the flyout open for ten minutes.

### 10.2 Tray and alerts

- [ ] **TRAY-01 — Provider/window selection.**
  - Modes: `Auto: highest used visible quota` and any currently available provider/window.
  - Claude-only, Codex-only, both, and neither behave correctly. An unavailable explicit choice falls back to Auto while keeping the preference. Auto tie-breaking is deterministic.

- [ ] **TRAY-02 — Used versus left.**
  - Setting `quota_display`: `used` (default) or `left`. The tray ring and text may invert; severity and alerts always use used %.
  - A stale value keeps the ring, with explicit stale text in the tooltip and UIA. The alert icon appears only when no selected or fallback value is usable.

- [ ] **TRAY-03 — Deterministic, bounded tooltip.**
  - Priority: selected metric, then error/freshness, then other providers. Respect the Windows tooltip length limit without cutting the most actionable line.

- [ ] **ALERT-03 — Constrained alert controls.**
  - One threshold: Off, 50%, 75%, or 90% (default 75%). Optional reset notification.
  - Windows Do Not Disturb is the quiet-hours mechanism. No custom scheduler, sound system, webhook, or rule engine.

- [ ] **ALERT-04 — Visible testing and timing.**
  - `Send test notification` with visible success or failure.
  - Reset notifications require an observed previous window; first observation is never a reset.

- [ ] **PRIV-03 — Hide windows from screen capture.**
  - Setting `hide_from_capture_enabled` (default off): applies `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (`0x11`; define a local const if `windows 0.58` does not export it) to the flyout and Settings on creation and when toggled.
  - Settings caption: `Usage windows stay out of screen shares and screenshots. The tray icon stays visible.`
  - Verify on real hardware with Snipping Tool and a Teams/OBS capture: the windows must be absent, not black. If the DirectComposition flyout renders black or the call fails, report it and keep the feature off (do not fall back to `WDA_MONITOR`).
  - Document the behavior in `PRIVACY.md`.

### `v0.12` acceptance

- Deterministic tests cover Claude-only, Codex-only, both, neither, provider error, disabled provider, disappearing window, and explicit fallback.
- Pace verdicts match the PACE-01 table; tray and alerts are unaffected by pace.
- Display and format changes apply immediately and survive restart.
- Threshold boundary, reset drift, missing reset, account switch, stale result, and restart deduplication are tested.
- Reset notifications fire once and never from stale or first-observed data.
- No extra tray icon, resident process, or continuous animation is introduced.

---

## 11. `v1.0`: signed distribution

**Goal:** install, update, rollback, and removal are verifiable and uneventful. Most tasks need human-provisioned credentials (§0.6).

- [ ] **SIGN-01 — Authenticode-sign every Windows artifact.**
  - Sign x64/ARM64 executables and installers in the protected, approval-gated `release` environment; timestamp signatures.
  - Verify with `WinVerifyTrust` in release smoke tests and before applying an update. Pin the expected publisher in updater policy.
  - Sign first, then hash the signed bytes and sign the manifest. Never mutate an artifact after its manifest exists.

- [ ] **DIST-01 — Native x64 and ARM64 artifacts.**
  - Portable signed executable per architecture; signed per-user installer needing no admin rights; architecture-aware manifest selection.

- [ ] **DIST-02 — Winget.**
  - Proposed ID `Dvaderfun.Claudometer`. Verify install, launch, upgrade, repair, and uninstall on clean x64 and ARM64.
  - Installed and portable update paths stay distinct.

- [ ] **DIST-03 — System-safe uninstall.**
  - Ask the running app to recover Vibecode and exit before removal.
  - Remove binaries, updater debris, Run entry, AUMID registration, toast icon, and Claudometer-owned bridge configuration. Never remove unrelated Claude/Codex configuration.
  - Offer retain/remove for settings and optional history. If a safety journal cannot resolve, stop and show the recovery action.

- [ ] **DOCS-01 — Public product surface.**
  - `README.md`: screenshot, accurate footprint, quick start, uninstall, verification, troubleshooting.
  - `SECURITY.md`: supported versions, private reporting, credential/update threat model.
  - `CONTRIBUTING.md`: setup, tests, architecture, fixture/redaction rules.
  - `docs/providers/claude.md` and `docs/providers/codex.md`, each with: what is tracked, where credentials come from (read-only), “Under the hood” (exact endpoints/RPCs), and “Troubleshooting” that maps every §5.3 short text to its fix.
  - `docs/flyout.md`, `docs/refreshing.md`, `docs/settings.md`: one behavior doc per surface.
  - Release checklist, source compatibility matrix, recovery guide; GitHub topics, social preview, issue/PR templates.

### `v1.0` acceptance

- `Get-AuthenticodeSignature` and `WinVerifyTrust` report the expected publisher for every executable and installer.
- The updater rejects unsigned, wrong-publisher, wrong-manifest, wrong-host, wrong-version, and wrong-architecture artifacts.
- Winget install/upgrade/uninstall passes on clean Windows 11 x64 and ARM64.
- Uninstall removes all selected Claudometer-owned state and restores every recoverable system effect.
- Tag, Cargo version, VERSIONINFO, installer, manifest, and artifact names match.
- The installed runtime meets §3.1 budgets. No critical accessibility or recovery defect is open.

---

## 12. `v1.1`: history-refined pacing (gated)

**Gate:** start only if real use shows the stateless PACE-01 verdict misleads (for example, bursty sessions flip between `Over` and `OnTrack`), recorded with examples in an ADR. If the gate never opens, `v1.1` ships reliability and polish instead.

- [ ] **HIST-01 — Opt-in normalized sample history.**
  - Fresh accepted samples only; partitioned by account, provider, stable limit ID, and reset instance.
  - At most one point per five minutes unless the value or reset identity changes. Eight-day retention, hard 1 MiB cap, 32 limits × 96 points.
  - Atomic compaction, corruption recovery, enable/disable/clear actions, no file while disabled.

- [ ] **PACE-02 — Robust projection from history.**
  - Current reset instance only; ignore stale points, decreases, and reset transitions.
  - Bounded Theil–Sen (or an ADR-approved linear-time robust fit) over ≤ 96 points; require ≥ 4 points spanning ≥ 20 minutes and ≥ 1% change; otherwise fall back to PACE-01.
  - Same verdict vocabulary and colors as PACE-01; always “at current pace”, never a guarantee.

Acceptance: no cross-account history; a simulated year of 30-second polling stays under 1 MiB; history adds ≤ 1 MiB private memory and ≤ 0.01 CPU percentage point idle.

Non-goals: token or dollar accounting, per-project/session/model attribution, charts, exports, ML predictions, reimplementing `ccusage`.

---

## 13. `v1.2`: one gated provider

A candidate needs all of: a documented machine-readable quota API or official local CLI IPC; provider-reported usage and reset identity; reuse of the provider's existing login; no copied, persisted, refreshed, or user-entered secret; no scraping or cookie extraction; no WebView or resident helper; a clean mapping to §4.3; legal sanitized fixtures; independent failure behavior within budgets.

Gemini/Antigravity and Copilot qualify only after their documented surfaces pass. If none passes, `v1.2` ships reliability and polish.

Acceptance: an ADR records the evidence first; new-only, new+Claude, new+Codex, and all-three states work; one provider's failure never changes another's data, alerts, icon, cooldown, or history; at most three providers through `v1.x`.

---

## 14. Test strategy

- **Unit/property:** parsers (complete, partial, unknown, malformed, oversized, hostile-but-valid), `Percent`, time formatting, source selection, stable IDs, tooltip truncation, reducer sequences with fake time, every config migration, alert crossing/reset/drift/missing-reset/account-switch/restart, PACE-01 boundaries. Fuzz parsers and manifest parsing for no panics and bounded allocation.
- **Fault injection:** filesystem create/write/flush/replace/delete and interruption; Vibecode after every journal and power-API transition; updater after every download, verification, journal, rename, spawn, readiness, and cleanup step; child process missing/bad protocol/timeout/hung descendant; renderer creation/resize/EndDraw/Present/device loss.
- **Contract/integration:** no live endpoints; sanitized fixtures and fake HTTP/app-server processes; fallback verified without contacting the fallback; redaction corpus over diagnostics and persisted state; a manual maintainer-only live smoke command, disabled in CI.
- **Windows UI matrix:** x64 and ARM64; 1280×720 and 1366×768; 100/125/150/200/225% scaling; light, dark, High Contrast Black/White; multi-monitor with different DPI, all taskbar edges, Explorer restart; keyboard-only and Narrator; notifications on/off, Do Not Disturb, toast activation; screen capture with PRIV-03 on/off.
- **Release evidence (every release):** source commit and toolchain; gate results; hashes, signatures, SBOM, provenance; size, startup, memory, CPU, GDI; UIA result; install/update/rollback/uninstall result.

---

## 15. Task dependencies

| Task | Depends on |
|---|---|
| WIP-00 | — |
| SIZE-01 | WIP-00 local gates/size/UIA checks; owner deferred Narrator on 2026-10-08 |
| REL-03 | WIP-00 local gates/size/UIA checks; owner deferred Narrator for notes preparation only on 2026-10-08 |
| MODEL-01 | — |
| STATE-01 | MODEL-01 |
| APP-01 | STATE-01 |
| CACHE-01 | STATE-01 |
| ERR-01 | MODEL-01 |
| DIAG-01 | ERR-01, APP-01 |
| DIAG-02 | DIAG-01 |
| CODEX-01 | MODEL-01, ERR-01 |
| CODEX-02 | CODEX-01, DIAG-01 |
| A11Y-01 | WIP-00 |
| A11Y-02 | A11Y-01 |
| LAYOUT-01 | WIP-00 |
| LAYOUT-02 | LAYOUT-01 |
| RENDER-01 | — |
| ONBOARD-01 | A11Y-01, ERR-01 |
| ONBOARD-02 | — |
| KEY-01 | A11Y-01 |
| UI-TEST-01 | A11Y-02, LAYOUT-02 |
| PACE-01 | MODEL-01 |
| ROW-01 | MODEL-01 |
| FRESH-01 | STATE-01, APP-01 |
| TRAY-01, TRAY-02, TRAY-03 | APP-01 |
| ALERT-03, ALERT-04 | APP-01 |
| PRIV-03 | — |
| SIGN-01, DIST-01..03 | human provisioning, `v0.12` |
| DOCS-01 | `v0.12` |
| HIST-01, PACE-02 | `v1.0` and the §12 gate |

---

## 16. Decisions

### 16.1 Recorded

| ID | Decision |
|---|---|
| D-01 | Update trust: embedded Ed25519 manifest trust root, plus Authenticode before `v1.0` |
| D-02 | Existing-user trust bootstrap: manual install and verification of the first trust-root release |
| D-03 | Installer and channels: ADR 0001 (portable self-swap; managed installs hand off and never self-modify) |
| D-04 | Concurrency: UI-owned state, short-lived std threads, message wakeup; no Tokio |
| D-05 | Vibecode: wake lock separate; persistent lid override Advanced and journaled |

### 16.2 New in revision 2 (record each as a short ADR under `docs/adr/` when its task starts)

| ID | Decision | Rationale |
|---|---|---|
| D-06 | Pace drives flyout bar color and notes only; tray and alerts keep used-% thresholds | Pace is a projection; alerts must stay predictable |
| D-07 | Source provenance lives in diagnostics, Settings, and UIA, not in the flyout | Users need “is it current?”, not transport details |
| D-08 | First run is an inline flyout card, not a separate Welcome window | Same disclosure, less UI and code |
| D-09 | Claude statusline bridge moves to post-`v1.x` | High complexity; useless when a `statusLine` already exists; data only while a session runs |
| D-10 | History-based pacing is gated on evidence (§12) | Stateless pace answers the question without storage |
| D-11 | Screen-capture exclusion and global shortcut default off | Avoid breaking users' own screenshots and existing shortcuts |
| D-12 | No continuous animation; visible timers repaint ≤ once per 30 s | Footprint and idle-CPU budget |

### 16.3 Scope

- Windows 11 x64/ARM64 through `v1.x`; Windows 10 only after explicit demand and testing.
- Claude + Codex through `v1.0`; at most one more provider in `v1.x`.

---

## 17. Non-goals through `v1.x`

- Cross-platform UI rewrite; Electron, Tauri, WebView, WinUI migration, or managed runtime.
- Runtime provider plugins or a provider marketplace; dozens of providers.
- Browser cookie extraction, web scraping, or terminal-output scraping.
- Owning provider login or token refresh; user-entered API keys.
- Consuming Codex reset credits or Claude reset grants; buying credits.
- Multiple simultaneous accounts per provider.
- Token/cost analytics, spend tiles from local logs, project dashboards, transcript indexing.
- Loopback HTTP API, cloud sync, remote dashboard, email/webhook/mobile alerts, crash upload.
- Custom quiet-hours scheduler, theme/font/color/alert-rule editors, drag-to-reorder customization, undo stacks.
- More power-management features.

---

## 18. Post-`v1.x` candidates

- Opt-in Claude statusline bridge, under revision 1's safety rules (never overwrite or chain an existing `statusLine`; account-correlated samples only).
- A stable `claudometer --json` one-shot snapshot for statuslines, scripts, and agents (reads `state.json`; no server).
- Claude reset-grant count, display only (requires an undocumented query flag; label Compatibility).
- Multiple account profiles with strict partitioning.
- Optional `ccusage --json` interoperability.
- Windows 10 fallback visuals.

---

## 19. Definition of done for `v1.0`

- No open P0/P1 correctness, safety, security, accessibility, install, update, or uninstall issue.
- Old-account data never appears after an identity change, including late completions and failed first fetches.
- Vibecode failure always ends in verified original state or a durable visible recovery state.
- The updater cannot install an unauthenticated executable; interruption at every mutation boundary recovers automatically.
- Codex prefers its documented source where eligible; compatibility paths are labeled in diagnostics and controllable.
- Every failure shows a §5.3 state and has a stable redacted diagnostic.
- Every quota row answers “used/left, when does it reset, am I on pace, is it current” in text and through UIA.
- Narrator, keyboard, High Contrast, text scaling, DPI changes, and minimum work areas pass the UI matrix.
- Claude-only, Codex-only, both, and neither are complete experiences.
- Signed x64/ARM64 portable and per-user installs install, update, roll back, and uninstall cleanly.
- Release evidence is published; §3.1 budgets hold; public docs describe every network request and side effect.

---

## 20. Reference implementations and contracts

Borrow ideas, not scope.

- [OpenUsage](https://github.com/robinebers/openusage) (macOS, Swift): pace-verdict bar colors with an even-pace tick; click-to-flip used/left and countdown/clock; an `Outdated` tag after about two refresh cycles; a per-provider in-flight indicator and `Next update in …` footer; “Not started” sessions; specific errors for inference-only tokens and provider throttling; previous-session cache shown instantly but never treated as fresh; hide-from-screen-share; global shortcut; one behavior doc per surface and one page per provider with Troubleshooting. **Do not copy:** its token refresh and credential write-back, crash telemetry, loopback HTTP API, cloud sync, API-key providers, local-log spend analytics, or the reset-credit claim button.
- [CodexBar](https://github.com/steipete/CodexBar): source provenance, provider contracts, diagnostics, docs structure.
- [Win-CodexBar](https://github.com/nesszer/Win-CodexBar): Windows install, Winget, support diagnostics. Not its WebView or provider breadth.
- [WhereMyTokens](https://github.com/jeongwookie/WhereMyTokens): account-bound normalized state, statusline-first Claude behavior.
- [RateTray](https://github.com/nowrap/rate-tray): Codex app-server integration, explicit thresholds, persistent last-good data.
- [ccusage](https://github.com/ccusage/ccusage): future JSON interoperability only.
- [Claude Code statusline contract](https://code.claude.com/docs/en/statusline).
- [Codex app-server contract](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md).
- [WinVerifyTrust](https://learn.microsoft.com/en-us/windows/win32/api/wintrust/nf-wintrust-winverifytrust).
- [SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity).
- [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).
