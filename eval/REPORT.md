# Evaluation: s1m on a real wiki

s1m ranks a wiki's pages for a query by walking its links, so an agent reads the ranges it returns instead of opening files until it finds them. This is what that is worth on the wiki vendored under `eval/wikis/llm-wiki-manager/wiki`: 20 queries, each with the pages a person would want, at `--max-files` 10 and 25.

| | |
| --- | --- |
| Corpus | 19 pages, 69231 characters (17308 tokens) |
| Gold set | `eval/gold/llm-wiki-manager.json` — 20 queries |
| Model | jev-1.13.0 |
| Price | $0.042 per million input tokens, output free |
| Walk | `--threshold` 0.6, `--max-depth` 6, 8 frontier files a round |
| Answers | `eval/cache` |
| Requests | 1215 behind those answers; more than one per answer means a file whose sections and links did not fit one post |
| Cost | $0.019224 for the gold set at `--max-files 10`, $0.019640 at `--max-files 25`; every answer this report used, at the list price above, $0.278838 |

## Headline

- **Recall and precision at `--max-files 10`**: mean recall 0.64, mean precision 0.30 — 22 of the 39 wanted pages are in the list the agent opens, over 71 files returned, 3.5 a query — against 0.27 over everything the walk visited: 85 files judged, 71 of them earned a place, and the rest are what the JSON reports as `walked`. The budget is not what binds: the walk runs out of links above `--threshold` first, and `--max-files 25` visits 87 files for the same mean recall (0.64), 73 of which earn a place, so everything below is a statement about the link graph and the threshold, not about the budget.
- **The keyword ranker finds more and reads far more**: recall 0.94 against s1m's 0.64, at 243070 tokens against 35771 — 6.8× the reading for 0.30 more of the wanted pages. On a wiki whose pages share their vocabulary with the queries, grep is the stronger recaller and s1m the cheaper reader.
- **What an agent reads**: 35771 tokens for the returned ranges, against 62085 for the same files whole and 346160 for every page on every query. Reading the returned files whole costs 18% of the corpus's text; the section scores take 42% off that, and the ranking 90% off reading everything.
- **What it costs**: $0.019224 for the gold set at `--max-files 10` — $0.000961 a query, at 0.19 s an answer, $0.019640 at `--max-files 25`; every answer this report used, at the price above, $0.278838. The figures are the input tokens the answers spent, priced at the list rate in the header: the cache fixes the tokens, and a rate change re-prices every row, so a rerun reproduces them only while that constant stands.
- **Where `--threshold` sits**: this report walked at 0.6. Against that walk, the swept thresholds move recall and reading by: 0.5: recall +0.08 and reading +41%; 0.6: recall +0.00 and reading +0%; 0.7: recall -0.18 and reading -27%; 0.8: recall -0.31 and reading -70%. The calibration says the same from the other side — the links the walk followed reach a wanted page 0.37 of the time, the ones it passed over 0.11, and 69 links clear the threshold and are still not followed.
- **The frontmatter earns its tokens**: dropping it from the preview costs 0.18 of recall (0.64 → 0.46) for -44% of the input tokens, and dropping previews altogether costs 0.26. It is the larger half of what a preview buys, and `related:` is why — on the hub page it is what lifts the links to `dogfooding.md` and `node-version-and-types.md` over the threshold. [#10]'s worry that the frontmatter misleads is the wrong way round on this wiki.

## How to reproduce

```bash
cargo run --release --bin eval -- \
  --wiki eval/wikis/llm-wiki-manager/wiki \
  --gold eval/gold/llm-wiki-manager.json \
  --cache eval/cache \
  --out PATH
```

This report goes to stdout without `--out`, and `--out PATH` writes it to a file instead. Every judgment is cached on the request that produced it, and the cache stores the tokens each call spent beside its answer, so the cache committed under that directory reproduces this report byte for byte with no `TYPESAFE_API_KEY` at all. The cost columns are those stored tokens at the list rate in the header — the cache fixes the tokens, not the rate — and `--no-cache` with a key buys every judgment again. `--wiki` and `--gold` are the only thing a private wiki needs, and nothing about either is committed here.

## The gold set

20 queries, written by reading the wiki: for each, the pages a person with that task would want open. `mode` is the criterion the query is judged by, `entry` the page a caller would start from. Labels are the queries' own — a page that is useful but unlisted costs precision, and no label says a page is useless — so precision is a lower bound.

<details><summary>The queries</summary>

| Query | Mode | Entry | Wanted | Why |
| --- | --- | --- | --- | --- |
| **how do I cut a release and publish the package** | `useful-for` | `index.md` | `concepts/release.md`, `concepts/node-version-and-types.md` | release.md is the wiki's page on releasing (it points at RELEASING.md for the runbook); node-version-and-types.md covers the release workflow's Node pin and the gate chain a release runs. |
| **how does init scaffold a wiki into a new project** | `useful-for` | `index.md` | `concepts/init-command.md`, `entities/commands.md`, `concepts/template-system.md` | init-command.md is the flow, commands.md the module it lives in, template-system.md what it copies. |
| **which files does init copy into a consumer project** | `answers` | `index.md` | `concepts/template-system.md`, `entities/templates.md` | templates/ is the scaffold source of truth; template-system.md is why and how it is copied. |
| **what node version must consumers run the CLI on** | `answers` | `index.md` | `concepts/node-version-and-types.md` | The runtime floor (engines.node) against the development pin (.nvmrc) is one page. |
| **why must @types/node match the .nvmrc major** | `answers` | `index.md` | `concepts/node-version-and-types.md` | Same page as the floor, asked for the rule rather than the number. |
| **what validates wiki pages before a commit lands** | `useful-for` | `index.md` | `concepts/wiki-scripts.md`, `concepts/dogfooding.md` | wiki-scripts.md is the lint/build/check subcommands; dogfooding.md is the pre-commit and pre-push hooks that run them. |
| **how does the wiki stay in sync with code changes** | `useful-for` | `index.md` | `concepts/dogfooding.md` | The maintenance-trigger workflow and code_refs are the answer, and dogfooding.md is where it is described. |
| **which tests exercise the compiled CLI binary** | `answers` | `index.md` | `concepts/e2e-tests.md`, `concepts/unit-tests.md` | e2e-tests.md runs the built binary; unit-tests.md says which of its script tests invoke dist/bin/cli.js. |
| **how do I run the unit test suite** | `answers` | `index.md` | `concepts/unit-tests.md` | One command and the config it reads. |
| **how does upgrade refresh an existing wiki without overwriting user pages** | `answers` | `index.md` | `concepts/dogfooding.md`, `concepts/template-system.md`, `entities/commands.md` | The refreshed/preserved table is in dogfooding.md, the meta-path list that decides it in template-system.md, and the pipeline in commands.md. |
| **what is the layout of the package source** | `about` | `index.md` | `concepts/repo-layout.md` | The table of bin/, src/, templates/, test/ and the dogfooded wiki. |
| **why is the entities directory flat with no subdirectories** | `answers` | `index.md` | `AGENTS.md`, `schema.md` | The scope-tag convention is stated in AGENTS.md §3a and specified in schema.md's flat namespace section. |
| **what generates the index.md tables** | `answers` | `index.md` | `AGENTS.md`, `concepts/wiki-scripts.md` | AGENTS.md says never to hand-edit them and names build; wiki-scripts.md documents the subcommand. |
| **how do I add a summary page for an ingested source artifact** | `useful-for` | `index.md` | `AGENTS.md`, `schema.md`, `raw/raw.md` | The ingest workflow is in AGENTS.md §7 and schema.md, with the raw tree's hub page as where the artifact goes. |
| **where do immutable source artifacts live** | `answers` | `index.md` | `raw/raw.md`, `schema.md` | raw/raw.md is the hub of the immutable tree; schema.md gives the directory layout and the never-edit rule. |
| **what is llm-wiki-manager for** | `about` | `index.md` | `README.md`, `concepts/repo-layout.md`, `concepts/dogfooding.md` | README.md is the human entry point; repo-layout.md and dogfooding.md say what the package is and what it is a consumer of. |
| **which module dispatches CLI subcommands** | `answers` | `index.md` | `entities/cli.md` | The dispatch table names the handler module for each subcommand. |
| **how are template variables interpolated during scaffolding** | `answers` | `index.md` | `concepts/template-system.md`, `entities/templates.md`, `concepts/init-command.md` | template-system.md has interpolate() and the variable list, init-command.md gathers the values, templates.md has the file set. |
| **what does src/utils/fs.ts handle** | `answers` | `index.md` | `entities/utils.md`, `concepts/template-system.md` | utils.md is the scope overview with the exports; template-system.md is the part of it that copies and interpolates. |
| **what does the wiki log record** | `answers` | `index.md` | `log.md`, `AGENTS.md` | log.md is the append-only record itself; AGENTS.md documents the entries it takes (ingest, query, lint, maintenance). |

</details>

## Results at a fixed file budget

`--max-files` is the number of files the walk may judge beyond the entry files, which are always visited, and the walk judges that many before the reading list is asked anything. `Visited` counts the files it judged; `Returned` is the list the agent opens — the ones that earn a place on their own, relevance at or above `--threshold` 0.6 or a section at or above it, most relevant first — and the rest, the entry files, hubs and near-misses, are what the JSON reports as `walked`. Recall is the wanted pages in that list over all of the query's wanted pages, and precision is the wanted pages in it over the files in it; precision (visited) is the same over everything the walk judged, which is the number this harness reported while the list was everything the walk had visited, so the two side by side are what the cutoff bought and cost. At `--max-files 10`: 85 files visited and 71 returned, mean precision 0.30 against 0.27 over everything visited. `read` is what the agent opens — the returned ranges only — and `whole` is those same files read entire.

### `--max-files 10`

| Query | Gold | Visited | Returned | Found | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 5 | 3 | 1 | 0.50 | 0.33 | 0.40 | 802 | 2279 | $0.001167 | 210 |
| `init-scaffold` | 3 | 11 | 10 | 3 | 1.00 | 0.30 | 0.27 | 4223 | 8696 | $0.002427 | 194 |
| `init-copies` | 2 | 7 | 5 | 2 | 1.00 | 0.40 | 0.29 | 2787 | 3434 | $0.001269 | 176 |
| `node-runtime-floor` | 1 | 2 | 2 | 1 | 1.00 | 0.50 | 0.50 | 1625 | 2017 | $0.000524 | 206 |
| `types-alignment` | 1 | 3 | 2 | 1 | 1.00 | 0.50 | 0.33 | 1625 | 2017 | $0.000683 | 162 |
| `wiki-validation` | 2 | 3 | 3 | 1 | 0.50 | 0.33 | 0.33 | 1819 | 2768 | $0.000785 | 199 |
| `wiki-code-sync` | 1 | 9 | 7 | 1 | 1.00 | 0.14 | 0.11 | 8390 | 9512 | $0.002256 | 197 |
| `compiled-cli-tests` | 2 | 4 | 4 | 2 | 1.00 | 0.50 | 0.50 | 2586 | 4318 | $0.001050 | 156 |
| `run-unit-tests` | 1 | 3 | 3 | 1 | 1.00 | 0.33 | 0.33 | 1361 | 2768 | $0.000761 | 227 |
| `upgrade-preserves` | 3 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000314 | 196 |
| `source-layout` | 1 | 4 | 4 | 1 | 1.00 | 0.25 | 0.25 | 2210 | 3477 | $0.001058 | 240 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000314 | 204 |
| `index-tables` | 2 | 1 | 1 | 0 | 0.00 | 0.00 | 0.00 | 628 | 628 | $0.000313 | 255 |
| `ingest-summary` | 3 | 4 | 2 | 1 | 0.33 | 0.50 | 0.25 | 214 | 884 | $0.000733 | 150 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 0.50 | 0.50 | 841 | 884 | $0.000347 | 180 |
| `what-is-it` | 3 | 11 | 10 | 2 | 0.67 | 0.20 | 0.18 | 1679 | 7812 | $0.002250 | 180 |
| `cli-dispatch` | 1 | 2 | 2 | 1 | 1.00 | 0.50 | 0.50 | 308 | 1003 | $0.000422 | 152 |
| `template-vars` | 3 | 5 | 5 | 1 | 0.33 | 0.20 | 0.20 | 1693 | 4255 | $0.001012 | 180 |
| `utils-fs` | 2 | 4 | 4 | 2 | 1.00 | 0.50 | 0.50 | 1777 | 3193 | $0.000777 | 157 |
| `wiki-log` | 2 | 3 | 2 | 0 | 0.00 | 0.00 | 0.00 | 1203 | 2140 | $0.000760 | 202 |
| **mean** |  |  |  |  | **0.64** | **0.30** | **0.27** | **35771** | **62085** | **$0.019224** | 188 |

### `--max-files 25`

| Query | Gold | Visited | Returned | Found | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 5 | 3 | 1 | 0.50 | 0.33 | 0.40 | 802 | 2279 | $0.001167 | 210 |
| `init-scaffold` | 3 | 11 | 10 | 3 | 1.00 | 0.30 | 0.27 | 4223 | 8696 | $0.002427 | 194 |
| `init-copies` | 2 | 7 | 5 | 2 | 1.00 | 0.40 | 0.29 | 2787 | 3434 | $0.001269 | 176 |
| `node-runtime-floor` | 1 | 2 | 2 | 1 | 1.00 | 0.50 | 0.50 | 1625 | 2017 | $0.000524 | 206 |
| `types-alignment` | 1 | 3 | 2 | 1 | 1.00 | 0.50 | 0.33 | 1625 | 2017 | $0.000683 | 162 |
| `wiki-validation` | 2 | 3 | 3 | 1 | 0.50 | 0.33 | 0.33 | 1819 | 2768 | $0.000785 | 199 |
| `wiki-code-sync` | 1 | 9 | 7 | 1 | 1.00 | 0.14 | 0.11 | 8390 | 9512 | $0.002256 | 197 |
| `compiled-cli-tests` | 2 | 4 | 4 | 2 | 1.00 | 0.50 | 0.50 | 2586 | 4318 | $0.001050 | 156 |
| `run-unit-tests` | 1 | 3 | 3 | 1 | 1.00 | 0.33 | 0.33 | 1361 | 2768 | $0.000761 | 227 |
| `upgrade-preserves` | 3 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000314 | 196 |
| `source-layout` | 1 | 4 | 4 | 1 | 1.00 | 0.25 | 0.25 | 2210 | 3477 | $0.001058 | 240 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000314 | 204 |
| `index-tables` | 2 | 1 | 1 | 0 | 0.00 | 0.00 | 0.00 | 628 | 628 | $0.000313 | 255 |
| `ingest-summary` | 3 | 4 | 2 | 1 | 0.33 | 0.50 | 0.25 | 214 | 884 | $0.000733 | 150 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 0.50 | 0.50 | 841 | 884 | $0.000347 | 180 |
| `what-is-it` | 3 | 13 | 12 | 2 | 0.67 | 0.17 | 0.15 | 6376 | 12509 | $0.002666 | 177 |
| `cli-dispatch` | 1 | 2 | 2 | 1 | 1.00 | 0.50 | 0.50 | 308 | 1003 | $0.000422 | 152 |
| `template-vars` | 3 | 5 | 5 | 1 | 0.33 | 0.20 | 0.20 | 1693 | 4255 | $0.001012 | 180 |
| `utils-fs` | 2 | 4 | 4 | 2 | 1.00 | 0.50 | 0.50 | 1777 | 3193 | $0.000777 | 157 |
| `wiki-log` | 2 | 3 | 2 | 0 | 0.00 | 0.00 | 0.00 | 1203 | 2140 | $0.000760 | 202 |
| **mean** |  |  |  |  | **0.64** | **0.30** | **0.27** | **40468** | **66782** | **$0.019640** | 187 |

## Against grep, and against reading the corpus

The keyword baseline is the harness's own keyword ranker, asked for the same number of hits and read whole: it is what a caller with grep and no model gets. Reading the corpus is the floor no ranking can beat on tokens, counted the way the rows above are — over every query, so reading all 19 pages once per query.

| Budget | s1m recall | s1m precision | s1m read (tok) | grep recall | grep precision | grep read (tok) |
| --- | --- | --- | --- | --- | --- | --- |
| 10 | 0.64 | 0.30 | 35771 | 0.94 | 0.18 | 243070 |
| 25 | 0.64 | 0.30 | 40468 | 1.00 | 0.12 | 326765 |
| whole corpus | 1.00 | 0.10 | 346160 | | | |

Reading every page for every query finds every wanted page and reads 346160 tokens for the gold set, 9.7× s1m's returned ranges. The precision column is the wanted pages over the 19 pages there are, averaged over the queries: that is what an unranked reader reads.

## The pages no walk reached

Wanted pages no walk reached at `--max-files 25`: 16 query/page pairs missed.
The cutoff costs 1 of the 23 wanted pages a walk did reach: those are in the JSON's `walked`, not in the list the agent reads, because reaching a page is not returning it.

| Query | Wanted but not reached |
| --- | --- |
| `wiki-validation` | `concepts/dogfooding.md` |
| `upgrade-preserves` | `concepts/dogfooding.md`, `concepts/template-system.md`, `entities/commands.md` |
| `flat-entities` | `AGENTS.md`, `schema.md` |
| `index-tables` | `AGENTS.md`, `concepts/wiki-scripts.md` |
| `ingest-summary` | `AGENTS.md`, `schema.md` |
| `raw-immutable` | `schema.md` |
| `what-is-it` | `README.md` |
| `template-vars` | `concepts/init-command.md`, `entities/templates.md` |
| `wiki-log` | `AGENTS.md`, `log.md` |

## Calibration: scent against arrival

Every file a link reached carries the scent of that link and the relevance the model then gave the file, which is the plan's curve for free. `gold` is the share of those arrivals the gold set wanted. At `--max-files 25`, over 20 queries:

| Scent | Arrivals | Mean scent | Mean relevance | Wanted |
| --- | --- | --- | --- | --- |
| 0.6–0.7 | 25 | 0.63 | 0.70 | 0.16 |
| 0.7–0.8 | 20 | 0.75 | 0.75 | 0.35 |
| 0.8–0.9 | 15 | 0.84 | 0.83 | 0.40 |
| 0.9–1.0 | 7 | 0.93 | 0.93 | 0.86 |

67 arrivals: a link the threshold followed. The bins below it are empty by construction — a link under `--threshold` is never followed, so nothing arrives by one — which is the next table's question.

Every link the walk judged whose target is a page of this wiki (568; 0 more left the wiki or are not there, and no label can say what they would have reached):

| Scent | Links | Followed | Wanted when followed |
| --- | --- | --- | --- |
| 0.0–0.1 | 38 | 0 | — |
| 0.1–0.2 | 146 | 0 | — |
| 0.2–0.3 | 109 | 0 | — |
| 0.3–0.4 | 62 | 0 | — |
| 0.4–0.5 | 41 | 0 | — |
| 0.5–0.6 | 33 | 0 | — |
| 0.6–0.7 | 47 | 26 | 0.19 |
| 0.7–0.8 | 38 | 21 | 0.38 |
| 0.8–0.9 | 38 | 16 | 0.44 |
| 0.9–1.0 | 16 | 7 | 0.86 |

| Decision | Links | Wanted |
| --- | --- | --- |
| followed | 70 | 0.37 |
| passed over | 498 | 0.11 |

The walk's own decision, in the same terms: the links it followed reach a wanted page 0.37 of the time, the ones it passed over 0.11. These two rows split on whether the walk followed a link, not on scent, which is why they do not partition the bins above the same way: 69 links clear `--threshold` and were still passed over, for want of depth or because their target had already been reached by a better path. A gold set is not the whole of what is useful — a link can lead to a page worth reading for the query without being one of the pages that query was labelled with — so both numbers are lower than they would be against a label of *relevant*, and it is the gap between them that says where the threshold belongs.

## The default threshold

The same gold set walked at `--max-files 10` with the link and section thresholds moved together, the way the CLI defaults them. These are judgments the runs above already made wherever the threshold never changed which page was worth visiting, so most of this table costs nothing.

| Threshold | Recall | Precision | Read (tok) | Cost |
| --- | --- | --- | --- | --- |
| 0.5 | 0.72 | 0.29 | 50515 | $0.021187 |
| **0.6** (default) | 0.64 | 0.30 | 35771 | $0.019224 |
| 0.7 | 0.47 | 0.36 | 26247 | $0.014785 |
| 0.8 | 0.33 | 0.33 | 10894 | $0.010437 |

## The preview experiment: frontmatter

A preview carries a target's title, its frontmatter and its first paragraph. The spike varied the whole preview as one knob and could not say which part did the work, and left the frontmatter — the part most likely to mislead, since `related:` makes every page look connected to every other — to this milestone. The same gold set, at `--max-files 10`, with the frontmatter dropped and with previews off:

| Preview policy | Recall | Precision | Read (tok) | Input (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- |
| previews on (default) | 0.64 | 0.30 | 35771 | 457709 | $0.019224 | 188 |
| previews, no frontmatter | 0.46 | 0.27 | 32089 | 256959 | $0.010792 | 172 |
| previews off | 0.38 | 0.29 | 16246 | 167987 | $0.007055 | 178 |

At the scale of one page: `index.md` — the entry file of query `release-and-publish` — judged by that query under each policy, with the scent each policy gave each of its links and whether that scent clears `--threshold` 0.6 so the walk would follow it (bold: it would):

| Target | previews on (default) | previews, no frontmatter | previews off |
| --- | --- | --- | --- |
| `raw/raw.md` | 0.08 | 0.07 | 0.14 |
| `entities/cli.md` | 0.15 | 0.12 | 0.20 |
| `entities/commands.md` | 0.13 | 0.19 | 0.27 |
| `entities/templates.md` | 0.12 | 0.09 | 0.11 |
| `entities/utils.md` | 0.13 | 0.08 | 0.12 |
| `concepts/dogfooding.md` | **0.87** | 0.21 | 0.10 |
| `concepts/e2e-tests.md` | 0.12 | 0.10 | 0.09 |
| `concepts/init-command.md` | **0.68** | 0.10 | 0.14 |
| `concepts/node-version-and-types.md` | **0.70** | 0.16 | 0.13 |
| `concepts/release.md` | **0.88** | 0.49 | **0.93** |
| `concepts/repo-layout.md` | 0.46 | 0.18 | 0.15 |
| `concepts/template-system.md` | 0.25 | 0.16 | 0.11 |
| `concepts/unit-tests.md` | 0.22 | 0.17 | 0.12 |
| `concepts/wiki-scripts.md` | 0.26 | 0.14 | 0.13 |

Judging that one page under each policy is the only measurement here that is not a walk, so it has no row in the tables above; it is in the run's total, three answers.

`previews, no frontmatter` against the default, on this page: 4 of 14 links change whether the walk would follow them, and the mean scent moves by 0.21.
`previews off` against the default, on this page: 3 of 14 links change whether the walk would follow them, and the mean scent moves by 0.21.

## The link context experiment: headings, leads and the path

A link is judged from one hop: the page it sits on, its anchor, its sentence and its heading, and the target's title, frontmatter and first paragraph. The failure analysis on a private wiki ([#36]) found the queries that reached nothing doing it two or three hops out, behind intermediate pages whose preview says nothing about what lies under them. [#46] measures four switches against that, each on its own and in the pairs the decision rule asks about, at `--max-files 10`:

- **`+ headings`**: the target's own H2/H3 headings, in order, at most 40 of them and each cut at 80 characters.
- **`+ leads_to`**: the anchor text of the target's own in-root links, in order, deduped, at most 30 and each cut at 60 characters — one hop of lookahead past the target.
- **`+ via`**: the titles of the pages the walk came through, in order, at the top level of the state.
- **`+ two-hop question`**: the link question reworded to ask what this link reaches directly or through the pages it links to, with the yes-criterion to match. The question is the only thing that changes; every state field is what it was.

| Variant | Recall | Precision | Read (tok) | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| what ships (default) | 0.64 | 0.30 | 35771 | 457709 | $0.019224 | 85 | 1.00 |
| + headings | 0.72 | 0.33 | 39799 | 511642 | $0.021489 | 89 | 1.00 |
| + leads_to | 0.70 | 0.28 | 38279 | 524890 | $0.022045 | 91 | 1.00 |
| + via | 0.61 | 0.27 | 37470 | 474542 | $0.019931 | 85 | 1.00 |
| + headings + leads_to | 0.69 | 0.29 | 47685 | 596166 | $0.025039 | 97 | 1.00 |
| + two-hop question | 0.72 | 0.30 | 42042 | 544303 | $0.022861 | 105 | 1.00 |
| + two-hop question + headings + leads_to | 0.83 | 0.26 | 68663 | 1057882 | $0.044431 | 181 | 1.00 |

`Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged, so 1.00 is a link table that fits one post: a variant above 1.00 is splitting pages the state budget no longer holds ([#37]). A `Requests` column that rose while `Req/answer` stayed at 1.00 is the other cost — a link the model now rates above `--threshold` is a page the walk visits and pays for, which is where a variant's recall comes from. The fullest variant asks 2.1× what ships does.

Recall per query, the queries the shipped walk found least first:

| Query | Gold | what ships (default) | + headings | + leads_to | + via | + headings + leads_to | + two-hop question | + two-hop question + headings + leads_to |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `index-tables` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 |
| `upgrade-preserves` | 3 | 0.00 | 0.67 | 0.00 | 0.00 | 0.67 | 0.33 | 1.00 |
| `wiki-log` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `ingest-summary` | 3 | 0.33 | 0.33 | 0.33 | 0.33 | 0.33 | 0.33 | 1.00 |
| `template-vars` | 3 | 0.33 | 0.67 | 1.00 | 0.67 | 0.67 | 1.00 | 1.00 |
| `raw-immutable` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `wiki-validation` | 2 | 0.50 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `what-is-it` | 3 | 0.67 | 0.67 | 0.67 | 0.67 | 0.67 | 0.67 | 0.67 |
| `cli-dispatch` | 1 | 1.00 | 1.00 | 1.00 | 0.00 | 1.00 | 1.00 | 1.00 |
| `compiled-cli-tests` | 2 | 1.00 | 1.00 | 1.00 | 0.50 | 0.50 | 1.00 | 1.00 |
| `init-copies` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `init-scaffold` | 3 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `node-runtime-floor` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `run-unit-tests` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `source-layout` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `types-alignment` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `utils-fs` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| **mean** |  | **0.64** | **0.72** | **0.70** | **0.61** | **0.69** | **0.72** | **0.83** |

Requests per query, the same order: what each variant asked of the API, where the split shows up.

| Query | Gold | what ships (default) | + headings | + leads_to | + via | + headings + leads_to | + two-hop question | + two-hop question + headings + leads_to |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `index-tables` | 2 | 1 | 1 | 1 | 1 | 1 | 1 | 8 |
| `upgrade-preserves` | 3 | 1 | 4 | 1 | 1 | 4 | 3 | 8 |
| `wiki-log` | 2 | 3 | 2 | 2 | 2 | 2 | 3 | 9 |
| `ingest-summary` | 3 | 4 | 2 | 2 | 2 | 2 | 5 | 10 |
| `template-vars` | 3 | 5 | 6 | 7 | 6 | 7 | 7 | 11 |
| `raw-immutable` | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 |
| `release-and-publish` | 2 | 5 | 4 | 4 | 5 | 5 | 7 | 6 |
| `wiki-validation` | 2 | 3 | 4 | 4 | 4 | 6 | 4 | 11 |
| `what-is-it` | 3 | 11 | 11 | 11 | 11 | 11 | 11 | 11 |
| `cli-dispatch` | 1 | 2 | 2 | 2 | 1 | 2 | 2 | 5 |
| `compiled-cli-tests` | 2 | 4 | 4 | 6 | 3 | 3 | 4 | 11 |
| `init-copies` | 2 | 7 | 7 | 8 | 7 | 9 | 8 | 11 |
| `init-scaffold` | 3 | 11 | 11 | 12 | 11 | 11 | 11 | 11 |
| `node-runtime-floor` | 1 | 2 | 2 | 3 | 2 | 5 | 3 | 11 |
| `run-unit-tests` | 1 | 3 | 4 | 4 | 4 | 4 | 4 | 11 |
| `source-layout` | 1 | 4 | 5 | 4 | 4 | 4 | 7 | 11 |
| `types-alignment` | 1 | 3 | 3 | 3 | 4 | 3 | 5 | 11 |
| `utils-fs` | 2 | 4 | 6 | 5 | 5 | 5 | 6 | 11 |
| `wiki-code-sync` | 1 | 9 | 8 | 9 | 9 | 10 | 11 | 11 |
| **total** |  | **85** | **89** | **91** | **85** | **97** | **105** | **181** |

## What these numbers are not

- **The corpus is thin.** 19 pages, so a budget of 25 can hold the corpus and the wider budget stops being a ranking question. The differences between configurations here are indicative, not a tuning set; nothing in this report should be treated as more than a direction on a wiki this size.
- **The labels are one reader's.** A page that is useful and unlisted counts against precision, so precision is a lower bound and recall is only as good as the list. The wanted sets were written from the wiki's own pages, not from a task run against it.
- **A hit is not an answer.** Recall counts the files the reading list returned, not whether an agent could do the task with them: a wanted page the walk reached but that earned no place on its own is not in that list, so it counts as missed. `read` counts characters at 4, not what a tokeniser would charge.
- **One model, one day.** Jev moves its numbers between identical requests, which is why every number here comes from the committed cache: rerun without it and the rankings hold while the numbers underneath them shift (`docs/spike-notes.md`).

## The committed cache

The answers came from `eval/cache`. That directory holds one file per request the runs above made: the query, the mode, the file and its links key the entry, and the entry carries the judgment and what the call cost. It is what makes this report a thing to check rather than a claim — the same command with no key on a machine that has never asked the API reads the same answers and prints the same bytes — and it is committed here because a wiki and a gold set are not: a private wiki is measured with the same command against its own directory, and nothing about it lands in this repository.

