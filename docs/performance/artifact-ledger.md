# Executable size ledger

All values are unsigned `--locked --release` artifacts built with Rust 1.97.1
and the repository release profile. A comparison baseline advances only after a
slice's focused/full tests and size review pass; changing
`ci/release-budgets.json` therefore requires review alongside this ledger. The
hard 1.25 MiB ceiling never advances.

| Local slice | x64 bytes | Change from prior measured slice | ARM64 bytes | Change from prior measured slice |
|---|---:|---:|---:|---:|
| Roadmap `v0.7.3` reference | 815,104 | — | — | — |
| PR 0–1 containment/gates | 812,544 | -0.31% | 766,464 | first ARM64 build |
| PR 2 fixtures/policy | 816,128 | +0.44% | 771,072 | +0.60% |
| PR 3 demo/baseline | 826,880 | +1.32% | 779,776 | +1.13% |
| PR 6 typed settings | 842,752 | +1.92% | not measured | — |
| PR 7 identities/state envelope | 857,088 | +1.70% | 779,776 | 0.00% from PR 3 |
| PR 8 Vibecode journal/recovery | 908,800 | +6.03% | 857,088 | +9.92% |
| PR 9 account/generation isolation | 941,056 | +3.55% | 889,344 | +3.76% |

PR 8 crossed the stale v0.7.3-relative 10% CI threshold cumulatively, but not
the roadmap's per-slice investigation threshold. Its x64 delta is 51,712 bytes;
its ARM64 delta is 77,312 bytes. The increase is attributable to the serialized
power journal, checked Windows power controller, lifecycle/UI states, and the
recovery paths (test code is not present in release artifacts). Both remain
below the 1.0 MiB signed soft target before signing and the 1.25 MiB hard
ceiling.

Current PR 9 hashes:

- x64: `d23dc891a73f21c4a2ef76f86914e6f62c4024fce916204a654950730c181dfb`
- ARM64: `96e41e670f766ce5c35e4fbc6a04c4cb5cd04a6efd8753613c78cdda23223177`
