# DIAG-02 — local logs and support commands (2026-10-09)

Normal startup enables `diagnostics.log` under APPDATA/Claudometer. Lines are
UTC epoch plus fixed operational code; no arbitrary message is accepted.
Rotation keeps the primary plus `.log.1` and `.log.2`, at most 256 KiB each.
Repeated identical failure codes are suppressed; append/rotation failures are
visible in Diagnostics without recursive logging or altering application state.
Raw provider/transport text, credentials, IDs, account hashes, email, username,
home paths, and response bodies are excluded. Unknown fields are replaced as
a whole by `[redacted]`; known operational fields remain useful.

Rendering initialization/draw, settings persistence, runtime-cache write,
theme/autostart registry access, acrylic, provider failure/success, update,
and clipboard failure paths report fixed codes. No raw error string is logged.
Settings Diagnostics includes last local issue and log-write status; existing
settings/runtime error status remains visible. Demos suppress log side effects.

`--version` and `--diagnose` run before watchdog/demo/normal startup. They write
stdout through inherited handles or parent-console attachment without creating
tray/windows/logs or polling providers/updater. Diagnose loads settings/state
read-only, preserving malformed/missing files without repair or migration.
Identity preparation runs locally in the dedicated support process, which
never creates a UI thread/window. Requests and secrets are dropped without
calling execute; only opaque identity reaches the snapshot. This is an
explicit exception to normal polling's short-lived worker model; it avoids
extra support-only concurrency and remains read-only. Cache may load after
local account/source matching. Support flags exit before other requested
actions. Stdout uses native Windows handles; no binding feature/crate is added.

## Evidence

- Baseline 165 tests passed; final fmt/clippy/**168 tests**/x64 release build.
- Redaction corpus includes bearer/refresh/access tokens, raw account ID,
  email, username, Windows/Unix home paths, account digest, and response JSON.
  Each is absent from snapshot and log-line output; only fixed vocabulary
  survives. Required operational fields remain present.
- Temp-store test exercises five full generations and retains exactly three
  bounded files in correct order. Oversized single writes and missing-parent
  failures are rejected. No sleep, provider network, or credential mutation.
- Isolated corrupt-profile runtime check: redirected version/diagnose stdout
  contain the expected output; two malformed settings/state files remain
  byte-for-byte unchanged and no files are created. Credential/config paths
  all point to the isolated empty profile; no live provider is contacted.
- Demo safety passes; Settings Diagnostics/Copy Invoke/keyboard/UIA HelpText
  and demo clipboard guard pass, plus all 22 existing UIA checks. Screenshots
  reviewed. Local evidence: ignored `target/diag-02/`.
- Privacy/dependency/workflow/artifact policies pass. No state schema change,
  credentials write, endpoint, telemetry, upload, or provider helper.
- The first provisioned artifact was 1,170,944 bytes and failed the cumulative
  10% CI gate (1,164,134 bytes), despite passing the hard ceiling. Formatting,
  fixed-field filtering, stdout, and log IO were reduced without changing
  budgets or checks. Final artifact passes the original gate; discarded size
  experiments are not used as release evidence.
- Final x64: 1,053,184 unprovisioned (+0.44%) / 1,163,776 provisioned
  (+0.40%) vs DIAG-01. Hard-ceiling headroom 146,944 bytes; original CI
  regression margin only 358 bytes. CODEX-01 requires footprint reduction.

Rotation/persistence is proven with temporary stores; normal live startup is
not used as an automated network test. Snapshot/commands expose only current
local state and cache, not a new provider fetch. No ARM64 runtime/Narrator or
new idle-CPU claim. Remaining size budget still needs attention for CODEX-01.

Rollback: prior binary/revert. Logs and rotations are optional and safe to
delete; no configuration or state migration. Never delete credentials or
unresolved power/update journals.

## §0.7 handoff

```text
Task: DIAG-02 — local diagnostics log and support commands
Result: done
Changed: diagnostics/main/app/config/runtime_state/util/updater; plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Gates: fmt ok | clippy ok | test ok (168 passed) | release build ok
Runtime check: support stdout and corrupt-profile no-writes; demo safety; diagnostics UI/keyboard/Copy; 22 UIA checks
Size: x64 1,053,184 unprovisioned (+0.44%) / 1,163,776 provisioned (+0.40%) vs DIAG-01; CI margin 358 bytes
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Deviations from plan: redaction drops whole unknown fields; logs fixed event codes; dedicated support process reads local identity without UI/concurrent worker or execute
Follow-ups: CODEX-01 next; size attention; Narrator deferred
```
