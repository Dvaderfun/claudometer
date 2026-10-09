# CODEX-02: default-source gate (2026-10-09)

Decision: keep app-server opt-in. All final live requests succeed, but the
nearest-rank p95 is **2,198 ms**, above the **2,000 ms** promotion threshold.
No default preference was changed. Settings exposes the explicit toggle.

Reference machine: Windows 11 build 28020, AMD64. Native installed Codex
0.159.1 (exact audited hash). Each sample uses a fresh isolated child/profile;
timing includes credential read, native image eligibility/hash, startup,
external auth, quota read, tree termination, and scratch cleanup. The actual
default signed-in account was used only for these owner-approved manual
measurements. No live measurement is part of `cargo test` or CI.

Final sample milliseconds:

```text
1684, 1971, 1847, 1692, 1580, 2048, 1644, 2122, 2198, 1628
```

Median **1,769.5 ms**. For N=10, nearest-rank p95 is sample ceil(0.95*N)=10
of sorted values. This is a small reference sample, not a general latency
guarantee. Earlier reader-thread series is retained: 1,715.5 ms median,
2,730 ms p95; the opt-in conclusion is consistent.

Credentials are SHA-256 identical before/after the series; hashes and values
are not published. Zero owned app-server children and scratch profiles remain.
Successful app-server polling does not execute Claudometer's direct WHAM
request; source selection is made before execution. The app-server itself
contacts the fixed ChatGPT usage/account-discovery backend under ADR 0007.
Fake lifecycle tests establish no second-source retry on an app-server error.

Reproduce manually, only with owner authorization and a signed-in Codex profile:

```powershell
.\target\x86_64-pc-windows-msvc\release\claudometer.exe --measure-codex-source
```

Redirect stdout to collect the fixed JSON timing/result. The command never
prints credentials, paths, account IDs, quota labels/plan, or response bodies.
It bypasses normal startup, state migration, windows, tray, alerts, and updater.
Never use fake-process timings to promote the source. New latency evidence
must include complete child/profile cleanup and unchanged credentials.

Rollback: app-server preference stays false by default. Select Compatibility
or revert CODEX-01; no provider credential or release-policy change.

```text
Task: CODEX-02 — default-source gate
Result: done (opt-in retained after failed latency promotion gate)
Changed: manual measurement command, measurement evidence, plan
Gates: shared CODEX-01 fmt/clippy/174 tests/build/demo/artifact policies
Runtime check: ten final real samples, all success; credential unchanged; zero owned children/scratch
Size: shared CODEX-01 artifact ledger
Docs updated: plan, performance, verification
Deviations: clarified no direct Compatibility request; official app-server necessarily contacts ChatGPT
Follow-ups: improve end-to-end p95 before promoting the default
```
