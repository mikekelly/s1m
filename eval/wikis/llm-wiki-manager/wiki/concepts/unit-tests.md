---
type: concept
title: Unit Tests
last_updated: 2026-07-02T12:00:00Z
tags: [testing, vitest]
related:
  [
    concepts/e2e-tests.md,
    concepts/wiki-scripts.md,
    concepts/dogfooding.md,
    concepts/node-version-and-types.md,
    concepts/repo-layout.md,
  ]
code_refs:
  [
    vitest.config.ts,
    src/utils/fs.test.ts,
    test/helpers/wiki.ts,
    test/scripts/lint.test.ts,
    test/commands/upgrade.test.ts,
    test/commands/doctor.test.ts,
    test/workflows/release.test.ts,
  ]
status: active
summary: Vitest unit and integration tests for src/ utilities, CLI helpers, and wiki subcommand behavior.
---

# Unit Tests

The package uses [Vitest](https://vitest.dev) for fast, isolated tests. Run the default suite from the repo root:

```bash
npm test
```

Configuration lives in `vitest.config.ts`: it includes `src/**/*.test.ts` and `test/**/*.test.ts`, and **excludes** `test/e2e/` (see [E2E Tests](e2e-tests.md)).

## Layout

| Path                             | Role                                                                                                           |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `src/utils/fs.test.ts`           | Template copy, interpolation, install config, scaffold helpers, managed markers, devDependency merge           |
| `test/commands/upgrade.test.ts`  | Upgrade step orchestration, AGENTS.md managed section, page migration                                          |
| `test/commands/doctor.test.ts`   | Health-check CLI: missing files, stale index, script mismatches, missing local install when wiki scripts exist |
| `test/scripts/*.test.ts`         | Behavior of each `llm-wiki-manager` subcommand via built CLI                                                   |
| `test/workflows/release.test.ts` | Release workflow and RELEASING.md post-release sync invariants                                                 |
| `test/helpers/wiki.ts`           | Temp wiki dirs, frontmatter fixtures, CLI helpers                                                              |

Packed-install smoke tests live under `test/e2e/tarball-smoke.test.ts` and run via `npm run test:e2e`, not `npm test`.

Script tests invoke the **built CLI** (`dist/bin/cli.js`) so behavior matches what consumers run.

## Script test coverage

Each wiki subcommand has a dedicated test file under `test/scripts/`:

| Test file               | CLI subcommand                                                          |
| ----------------------- | ----------------------------------------------------------------------- |
| `lint.test.ts`          | `lint`                                                                  |
| `build-index.test.ts`   | `build`, `check`                                                        |
| `sync-see-also.test.ts` | `sync`                                                                  |
| `log.test.ts`           | `log`                                                                   |
| `help.test.ts`          | `help`                                                                  |
| `setup-husky.test.ts`   | `setup-husky`                                                           |
| `migrate-pages.test.ts` | `runMigrate` legacy frontmatter and fenced-code-safe wikilink migration |
| `scripts.test.ts`       | npm alias smoke tests                                                   |

Tests use temporary wiki directories created by `makeTmpWikiDir()` and tear them down in `afterEach` hooks.

## CI and release

`npm run release:check` runs the full gate chain before publish validation:

```
check:node-types → lint → format:check → test → build → test:e2e → wiki:lint → wiki:check
```

CI (`.github/workflows/ci.yml`) runs the same gates on Node 20 and 24. Note: CI runs `build` before `test`; `release:check` runs `test` then `build` then `test:e2e` (e2e global setup rebuilds anyway).

See [Node Version and @types/node Alignment](node-version-and-types.md) for the Node/types guard.

## See also

- [E2E Tests](e2e-tests.md)
- [Wiki Management Scripts](wiki-scripts.md)
- [Dogfooding](dogfooding.md)
- [Node Version and @types/node Alignment](node-version-and-types.md)
- [Repository Layout](repo-layout.md)
