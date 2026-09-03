# Runtime state v1 envelope

`state.json` is optional, sanitized runtime state. It is deliberately separate
from user preferences and from safety-critical power/update journals. Deleting
it is safe: cached/deduplicated state is lost and a new install salt is created.

Initial shape:

```json
{
  "schema_version": 1,
  "install_salt": "64 lowercase hexadecimal characters",
  "alert_receipts": [],
  "legacy_alert_migrations": []
}
```

The 32-byte salt comes from Windows CNG `BCryptGenRandom` with the system
preferred RNG. It enters memory only after the atomic file commit succeeds.
Failure to obtain entropy or persist the file disables account-key persistence
and produces a safe visible diagnostic; no weak fallback is generated.

The receipt schema is provider + opaque account key + stable limit ID +
threshold + reset instance + below-threshold observation. On the first stable
account observation for a provider, compatible legacy `settings.json.alerted`
entries migrate once and the provider is added to `legacy_alert_migrations`.
That marker prevents ambiguous legacy receipts from following a later account.
Legacy entries continue to be dual-written for v0.7.3 downgrade compatibility,
but new code reads account-scoped receipts only.

An account switch atomically removes the previous account's receipts for that
provider before its replacement fetch. A token-fingerprint fallback is marked
memory-only and is never written to `state.json`.

Malformed JSON is preserved as a timestamped `.corrupt` file before a fresh
state is created. A verified `.bak` is restored first when present. A future
schema is left untouched and disables cached state rather than being
overwritten. Unknown current-schema fields are preserved by envelope extension.

Rollback requires no reverse migration: v0.7.3 ignores `state.json` and still
reads its legacy receipt map from `settings.json`. Removing `state.json` never
removes settings or either safety journal.
