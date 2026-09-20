# llm-wiki-manager Wiki

Human entry point for browsing this vault in [Obsidian](https://obsidian.md/) or any markdown viewer.

## Start here

| File                   | Purpose                                            |
| ---------------------- | -------------------------------------------------- |
| [index.md](index.md)   | Auto-generated catalog of all wiki pages           |
| [schema.md](schema.md) | Full conventions — frontmatter, layout, operations |
| [AGENTS.md](AGENTS.md) | Agent instructions (Cursor / LLM maintainers)      |
| [log.md](log.md)       | Chronological record of wiki operations            |

## Layout (short)

- **`entities/`** — flat namespace; one page per documented source scope (`entities/<slug>.md`)
- **`concepts/`** — cross-cutting architecture and mechanisms
- **`sources/`** — LLM summaries of ingested raw artifacts
- **`raw/`** — immutable inputs ([raw.md](raw/raw.md) hub → `articles/`, `prs/`, etc.)

These are plain markdown files — no editor config ships with the scaffold. Open the folder as a vault in [Obsidian](https://obsidian.md/) for a linked graph view; add your own `.obsidian/` settings if you want, and the wiki tooling will ignore them.

Content in `entities/`, `concepts/`, and `sources/` grows through your day-to-day work — the scaffold only creates empty structure and scope stubs.
