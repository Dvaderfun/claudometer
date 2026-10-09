# FRESH-01 footprint — 2026-10-10

Rust 1.97.1, locked dependency graph, existing z/fat-LTO release profile.
Provisioned artifacts use the RFC 8032 synthetic public key and sequence 1.
No production provisioning, signing or publication occurred.

| Artifact | Bytes | Delta from ROW-01 |
|---|---:|---:|
| x64 unprovisioned | 1,071,104 | 0 (0.00%) |
| x64 synthetic-root provisioned | 1,163,776 | 0 (0.00%) |
| ARM64 unprovisioned | 978,944 | +512 (+0.052%) |
| ARM64 synthetic-root provisioned | 1,018,880 | +512 (+0.050%) |

Original cumulative artifact gates remain 1,164,134 x64 / 1,034,598 ARM64;
the hard ceiling stays 1,310,720. Final x64 retains 358 bytes of cumulative
margin and 146,944 bytes of hard-ceiling margin. No crate/features/profile/
budget/baseline changed. Replacing duplicated legacy flyout assembly with the
reducer projection pays for the added footer/status/accessibility presentation.
ARM64 measurements are cross-build only.

Final artifact hashes (SHA-256):

- x64 unprovisioned: 69DA635674FB776BFB51A6E32EB955B00E304307C67FD37DE81E305579600921
- x64 provisioned: 2CEBD6A7B1777C7E86341A8FFF884B59DC6CE040AAA7D28AD67F576ECC09D200
- ARM64 provisioned: 65D33AB47EEDD35775B1B0C216047D286309BFCBFAEC0083DEABA9BB020276A4

Runtime reproduction:

```powershell
./ci/measure-baseline.ps1 -ExePath target/fresh-01/final-v3.exe -ColdStarts 5 -WarmupSeconds 60 -SampleSeconds 600 -SampleIntervalSeconds 5
```

Separate hidden/visible Both demos use empty isolated profiles, nonce-bound
tray readiness, a one-minute warmup and ten-minute samples. Provider/TLS work
is excluded. Five starts are smoke measurement rather than a replacement for
the fifty-start Foundation baseline. No live provider endpoint was called.

An initial 1,163,776-byte candidate passed ten-minute budgets: hidden/visible
p95 private working set 1,671,168 / 5,160,960 bytes, CPU 0.0003 / 0.0045%,
GDI 10/13, no TCP. Startup median/p95 was 49.707/117.322 ms. The final run
below includes cached caption/UIA age and the eight-pixel footer focus gap.
Raw reports/captures remain under ignored target/fresh-01/.

Rollback: previous binary; settings/state schemas and provider behavior remain
compatible. No credential, cache migration or safety-journal action is needed.


Final ten-minute x64 results (99 samples, Windows 11 build 28020):

| Metric | Hidden-requested process | Visible, two providers | Budget |
|---|---:|---:|---:|
| Private working set, p95 | 3,403,776 bytes (3.25 MiB) | 4,771,840 bytes (4.55 MiB) | 2.5 / 6 MiB |
| Idle CPU | 0.0010% | 0.0033% | 0.01% |
| GDI handles, p95/max | 13 | 13 | 16 / 24 |
| TCP connections, max | 0 | 0 | no demo network |

Startup median/p95: 38.023 / 78.379 ms. Required FRESH-01 ten-minute visible
idle CPU passes, as do visible memory/handles and no-network checks. The
hidden-requested process acquired graphics during overlapping UI automation
and exceeded the hidden memory target; 13 GDI handles rather than the normal
10 are consistent with a displayed flyout. This run is not hidden-memory proof.
An intermediate run showed the same interference. The initial ten-minute
candidate's hidden 1.59 MiB result remains candidate-only evidence. A separate
short isolated final-binary measurement without concurrent UI automation
is recorded below; it does not replace a ten-minute hidden baseline.


Isolated final-binary short check (10-second warmup, 60-second sample,
10 observations; no concurrent UI automation): hidden/visible p95 private
working set 1,675,264 / 4,784,128 bytes (1.60 / 4.56 MiB), CPU
0.0000 / 0.0062%, GDI 10/13 and zero TCP. Hidden graphics did not appear.
This supports UI interference in the concurrent ten-minute hidden run;
it is a short diagnostic check, not a ten-minute hidden baseline replacement.
