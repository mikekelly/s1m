---
type: concept
title: Release
last_updated: 2026-07-02T04:30:00Z
tags: [release, maintainers]
related: [concepts/dogfooding.md, concepts/repo-layout.md, concepts/node-version-and-types.md]
code_refs: [RELEASING.md, .github/workflows/release.yml]
status: active
summary: Pointer to RELEASING.md — maintainer runbook for cutting releases and syncing main back into develop.
---

# Release

Release procedures for **llm-wiki-manager** are maintained outside the wiki vault.

**Source of truth:** `RELEASING.md` at the repo root covers branching (`develop` / `main`), semver, tagging, npm Trusted Publishing, the release workflow (`.github/workflows/release.yml`), post-release sync of `main` back into `develop`, troubleshooting, and manual fallbacks.

Do not duplicate that runbook here. When cutting a release or debugging a failed publish, read `RELEASING.md` directly.

For how release validation fits into local gates and CI, see [Node Version and @types/node Alignment](node-version-and-types.md) and [Dogfooding](dogfooding.md).

## See also

- [Dogfooding](dogfooding.md)
- [Repository Layout](repo-layout.md)
- [Node Version and @types/node Alignment](node-version-and-types.md)
