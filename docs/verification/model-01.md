# MODEL-01 — normalized provider model (2026-10-08)

Shared `UsageSnapshot`, `UsageLimit`, and `FetchOutcome` move from `api.rs` to
`provider/model.rs`. Snapshots embed provider/account/source identity; limits
carry stable IDs, typed kind/class/severity, validated percentages, reset epoch,
and optional duration. Plan is optional. Existing field names `rows`,
`percent`, `fetched_unix`, `severity`, and `resets_unix` remain to reduce churn;
their types implement the §4.3 contract.

`gfx::LimitRow` contains the formatted presentation fields; snapshots contain
no formatted reset text. Reset formatting uses the existing Windows target-
instant timezone conversion. The renderer and UIA consume the same view.
Alerts skip typed Spend rows, use stable IDs, and verify snapshot account/
provider identity. The completion boundary rejects embedded identity mismatches
before changing last-good state, cooldowns, or pending fetch state.

Provider requests, login behavior, response bounds, refresh policy, settings,
and recovery journals are preserved. Adapters parse raw kind/severity strings;
downstream policy compares enums. Missing Codex duration retains its legacy
caption but remains Other in the model; it is not classified by primary/
secondary position. A weekly-only 168-hour fixture produces one Weekly row.
Existing short limit identities remain unchanged for alert compatibility;
long/empty unknown identities use a bounded SHA-256 digest independent of label.
Error-category expansion remains the separately queued ERR-01 task.

## Evidence

- A weekly-only/missing-duration characterization passed before restructuring.
  Fixture JSON files remain byte-for-byte unchanged. Fixture assertions now
  reference typed kinds/Percent and include provider/account/provenance/duration.
- Percent tests reject NaN/infinity and clamp finite inputs once. The old
  standalone clamp helper/test is replaced by these domain tests, with unchanged
  finite out-of-range fixture results. JSON non-finite input remains rejected.
- Stable identity/Spend and mismatched snapshot-envelope tests pass alongside
  every existing state, persistence, recovery, and signature test.
- Gates run individually in §0.3 order: fmt, clippy, **118 tests**, x64 release
  build. Provisioned x64 and ARM64 release builds pass artifact checks. No test
  called a live provider endpoint or mutated provider credentials.
- Demo safety passes; 22 UIA checks pass for `--demo both` / `--demo settings`.
  Reviewed screenshots preserve labels, percentages, reset text, and intact
  layout. Local evidence is in ignored `target/model-01/uia/`.
- Privacy/dependency/workflow policies pass. No dependency or package-version
  change, durable data field, network site, process, or system effect is added.
- Sizes: x64 950,784 bytes unprovisioned / 1,061,376 provisioned; ARM64 942,592
  provisioned. Same synthetic public key/sequence and profile as SIZE-01;
  deltas +0.32% / +0.29% x64 and +0.22% ARM64. Real ARM64 runtime and Narrator
  remain unverified/deferred, respectively.

Rollback: revert MODEL-01 or run the prior binary. No persisted snapshot schema
or settings migration is introduced; alert receipt formats remain compatible.
Do not modify provider credentials or recovery journals for rollback.

## §0.7 handoff

```text
Task: MODEL-01 — normalized provider model
Result: done
Changed: src/provider/model.rs, src/api.rs, src/codex.rs, src/main.rs, src/alerts.rs, src/gfx.rs, src/demo.rs, src/util.rs; plan, CHANGELOG, ARCHITECTURE, ledger, verification report
Gates: fmt ok | clippy ok | test ok (118 passed) | release build ok (x64; provisioned x64/ARM64 artifact checks ok)
Runtime check: demo safety, 22 UIA checks, screenshot review passed; requests/fixtures preserved
Size: x64 1,061,376 bytes (+0.29% vs SIZE-01 provisioned); unprovisioned 950,784 (+0.32%); ARM64 provisioned 942,592 (+0.22%)
Docs updated: plan checkbox/board, CHANGELOG, ARCHITECTURE, ledger, verification; PRIVACY unchanged (no new side effect)
Deviations from plan: existing field names retained with typed values; adapter-only raw string interpretation; missing Codex duration preserves caption with unknown typed kind; ERR-01 handles error categories separately
Follow-ups: STATE-01 next; deferred A11Y work; human-only release signing/provisioning/runtime prerequisites
```
