# LLM Wiki — Agent Entry

The wiki implements Karpathy's LLM-Wiki pattern: a persistent, compounding knowledge base that an LLM agent owns and maintains, sitting between the team and the raw sources. It is plain markdown, doubles as an Obsidian vault, and is wired into the repo's tooling (npm scripts, a pre-commit hook, and tool-specific discovery shims) so it stays current as the code changes. Code in your app's source code remains the source of truth for behavior; wiki pages describe and cite code (via `code_refs:` frontmatter) but never duplicate it.

Read **`schema.md`** first for the full specification. This file is the quick orientation for agents maintaining this vault.

**Documentation scope** — the entire project

---

## Architecture

See [schema.md](schema.md) § Directory Layout. Key rules:

- **`entities/`** is flat — no subdirectories; scope = first tag in `tags:`
- **`concepts/`** — cross-cutting topics (shared mechanisms, architecture)
- **`raw/raw.md`** — hub for immutable ingested artifacts
- **`index.md`** — auto-generated; run `npm run wiki:build`

Run `npm run wiki:help` from the project root for wiki commands.

---

## Gotchas

Quick pitfalls for new maintainers:

- **Never hand-edit `index.md` tables** — they are regenerated from frontmatter. Edit only the prose preamble; run `npm run wiki:build` after frontmatter changes.
- **`raw/` is immutable.** Corrections mean ingesting a new dated source — do not edit the artifact in place.
- **No sub-folder hubs under `entities/`.** The directory is flat by design; scope bucketing is the first tag in `tags:`.
- **New app dir under `src/ui/` or `src/api/`** (or any path listed in `.entity-scopes`) → add `entities/<slug>.md` with `type: overview` or lint fails.
- **Stop-and-ask code triggers** (e.g. shared CSS variables, z-index tokens, overlay placement — see repo-root AGENTS.md or CLAUDE.md if present) usually require a matching wiki update via the **Maintenance trigger** workflow in §7.
- **Deleting a wiki page needs user confirmation.** Prefer `status: deprecated` plus a `log.md` entry via `npm run wiki:log -- add maintenance "..."`.

---

## §3 Page types

Every page declares its role with `type:` in frontmatter. `npm run wiki:lint` enforces type values and placement.

| Type         | Placement                                     | Purpose                                                                                            |
| ------------ | --------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `overview`   | `entities/<slug>.md`                          | One scope entry point per documented source area (see `.entity-scopes`)                            |
| `entity`     | `entities/<slug>.md`                          | A concrete feature, module, or component                                                           |
| `comparison` | `entities/<slug>.md`                          | Same topic across two or more scopes                                                               |
| `deep-dive`  | `entities/<slug>.md`                          | Long-form reference; co-locate by filename (e.g. `bubbles.md` + `bubbles-architecture-diagram.md`) |
| `concept`    | `concepts/<slug>.md`                          | Genuinely cross-scope pattern or mechanism                                                         |
| `source`     | `sources/<slug>.md`                           | LLM summary of one raw artifact                                                                    |
| `hub`        | `README.md`, `index.md`, or `raw/raw.md` only | Vault navigation/meta pages                                                                        |

Quick placement guide:

- Scope entry points and entity-family pages → **`entities/`** (never nested)
- Cross-cutting synthesis → **`concepts/`**
- Ingest summaries → **`sources/`** (basename must match the paired raw file)
- Meta/navigation → **`hub`** at the paths above

---

## §3a Scope-tag convention

On every `entities/<slug>.md` page (`overview`, `entity`, `comparison`, `deep-dive`), the **first tag** is the scope slug. It maps to a documented source directory — see [schema.md](schema.md) § Flat `entities/` namespace.

Do not encode scope with folder nesting under `entities/`.

---

## §4 Frontmatter

### Required

| Field          | Notes                                                               |
| -------------- | ------------------------------------------------------------------- |
| `type`         | One of the page types in §3                                         |
| `title`        | Human-readable; used in `index.md`                                  |
| `last_updated` | UTC ISO 8601 timestamp `YYYY-MM-DDTHH:MM:SSZ`; update on every edit |

### Encouraged

| Field       | Notes                                                                                  |
| ----------- | -------------------------------------------------------------------------------------- |
| `aliases`   | Alternate titles for search                                                            |
| `tags`      | Topic labels; first tag is the scope slug on entity-family pages                       |
| `related`   | Wiki-root-relative paths to related pages                                              |
| `sources`   | Paths to source summaries that back this page (entities and concepts)                  |
| `code_refs` | Repo-root-relative code paths (e.g. `src/commands/init.ts`); lint verifies each exists |
| `status`    | `active` · `deprecated` · `wip`                                                        |
| `summary`   | One sentence; shown in index tables                                                    |

Two field semantics worth internalizing:

- **`code_refs:`** — repo-root-relative paths. Lint verifies each exists on disk. This is the canonical place for code links — never wrap code paths in markdown links in the body.
- **`related:` ↔ body links** — every `related:` entry must also appear as a body markdown link (typically under `## See also`). Obsidian only renders graph edges from body links, not plain-string frontmatter. Lint warns on unmirrored entries; `npm run wiki:sync` auto-fixes.

---

## §5 Linking conventions

Lint enforces these rules:

- **Markdown links only** — no Obsidian `[[wikilinks]]` (breaks GitHub, IDE, and lint)
- **Relative paths** between wiki pages; code paths stay as inline backticks, not links
- **Body links target `.md` pages** — not directories or repo files (except `raw/raw.md` may link to `raw/` category folders)
- **No phantom-node links** — targets outside the vault or pointing at directories
- **No self-loop links** — a `sources/<id>.md` page must not body-link to its paired `raw/.../<id>.md`; put the raw path in frontmatter instead
- **Kebab-case filenames** — lowercase, hyphen-separated (e.g. `init-command.md`)

Source pairing: `sources/<slug>.md` shares the basename of its raw artifact (`raw/articles/<slug>.md`, etc.).

---

## §7 The three workflows

These are what the agent actually does with the wiki.

### Ingest (a new artifact lands in raw/)

1. Read the artifact end-to-end.
2. Discuss key takeaways with the user; confirm scope.
3. Place the artifact under `raw/<category>/` if not already there (articles, prs, tickets, design-notes, transcripts).
4. Write `sources/<id>.md` summarizing it (`type: source`; basename must match the raw file).
5. Update affected `entities/` and `concepts/` pages: bump `last_updated`, add the source path to `sources:`, weave in new claims, flag contradictions (see **Contradictions** below).
6. A single source can touch 5–15 pages in one pass — that's normal.
7. `npm run wiki:sync` — sync new `related:` entries to body links.
8. `npm run wiki:build` — regenerate `index.md`.
9. `npm run wiki:log -- add ingest "<title>"`.
10. `npm run wiki:lint`; fix errors.

### Query (user asks a question)

1. **Find candidates:** prefer `qmd query "<q>" -c <collection> --files --min-score 0.3` if qmd is installed; otherwise read `index.md` and grep.
2. Drill into the relevant `entities/`, `concepts/`, and `sources/` pages; cite each page and its `code_refs:`.
3. If the answer is novel and reusable (a synthesis, comparison, or discovered connection), file it back — usually a new deep-dive next to the most relevant entity. Update neighbors' `related:`, bump `last_updated`, append `npm run wiki:log -- add query "<summary>"`.
4. Stub gaps as `status: wip` (see **Gaps** below).

### Maintenance trigger (agent edits src/ non-trivially)

1. Grep `entities/*.md` and `concepts/*.md` for the touched file path in `code_refs:`.
2. For each match, update the page (and `last_updated`) in the same change.
3. If a top-level `src/ui/` or `src/api/` dir was added or removed, add or deprecate the corresponding `entities/<slug>.md` overview and note it in `log.md` via `npm run wiki:log -- add maintenance "<note>"`.
4. `npm run wiki:lint`.

This is the "compound interest" mechanism: docs move in the same PR as the code, so the wiki never drifts.

---

## Contradictions

Flag both pages with a `> ⚠️ Contradiction:` blockquote and create a reconciliation concept page.

## Gaps

Create stub pages (`status: wip`) rather than leaving broken `related:` references.

---

## Where to go next

- **Schema & workflows (authoritative):** `wiki/AGENTS.md`
- **Human onboarding / browsing:** `wiki/README.md`
- **Content catalog:** `wiki/index.md`
- **Event log:** `wiki/log.md`
- **Search setup & CLI:** `wiki/concepts/wiki-search.md`
- **CLI:** `llm-wiki-manager` subcommands (lint, build, check, sync, log, help, setup-husky)
