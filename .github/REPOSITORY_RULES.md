# Required GitHub repository rules

These settings cannot be enforced by files in the repository. A repository
administrator must configure and verify them before the corresponding roadmap
acceptance items can be checked.

This is a solo-maintainer repository. Independent review is unavailable, so the
policy prevents accidental/direct publication but cannot protect against owner
account compromise. `REL-02` remains incomplete until production release
variables and secrets are provisioned and the workflow produces its first
evidence-bearing release.

Live audit on 2026-09-03: the `Protect main`, `Release tag creation`, and
`Immutable release tags` rulesets are active; immutable releases are enabled;
and the `release` environment requires `Dvaderfun`, permits self-review, blocks
administrator bypass, and accepts only `v*` tags. The production variables and
environment secrets below are not yet provisioned.

## `main` branch

- Create an active branch ruleset targeting `refs/heads/main`, with no bypass
  actors. Require pull requests, but set required approvals to zero because the
  owner cannot approve their own pull request.
- Dismiss stale approvals; do not require approval of the latest reviewable
  push in the zero-reviewer policy.
- Require conversation resolution.
- Require these checks from `.github/workflows/build.yml`:
  `Source gate`, `RustSec and OSV`, `Dependency review`, `Release build (x64)`,
  and `Release build (arm64)`.
- Require branches to be up to date, block force pushes and deletion, and do
  not allow bypass except a separately audited emergency role.

## Release tags and environment

- Create an active tag ruleset targeting `refs/tags/v*` that restricts tag
  creation to its release-role bypass actor. Use a separate active ruleset with
  no bypass actors to restrict updates and deletion of the same tags. Keeping
  creation separate means the release role cannot rewrite an existing tag.
- Protect the `release` environment with `Dvaderfun` as its required reviewer,
  allow self-review, and disable administrator bypass. Allow deployments from
  exactly the `v*` tag pattern.
- Store `CLAUDOMETER_RELEASE_PRIVATE_KEY_PKCS8_B64` and the fine-grained,
  Administration-read and Actions-read
  `CLAUDOMETER_RELEASE_ADMIN_READ_TOKEN` as `release` environment secrets. The
  latter lets the workflow prove that release immutability and every required
  protection are still enabled before it uses signing material.
- Configure these repository variables so the reusable build and protected
  release job receive the same public policy:
  `CLAUDOMETER_RELEASE_PUBLIC_KEY_HEX`, `CLAUDOMETER_RELEASE_SEQUENCE`, and
  `CLAUDOMETER_MINIMUM_UPDATER_VERSION`. Increment the sequence before each
  new release and never reuse a value.
- Keep workflow token permissions read-only by default; grant
  `contents: write`, `id-token: write`, `attestations: write`, and
  `artifact-metadata: write` only to the protected release job. SBOM generation
  runs separately with `contents: read` and never receives release secrets.
- Enable immutable releases under **Settings → General → Releases**. The
  workflow refuses to create a draft unless this setting and every ruleset and
  environment protection above are active. Never edit or reuse a version/tag;
  replace it with a new version.

The workflow creates a draft from the exact artifacts produced by the reusable
source gate, downloads and verifies every asset, verifies GitHub provenance and
SBOM attestations, exercises the downloaded x64 binary in isolated demo mode,
and only then publishes. A failed smoke test removes the draft. Signing-key
provisioning, approval ownership, and repository-setting changes remain
administrator operations.
