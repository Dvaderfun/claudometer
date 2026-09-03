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
  "version": "0.9.0",
  "tag": "v0.9.0",
  "issued_at": "2026-09-03T12:00:00Z",
  "policy_expires_at": "2026-09-10T12:00:00Z",
  "architecture": "x64",
  "asset": "claudometer-v0.9.0-windows-x64.exe",
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

Existing users must manually install the first trust-root-enabled release. That
bootstrap artifact must be Authenticode-signed so Windows supplies an
independent trust path for the executable. Its release notes must label it as
the bootstrap release and publish all of these values before the release is
made public:

- exact version, tag, architecture-specific asset names, and SHA-256 hashes;
- Authenticode publisher subject and SHA-256 certificate fingerprint;
- the 32-byte Ed25519 release public key and its SHA-256 fingerprint;
- the initial positive release sequence embedded in the executable;
- the UTC provisioning time and a separately authenticated location from which
  users can obtain the expected verification values.

The bootstrap release must publish only the versioned asset names specified in
this document. In particular, it must not contain the legacy `claudometer.exe`
or `claudometer.exe.sha256` aliases recognized by the 0.7.x updater. Omitting
those aliases makes old clients report no automatic-update candidate instead
of letting their unauthenticated path cross the bootstrap boundary.

Do not treat a checksum file downloaded beside the executable as independent
evidence. Obtain the expected values from the bootstrap release notes and the
separately authenticated location named there, and require them to agree.
Then, from PowerShell, verify the downloaded executable before running it:

```powershell
$asset = Resolve-Path '.\claudometer-vX.Y.Z-windows-x64.exe'
$expectedSha256 = '<64 lowercase hex characters from both published records>'
$expectedPublisher = '<exact Authenticode subject from both published records>'
$expectedCertSha256 = '<64 lowercase hex characters from both published records>'

$actualSha256 = (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualSha256 -cne $expectedSha256) { throw 'Claudometer SHA-256 mismatch' }

$signature = Get-AuthenticodeSignature -LiteralPath $asset
if ($signature.Status -ne 'Valid') { throw "Invalid Authenticode signature: $($signature.Status)" }
if ($signature.SignerCertificate.Subject -cne $expectedPublisher) { throw 'Unexpected publisher' }
$hasher = [Security.Cryptography.SHA256]::Create()
try {
    $actualCertSha256 = ([BitConverter]::ToString(
        $hasher.ComputeHash($signature.SignerCertificate.RawData)
    )).Replace('-', '').ToLowerInvariant()
} finally {
    $hasher.Dispose()
}
if ($actualCertSha256 -cne $expectedCertSha256) { throw 'Unexpected signing certificate' }
```

Use the matching `arm64` asset name on ARM64. Install or replace the existing
copy only after every check succeeds. A mismatch, invalid or missing signature,
absent independent record, or still-placeholder value is a stop condition.

### Production bootstrap record

Production inputs are not yet provisioned. This table is the authoritative
record and must be completed atomically with the first trust-root-enabled
release; placeholders are forbidden in published release notes.

| Field | Recorded value |
|---|---|
| Status as of 2026-09-03 | Not provisioned |
| Provisioned at (UTC) | Pending |
| Bootstrap version and tag | Pending |
| Ed25519 public key (lowercase hex) | Pending |
| Ed25519 raw-key SHA-256 fingerprint | Pending |
| Initial embedded release sequence | Pending; must be greater than zero |
| Authenticode publisher subject | Pending |
| Authenticode certificate SHA-256 fingerprint | Pending |
| Separately authenticated verification record | Pending |

The release owner must record the real values here and in the bootstrap release
notes when provisioning occurs. The public key and sequence must be the exact
values supplied to the build below, and release evidence must show that the
produced binaries contain that policy. Later binaries embed the highest release
sequence they represent so replayed manifests remain rejected.

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
  "target_version": "0.9.0",
  "target_tag": "v0.9.0",
  "architecture": "x64",
  "asset": "claudometer-v0.9.0-windows-x64.exe",
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

After authentication and policy checks, the updater verifies the checksum
sidecar against the signed hash, reads the executable into bounded memory,
checks its exact signed size, hash, and PE magic, and only then creates a
candidate file. It re-hashes the written file and revalidates the install
channel before any executable rename.
