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
| SIZE-01 `opt-level = "z"`, unprovisioned | 947,712 | -9.35% vs WIP-00 unprovisioned | 892,928 | -8.74% vs WIP-00 unprovisioned |
| SIZE-01 `opt-level = "z"`, synthetic trust root provisioned | 1,058,304 | -6.56% vs WIP-00 provisioned | 940,544 | -8.61% vs WIP-00 provisioned |
| MODEL-01 typed domain model, unprovisioned | 950,784 | +0.32% vs SIZE-01 unprovisioned | not measured | — |
| MODEL-01 typed domain model, synthetic trust root provisioned | 1,061,376 | +0.29% vs SIZE-01 provisioned | 942,592 | +0.22% vs SIZE-01 provisioned |
| STATE-01 pure reducer, unprovisioned | 950,784 | 0.00% vs MODEL-01 unprovisioned | not measured | — |
| STATE-01 pure reducer, synthetic trust root provisioned | 1,061,376 | 0.00% vs MODEL-01 provisioned | not measured | — |
| APP-01 UI-owned state, unprovisioned | 964,096 | +1.40% vs STATE-01 unprovisioned | not measured | — |
| APP-01 UI-owned state, synthetic trust root provisioned | 1,074,688 | +1.25% vs STATE-01 provisioned | not measured | — |
| CACHE-01 normalized runtime cache, unprovisioned | 998,912 | +3.61% vs APP-01 unprovisioned | not measured | — |
| CACHE-01 normalized runtime cache, synthetic trust root provisioned | 1,109,504 | +3.24% vs APP-01 provisioned | not measured | — |

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

## SIZE-01 audit and selected profile (2026-10-08)

Select `opt-level = "z"`; retain fat LTO, one codegen unit, stripping,
abort-on-panic, and every existing dependency feature. No crate was added to
the app. Local analysis used cargo-bloat 0.12.1; it is an installed development
tool only. All builds use Rust 1.97.1, locked dependencies, and the synthetic
public test key/sequence recorded above when provisioned. The with/without-UIA
comparison reuses the WIP-00 builds of `0fdfd19` and `89241c1`; those sources
and locked dependencies remain unchanged apart from the selected profile.

| Experiment | x64 provisioned bytes | Delta vs `s` | Decision |
|---|---:|---:|---|
| `s`, all existing features | 1,132,544 | — | Measured baseline |
| `z`, all existing features | 1,058,304 | -74,240 (-6.56%) | Selected |
| `s`, remove explicit Windows `Foundation`/`UI` entries | 1,132,544 | 0 | Reject: already transitively enabled |
| `s`, trimmed entries plus Ed25519 without `fast` | 1,113,600 | -18,944 (-1.67%) | Reject for this slice: retain existing crypto performance |

Windows `Foundation_Numerics`, notifications, ViewManagement, Win32 UIA,
Ole/Variant, and graphics features have direct call sites or binding-signature
requirements. There is no measured unused feature reduction to land. Ed25519
already disables defaults; `fast` enables precomputed curve tables. The audit
measures their footprint without changing verifier behavior or adding a
runtime feature selector.

`cargo bloat --locked --release --target x86_64-pc-windows-msvc --crates`
attributed the `s` baseline's 836,608-byte `.text` section as follows (five
largest contributors first):

| Contributor | Named code bytes |
|---|---:|
| `std` (includes core/alloc attribution) | 265,946 |
| Claudometer | 255,140 |
| `serde_json` | 55,743 |
| `ureq` | 49,270 |
| `serde_core` | 46,391 |
| `url` | 28,654 |
| `sha2` | 18,649 |
| `curve25519_dalek` | 16,798 |
| `windows` | 6,399 |

Raw per-contributor data: [`size-01/s-crates.json`](size-01/s-crates.json).
Fat LTO/inlining and MSVC PDB naming blur crate ownership; 27,681 `.text` bytes
are not covered by the named sums, and 295,936 executable bytes are outside
`.text`. These figures are code attribution, not additive whole-crate disk
costs. cargo-bloat enables PDB/debug symbols for analysis; final release sizes
above come from separately stripped normal builds.

### Startup comparison

Surface Laptop Studio, Windows 11 build 28020, eight logical processors;
50 new demo processes per run, nonce-bound tray-readiness event, nearest-rank
median/p95, isolated profile, no provider workers. The script's one-second
memory/CPU sample is excluded from this audit's retained startup evidence;
it cannot establish the ten-minute idle budget.

| Profile / run | Median ms | p95 ms | Result |
|---|---:|---:|---|
| `s`, initial | 42.240 | 51.741 | Pass |
| `z`, initial | 48.940 | 200.973 | Failed sample; retained, not hidden |
| `z`, repeat without compilation | 40.118 | 51.509 | Pass |
| `s`, quiet repeat | 39.130 | 45.285 | Pass |
| `z`, final exact build | 47.774 | 58.141 | Pass |

The initial `z` outlier did not reproduce; external scheduling/antimalware
interference is plausible, not proven. All quiet/final runs pass 150 ms p95.
The final executable was tested separately because rebuilds may change hashes
without changing size. Every 50-sample sequence is retained under
[`size-01/`](size-01/). Full methodology and limits are in
[`size-01.md`](size-01.md).

### Soft target and remaining milestone allowances

Set the **unsigned, provisioned x64 soft target to 1,245,184 bytes (1.1875
MiB)**, leaving **65,536 bytes** inside the unchanged 1,310,720-byte hard
ceiling for Authenticode/certificate overhead and release contingency. This
reserve is a planning allowance, not a measured signing size; signed artifacts
must still pass the hard check. Prefer smaller artifacts and measure every
slice rather than treating allowances as automatic baseline increases.

| Remaining work | Additional x64 allowance | Cumulative provisioned bytes |
|---|---:|---:|
| SIZE-01 selected baseline | — | 1,058,304 |
| `v0.10` model/state/cache/diagnostics/Codex | 32,768 | 1,091,072 |
| `v0.11` remaining UIA/layout/first-run | 40,960 | 1,132,032 |
| `v0.12` pace/rows/tray/alerts | 49,152 | 1,181,184 |
| `v1.0` distribution/handshake code | 32,768 | 1,213,952 |
| Unallocated roadmap contingency | 24,576 | 1,238,528 |
| Remaining to unsigned soft target | 6,656 | 1,245,184 |

These estimates are not acceptance proof for future features. If a milestone
cannot fit, reduce its scope or optimize with measurements; the hard ceiling
requires an ADR to change. ARM64's selected provisioned build is smaller by
117,760 bytes, but both architecture artifacts must pass independently.

`ci/release-budgets.json` lowers comparison baselines to 1,058,304 / 940,544
bytes only because this measured size reduction lands. It preserves the hard
ceiling and 10% regression gate. The new baselines are also below REL-02.

Verification: all §0.3 gates (115 tests), both architectures in both trust-root
modes, artifact checks, privacy/dependency/workflow policy, demo safety in both
x64 modes, 22 UIA checks, and screenshot review passed. ARM64 is cross-compiled
only; Narrator remains deferred by the owner. Final sizes/hashes are in
[`size-01/artifacts.json`](size-01/artifacts.json).

Rollback: restore `opt-level = "s"` and the previous comparison baselines
together; no product state or data migration is involved. Never modify provider
credentials or recovery journals for this rollback.

## MODEL-01 (2026-10-08)

Rust 1.97.1, locked dependencies, the selected `z` profile, and the same RFC
8032 public test key/sequence as SIZE-01. The normalized model adds 3,072 x64
bytes and 2,048 ARM64 bytes to the provisioned baseline. No crate was added,
and CI comparison budgets remain at SIZE-01. Both provisioned artifacts pass
PE architecture/version, regression, and the unchanged 1.25 MiB ceiling;
x64 headroom is 249,344 bytes. The unprovisioned x64 build is 950,784 bytes.
Test-only changes after these builds do not enter release artifacts. Evidence
and rollback: `docs/verification/model-01.md`.

## APP-01 (2026-10-09)

The UI-owned reducer and worker AppEvent/ticket handoff add 13,312 bytes to
both x64 modes: 964,096 unprovisioned / 1,074,688 synthetic-trust-root
provisioned. No crate added; profile/CI budgets remain unchanged. Both modes
pass PE version/architecture, regression, and hard-ceiling checks. Provisioned
headroom is 236,032 bytes. Test-only legacy parity code is stripped from the
release. The full idle run passes all runtime budgets; evidence and limits:
[`app-01.md`](app-01.md), [`../verification/app-01.md`](../verification/app-01.md).

STATE-01 introduced the pure reducer without runtime integration, so LTO
preserved the prior x64 sizes in both modes. APP-01 connects it to runtime;
the new ledger delta includes the actual reducer, reply channel, event queue,
and UI-owned compatibility presentation.

## CACHE-01 (2026-10-09)

Normalized snapshot serialization/validation and restart integration add
34,816 bytes in each x64 mode. Unprovisioned: 998,912; synthetic public
trust root provisioned: 1,109,504. No crate or profile/CI-budget change.
Both modes pass architecture/version, regression, and the 1.25 MiB ceiling;
provisioned headroom is 201,216 bytes. The cumulative v0.10 growth exceeds
SIZE-01's provisional 32,768-byte milestone allowance by 18,432 bytes; this
is planning pressure for the remaining diagnostics/Codex work, not a raised
soft/hard budget. The per-slice growth remains below 10%.

SHA-256: unprovisioned
`597849684d0420ce29052f877041c46c668a208723e8269e9c66b952b39f61b9`;
provisioned
`c3daf04212e16baa5132eb71940dfb0ece3d039152e574df904924bcf8ff2c54`.
Same synthetic RFC 8032 public key/sequence, Rust 1.97.1, locked dependencies,
and `z` profile. Gates: 151 tests, fmt/clippy/release, demo safety, 22 UIA
checks, screenshot review, privacy/dependency/workflow checks. No idle-CPU or
ARM64-runtime claim is added. Rollback/compatibility:
[`../verification/cache-01.md`](../verification/cache-01.md).
