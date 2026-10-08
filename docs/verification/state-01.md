# STATE-01 — pure provider reducer (2026-10-09)

`provider/state.rs` absorbs `state_policy.rs` and adds `ProviderState`, the
§4.4 phases/events, identity-bound request tickets, explicit transitions, and
all eight required derived views. Time enters through `Clock`; the reducer
does no network, disk, registry, process, rendering, or alert work. Account
types retain their lack of Debug/serialization of secret material.

`AcceptedSuccess` is the only fresh-fetch effect eligible for alerts and
sanitized snapshot persistence. A 429 effect carries the wall-clock retry
deadline; manual refresh returns the next allowed time. Obsolete completions
return Ignored before changing any field. Credential changes, disablement,
and authentication failure clear account-bound state. Same-account transient
failures retain data and derive Outdated at max(two poll intervals, ten
minutes). Cached snapshots keep their real age and never prevent a first
fetch; late cache reads cannot overwrite live data or a running request.

The existing SLOTS runtime shell imports the moved policy helpers. Connecting
the reducer to workers/UI belongs to APP-01; persistence belongs to CACHE-01.
The legacy shell's ten-minute error-preserved display cutoff remains intact.
The reducer adds only the failure classes needed for transitions; ERR-01
will supply stable adapter categories/codes and detect HTTP authentication
failures without interpreting message strings.

## Evidence

- All 118 original tests passed before structural edits. An added refresh-gate
  precedence characterization passed before moving the policy module.
- The §4.4 table covers all ten event/result rows. Detailed tests check every
  completion envelope field, duplicate success, disable/reenable, same-account
  credential replacement, cached identity/age, refresh gates, 429 cooldown,
  authentication clearing, and exact age boundaries/extreme wall times.
- 128 fixed seeds × 96 event steps exercise arbitrary event orders with old
  and duplicate worker tickets. Ignored events preserve state; snapshots
  always match the current account. No sleeps or provider IO are used.
- Required gates: fmt, clippy, 131 tests, x64 release build. Privacy,
  dependency, and workflow policy pass. Demo safety passes in unprovisioned
  and synthetic-trust-root build modes.
- 22 local UIA client checks pass; the both-provider and Settings screenshots
  retain intact layout/copy. Evidence: ignored `target/state-01/uia/`.
- x64: 950,784 bytes unprovisioned / 1,061,376 provisioned, both unchanged
  from MODEL-01 and passing architecture/version/size/regression checks.
  The same RFC 8032 public key/sequence and `z` profile are used. The pure
  reducer is not runtime-connected yet and is stripped by release LTO.
- No crate, package-version change, durable field, network destination,
  credential operation, or system side effect. PRIVACY remains accurate.

Rollback: revert STATE-01 or use the prior binary. No persisted schema or
settings migration; never change provider credentials or recovery journals.

## §0.7 handoff

```text
Task: STATE-01 — pure provider reducer
Result: done
Changed: src/provider/state.rs, src/provider/mod.rs, src/main.rs, removed src/state_policy.rs; roadmap, CHANGELOG, ARCHITECTURE, ledger, verification
Gates: fmt ok | clippy ok | test ok (131 passed) | release build ok
Runtime check: demo safety passed in both x64 modes; 22 UIA checks and screenshot review passed
Size: x64 950,784 unprovisioned / 1,061,376 provisioned (0.00% vs MODEL-01)
Docs updated: plan, CHANGELOG, ARCHITECTURE, ledger, verification; PRIVACY unchanged (no side effect)
Deviations from plan: reducer effect contracts defer actual IO/runtime ownership to CACHE-01/APP-01; stable error codes remain ERR-01
Follow-ups: APP-01 next; then CACHE-01; Narrator remains owner-deferred
```
