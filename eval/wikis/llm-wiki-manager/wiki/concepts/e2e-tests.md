---
type: concept
title: E2E Tests
last_updated: 2026-07-02T18:00:00Z
tags: [testing, vitest, cli]
related:
  [concepts/unit-tests.md, concepts/init-command.md, concepts/dogfooding.md, entities/commands.md]
code_refs:
  [
    vitest.e2e.config.ts,
    test/e2e/cli-workflow.test.ts,
    test/e2e/tarball-smoke.test.ts,
    test/e2e/lint-staged-idempotence.test.ts,
    test/e2e/global-setup.ts,
    test/helpers/cli.ts,
    dist/bin/cli.js,
    src/wiki/migrate-pages.ts,
  ]
status: active
summary: End-to-end CLI workflow tests that scaffold temp projects and exercise init, wiki scripts, and upgrade against the compiled binary.
---

# E2E Tests

End-to-end tests verify the **compiled CLI** against disposable project directories — the same path consumers use after `npm install`. Run them separately from unit tests:

```bash
npm run test:e2e
```

Configuration is in `vitest.e2e.config.ts`. It includes only `test/e2e/**/*.test.ts`, sets a 30s timeout, and runs `test/e2e/global-setup.ts` before any test file.

## Global setup

`global-setup.ts` runs `npm run build` once so `dist/bin/cli.js` exists. E2e tests always target the built binary, not TypeScript sources directly.

## Test files

### `test/e2e/cli-workflow.test.ts`

Covers init, upgrade, and CLI meta behavior:

| Test area                 | What it verifies                                                                             |
| ------------------------- | -------------------------------------------------------------------------------------------- |
| Init scaffold             | Creates wiki, `AGENTS.md`, install config, `wiki:*` npm scripts, and `devDependencies` entry |
| Init npm install outro    | Shows **Final Step: npm install** when local bin is missing; omits it when bin exists        |
| Wiki CLI after init       | `lint`, `build`, and `check` on a fresh wiki with a user-added concept page                  |
| Upgrade refresh           | Restores meta files without overwriting user content pages                                   |
| Upgrade npm install outro | Shows **Final Step: npm install** when local bin is missing; omits it when bin exists        |
| Legacy page migration     | Upgrade calls internal `runMigrate` to rewrite deprecated frontmatter and body wikilinks     |
| Init without package.json | Scaffold succeeds; no npm scripts added                                                      |
| Re-init idempotency       | Preserves existing `log.md` and `schema.md`                                                  |
| Upgrade dry-run           | `--dry-run` reports changes without writing files                                            |
| Post-init index           | `index.md` is fresh so `check` passes without a manual build                                 |
| Managed section           | Upgrade preserves user content after the AGENTS.md end marker                                |
| UTF-8 BOM package.json    | Init succeeds; `doctor` reports no problems (tests stub local bin when needed)               |
| CLI meta flags            | `--version`, `--help`, and unknown-command error handling                                    |

Each test creates a temp directory with a minimal `package.json`, runs CLI commands via helpers, and cleans up in `afterEach`.

### `test/e2e/tarball-smoke.test.ts`

Packs the package with `npm pack`, installs it into a temp consumer project, and verifies `lint`, `build`, `check`, `sync`, and `doctor` work from the published tarball layout. Also covers the npx-style workflow: `init` → verify `devDependencies` → `npm install` tarball → `npm run wiki:lint`. After `build`, asserts bundled `prettier` is present and that `prettier --write` on `wiki/index.md` is a no-op.

### `test/e2e/lint-staged-idempotence.test.ts`

Reproduces the lint-staged empty-commit bug in a temp consumer project: `init` with `--focus-dirs src`, commit formatted `index.md`, then run `build` → `lint` → `prettier --write` and assert `index.md` is unchanged and `check` still passes.

## Helpers

| File                   | Role                                                                 |
| ---------------------- | -------------------------------------------------------------------- |
| `test/helpers/cli.ts`  | `runBuiltCli()` spawns `dist/bin/cli.js` in a temp project directory |
| `test/helpers/wiki.ts` | Shared frontmatter fixtures (`fm`, `writePage`) reused by unit tests |

Init invocations pass non-interactive flags (`--project-name`, `--wiki-dir`, `--focus-dirs`) so tests need no TTY input.

## CI and release

`npm run test:e2e` runs in `release:check` after unit tests and a production build. Together with [Unit Tests](unit-tests.md), it guards init and upgrade regressions before wiki lint.

Full `release:check` chain: `check:node-types` → `lint` → `format:check` → `test` → `build` → `test:e2e` → `wiki:lint` → `wiki:check`.

## See also

- [Unit Tests](unit-tests.md)
- [Init Command](init-command.md)
- [Commands](../entities/commands.md)
- [Dogfooding](dogfooding.md)
