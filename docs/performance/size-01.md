# SIZE-01 audit — 2026-10-08

Result: done on `chore/size-audit`, branched from the unmerged UIA PR #2
(`feat/a11y-uia-and-plan-v2`, parent `cf6d929`). The owner explicitly deferred
Narrator and authorized this queue exception; WIP-00/A11Y-01 stay unchecked.

The [artifact ledger](artifact-ledger.md#size-01-audit-and-selected-profile-2026-10-08)
contains the experiment matrix, per-crate breakdown, profile decision, startup
results, unsigned soft target, and per-milestone allowances. Raw JSON evidence
is under [size-01/](size-01/).

Only `Cargo.toml`'s release `opt-level` changes product build behavior, from
`s` to `z`. Dependency features remain unchanged. Windows trimming saved zero
bytes; disabling Ed25519 `fast` saved 18,944 bytes but is not selected. No
new dependency or source behavior was introduced. Existing fixture/state/
signature/recovery tests provide regression coverage; no redundant test was
added for a compiler profile switch.

## Reproduction

Run each gate individually and stop Claudometer before each build:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
Stop-Process -Name claudometer -Force -ErrorAction SilentlyContinue
cargo build --locked --release --target x86_64-pc-windows-msvc
Stop-Process -Name claudometer -Force -ErrorAction SilentlyContinue
cargo build --locked --release --target aarch64-pc-windows-msvc
```

All passed, including 115 tests. Normal unprovisioned builds emit the expected
fail-closed trust-root warning. Provisioned measurements set the RFC 8032
public test key and sequence `1` in process-local variables, never production
secrets or provider credentials:

```powershell
$env:CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX = 'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a'
$env:CLAUDOMETER_RELEASE_SEQUENCE = '1'
```

Compare `s` with `z` using `CARGO_PROFILE_RELEASE_OPT_LEVEL` only in an
experiment shell. Preserve each executable before another build. For the
Windows/Ed25519 experiments, edit only the detached experimental worktree's
manifest. The same locked graph is used; those edits do not land.

Local tool: cargo-bloat 0.12.1, installed with `cargo install cargo-bloat
--locked`; it is not an app dependency. It forces debug/PDB symbols on MSVC:

```powershell
cargo bloat --locked --release --target x86_64-pc-windows-msvc --crates --message-format json -n 0
./ci/measure-baseline.ps1 -ExePath <preserved-exe> -ColdStarts 50 -WarmupSeconds 1 -SampleSeconds 1 -SampleIntervalSeconds 1
```

The timing script's short ancillary resource sample is not used to claim
memory or ten-minute CPU compliance. Retained JSON contains the startup
samples, machine, and readiness method. The initial `z` p95 failure remains
visible; quiet/final repeat runs passed. This is local Windows x64 evidence,
not ARM64 runtime proof or cold filesystem-cache testing.

Both x64 modes passed `ci/verify-demo.ps1`. The final provisioned x64 binary
passed 22 UIA checks in `--demo both` and `--demo settings`; screenshots were
reviewed for intact text/controls. Demo actions remain guarded and do not
prove live setting changes. Local UIA scaffolding/results are under ignored
`target/size-01/uia/`. Privacy, dependency, workflow, and all four architecture/
mode artifact checks passed. No provider endpoint was called by a test.

## Rollback

Restore the `s` profile and REL-02 comparison baselines in one reviewed commit,
or revert SIZE-01. No irreversible migration or privacy side effect occurs.
WARP, acrylic policy, resource-ID handling, CLI console behavior, updater trust
verification, credential ownership, and recovery journals are preserved.

## §0.7 handoff

```text
Task: SIZE-01 — size audit and headroom plan
Result: done
Changed: Cargo.toml, ci/release-budgets.json, docs/performance/artifact-ledger.md, docs/performance/size-01.md, docs/performance/size-01/*.json, plans/state-of-the-art-v1.md, CHANGELOG.md, CLAUDE.md
Gates: fmt ok | clippy ok | test ok (115 passed) | release build ok (x64/ARM64, provisioned and unprovisioned)
Runtime check: 50-start s/z comparisons and final z p95 passed; demo safety passed both x64 modes; 22 UIA checks and screenshots passed
Size: x64 1,058,304 bytes (-6.56% vs WIP-00 provisioned); ARM64 940,544 (-8.61%). Unprovisioned 947,712 / 892,928 bytes
Docs updated: plan checkbox and board, CHANGELOG, ledger, audit/evidence, CLAUDE.md; PRIVACY unchanged (no product side effect change)
Deviations from plan: owner deferred Narrator and authorized SIZE-01 before WIP-00 completion; existing with/without-UIA builds reused; initial z timing outlier retained and repeated
Follow-ups: REL-03 notes preparation; Narrator/remaining A11Y-01 deferred; real ARM64 runtime and signed size verification remain human release prerequisites
```
