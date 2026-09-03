# Account and generation isolation

Provider credentials remain worker-local. Claude Code and Codex are the only
durable credential owners; Claudometer never writes their files or serializes a
token, raw account ID, email address, or organization name.

## Identity derivation

- Prefer the provider's stable account identifier (`accountUuid` for Claude,
  `account_id` for Codex).
- Hash domain + provider + identity kind + 32-byte install salt + identifier
  through Windows CNG SHA-256.
- If no stable identifier exists, hash the access token with a process-only CNG
  salt. That context is explicitly memory-only and cannot be persisted.
- `AccountKey` has no `Display` or `Debug`; secret strings have no serialization
  implementation and overwrite their buffers before deallocation.

## Acceptance boundary

Each provider slot owns an account context, monotonically changing generation,
reserved request ID, and pending request. A worker must match all four fields:

```text
provider + generation + request_id + account_key
```

The match occurs before snapshot, last-good, plan, error, retry streak,
cooldown, alert receipt, render wakeup, or fetching-state mutation. Explicit
login invalidates the old generation before starting replacement work. A
credential preparation failure classified as missing/malformed/unsupported
also clears the account; a transient atomic-replacement read failure retains
same-account stale data only.

Tests cover account switch followed by failed first fetch, obsolete 429
completion during a newer fetch, provider isolation, plan-cache mismatch,
one-time legacy alert migration, sign-out invalidation, and memory-only receipt
exclusion. No test contacts a provider endpoint.

Rollback keeps the legacy receipt map dual-written for two releases. Older
binaries ignore `state.json`; newer binaries never reassign a legacy receipt
after its provider migration marker is committed.
