# CACHE-01 — sanitized runtime cache (2026-10-09)

Optional `provider_cache` version 1 extends state schema 1. Each of at most
two entries contains provider/account/source, an optional normalized snapshot,
and a retry deadline. Snapshot fields are plan, observation time, and bounded
typed quota rows. Raw bodies, access/refresh tokens, raw account IDs, and
formatted reset strings are absent. Memory-only account fingerprints cannot
restore or persist. Existing salt/receipts and unknown state fields survive.

Worker preparation establishes the current opaque account before any cache
loads. Only matching provider/account/source may restore. Values retain their
observation time and a Cached values footer note; cache loading never emits
accepted success or alerts. The first HTTP request after restart always runs
unless a still-valid provider deadline blocks the fetch ticket. Restored
cooldown uses an injected wall/monotonic clock and reports Paused by provider
plus the next allowed time. Accepted completion is the sole snapshot/retry
write boundary; obsolete completion/preparation cannot write or load cache.

Cache validation rejects duplicate providers/limit IDs, more than 64 rows,
strings above 512 UTF-8 bytes, IDs above 128 bytes, invalid finite percent
ranges, inconsistent identity/source/class, invalid timestamps, and durations
outside 1 second–366 days. Future snapshots and snapshots older than eight
days are ignored; reset-expired rows are omitted, keeping reset-free limits.
Expired/implausible retry deadlines are ignored; live deadlines are bounded
by the existing 900-second policy. No error body/message is serialized.

Malformed optional cache does not poison the salt/receipt envelope. Unknown
future cache versions remain read-only. Atomic writes use the existing store;
failure preserves previous disk and in-memory generations and exposes the
runtime-state error in existing diagnostics. A surviving verified backup can
restore a deleted primary; a complete cache reset removes both primary and
backup. Provider credentials and safety/update journals are never removed.

## Evidence

- Baseline 140 tests passed before changes. Final gates: fmt, clippy,
  **151 tests**, x64 release build. Existing parser fixtures are unchanged.
- Tests cover persisted round trip, identity/source mismatch, memory-only
  identity rejection, restart during/after backoff, first-fetch behavior,
  true cache age, reset/age/timestamp boundaries, malformed/bounded payloads,
  missing/corrupt state, failed writes, old reader/writer preservation,
  unknown payload fields excluded from normalized writes, and obsolete events.
  No test uses live provider endpoints, credential mutation, or sleeps.
- Demo safety passes in both x64 trust-root modes. 22 UIA client checks and
  screenshot review pass; demo modes retain existing synthetic rendering and
  perform no writes. Local evidence: ignored `target/cache-01/uia/`.
- Privacy/dependency/workflow policies and x64 PE architecture/version/size
  checks pass. No dependency or package-version change.
- x64 sizes: 998,912 unprovisioned (+3.61%) / 1,109,504 provisioned (+3.24%),
  both +34,816 bytes versus APP-01. Provisioned hard-ceiling headroom is
  201,216 bytes; remaining v0.10 work needs size attention because the initial
  milestone allowance is exceeded. CI/soft/hard budgets are unchanged.
- Actual storage and restart transitions are proven with temp stores and
  fake-clock/synthetic-account tests; demo safety itself does not exercise
  cache writes. HTTP error-category/authentication expansion remains ERR-01.

Rollback: prior binary/revert. State schema stays 1; older readers ignore the
cache and their existing raw-map writer preserves it. Optional cache can be
deleted with the state store/backup, losing cache/dedup only. Do not touch
provider credentials or unresolved recovery journals.

## §0.7 handoff

```text
Task: CACHE-01 — sanitized runtime cache
Result: done
Changed: src/runtime_state.rs, src/provider/model.rs, src/provider/state.rs, src/app.rs, src/main.rs; plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Gates: fmt ok | clippy ok | test ok (151 passed) | release build ok
Runtime check: demo safety, 22 UIA checks, screenshot review; temp-store/fake-clock restart and cache tests
Size: x64 998,912 unprovisioned (+3.61%) / 1,109,504 provisioned (+3.24%) vs APP-01
Docs updated: plan, CHANGELOG, PRIVACY, CLAUDE, ARCHITECTURE, ledger, verification
Deviations from plan: schema remains 1 with a separately versioned additive cache; compatibility selected source remains the current adapter; restore waits for worker-local identity preparation
Follow-ups: ERR-01 next; richer freshness presentation remains FRESH-01; Narrator deferred
```
