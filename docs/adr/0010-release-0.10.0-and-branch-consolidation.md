# ADR 0010: Release 0.10.0 and consolidate branches

- Status: Accepted by explicit owner request, 2026-10-10
- Scope: merge all completed work into main, clean integrated branches, release

The owner explicitly requested merging the completed work onto main, clearing
branches and publishing a new version. This authorizes the version PR/merge,
new v0.10.0 immutable tag, release-environment approval and publication through
the existing protected workflow. It also authorizes the necessary monotonic
release-sequence input (3, following 0.9.1 sequence 2) and temporary protected
administration-read audit credential, removed after the workflow finishes.
Existing main/tag/release protections stay enabled. No provider credential
operation, new signing identity, key rotation or general future release is
covered by this request.

0.10.0 includes CODEX-01/02, PACE-01, ROW-01, FRESH-01, UI-CLARITY-01 and the
Vibecode caption alignment fix. The documented Codex source remains opt-in
because its measured p95 exceeds the default-source gate. Windows executables
remain portable and Authenticode-unsigned; manifests and provenance follow the
existing R0 distribution contract. Accessibility/real ARM64 and physical
lid-close verification limits are disclosed in the release notes.

Branches may be deleted only after ancestry, patch equivalence or exact tree
comparison proves their work survives on main. Historical immutable release
tags remain intact, including the failed/unreleased v0.9.0 tag. Detached
measurement worktrees are retained as verification artifacts. No force push,
protection bypass or release asset/tag reuse is allowed.

Rollback: publish a new higher-sequence authorized rollback; never overwrite a
tag or asset. Disable Vibecode or exit normally before replacing a portable
binary; verified journal recovery remains mandatory. Settings/cache schemas
are compatible with 0.9.1, which resumes the former independent power controls.
