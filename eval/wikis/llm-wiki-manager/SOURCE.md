# Vendored wiki: lggarrison/llm-wiki-manager

Not written by this project. Copied verbatim so the spike in
[#5](https://github.com/mikekelly/s1m/issues/5) and later stages have a real linked
wiki to score, with no network access and no private content.

| | |
| --- | --- |
| Source | <https://github.com/lggarrison/llm-wiki-manager> |
| Path copied | `wiki/` |
| Commit | `d532d8a23c697ff5f4aa8876e34894c2ed46af2f` (2026-07-10, `develop`) |
| Licence | MIT, Copyright (c) 2026 Lacy Garrison — [`LICENSE`](LICENSE) |
| Copied | `wiki/` to `wiki/`, `LICENSE` to `LICENSE` |

`wiki/` is the vault the project's own tooling maintains: one index page, nine
`concepts/` pages, four `entities/` pages, plus the schema, log and raw-source pages —
19 pages of relative markdown links, which is what `s1m::parse` follows. Empty
placeholder directories (`.gitkeep`) were dropped; nothing else was changed, so the
copy carries the vault's own `AGENTS.md` and `README.md` as well — they are the
vault's conventions for its maintainers, and mean nothing for this repository.

Entry points used by the spike: [`wiki/index.md`](wiki/index.md) (a hub page: 14 links,
no content of its own) and [`wiki/concepts/release.md`](wiki/concepts/release.md) (a leaf
page that only says the runbook lives elsewhere).

To re-copy or update:

```bash
git clone --depth 1 --branch develop https://github.com/lggarrison/llm-wiki-manager /tmp/llm-wiki-manager
cp -r /tmp/llm-wiki-manager/wiki eval/wikis/llm-wiki-manager/wiki
cp /tmp/llm-wiki-manager/LICENSE eval/wikis/llm-wiki-manager/LICENSE
```

Then update the commit above.
