---
type: overview
title: Utils
last_updated: 2026-07-02T12:00:00Z
tags: [utils]
related: [entities/commands.md, entities/templates.md, concepts/template-system.md]
code_refs: [src/utils/fs.ts, src/utils/upgrade.ts, src/utils/fs.test.ts]
status: active
summary: Overview of src/utils/ — template copy, interpolation, package.json merge, and upgrade orchestration.
---

# Utils (`src/utils/`)

Scope tag: **`utils`** (first tag).

## `fs.ts` — scaffold and config

| Concern        | Key exports                                                                                                                                                                                                                 |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Template paths | `templatePath`, `copyTemplate`, `scaffoldWikiTemplates`, `buildTemplateVars`                                                                                                                                                |
| Interpolation  | `interpolate`, `WIKI_META_UPGRADE_PATHS`, `WIKI_INIT_ONLY_PATHS`                                                                                                                                                            |
| Install config | `readInstallConfig`, `writeInstallConfig`, `isExistingInstall`, `inferInstallConfig`                                                                                                                                        |
| Entity stubs   | `scaffoldEntityOverviews`, `entityOverviewStub`, `scopeSlugFromFocusDir`                                                                                                                                                    |
| package.json   | `mergePackageJsonScripts`, `syncPackageJsonScripts`, `mergePackageJsonDevDependency`, `syncPackageJsonDevDependency`, `isPackageBinInstalled`, `packageBinPath`, `PACKAGE_NAME`, `wikiScriptCandidates`, `WIKI_SCRIPT_KEYS` |
| AGENTS.md      | `amendFile`, `replaceManagedSection`, managed marker normalization, standalone end-marker detection, and legacy-tail preservation                                                                                           |
| Misc           | `getPackageVersion`, `scaffoldWikiEmptyDirs`, `WIKI_EMPTY_DIRS`                                                                                                                                                             |

## `upgrade.ts` — upgrade orchestration

| Export                  | Role                                               |
| ----------------------- | -------------------------------------------------- |
| `runUpgradeSteps`       | Refresh templates, scripts, and AGENTS.md sections |
| `runPostUpgradeScripts` | Migrate pages, sync, build, warn-only lint         |
| `appendUpgradeLog`      | Append upgrade entry to `wiki/log.md`              |

## Tests

`fs.test.ts` covers template copy, interpolation, install config, scaffold helpers, managed `AGENTS.md` marker behavior (including fenced copied-marker examples), devDependency merging, and local binary detection.

## See also

- [Commands](commands.md)
- [Templates](templates.md)
- [Template System](../concepts/template-system.md)
