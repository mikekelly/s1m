---
type: concept
title: Wiki Management Scripts
last_updated: 2026-07-02T18:00:00Z
tags: [scripts, lint, maintenance]
related:
  [
    concepts/repo-layout.md,
    concepts/template-system.md,
    concepts/unit-tests.md,
    concepts/init-command.md,
    entities/cli.md,
  ]
code_refs:
  [
    bin/cli.ts,
    src/wiki/lint.ts,
    src/wiki/build-index.ts,
    src/wiki/sync-see-also.ts,
    src/wiki/log.ts,
    src/wiki/help.ts,
    src/wiki/doctor.ts,
    src/wiki/setup-husky.ts,
    src/wiki/migrate-pages.ts,
    src/wiki/constants.ts,
  ]
status: active
summary: llm-wiki-manager CLI subcommands that validate, index, sync, and log wiki operations.
---

# Wiki Management Scripts

After [Init Command](init-command.md), `package.json` gains `wiki:*` npm scripts that delegate to `llm-wiki-manager` subcommands and records `llm-wiki-manager` in `devDependencies` when absent. Run `npm install` so `npm run wiki:*` resolves the local CLI binary in `node_modules/.bin/`. Both `init` and `upgrade` show a **Final Step: npm install** outro when the local bin is missing. Implementation lives in `src/wiki/` inside the package — nothing is copied into consumer projects.

## Commands

| CLI subcommand | npm command        | Purpose                                                  |
| -------------- | ------------------ | -------------------------------------------------------- |
| `help`         | `wiki:help`        | List commands and typical workflows                      |
| `lint`         | `wiki:lint`        | Validate frontmatter, links, orphans; scan AGENTS.md     |
| `build`        | `wiki:build`       | Regenerate `index.md`                                    |
| `check`        | `wiki:check`       | Verify `index.md` is up to date (read-only)              |
| `sync`         | `wiki:sync`        | Add body links for `related:` frontmatter entries        |
| `log`          | `wiki:log`         | Append ingest/query/lint/maintenance entries to `log.md` |
| `doctor`       | `wiki:doctor`      | Health-check scaffold; suggest fixes for common issues   |
| `setup-husky`  | `wiki:setup:husky` | Wire pre-push `wiki:check`; print lint-staged guide      |

`doctor` also reports when `wiki:*` scripts exist but `llm-wiki-manager` is not installed locally (missing `node_modules/.bin/` shim) and suggests `npm install`.

`migrate-pages` runs internally during `upgrade` via `runMigrate` — not exposed as a public subcommand. It rewrites legacy frontmatter status/timestamp fields only inside YAML frontmatter, and rewrites body wikilinks to markdown links relative to the page being migrated while leaving fenced code examples untouched. Bare-slug wikilinks prefer an unambiguous existing page target.

## Flags

Wiki subcommands accept `--wiki-dir` and `--repo-root` for path resolution. Additional flags:

| Subcommand | Flag          | Effect                          |
| ---------- | ------------- | ------------------------------- |
| `lint`     | `--warn-only` | Report issues without exiting 1 |
| `sync`     | `--dry`       | Preview link additions only     |

## Path resolution

Subcommands resolve the wiki directory from `--wiki-dir`, then `.llm-wiki-manager.json`, then root `AGENTS.md`, defaulting to `wiki/`. All paths are relative to the consumer's project root (`process.cwd()`).

## Meta files excluded from page lint

`lint` and `build` skip structural/meta files (not wiki pages with frontmatter): `index.md`, `log.md`, `schema.md`, `README.md`, and `AGENTS.md` at the wiki root (`META_SKIP` in `src/wiki/constants.ts`).

## Typical workflow

After editing concept pages:

```
npm run wiki:sync
npm run wiki:build
npm run wiki:lint
```

Use `wiki:check` (read-only) in consumer CI and pre-push hooks to catch stale `index.md` without rewriting files.

## Git hooks

- **Consumers:** `wiki:setup:husky` wires pre-push `wiki:check` and prints a lint-staged snippet for pre-commit wiki validation.
- **This repo (maintainers):** pre-push runs the full `release:check` chain (see [Dogfooding](dogfooding.md)), not just `wiki:check`.

## See also

- [Template System](template-system.md)
- [Repo Layout](repo-layout.md)
- [Unit Tests](unit-tests.md)
- [Init Command](init-command.md)
- [CLI](../entities/cli.md)
