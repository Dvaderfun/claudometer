# Foundation performance baseline

Measured 2026-09-03 on the uncommitted Foundation working tree after local PR
slices 0–3. The source version remains `0.7.3`; its baseline ancestor is
`5b735ce`. Results are evidence for local development, not signed-release or
ARM64-runtime claims.

## Reference machine

- Microsoft Surface Laptop Studio, 11th Gen Intel Core i7-11370H, 8 logical
  processors, 34,163,216,384 bytes physical memory.
- Windows 11 Pro Insider Preview x64, version `10.0.28020`, build `28020`.
- Rust `1.97.1` (`x86_64-pc-windows-msvc`), release profile from `Cargo.toml`.

## Method

Command:

```powershell
./ci/measure-baseline.ps1 `
  -ExePath target/x86_64-pc-windows-msvc/release/claudometer.exe
```

The executable runs `--demo=both` against an isolated empty profile. This
exercises the real tray icon and Direct2D/DirectComposition flyout without
reading credentials, mutating settings/power/registry, starting provider
workers, or contacting a network. `ci/verify-demo.ps1` separately checks those
side-effect boundaries.

- Tray readiness is a nonce-bound kernel event signaled immediately after
  `Shell_NotifyIcon(NIM_ADD)` returns.
- Startup uses fresh processes and nearest-rank median/p95.
- Hidden and visible processes warm for 60 seconds, then run concurrently for
  600 seconds. Private working set, GDI/USER handles, and active TCP connections
  are sampled every five seconds. CPU is process CPU time divided by wall time
  and eight logical processors.
- Provider refresh work is deliberately excluded. Zero network requests is a
  demo-path invariant; runtime observation also found zero active TCP
  connections in every sample. This is not a packet-capture claim for live
  provider mode.

Acceptable repeat variance is 10% for medians, memory, handles, and artifact
size, and 0.01 CPU percentage point at this measurement resolution. For a
sub-100 ms startup p95, Windows scheduling and antimalware introduce a 25 ms
absolute noise floor; the 500 ms budget still fails without exception.

## Artifacts

| Target | Bytes | SHA-256 | Evidence |
|---|---:|---|---|
| x64 | 826,880 | `06480ceddfcea1f539e69bec7b454620d1e8ca945e93e6246684aff9f507f5be` | Built and measured locally |
| ARM64 | 779,776 | `ebe0f9ae0f95ac619ea8549d55f82774263d176d6faa981a31b1143e6226e866` | Cross-compiled only; no ARM64 runtime claim |

The x64 artifact is 1.44% above the roadmap's 815,104-byte `v0.7.3`
reference and below its 10% investigation threshold and 1.25 MiB ceiling.

## Full ten-minute runs

| Metric | Run 1 | Run 2 | Budget/result |
|---|---:|---:|---|
| Five-start tray median | 55.532 ms | 44.318 ms | ≤500 ms; pass |
| Five-start tray p95 | 65.149 ms | 88.303 ms | ≤500 ms; pass; variance investigated below |
| Hidden private WS p95 | 1,593,344 B | 1,601,536 B | ≤10 MiB; pass |
| Hidden private WS max | 1,593,344 B | 1,601,536 B | informational |
| Visible private WS p95 | 4,661,248 B | 4,661,248 B | ≤15 MiB; pass |
| Visible private WS max | 4,882,432 B | 4,661,248 B | informational |
| Hidden idle CPU | 0.0003% | 0.0000% | <0.1%; pass |
| Visible idle CPU | 0.0013% | 0.0006% | <0.1%; pass |
| Hidden GDI handles p95/max | 10 / 10 | 10 / 10 | stable; pass |
| Visible GDI handles p95/max | 13 / 13 | 13 / 13 | stable; pass |
| Hidden USER handles p95/max | 7 / 7 | 7 / 7 | stable; pass |
| Visible USER handles p95/max | 18 / 18 | 18 / 18 | stable; pass |
| Active TCP connections max | 0 | 0 | zero; pass |
| Samples per hidden/visible state | 101 / 101 | 99 / 99 | 600-second windows |

Hidden p95 differs by 0.51%, visible p95 is identical, and visible median
differs by 1.10%. CPU is below the counter's useful resolution in both runs.

## Startup-variance investigation

The five-start p95 values differed by more than 10% because nearest-rank p95
selects the single slowest launch. Two additional independent 50-start samples
were collected:

| Startup sample | Median | p95 | Maximum |
|---|---:|---:|---:|
| Investigation run 1 (50 starts) | 47.077 ms | 59.775 ms | 90.393 ms |
| Investigation run 2 (50 starts) | 50.387 ms | 76.551 ms | 92.182 ms |

The medians differ by 7.03%. The p95 difference is 16.776 ms and stays within
the documented 25 ms scheduler noise floor; all 100 launches are under 103 ms
and well inside the 500 ms hard acceptance budget.

## Reproduction and limitations

- `ci/measure-baseline.ps1` accepts explicit run/warm-up/sample intervals but
  defaults to the release method above.
- `ci/check-artifact.ps1` enforces the checked baseline, 10% growth trigger,
  and 1.25 MiB ceiling in CI.
- ARM64 compile and artifact size are verified. ARM64 memory, startup, UI, and
  hardware behavior remain unverified until real ARM64 capacity is available.
- Installer size and signed-artifact size do not exist yet and are intentionally
  not claimed here.
