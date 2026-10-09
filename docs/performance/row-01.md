# ROW-01 footprint — 2026-10-10

Rust 1.97.1, locked graph and existing z/fat-LTO release profile. Provisioned
artifacts use the RFC 8032 synthetic public key and sequence 1; no signing or
production provisioning occurred.

| Artifact | Bytes | Delta from PACE-01 |
|---|---:|---:|
| x64 unprovisioned | 1,071,104 | -512 (-0.048%) |
| x64 synthetic-root provisioned | 1,163,776 | 0 (0.00%) |
| ARM64 unprovisioned | 978,432 | 0 (0.00%) |
| ARM64 synthetic-root provisioned | 1,018,368 | +512 (+0.050%) |

Original cumulative artifact gates remain 1,164,134 x64 / 1,034,598 ARM64,
and the hard ceiling stays 1,310,720. Final x64 has 358 bytes of cumulative
margin and 146,944 bytes of hard-ceiling margin. No baseline or budget changed.

The first resumed provisioned candidate was 1,168,384 bytes and failed the
cumulative gate by 4,250. Shared font/brush creation, UTF-16 draw/alignment,
Settings button rendering, live/demo render dispatch and one Settings view
for rendering/UIA recover the necessary margin. Existing schema/store and
provider/alert decisions remain intact. Several inlining/array/formatting
experiments provided no size benefit and were removed. cargo-bloat output is
debug/PDB analysis only; normal stripped builds supply every ledger size.

Runtime reproduction:

```powershell
./ci/measure-baseline.ps1 -ExePath target/row-01/final.exe -ColdStarts 5 -WarmupSeconds 60 -SampleSeconds 600 -SampleIntervalSeconds 5
```

Separate hidden/visible Both demos use empty isolated profiles, nonce-bound
tray readiness, one-minute warmup and ten-minute samples. This excludes provider/TLS work and is x64
runtime proof; ARM64 is cross-built only. Five starts are smoke measurement,
not a replacement for the fifty-start Foundation baseline.

An earlier 1,167,360-byte candidate, before final size consolidation, passed
ten-minute budgets: hidden/visible p95 1,708,032/5,079,040 private-working-set
bytes, CPU 0.0000/0.0032%, GDI 10/13, zero TCP; five-start median/p95
37.384/82.880 ms. Final measurement uses the provisioned 1,163,776-byte binary.
Raw JSON is under ignored target/row-01.

Final ten-minute results on Windows 11 build 28020 / Surface Laptop Studio:

| Metric | Hidden | Visible, two providers | Budget |
|---|---:|---:|---:|
| Private working set, p95 | 1,691,648 bytes (1.61 MiB) | 5,185,536 bytes (4.95 MiB) | 2.5 / 6 MiB |
| Idle CPU | 0.0000% | 0.0013% | 0.01% |
| GDI handles, p95/max | 10 | 13 | 16 / 24 |
| TCP connections, max | 0 | 0 | no demo network |

Startup median/p95: 37.531 / 67.446 ms. Each process has 107 samples.
Final timer captures independently show
countdown progression across 66 seconds; hide/reopen and source guards pass.

Rollback: Clock/Used defaults or previous binary; additive settings schema 1.
