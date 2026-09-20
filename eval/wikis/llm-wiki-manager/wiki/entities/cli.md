---
type: overview
title: CLI
last_updated: 2026-07-01T22:00:00Z
tags: [cli]
related: [entities/commands.md, concepts/wiki-scripts.md]
code_refs: [bin/cli.ts, dist/bin/cli.js]
status: active
summary: Overview of bin/ — CLI entry point compiled to dist/bin/cli.js.
---

# CLI (`bin/`)

Scope tag: **`cli`** (first tag).

| File     | Role                                                               |
| -------- | ------------------------------------------------------------------ |
| `cli.ts` | Parses argv, dispatches subcommands, compiled to `dist/bin/cli.js` |

Default command when none is given: **`init`**. Global options: `--help`, `--version`.

## Command dispatch

| Command       | Handler module              |
| ------------- | --------------------------- |
| `init`        | `src/commands/init.ts`      |
| `upgrade`     | `src/commands/upgrade.ts`   |
| `help`        | `src/wiki/help.ts`          |
| `lint`        | `src/wiki/lint.ts`          |
| `build`       | `src/wiki/build-index.ts`   |
| `check`       | `src/wiki/build-index.ts`   |
| `sync`        | `src/wiki/sync-see-also.ts` |
| `log`         | `src/wiki/log.ts`           |
| `doctor`      | `src/wiki/doctor.ts`        |
| `setup-husky` | `src/wiki/setup-husky.ts`   |

Wiki subcommands accept `--wiki-dir` and `--repo-root`. See [Wiki Management Scripts](../concepts/wiki-scripts.md) for flags and npm script aliases.

## See also

- [Commands](commands.md)
- [Wiki Management Scripts](../concepts/wiki-scripts.md)
