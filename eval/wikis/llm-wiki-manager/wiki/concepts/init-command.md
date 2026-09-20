---
type: concept
title: Init Command
last_updated: 2026-07-02T12:00:00Z
tags: [cli, scaffold]
related: [concepts/template-system.md, concepts/repo-layout.md, concepts/wiki-scripts.md]
code_refs: [src/commands/init.ts]
status: active
summary: How the init CLI scaffolds a wiki, AGENTS.md, npm scripts, and install config into a consumer project.
---

# Init Command

The `init` command in `src/commands/init.ts` interactively collects project settings (or reads non-interactive flags), then scaffolds artifacts into the consumer's working directory.

## Prompts and variables

| Input             | Default                                 | Used in                                          |
| ----------------- | --------------------------------------- | ------------------------------------------------ |
| Project name      | basename of cwd                         | `AGENTS.md`, `schema.md`                         |
| Wiki directory    | `wiki`                                  | paths, npm script targets                        |
| Focus directories | `src` (clear to document whole project) | `schema.md`, `AGENTS.md` scope, entity overviews |

These become interpolation variables (`PROJECT_NAME`, `WIKI_DIR`, `FOCUS_DIRS`, `FOCUS_DIRS_LIST`, `INIT_TIMESTAMP`) passed to [Template System](template-system.md).

## Non-interactive flags

For CI and tests, pass all values via flags (skips prompts):

```bash
llm-wiki-manager init --project-name my-app --wiki-dir wiki --focus-dirs src,api
```

| Flag             | Required | Default           |
| ---------------- | -------- | ----------------- |
| `--project-name` | yes      | —                 |
| `--wiki-dir`     | no       | `wiki`            |
| `--focus-dirs`   | no       | _(whole project)_ |

## Scaffold steps

1. **Wiki directory** — copies `templates/wiki/` via `scaffoldWikiTemplates`, creates empty dirs, and writes entity overview stubs when focus dirs are provided.
2. **index.md** — runs `runBuild` so `wiki:check` passes immediately after init.
3. **package.json** — when present:
   - merges missing `wiki:*` npm scripts (skipped if all wiki scripts already exist)
   - adds `llm-wiki-manager` to `devDependencies` when absent (runs even when script merge is skipped)
   - run `npm install` afterward so `npm run wiki:*` resolves the local CLI
4. **AGENTS.md** — amends repo-root `AGENTS.md` from `templates/AGENTS.md` (pointer template); vault copy comes from `templates/wiki/AGENTS.md` via the wiki scaffold.
5. **`.llm-wiki-manager.json`** — records install metadata (version, wiki dir, focus dirs).

Wiki management logic lives in the published package (`src/wiki/`), not as copied files in the consumer repo.

## Idempotency

Re-running `init` on an already-initialized project only creates missing scaffold files. It does not overwrite existing wiki content, `log.md`, or `schema.md`. Use `upgrade` to refresh template files.

## See also

- [Template System](template-system.md)
- [Repo Layout](repo-layout.md)
- [Wiki Management Scripts](wiki-scripts.md)
