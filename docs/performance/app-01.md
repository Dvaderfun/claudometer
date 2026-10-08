# APP-01 runtime baseline (2026-10-09)

Surface Laptop Studio, Windows 11 build 28020, i7-11370H, eight logical
processors. Rust 1.97.1, locked dependencies, `z`/fat-LTO release profile,
same RFC 8032 synthetic public trust root/sequence as STATE-01.

Command: `ci/measure-baseline.ps1 -ExePath target/app-01/claudometer-final-provisioned.exe -ColdStarts 10 -WarmupSeconds 60 -SampleSeconds 600 -SampleIntervalSeconds 5`.
Raw result: [app-01-runtime.json](app-01-runtime.json).

The reference artifact is 1,074,688 bytes, SHA-256
`0c20be2b7e8ed9687cd9ede9d1da44eb11afd862817457fdfd00a30db24cbf89`.
The last worker-only change avoids resetting same-account alert state after
a transient preparation failure; its release artifact remains 1,074,688
bytes, hash `a27eaa52cd4a386802573ca73802679d795d1995e65ed31bbc287807850230ff`.
That path never executes in demo mode; demo/runtime/rendering source is
unchanged between these two artifacts. Final gates and artifact checks use
the latter, while the retained full idle run uses the reference artifact.

After a 60-second warmup, hidden and visible demo processes run for 600
seconds. 104 samples per process were collected at five-second intervals
plus CIM/TCP query overhead. Values use nearest-rank percentiles. Process
CPU is divided by measured wall time and eight logical processors. The mode
uses an empty isolated profile and excludes provider refresh/TLS work.

| Metric | Hidden | Visible, two providers | Budget |
|---|---:|---:|---|
| p95 private working set | 1,687,552 bytes (1.61 MiB) | 4,976,640 bytes (4.75 MiB) | ≤2.5 / ≤6.0 MiB |
| Maximum private working set | 1,687,552 | 5,099,520 | — |
| Idle CPU | 0.0000% | 0.0045% | ≤0.01% |
| GDI handles p95/max | 10/10 | 13/13 | ≤16 / ≤24 |
| USER handles p95/max | 7/7 | 18/21 | — |
| Active TCP connections maximum | 0 | 0 | demo invariant |

Ten fresh-process tray-readiness samples: median 38.062 ms, p95 80.454 ms,
below 150 ms. This ten-start check does not replace SIZE-01's fifty-start
profile comparison. Zero CPU is rounded to four decimal places, not proof
that the process never consumed a cycle.

Earlier overlapping measurements exited before completing their sample;
one UIA attempt also lost its Settings process. Windows had no matching
application crash record, and a UIA repeat passed. Those incomplete attempts
are excluded; their exit cause is unestablished. The accepted full run was
isolated from UI automation and survived the entire interval.

All runtime budgets pass. No real ARM64 runtime or Narrator claim. Rollback:
use the STATE-01 binary; no persisted schema change or journal operation.
