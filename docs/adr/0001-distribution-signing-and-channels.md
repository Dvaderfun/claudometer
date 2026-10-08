# ADR 0001: Distribution, signing, and update channels

- Status: Accepted for local implementation; external onboarding blocked. R0 signing/bootstrap prerequisites superseded by ADR 0006 (2026-10-09).
- Date: 2026-09-03
- Roadmap: `DIST-00`, prerequisite for `UPD-00` and `SIGN-01`

## Context

Claudometer must support portable and managed per-user installations without a
service, helper, managed runtime, or unauthenticated execution boundary. The
portable executable can replace itself only through a crash-safe journal. A
managed install must preserve package-manager ownership and therefore cannot
rename its installed executable in place.

Signing also has two independent trust roots:

1. Authenticode establishes the Windows publisher and executable trust chain.
2. An embedded Ed25519 public key authenticates the exact release manifest and
   its channel/version/architecture policy.

Neither trust root substitutes for the other.

The exact Ed25519 wire format, replay/downgrade policy, and cross-signing
transition are specified in [`../release-manifest-v1.md`](../release-manifest-v1.md).

### Existing-user trust bootstrap

Versions without an embedded trust root—including all published versions
through 0.7.3—cannot authenticate a newly supplied Ed25519 public key. The first
trust-root-enabled release is therefore a manual migration boundary: existing
users must download it themselves and verify its published SHA-256 and
GitHub release-workflow provenance before installation (ADR 0006). The old updater must not download a
key, accept a consent-based exception, or automatically install across this
boundary. The bootstrap release omits the legacy `claudometer.exe` and
`claudometer.exe.sha256` aliases recognized by 0.7.x clients. The verification
procedure and production provisioning record live in
[`../release-manifest-v1.md`](../release-manifest-v1.md#trust-root-bootstrap).

## Decision

### Installer

Use **Inno Setup 7**, pinned in the release toolchain, to produce separate x64
and ARM64 per-user installer assets. It produces a native Windows installer and
does not add a runtime to Claudometer. Configuration must include:

- `PrivilegesRequired=lowest`; no elevation or machine-wide fallback.
- `%LOCALAPPDATA%\Programs\Claudometer` as the managed install root.
- Separate architecture-gated builds: x64 excludes Arm64; ARM64 requires
  `arm64` and carries only the ARM64 application binary.
- No service, scheduled task, daemon, browser runtime, or global PATH edit.
- A stable uninstall entry and install-channel marker owned by Claudometer.
- The uninstall entry key is
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Claudometer_is1`;
  its `InstallLocation` must equal the executable directory.
- The managed install root contains `claudometer.install-channel` with the exact
  UTF-8 value `managed`. The marker, expected install root, and uninstall
  registration must all agree before the app treats itself as managed.
- Installer code must stop when the maintenance handshake or safety recovery
  cannot be verified. It must never force-kill past an unresolved power journal.

Official Inno contracts:

- <https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm>
- <https://jrsoftware.org/ishelp/topic_setup_architecturesallowed.htm>
- <https://jrsoftware.org/ishelp/topic_setupcmdline.htm>

MSIX is not selected for v1.0 because converting the current unpackaged tray,
toast, startup, portable, and recovery lifecycle would widen the migration.
WiX is not selected because MSI/Burn adds more build/runtime surface without a
v1.0 acceptance benefit. Revisit only through a superseding ADR.

### Install channels

| Channel | Ownership | Update behavior | Rollback |
|---|---|---|---|
| Portable x64/ARM64 | User owns one executable and adjacent update files | Signed-manifest-authorized, journaled self-swap | Journal restores the retained last-known-good executable |
| Managed x64/ARM64 | Inno/Windows uninstall registration owns the install | Verified signed installer handoff; the app never renames its installed executable | Run the immediately previous signed installer only with signed rollback authorization |
| Winget | Winget selects the matching managed installer | Package-manager/installer handoff; never portable self-swap | Winget or explicit previous signed installer, subject to signed rollback policy |

The install-channel marker is advisory input only. Before mutation, the updater
must also verify that the executable path and uninstall registration agree. An
ambiguous installation fails closed to a manual signed-download action.

Release downloads start at fixed `github.com` repository/tag/asset URLs and
may redirect only to GitHub's documented `release-assets.githubusercontent.com`
release-asset host. See GitHub's
[self-hosted runner network reference](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#communication-requirements).

### Signing provider and custody

Select **Microsoft Artifact Signing, Public Trust** as the v1.0 Authenticode
provider. Signing uses SHA-256 plus the Microsoft RFC 3161 timestamp endpoint
`http://timestamp.acs.microsoft.com`. Artifact Signing retains private keys in
its managed HSM service; no PFX, private key, client secret, or refresh token is
stored in this repository or GitHub Actions.

Release authentication uses GitHub OIDC to a narrowly scoped Azure identity
with only the Artifact Signing Certificate Profile Signer role. The signing job
runs only in the protected `release` environment after source, security,
architecture, size, and human-approval gates. It signs the already-tested
artifacts, verifies them with both SignTool and `WinVerifyTrust`, then hashes the
immutable signed bytes and signs the release manifest. No later job may mutate
those bytes.

Microsoft contracts:

- <https://learn.microsoft.com/en-us/azure/artifact-signing/overview>
- <https://learn.microsoft.com/en-us/azure/artifact-signing/concept-trust-models>
- <https://learn.microsoft.com/en-us/azure/artifact-signing/concept-certificate-management>
- <https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations>

### Silent commands

Asset names are versioned and architecture-specific. With `X.Y.Z` substituted:

```text
claudometer-vX.Y.Z-windows-x64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER
claudometer-vX.Y.Z-windows-arm64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER
%LOCALAPPDATA%\Programs\Claudometer\unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

Running the same signed installer/version is the repair operation. Silent
uninstall retains ordinary settings/history by default; an explicit
`/REMOVEDATA=1` may remove them only after all safety journals resolve.
Interactive uninstall offers the same retain/remove choice. Neither path ever
removes Claude or Codex files.

### App-to-installer/uninstaller handshake

The maintenance executable creates a 128-bit nonce and a nonce-named local
event, then asks the running app to enter `update` or `uninstall` maintenance.
The app must:

1. Stop new refresh/update work and reject stale operations.
2. Drop the wake lock.
3. Recover any `power-override.v1.json` transaction.
4. Flush validated settings/state writes.
5. Signal readiness bound to the nonce, operation, expected PID, and app
   version, then exit.

The maintenance executable verifies the matching readiness payload and process
exit before touching installed files. Timeout, spoofed/stale readiness, or an
unresolved safety journal stops maintenance with an actionable recovery error.
The later implementation may choose named-pipe or inherited-handle transport,
but it may not weaken the nonce/PID/version binding or introduce a server.

## External prerequisites and exact unblock conditions

These are deliberately not claimed complete:

| Prerequisite | Current evidence | Unblock condition |
|---|---|---|
| Legal publisher subject | Unknown; cannot be inferred from repository/user name | Owner supplies the identity that passes Artifact Signing Public Trust validation; exact certificate subject is recorded and pinned |
| Azure signing account/subscription | Not authorized or provisioned | Owner provisions Artifact Signing account/profile and a GitHub OIDC identity with signer-only scope |
| Key/security custodian | Artifact Signing HSM selected; administrative custodian unassigned | A named owner accepts Azure resource/IAM/revocation responsibility |
| Release approval owner | Solo maintainer `Dvaderfun`; independent approval is unavailable | Protected environment requires explicit owner approval, permits self-review, and disables administrator bypass |
| Timestamp service | Microsoft endpoint selected; no signed artifact exists | First signed test artifact has a valid RFC 3161 countersignature verified offline/online |
| ARM64 capacity | ARM64 cross-compile passes; artifact measured at 779,776 bytes | Real Windows 11 ARM64 machine/VM passes runtime, UI, update, rollback, and uninstall suites |
| Winget package ownership | Proposed ID `Dvaderfun.Claudometer`; no namespace/public package claim | Owner confirms publisher/ID and an accepted `winget-pkgs` submission establishes ownership |
| Immutable releases/tag rules | Repository immutability enabled and verified on 2026-09-03; release workflow fails closed on that API and verifies downloaded draft assets | Active `main`/`v*` rulesets and the owner-approved `release` environment pass the workflow's live preflight |

No signing, package publication, certificate-store change, GitHub setting, or
Winget submission is authorized by this ADR.

## Consequences

- Updater implementation can branch on a fixed two-channel contract.
- Managed installations never self-modify, avoiding package-state drift.
- Release CI gains an external Azure/OIDC dependency, but no exportable signing
  secret.
- `DIST-00` remains unchecked until every named external owner/capacity item is
  actually evidenced; local implementation may continue independently.
