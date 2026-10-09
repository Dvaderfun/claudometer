# Claudometer release manifest v1

The release manifest is the updater's authorization boundary. GitHub release
metadata and TLS locate bounded files; neither authorizes an executable. The
updater accepts a candidate only after a maintained Ed25519 verifier validates
the detached signature over the exact manifest file bytes and every policy
field below matches local state.

## Release files

For tag `vX.Y.Z` and architecture `x64` or `arm64`, a release contains:

```text
claudometer-vX.Y.Z-windows-<arch>.exe
claudometer-vX.Y.Z-windows-<arch>.exe.sha256
claudometer-vX.Y.Z-windows-<arch>.manifest.json
claudometer-vX.Y.Z-windows-<arch>.manifest.signatures.json
claudometer-vX.Y.Z.sbom.spdx.json
```

Every name is reconstructed under the fixed repository/tag URL. The GitHub API
response cannot substitute a URL or asset. Each required name must occur
exactly once.

## Exact manifest document

The manifest is UTF-8 JSON with exactly these fields; unknown, duplicate,
missing, or incorrectly typed fields are invalid:

```json
{
  "schema": "claudometer-release-manifest-v1",
  "channel": "stable",
  "sequence": 18,
  "version": "0.9.1",
  "tag": "v0.9.1",
  "issued_at": "2026-09-03T12:00:00Z",
  "policy_expires_at": "2026-09-10T12:00:00Z",
  "architecture": "x64",
  "asset": "claudometer-v0.9.1-windows-x64.exe",
  "size": 812345,
  "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "minimum_updater_version": "0.8.0"
}
```

- `sequence` is positive and strictly greater than the sequence embedded in
  the running updater. It never resets or reuses a value.
- Versions are canonical unsigned `major.minor.patch` integers with no leading
  zeroes. `tag` and the architecture-specific `asset` must be their exact
  derived values.
- Timestamps are UTC RFC 3339 seconds in the exact `YYYY-MM-DDTHH:MM:SSZ`
  spelling. Expiry is exclusive. Issuance may be at most five minutes ahead of
  the local clock.
- `size` is the exact byte count and must be within the updater's bound.
  `sha256` is exactly 64 lowercase hexadecimal characters.
- The running updater must meet `minimum_updater_version`.

The signature covers the manifest bytes exactly as published, including JSON
member order, whitespace, and a final newline if present. Producers may choose
their formatting, but must sign the resulting immutable byte string.

## Detached signatures and key rotation

A normal signature document is:

```json
{
  "schema": "claudometer-release-signatures-v1",
  "signatures": [
    {
      "public_key": "<64 lowercase hex characters>",
      "signature": "<128 lowercase hex characters>"
    }
  ]
}
```

The signature must verify under the public key embedded in the running binary.
The protected signing key is never passed to the build or stored in this
repository.

The release workflow accepts that key only as an Ed25519 PKCS#8 DER document,
base64-encoded in the protected `release` environment secret
`CLAUDOMETER_RELEASE_PRIVATE_KEY_PKCS8_B64`. It derives the public key from the
private document and refuses to sign unless it exactly matches the lowercase
hex `CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX` repository variable. Temporary private
key files are created under `RUNNER_TEMP` and removed immediately after the
manifest signatures are produced.

A rotation release uses exactly two signatures over the same manifest bytes
and adds `next_public_key`. One signature must verify under the currently
embedded key and one under that exact next key. The released executable embeds
the next public key. A next key supplied without both proofs is rejected. This
deliberately makes rotation a controlled transition release; clients that skip
it require the documented manual bootstrap path.

```json
{
  "schema": "claudometer-release-signatures-v1",
  "signatures": [
    { "public_key": "<current key>", "signature": "<current signature>" },
    { "public_key": "<next key>", "signature": "<next signature>" }
  ],
  "next_public_key": "<next key>"
}
```

## Trust-root bootstrap

Every release before the bootstrap release is an unsigned-channel client. This
currently includes all published versions through 0.7.3. Such clients contain
no Ed25519 release public key and therefore have no authenticated way to
acquire one from GitHub release metadata, a checksum beside a download, or a
new manifest. No automatic update, one-time consent prompt, downloaded key, or
other network response may bridge this boundary. An unprovisioned build
likewise rejects every manifest, even when the manifest is otherwise valid and
correctly signed.

Existing users must manually install 0.9.1. Owner decision ADR 0006 defers
Authenticode signing to v1.0: these executables are unsigned for Windows.
Bootstrap verification uses SHA-256 plus GitHub build provenance for the
`Dvaderfun/claudometer` repository and `.github/workflows/release.yml` at the
exact release tag. This proves GitHub source/workflow identity, not a Windows
publisher identity; users must trust that identity to obtain the initial key.
No downloaded key or checksum alone can authorize automatic bootstrap.

Release notes publish exact executable hashes/sizes, Ed25519 public key and
fingerprint, sequence, and provisioning time before publication. The workflow
renders these values from the tested artifacts, creates a draft, and publishes
only after downloaded manifests, signatures, SBOM, provenance, and x64 demo
checks pass. Windows signing and hardware/Narrator verification remain v1.0 work.

The bootstrap release publishes only versioned asset names. It never contains
legacy `claudometer.exe` or `claudometer.exe.sha256` aliases recognized by old
clients, so they report no automatic-update candidate across this boundary.

Verify before running (replace the hash with the value in release notes):

```powershell
$asset = Resolve-Path '.\claudometer-v0.9.1-windows-x64.exe'
$expectedSha256 = '<64 lowercase hex characters from verified release notes>'
if ((Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expectedSha256) {
    throw 'Claudometer SHA-256 mismatch'
}
gh attestation verify $asset.Path --repo Dvaderfun/claudometer --signer-workflow Dvaderfun/claudometer/.github/workflows/release.yml --source-ref refs/tags/v0.9.1
if ($LASTEXITCODE -ne 0) { throw 'Claudometer provenance verification failed' }
```

Use the matching ARM64 file/hash on ARM64. A mismatch or failed attestation
verification stops installation. The released notes include executable hashes
and exact sizes; the public key below authenticates subsequent updates.

### Production bootstrap record

| Field | Recorded value |
|---|---|
| Status | Published immutable v0.9.1 at 2026-10-09T00:09:00Z |
| Provisioned at (UTC) | 2026-10-08T23:33:54Z |
| Bootstrap version and tag | 0.9.1 / v0.9.1 |
| Ed25519 public key (lowercase hex) | 5741ac7eec5c56c96c5dcf12e7be47a2586b2739ec1f309f26b52527578660f9 |
| Ed25519 raw-key SHA-256 fingerprint | ee7fcaf40164c6d8f8b9f3a34050b0173a7d3e28ef5c5b6bffc12b61af86d1a5 |
| Initial embedded release sequence | 2 |
| Minimum updater version | 0.9.1 |
| Windows publisher/signature | Unsigned; SIGN-01 deferred to v1.0 by ADR 0006 |
| Verification record | Tagged source record plus verified GitHub release-workflow attestations |

The private key is outside git with a protected local backup and a protected
release-environment secret. Release keys never enter provider state. Subsequent
releases increment sequence monotonically and preserve this public key until
an authenticated rotation. Production hashes are generated from final GitHub
artifacts; local-build hashes are not substituted for published artifacts.

## Rollback authorization

Sequence rollback is never permitted. A lower version with a newer sequence
additionally requires these two fixed assets:

```text
claudometer-vX.Y.Z-windows-<arch>.rollback.json
claudometer-vX.Y.Z-windows-<arch>.rollback.signatures.json
```

The authorization has exactly these fields and is independently signed by the
currently trusted key using the normal one-signature envelope:

```json
{
  "schema": "claudometer-rollback-authorization-v1",
  "channel": "stable",
  "from_sequence": 19,
  "from_version": "0.9.1",
  "target_sequence": 20,
  "target_version": "0.9.1",
  "target_tag": "v0.9.1",
  "architecture": "x64",
  "asset": "claudometer-v0.9.1-windows-x64.exe",
  "issued_at": "2026-09-03T12:00:00Z",
  "expires_at": "2026-09-04T12:00:00Z"
}
```

Every source and target field must match exactly, and both the release policy
and rollback authorization must remain unexpired.

## Build-time trust inputs

Official binaries set both variables before compilation:

```text
CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX=<32-byte public key as lowercase hex>
CLAUDOMETER_RELEASE_SEQUENCE=<positive integer for this binary>
```

Only those public values enter the executable. A developer build with neither
value still compiles, but update verification fails closed with an unprovisioned
trust-root error. Supplying only one value or malformed values fails the build.

The tag release workflow additionally requires the repository variable
`CLAUDOMETER_MINIMUM_UPDATER_VERSION`. It builds each architecture once with
`--locked` after the reusable source/security gate, hashes those exact bytes,
generates and verifies the signed manifests, and creates a single SPDX 2.3
SBOM from the tagged locked source. GitHub build-provenance and SBOM
attestations bind both executables to the tag workflow. The workflow then:

1. creates a draft containing the executables, checksums, manifests,
   signatures, and SBOM;
2. downloads the assets from that draft and repeats all local evidence checks;
3. verifies both GitHub attestation predicate types and runs the downloaded x64
   executable in isolated hidden demo mode; and
4. publishes only after every smoke check succeeds.

Manifest policy windows are generated for 30 days. A failed smoke test removes
the draft. Existing releases and tags are never overwritten or reused, and the
workflow proves the repository's immutable-release setting through an
administration-read-only token before accessing the signing key. The same
preflight requires an active no-bypass `main` ruleset, split `v*` creation and
mutation rulesets, and a reviewer-gated, no-admin-bypass `release` environment
restricted to `v*` tags.

After authentication and policy checks, the updater verifies the checksum
sidecar against the signed hash, reads the executable into bounded memory,
checks its exact signed size, hash, and PE magic, and only then creates a
candidate file. It re-hashes the written file and revalidates the install
channel before any executable rename.
