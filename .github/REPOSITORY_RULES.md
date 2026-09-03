# Required GitHub repository rules

These settings cannot be enforced by files in the repository. A repository
administrator must configure and verify them before the corresponding roadmap
acceptance items can be checked.

## `main` branch

- Require pull requests and at least one approval.
- Dismiss stale approvals and require approval of the latest reviewable push.
- Require conversation resolution.
- Require these checks from `.github/workflows/build.yml`:
  `Source gate`, `RustSec and OSV`, `Dependency review`, `Release build (x64)`,
  and `Release build (arm64)`.
- Require branches to be up to date, block force pushes and deletion, and do
  not allow bypass except a separately audited emergency role.

## Release tags and environment

- Protect `v*` tags from creation, update, and deletion except through the
  approval-gated release role.
- Protect the `release` environment with a named human approver and no
  administrator bypass.
- Keep workflow token permissions read-only by default; grant
  `contents: write` only to the draft-release job.
- Enable immutable releases. Until that setting is available, never edit a
  published release or reuse a version/tag; replace it with a new version.

The workflow deliberately creates a draft from the exact artifacts produced by
the reusable source gate. Publishing, signing-provider configuration, approval
ownership, and repository-setting changes remain external operations.
