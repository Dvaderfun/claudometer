# APP-01 — UI-owned provider state (2026-10-09)

`app.rs` owns both provider reducers and pending preparation on the UI thread.
`poller.rs` owns short-lived worker execution and the AppEvent queue, woken by
WM_DATA_READY. All runtime provider mutex/atomic SLOTS state is removed from
`main.rs`, which shrinks from 2,958 to 2,125 lines.

Workers prepare credentials locally, publish only opaque account identity,
then wait up to ten seconds for an accepted request ticket. Preparation is
bound to a generation and operation ID; invalidated/disabled operations
cannot start HTTP work. Completion acceptance checks the reducer's full
provider/generation/request/account/snapshot envelope. Only accepted success
drives alerts, after releasing the APP borrow. Duplicates/obsolete completions
cannot alert, clear pending requests, change cooldowns, or trigger redraws.
Failed worker startup and abandoned replies do not execute a request. If
Windows rejects the wakeup, the event remains queued for the polling tick;
completed workers cannot leave a provider stuck in Fetching. No persistent
worker/helper or new dependency is added.

The existing stale/error presentation is pinned for APP-01 parity. An old
snapshot survives in the reducer but error-preserved values are displayed
for the characterized ten-minute window. A failed attempt remains visible
while the next fetch runs. FRESH-01 adopts richer freshness copy later.
`state_reference.rs` is test-only: it preserves original characterization
and supplies direct comparisons against the old implementation.

## Evidence

- All original characterization tests still pass. Direct comparisons prove
  success/error display, backoff, refresh timing, and stale boundaries match
  the old implementation. Additional tests cover independent providers,
  invalidated preparation, obsolete success, duplicate alert candidates,
  transient/missing credential failures, and worker ticket acceptance.
- fmt, clippy, 140 tests, x64 release build pass. Privacy/dependency/workflow
  policies pass without weakening checks. Demo safety passes in both x64
  trust-root modes. No automated test calls a live provider endpoint.
- All ten demo scenarios have zero differing pixels in before/after window
  captures. 22 UIA client checks pass. Ignored evidence:
  `target/app-01/demo-comparison.json`, PNGs, and `target/app-01/uia/`.
- A subsequent UIA attempt lost its Settings process after capture; a repeat
  passed all 22 checks and Windows had no matching application crash record.
  Earlier concurrent idle measurements also lost demo processes and are
  excluded. The accepted idle run is isolated from UI automation; the cause
  of those process exits is not established.
- x64: 964,096 bytes unprovisioned (+13,312 / +1.40%), 1,074,688 provisioned
  (+13,312 / +1.25%). PE architecture/version/size/regression checks pass.
  The latter has 236,032 bytes of hard-ceiling headroom. Same `z` profile,
  RFC 8032 synthetic public trust root and sequence as STATE-01.
- Ten-minute idle runtime evidence is recorded separately in
  `docs/performance/app-01.md`; no ARM64 runtime or Narrator claim.

Rollback: revert APP-01 or run the STATE-01 binary. No schema/version change;
settings, state.json, and alert receipts remain compatible. Do not modify
provider credentials or safety/update journals for rollback.

## §0.7 handoff

```text
Task: APP-01 — UI-owned provider state
Result: done
Changed: src/app.rs, src/poller.rs, src/main.rs, src/provider/state.rs, src/state_reference.rs; plan, CHANGELOG, ARCHITECTURE, PRIVACY, CLAUDE, ledger, verification/performance
Gates: fmt ok | clippy ok | test ok (140 passed) | release build ok
Runtime check: ten pixel-identical demo scenarios; 22 UIA checks; demo safety; ten-minute idle baseline
Size: x64 964,096 unprovisioned (+1.40%) / 1,074,688 provisioned (+1.25%) vs STATE-01
Docs updated: plan, CHANGELOG, ARCHITECTURE, PRIVACY, CLAUDE, ledger, verification, performance
Deviations from plan: original slot logic retained only as a cfg(test) parity oracle; richer freshness presentation remains FRESH-01
Follow-ups: CACHE-01 next; ERR-01 supplies stable adapter error categories; Narrator remains owner-deferred
```
