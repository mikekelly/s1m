---
type: concept
title: Template System
last_updated: 2026-07-02T12:00:00Z
tags: [templates, scaffold]
related: [concepts/init-command.md, concepts/repo-layout.md, concepts/dogfooding.md]
code_refs: [src/utils/fs.ts, templates/AGENTS.md, templates/wiki/schema.md]
status: active
summary: templates/ is the source of truth for wiki scaffold files; init copies and interpolates placeholders into consumer projects.
---

# Template System

Published package contents include `templates/` (see `package.json` `"files"`). The CLI resolves template paths relative to the installed package root via `templatePath()` in `src/utils/fs.ts`.

## Directory layout

```
templates/
├── wiki/
│   ├── schema.md, AGENTS.md, README.md
│   ├── index.md, log.md
│   ├── raw/raw.md
│   └── .entity-scopes
├── AGENTS.md       repo-root pointer template
└── scripts/        legacy .mjs mirror (dogfood sync only — not copied by init)
```

Wiki management logic (lint, build, sync, log, doctor, etc.) lives in `src/wiki/` and ships as compiled JavaScript in `dist/` — not as copied template scripts.

## Interpolation

`copyTemplate` and scaffold helpers recursively copy template files, then walk interpolated file types replacing `{{VAR}}` placeholders via `interpolate()`.

Interpolated extensions: `.md`, `.mjs`, `.js`, `.json`, and `.entity-scopes` (`shouldInterpolateFile` in `src/utils/fs.ts`).

Common variables:

| Variable          | Example                    |
| ----------------- | -------------------------- |
| `PROJECT_NAME`    | `llm-wiki-manager`         |
| `WIKI_DIR`        | `wiki`                     |
| `FOCUS_DIRS`      | `` `src/`, `templates/` `` |
| `FOCUS_DIRS_LIST` | bullet list for schema.md  |
| `INIT_TIMESTAMP`  | UTC ISO timestamp of init  |

## Templates vs consumer output

Consumers receive **copies** of wiki templates under their chosen wiki path. npm scripts invoke `llm-wiki-manager` subcommands — no script files are vendored into consumer repos.

Upgrade refreshes meta paths in `WIKI_META_UPGRADE_PATHS`; init-only paths (`index.md`, `log.md`) are written on first init but not overwritten on re-init.

Root `AGENTS.md` is managed between explicit start/end markers. The managed-section helpers normalize marker-bearing templates, require standalone end-marker lines outside fenced code, and avoid discarding unbounded legacy content when upgrading files created before end markers existed.

## See also

- [Dogfooding](dogfooding.md)
- [Init Command](init-command.md)
- [Repo Layout](repo-layout.md)
