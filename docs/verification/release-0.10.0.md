# Published 0.10.0 and branch consolidation

Owner request 2026-10-10; authorization recorded in ADR 0010.

- Release: https://github.com/Dvaderfun/claudometer/releases/tag/v0.10.0
- Published: 2026-10-10T00:17:25Z; release ID 408446904; immutable, not draft.
- Tag source: 38919c6f1f78176870792ece46a256deeee42b1e on main.
- PR 10: https://github.com/Dvaderfun/claudometer/pull/10 (rebase merged).
- Release workflow: https://github.com/Dvaderfun/claudometer/actions/runs/38007877887
- PR and main build runs: 38006957177 / 38007400089; successful.

All seven feature/fix commits and version preparation survived the rebase
merge. Main's tree exactly matched the checked PR head b59f4a5. Old branches
were removed only after patch/tree proof: release/0.9.0 equals main e616688;
release/0.9.1 equals 680f494; docs/0.9.1-release-evidence equals 8b301f4.
The archive preflight is an ancestor of the former 0.9.0 branch; its work
survives that complete tree on main. Other feature branches are patch-equivalent
to main's rebased commits. Only main remains locally and remotely. Historical
tags, including failed/unreleased v0.9.0, and detached measurement worktrees
remain intact. No force push or weakening of repository protection occurred.

## Published artifacts

| Architecture | Bytes | SHA-256 |
|---|---:|---|
| x64 | 1,160,704 | 7ce9bd464c55234ecd66a3cd13ec6a9b27163a15d48d684c297ea6c2abea2967 |
| ARM64 | 1,015,808 | 3e0316d1daf47d934c7c3c68b9199a2a0bcf98647faf20c656bf8e5a2c167832 |

Both manifests carry sequence 3 and minimum updater 0.9.1, using the existing
public root 5741ac7eec5c56c96c5dcf12e7be47a2586b2739ec1f309f26b52527578660f9.
The previous published 0.9.1 manifests carry sequence 2. No key rotation.

Validation: local ordered fmt/Clippy/192 tests, production-root x64/ARM64
builds (1,163,264 / 1,018,368), 59 versioned UI clarity checks, demo safety,
privacy/dependency/workflow policy, exact local release-input/note rendering.
CI source/RustSec/OSV and both architecture jobs pass; PR dependency review
passes. Original cumulative size gates and hard ceiling remain unchanged.

The release workflow verifies its exact assets, creates signed manifests,
attests SLSA provenance and SPDX SBOM, downloads/smoke-tests the draft, rechecks
protected infrastructure and confirms immutable publication. Downloaded
published files independently pass ci/check-release.ps1 -RequireEvidence with
both Ed25519 signatures. GitHub CLI verifies SLSA and SPDX attestations for
both architectures against this tag/workflow/source. The first local x64
attestation attempt could not initialize the public-good verifier; a retry
with the same policy passed. No verification policy was relaxed.

Published notes exactly match the template rendered from downloaded GitHub
bytes. Downloaded x64 --demo passes no file/registry/power/child/TCP changes.
Windows executables remain portable and Authenticode-unsigned. Narrator,
Selection/RangeValue/events, physical lid-close and real ARM64 runtime remain
unverified. No live provider endpoint was called by tests or credential changed.

The release environment was approved before the agent's API approval arrived;
the latter returned no outstanding approval, then all publication steps passed.
The temporary protected administration-read secret was removed after completion.
Main/tag/environment protection and immutable-release settings remain enabled.
Raw downloaded assets and verification JSON are under target/release-0.10.0/.

Rollback: a new higher-sequence authorized release only; never change/reuse
published tags/assets. Disable Vibecode or quit normally before replacement,
verifying lid restoration. Retain unresolved/corrupt safety journals. Existing
settings/cache schemas remain compatible with 0.9.1; its independent power UI
returns on downgrade. Future releases require separate owner authorization.
