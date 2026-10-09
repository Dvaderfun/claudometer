# ADR 0007: Isolated Codex external-token quota reads

- Status: Accepted for implementation; default-source promotion requires CODEX-02 measurements.
- Date: 2026-10-09
- Tasks: CODEX-01, CODEX-02

The owner explicitly approved three exceptions in this session: private stdio
handoff of the existing access token to a short-lived isolated native Codex
app-server; read-only account discovery/cloud configuration and temporary
account-ID caching; and manual live timing using the existing signed-in account.
No credential refresh, token exchange, credential-file write, tag, release,
repository setting, or secret provisioning is authorized.

Use experimental `chatgptAuthTokens`, with only an access token and the prepared
account ID. Supply the local plan hint `unknown`, so the audited upstream
cloud-config eligibility check never loads enterprise configuration. Display
the authoritative plan from the quota response instead. Never supply a refresh
token, API key, or managed auth file. Refuse
all server requests, including external refresh, and stop the cycle. Never call
managed login, logout, turns, credit consumption, or arbitrary RPC methods.

Use a fresh scratch profile, a cleared environment, ephemeral auth storage,
disabled analytics/OTEL/plugins/remote control, a static synthetic model catalog,
and a native executable whose exact audited x64 bytes match the accepted list.
Machine configuration must be absent; other versions/architectures use the
separately labeled compatibility source until independently audited. Changes
to the accepted list require renewed source/protocol/side-effect evidence.

Assign the child to a kill-on-close Windows Job Object before it can execute.
Bound requests/frames/notifications; kill its tree on every exit, then remove
the scratch profile. Cleanup failures are operational errors. Process crashes
can leave scratch data; subsequent normal polls remove only owned scratch
directories, without following reparse points. Never touch provider-owned files.

Source selection occurs before a provider request and is included in the
account-bound UI handoff/cache matching. App-server failure never triggers a
second provider request to the compatibility endpoint in that cycle. Generic
upstream RPC messages are discarded, never classified by their text or logged.

Settings exposes an app-server preference; it may be enabled by default only
after real reference-machine p95 is at most two seconds with verified child and
scratch cleanup. Manual measurement is separate from automated fixture tests.

Rollback: revert code/default preference or select Compatibility in Settings.
Optional source fields remain additive in settings schema 1; cached data from
another selected source is never restored. Existing credential restrictions and
the hard footprint ceiling remain binding.
