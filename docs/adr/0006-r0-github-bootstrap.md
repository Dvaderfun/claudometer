# ADR 0006: R0 bootstrap through GitHub

- Status: Accepted by the owner, 2026-10-09
- Scope: first trust-root release 0.9.0; supersedes ADR 0001's R0 signing prerequisite

The owner authorized completing GitHub setup and publishing the existing work.
The release remains portable and uses an embedded Ed25519 key, signed exact-byte
manifests, immutable GitHub assets, and verified GitHub build/SBOM attestations.
Windows Authenticode publisher signing stays a v1.0 requirement (SIGN-01).

The 0.9.0 executables are unsigned for Windows. Manual bootstrap verification
uses the expected SHA-256 and GitHub provenance for this repository and its
release workflow. This establishes GitHub source/workflow identity, not a
Windows publisher identity or SmartScreen reputation. Users must trust that
repository identity when obtaining the first public key; a downloaded key or
checksum alone never authorizes an automatic bootstrap.

Old clients still receive no legacy asset aliases and cannot automatically
cross the trust boundary. The updater's signature, sequence, exact hash/size,
host, version, architecture, expiry, and journal checks remain unchanged.
Published notes explicitly disclose incomplete Narrator and ARM64 hardware
verification. ARM64 is cross-built and artifact-checked but runtime-unverified.
Those are v1.0 completion requirements, not claimed R0 evidence.

The owner's explicit authorization permits the agent to provision release
inputs, prepare/merge the version PR, create the new immutable tag, approve the
protected environment, and publish this release. Existing protections remain
enabled. General future agent restrictions still apply without authorization.

Private manifest keys remain outside git and are supplied only to the protected
release environment. The existing local GitHub credential may be used temporarily
for the workflow's read-only infrastructure audit; remove that environment secret
after the release finishes. Future releases should use a dedicated administration
read token. A protected local key backup is retained for recovery.

Rollback: publish a new higher-sequence authorized rollback; never overwrite the
tag, assets, key, or sequence. Users of pre-bootstrap binaries install manually.
No provider credential, system journal, or application schema change is required.
