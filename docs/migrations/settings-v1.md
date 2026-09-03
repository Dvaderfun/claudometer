# Settings v1 migration and rollback

## Forward path

At normal startup, `config.rs` loads `settings.json` once through
`AtomicJsonStore` and validates it into `SettingsV1`. Missing settings use
defaults in memory and do not create a file until a preference changes.

An unversioned v0.7.x object is expanded atomically with these canonical keys:

| Canonical v1 | Legacy compatibility key | Default |
|---|---|---|
| `poll_interval_seconds` | `poll_secs` | `60` (validated to 30–300) |
| `codex_enabled` | `show_codex` | `true` |
| `alerts_enabled` | `alerts` | `true` |
| `wake_lock_enabled` | `vibecode` | `false` |

`persistent_lid_override_enabled` is an additive v1-only Advanced preference;
old binaries ignore it. It is never treated as evidence that the override is
active—the power journal and read-back verification are authoritative.

`schema_version: 1` and both columns are written on every successful settings
change. New readers prefer the canonical key and fall back to the legacy key.
Legacy unversioned reads remain supported indefinitely. Legacy writes remain
through at least v0.8 and v0.9; contraction is forbidden before a later
roadmap slice records successful downgrade evidence.

Unknown fields are copied unchanged. In particular, `alerted` remains available
for the state-file migration and `vibecode_lid` remains untouched for the
conservative power-recovery migration.

## Failure and corruption behavior

- The in-memory settings snapshot changes only after the atomic save succeeds.
- A failed create/write/flush/replace leaves the previous valid file and
  in-memory snapshot intact; Settings shows a safe stage-only diagnostic.
- Malformed JSON/non-object input is moved to a timestamped `.corrupt` sibling
  before defaults are used. A verified `.bak` is restored when available.
- `schema_version > 1` is parsed for safe known preferences but remains
  read-only. No byte in the future document is rewritten.
- An invalid schema field is also read-only rather than guessed.

## Rollback proof

The migration test reads a v0.7.x document, writes v1, and asserts that every
legacy key still contains the same validated value. A v0.7.3 binary therefore
continues to read poll/Codex/alert/wake-lock preferences and safely ignores the
canonical keys and `schema_version`. Unknown fields and legacy recovery/alert
data survive the round trip.

Rolling back the executable requires no reverse migration. Do not delete the
legacy keys during rollback. Deleting `settings.json` remains safe for ordinary
preferences, but a file containing `vibecode_lid` must instead follow the
documented legacy recovery flow.
