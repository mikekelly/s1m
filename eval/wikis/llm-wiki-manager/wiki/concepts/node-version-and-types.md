---
type: concept
title: Node Version and @types/node Alignment
last_updated: 2026-07-01T22:00:00Z
tags: [toolchain, typescript, ci]
related:
  [
    concepts/repo-layout.md,
    concepts/dogfooding.md,
    concepts/unit-tests.md,
    concepts/e2e-tests.md,
    concepts/release.md,
  ]
code_refs:
  [
    .nvmrc,
    package.json,
    tsconfig.json,
    .github/dependabot.yml,
    .github/workflows/ci.yml,
    .github/workflows/release.yml,
  ]
status: active
summary: Runtime floor is Node >=20.12 (tested by a CI matrix on 20 and 24, Linux and Windows); development is pinned to Node 24 via .nvmrc with @types/node aligned.
---

# Node Version and @types/node Alignment

This repo distinguishes a **runtime floor** (what consumers of the published CLI need) from the **development pin** (what maintainers and CI primarily use). Both are tested.

## Current policy

| Signal              | Location                        | Value                         | Purpose                                                           |
| ------------------- | ------------------------------- | ----------------------------- | ----------------------------------------------------------------- |
| Version manager pin | `.nvmrc`                        | `24`                          | Development pin: `nvm use` / `fnm use`; release workflow Node     |
| npm engines         | `package.json` `engines.node`   | `>=20.12.0`                   | Runtime floor for consumers (set by `@clack/prompts` `>=20.12.0`) |
| TypeScript types    | `package.json` `@types/node`    | `^24`                         | Compile-time API surface, aligned to `.nvmrc`                     |
| CI matrix           | `.github/workflows/ci.yml`      | Node 20 + 24, Linux + Windows | Every gate runs on the floor and the pin, on both OSes            |
| Release workflow    | `.github/workflows/release.yml` | Node from `.nvmrc`            | Releases build on the development pin                             |

## Why the floor is >=20.12 (not >=24)

Node 22 is in LTS well into 2027, and the CLI only uses `fs`, `path`, `url`, and `child_process` APIs available since Node 20. Requiring `>=24` cut off a large share of potential users for no technical reason. The floor is `20.12.0` because the runtime dependency `@clack/prompts` requires `>= 20.12.0`.

The earlier `>=24` policy existed because CI only tested one Node version, and claiming untested support would have been dishonest. That objection is resolved: the CI matrix now runs the full gate (lint, tests, e2e including the packed-tarball smoke test, wiki checks) on Node 20 and 24, on both Ubuntu and Windows.

## @types/node alignment

`@types/node` major versions track Node.js major versions (`@types/node@24` → Node 24 APIs, etc.).

**Rule:** `@types/node` must match the **`.nvmrc` major**, not the latest DefinitelyTyped release.

| Mismatch                            | Risk                                                         |
| ----------------------------------- | ------------------------------------------------------------ |
| `@types/node@26` with `.nvmrc` `24` | TypeScript accepts Node 26 APIs that do not exist at runtime |
| `@types/node@22` with `.nvmrc` `24` | Missing types for Node 24 APIs you may legitimately use      |

Because types track the development pin (24) while the floor is 20, TypeScript alone will not catch use of a Node-24-only API. The Node 20 legs of the CI matrix are the guardrail: code that calls an API missing on Node 20 fails there at test time.

TypeScript loads these types via `tsconfig.json` (`"types": ["node"]`).

## Enforcement

The [`check-node-types`](https://www.npmjs.com/package/check-node-types) dev dependency compares majors:

```bash
npm run check:node-types
# → check-node-types --source nvmrc
```

It reads `.nvmrc` and `@types/node` from `package.json`, then exits non-zero on mismatch.

**Where it runs:**

| Context                  | Command chain                                                       |
| ------------------------ | ------------------------------------------------------------------- |
| Local release validation | `npm run release:check` (first step)                                |
| CI                       | `.github/workflows/ci.yml` — after `npm ci`, before lint (all legs) |
| Release workflow         | `.github/workflows/release.yml` uses the same Node pin via `.nvmrc` |

## Dependabot

Dependabot **cannot read `.nvmrc`** when choosing npm version bumps. Without guardrails it will propose `@types/node` major upgrades (e.g. 24 → 26) that violate the alignment rule.

`.github/dependabot.yml` ignores semver-major updates for `@types/node`. Patch and minor updates within the current major still flow through normally.

## Raising the floor or the pin

When intentionally moving to a new development Node major:

1. Bump `.nvmrc`
2. Bump `@types/node` to the matching major (e.g. `^26.0.0`)
3. Add the new major to the CI matrix in `.github/workflows/ci.yml`
4. Run `npm install` to refresh the lockfile
5. Update README and CONTRIBUTING
6. Run `npm run check:node-types` and `npm run release:check`

When raising the **runtime floor** (`engines.node`), also remove the dropped major from the CI matrix, and confirm dependency engines (`@clack/prompts`) still fit the new floor. Raising the floor is a **breaking change for consumers** — bump the major version.

## See also

- [Repository Layout](repo-layout.md)
- [Dogfooding](dogfooding.md)
- [Unit Tests](unit-tests.md)
- [E2E Tests](e2e-tests.md)
- [Release](release.md)
