---
name: s1m
license: MIT
description: >
  Find what to read in a local markdown wiki. s1m reads local markdown files
  and ranks them for a query, walking the links between them, so an agent opens
  only the pages and the line ranges that matter instead of reading a corpus to
  find three pages. Use it when a task starts from a wiki, a notes vault or a
  docs tree and the answer is somewhere in it, when a grep would match wording
  rather than meaning, or when opening files to find files is what is eating
  the context window. It returns a ranked reading list with line ranges, not
  prose: no summary, no answer, nothing generated. The name is pronounced
  "sim" — short for System 1 memex — and s1m is not a simulator. Point it at
  pages, then read what it hands back.
---

# s1m: find what to read

`s1m` walks the links of a markdown wiki from one or more entry files, judging each page and
each outgoing link against a query with a fast model, and prints a ranked reading list: which
files to open, which lines inside them, and the link path that reached each one. The work of
choosing is done by code and small judgments; the caller still does the reading.

## When to reach for it

- A task starts "look in the wiki / the vault / `docs/`" and you do not know which page holds it.
- A keyword search would match wording, not meaning, and would ignore the links the authors built.
- You would otherwise open pages one by one until you found the right one — that cost is the
  context window, and s1m exists to cut it.

Do not use it to answer a question directly or to summarise anything: it generates no text. It
tells you where to read.

## Install

```bash
# macOS or Linux, prebuilt, no Rust toolchain: $XDG_BIN_HOME, else ~/.local/bin
curl -LsSf https://github.com/mikekelly/s1m/releases/latest/download/s1m-installer.sh | sh

export TYPESAFE_API_KEY=...        # see .env.example in the repository
```

Or from source, which needs stable Rust (1.85+): `cargo install --git
https://github.com/mikekelly/s1m`, or `cargo build --release` in a checkout, which leaves the
binary at `target/release/s1m`.

## One command

```bash
s1m "<what you want to find out>" <entry-file> [more-entry-files...]
```

The entry file is where the walk starts: usually the wiki's `index.md`, `README.md` or the page
you already know is close. Everything else is flags, and every one of them has a default that is
right for a first run:

| Flag | Default | Reach for it when |
| --- | --- | --- |
| `--mode about\|useful-for\|answers` | `useful-for` | Browsing a subject (`about`) or looking for one page that answers a question (`answers`) rather than doing a task |
| `--criteria FILE` | none | The judgment you need is not one of the three modes; the file's content is the criterion |
| `--max-files N` | 25 | You want a shorter list, or a wider net. It counts the files the walk judges beyond the entry files, which are always visited |
| `--max-depth N` | 6 | The useful pages are further from the entry than six hops |
| `--threshold F` | 0.6 | Fewer links followed, or more (`--max-files` binds first on most wikis); it is also the least relevance or section score that earns a file a place in the list |
| `--format json\|md\|tree` | `json` | `md` to paste the list into your own context — the files that earned a place, with the lines to read; `tree` to see the whole walk, including the hubs it passed through, and why a link was not followed |
| `--root DIR` | entry file's directory | The walk should be bounded somewhere else |

A typical run: `s1m "how do I cut a release and publish the package" docs/index.md`.

## Read the result

`--format json` (the default) is what a program parses. The shape, with the middle elided:

```json
{
  "query": "how do I cut a release and publish the package",
  "mode": "useful-for",
  "visited": 5,
  "calls": 5,
  "results": [
    {
      "path": "docs/concepts/release.md",
      "relevance": 0.78,
      "scent": 0.88,
      "via": ["docs/index.md"],
      "sections": [
        {"heading": "Release", "lines": [12, 26], "score": 0.74}
      ],
      "links": [
        {"target": "docs/concepts/dogfooding.md", "scent": 0.19, "followed": false, "reason": "below-threshold"}
      ]
    }
  ],
  "walked": [
    {
      "path": "docs/concepts/init-command.md",
      "relevance": 0.27,
      "scent": 0.68,
      "via": ["docs/index.md"],
      "links": [
        {"target": "docs/concepts/repo-layout.md", "scent": 0.32, "followed": false, "reason": "below-threshold"}
      ]
    }
  ]
}
```

How to act on it:

- `results` is sorted by `relevance`: the first entry is where to start. It holds only the files
  that earn a place: relevance at or above `--threshold`, or a section at or above it.
- `sections[].lines` is `[first, last]`, inclusive, and is the whole point: read those lines of
  that file, not the file. A section's range contains its subsections', so reading a returned
  range reads everything returned inside it.
- `walked` is the rest of the walk: the entry pages, hubs and section indexes that got you to the
  list, and any page whose relevance and every section fell short. They carry no `sections` —
  they are not something to read — so when a page you expected is missing, look there before
  assuming the walk never saw it. `visited` counts both lists.
- `scent` is the link that reached the file and `via` is the path it came along, so you can see
  why it is in the list.
- `links` is what the walk judged and what became of each one: `followed`, or a `reason` naming
  the rule that queued nothing — `below-threshold`, `not-kept`, `unjudged`, `out-of-root`,
  `past-depth`, `already-reached` (the target is in this list), `already-queued` (another path
  had already queued it, and may yet be dropped). Useful when a page you expected is missing: it
  may have been passed over below `--threshold`, or another path may have reached it first.
- A result with no `sections` cleared `--threshold` on its relevance alone: it is in the list but
  has nothing worth quoting.

Exit codes: `0` the walk reached a page beyond the entry files that earned a place, `1` nothing
beyond them did (the list is the entry files alone, or empty with the walk under `walked`, and
stderr says so), `2` an error — the reason is one line on stderr, and on a mistake the message
names the path or the flag.

## Rules of the road

- **The content leaves the machine.** Scoring sends the query and the text of every page the
  walk visits to the TypeSafe API. Do not point s1m at a knowledge base you are not allowed to
  send to a third party.
- **A wiki can exclude paths.** A `.s1mignore` in the root, in gitignore syntax, is never read:
  a matched link target is not sent, not previewed and not followed, and naming one as an entry
  file exits 2. If a page you want is ignored, do not fight it — tell the person who owns the
  wiki.
- **Repeat runs are free and identical.** Answers are cached on the request that produced them,
  under `$S1M_CACHE_DIR`, else `$XDG_CACHE_HOME/s1m`, else `~/.cache/s1m`. `--no-cache` buys
  everything again; the thresholds are the caller's and never part of a cache key, so tuning one
  costs nothing.
- **`s1m --help`** carries the same description, every flag with its default, and the exit codes.

The design, the output schema in full and the measured results on a real wiki are in the
[README](README.md), its [reference](docs/reference.md) and [eval/REPORT.md](eval/REPORT.md).
