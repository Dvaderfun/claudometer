# PACE-01 footprint — 2026-10-09

Rust 1.97.1, locked dependencies, existing `z`/fat-LTO/one-codegen-unit release
profile. The provisioned measurement uses only the synthetic public Ed25519
test key and sequence 1; it is not an Authenticode-signed artifact.

| Artifact | Bytes | Delta from matching CODEX-01 row |
|---|---:|---:|
| x64 unprovisioned | 1,071,616 | +512 (+0.048%) |
| x64 provisioned | 1,163,776 | 0 (0.00%) |
| ARM64 unprovisioned | 978,432 | no matching CODEX-01 unprovisioned row |
| ARM64 provisioned | 1,017,856 | 0 (0.00%) |

All four artifacts pass the original `ci/check-artifact.ps1` gates: x64
1,164,134-byte cumulative limit, ARM64 1,034,598-byte cumulative limit, and
the 1,310,720-byte hard ceiling. Provisioned x64 retains 358 bytes of cumulative
CI margin and 146,944 bytes of hard-ceiling margin. No budget/profile/baseline
was raised. The remaining roadmap still needs measured size discipline.

The initial build was 1,166,848 provisioned bytes. Shared settings commit and
boolean dispatch, looped boolean encoding, one local reset/run-out formatter,
appended UIA text, and one display label for the quota name/pace note recover
the necessary headroom. The final note uses the existing single-line caption
format; a separate formatted/cloned note string was removed. No dependency,
Windows feature, provider request, timer, or store behavior changed.

Local cargo-bloat analysis forced MSVC debug/PDB generation. Its separate
symbol report found `main` (20.5 KiB), update check (16.6 KiB), isolated Codex
fetch (12.7 KiB), reducer dispatch (11.6 KiB), and accessibility items (5.5 KiB)
as the largest app functions. The pure projection is approximately 411 bytes.
These are attribution evidence, not stripped release sizes.

Runtime reproduction:

```powershell
./ci/measure-baseline.ps1 -ExePath target/pace-01/final.exe -ColdStarts 5 -WarmupSeconds 60 -SampleSeconds 600 -SampleIntervalSeconds 5
```

The script launches separate hidden and visible `--demo=both` processes with
isolated empty profiles, no provider refresh/TLS work, nonce-bound tray readiness,
one-minute warmup, and a full ten-minute sample. Values use nearest-rank p95;
the effective sample spacing includes the Windows counter/query overhead.
This proves controlled idle behavior, not real-provider fetch cost. ARM64 is
cross-built and artifact-checked only; no ARM64 runtime/Narrator claim.

An earlier candidate with the same pace behavior but a separate note string
measured hidden/visible p95 1,703,936/3,530,752 private-working-set bytes,
10/13 GDI handles, 0.0000/0.0000% idle CPU and no TCP connections over ten
minutes. Five starts: 50.237 ms median / 141.674 ms p95. Raw output remains under
ignored `target/pace-01/`.

Final renderer run completed at 18:03:15 UTC on Windows 11 build 28020,
Surface Laptop Studio / i7-11370H / eight logical processors. Ninety-four
samples per process over 600 seconds after the 60-second warmup:

| Metric | Hidden | Visible, two providers | Budget |
|---|---:|---:|---:|
| Private working set, p95 | 1,716,224 bytes (1.64 MiB) | 4,206,592 bytes (4.01 MiB) | 2.5 / 6.0 MiB |
| Idle CPU | 0.0000% | 0.0006% | 0.01% |
| GDI handles, p95/max | 10 | 13 | 16 / 24 |
| TCP connections, max | 0 | 0 | no demo network |

Five-start tray readiness: 59.688 ms median / 97.351 ms p95, below 150 ms.
The five-start series is a focused smoke measurement, not a new 50-start
reference baseline. The earlier candidate and final runs both meet idle budgets.
The final process owns no additional provider workers, storage, or timers.

Rollback: prior binary or disable pace; additive settings schema 1, no migration.
