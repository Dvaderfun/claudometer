# Executable size ledger

All values are `--locked --release` artifacts built with Rust 1.97.1 and the
repository release profile. Rows are unprovisioned unless explicitly labeled.
A comparison baseline advances only after a slice's focused/full tests and size
review pass; changing `ci/release-budgets.json` therefore requires review
alongside this ledger. The hard 1.25 MiB ceiling never advances.

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
| REL-02 authenticated updater, unprovisioned | 1,010,688 | +7.40% | 945,152 | +6.28% |
| REL-02 authenticated updater, trust root provisioned | 1,097,728 | +8.61% | 995,840 | +5.36% |
| WIP-00 UIA `0fdfd19`, unprovisioned | 1,045,504 | +3.44% vs REL-02 unprovisioned | 978,432 | +3.52% vs REL-02 unprovisioned |
| WIP-00 UIA `0fdfd19`, synthetic trust root provisioned | 1,132,544 | +3.17% vs REL-02 provisioned | 1,029,120 | +3.34% vs REL-02 provisioned |

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

REL-02 measures both build modes because a missing compile-time trust root lets
LTO eliminate the fail-closed manifest-verification path. Supplying a synthetic
Ed25519 public key and positive sequence retains the production updater path and
adds 87,040 bytes on x64 and 50,688 bytes on ARM64. That mechanism was
confirmed by rebuilding the same commit and toolchain with only those two
public build variables changed.

The provisioned binaries are the new CI comparison baseline because they are
the bytes the release workflow publishes. Both remain below the 1.25 MiB hard
ceiling. x64 is 49,152 bytes above the 1.0 MiB unsigned soft target; this is an
investigated authenticated-updater cost, not a raised hard limit, and remains a
candidate for later size optimization.

## WIP-00 verification (2026-10-08)

Rust 1.97.1, locked dependencies, and the unchanged `opt-level = "s"` / fat-LTO
release profile were used for both architectures. The provisioned measurements
use the RFC 8032 public test key
`d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a` and sequence
`1` in process-local build variables. No production key, provider credential,
repository variable, or secret was provisioned. These binaries are local test
artifacts, not release artifacts.

The immediate pre-UIA parent was also rebuilt in a detached worktree, with the
same compiler, locked dependencies, profile, and public build variables:

| Commit / mode | x64 bytes | ARM64 bytes | UIA growth vs this parent (x64 / ARM64) |
|---|---:|---:|---|
| `89241c1`, unprovisioned | 1,011,200 | 945,664 | +34,304 / +32,768 bytes (+3.39% / +3.47%) |
| `89241c1`, synthetic trust root provisioned | 1,098,240 | 996,864 | +34,304 / +32,256 bytes (+3.12% / +3.24%) |

The parent rebuild differs from the older REL-02 ledger: +512 bytes x64 in both
modes; +512 bytes ARM64 unprovisioned and +1,024 bytes ARM64 provisioned. The
existing REL-02 rows remain historical measurements. Both the historical and
same-parent comparisons remain below the 10% threshold. The provisioned UIA
x64 artifact has 178,176 bytes of hard-ceiling headroom; ARM64 has 281,600.
`ci/check-artifact.ps1` passed PE architecture, version, ceiling, and regression
checks on all four UIA artifacts. CI budgets remain at REL-02 pending SIZE-01.

| UIA mode | x64 SHA-256 | ARM64 SHA-256 |
|---|---|---|
| Unprovisioned | `f32e30edade8acbb3882604ce264b898aeca3e63d17ed379a2e47e9de2385e32` | `879f4fabacd4eed5ec6b8b98c1c4cf9b102658c3b3b4105e07f2811097fcfd6` |
| Synthetic trust root provisioned | `d4dd69d1f1b9ede0660c6bc9edff00329e241cf72fc65e4ad51798fbda7f2595` | `ad0db91822f23d118a3dd5b27cc250288e744d16f8c9705dfc9f7e7fd429105a` |

WIP-00 remains blocked on Narrator verification; these size results do not
complete A11Y-01 or replace the later runtime-memory/CPU measurements.
