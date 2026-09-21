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
| Requests | 3195 behind those answers; more than one per answer means a file whose sections and links did not fit one post |
| Cost | $0.045945 for the gold set at `--max-files 10`, $0.051264 at `--max-files 25`; every answer this report used, at the list price above, $0.754844 |

## Headline

- **Recall and precision at `--max-files 10`**: mean recall 0.83, mean precision 0.27 — 32 of the 39 wanted pages are in the list the agent opens, over 126 files returned, 6.3 a query — against 0.19 over everything the walk visited: 186 files judged, 126 of them earned a place, and the rest are what the JSON reports as `walked`. The budget is not what binds: the walk runs out of links above `--threshold` first, and `--max-files 25` visits 213 files for the same mean recall (0.83), 144 of which earn a place, so everything below is a statement about the link graph and the threshold, not about the budget.
- **Section recall**: 0.74 — 27 of the 39 wanted parts the gold set labels are covered by the ranges the list returns, against 0.83 of the 32 wanted pages in the list at all, for 3054 lines returned. A part is the heading or line range an entry names, and an entry that names none is wanted whole, so its part is its page; where the two numbers differ, the list returned a page with nothing to read in it, or ranges that miss the part the label points at.
- **The keyword ranker finds more and reads far more**: recall 0.94 against s1m's 0.83, at 243070 tokens against 44906 — 5.4× the reading for 0.11 more of the wanted pages. On a wiki whose pages share their vocabulary with the queries, grep is the stronger recaller and s1m the cheaper reader.
- **What an agent reads**: 44906 tokens for the returned ranges, against 120322 for the same files whole and 346160 for every page on every query. Reading the returned files whole costs 35% of the corpus's text; the section scores take 63% off that, and the ranking 87% off reading everything.
- **What it costs**: $0.045945 for the gold set at `--max-files 10` — $0.002297 a query, at 0.20 s an answer, $0.051264 at `--max-files 25`; every answer this report used, at the price above, $0.754844. The figures are the input tokens the answers spent, priced at the list rate in the header: the cache fixes the tokens, and a rate change re-prices every row, so a rerun reproduces them only while that constant stands.
- **Where `--threshold` sits**: this report walked at 0.6. Against that walk, the swept thresholds move recall and reading by: 0.5: recall +0.02 and reading +26%; 0.6: recall +0.00 and reading +0%; 0.7: recall -0.20 and reading -43%; 0.8: recall -0.32 and reading -60%. The calibration says the same from the other side — the links the walk followed reach a wanted page 0.17 of the time, the ones it passed over 0.14, and 410 links clear the threshold and are still not followed.
- **The frontmatter earns its tokens**: dropping it from the preview costs 0.07 of recall (0.83 → 0.77) for -33% of the input tokens, and dropping previews altogether costs 0.48. It is the larger half of what a preview buys, and `related:` is why — on the hub page it is what lifts the links to `dogfooding.md` and `node-version-and-types.md` over the threshold. [#10]'s worry that the frontmatter misleads is the wrong way round on this wiki.

## How to reproduce

```bash
cargo run --release --bin eval -- \
  --wiki eval/wikis/llm-wiki-manager/wiki \
  --gold eval/gold/llm-wiki-manager.json \
  --cache eval/cache \
  --relative-judge \
  --wordings \
  --out PATH
```

This report goes to stdout without `--out`, and `--out PATH` writes it to a file instead. Every judgment is cached on the request that produced it, and the cache stores the tokens each call spent beside its answer, so the cache committed under that directory reproduces this report byte for byte with no `TYPESAFE_API_KEY` at all. The cost columns are those stored tokens at the list rate in the header — the cache fixes the tokens, not the rate — and `--no-cache` with a key buys every judgment again. `--wiki` and `--gold` are the only thing a private wiki needs, and nothing about either is committed here. `--relative-judge` is what adds the relative judge's rows below and `--wordings` the wording table's, and both are this report's own asks: a run without either flag prints the report without that table, and the cache answers the rest either way.

## The gold set

20 queries, written by reading the wiki: for each, the pages a person with that task would want open. `mode` is the criterion the query is judged by, `entry` the page a caller would start from. Labels are the queries' own — a page that is useful but unlisted costs precision, and no label says a page is useless — so precision is a lower bound. An entry that names a heading or lines is a page whose answer lives in part of it ([#58]), and those are the parts **Section recall** below is counted over: a page named on its own is wanted whole, so its part is its page.

<details><summary>The queries</summary>

| Query | Mode | Entry | Wanted | Why |
| --- | --- | --- | --- | --- |
| **how do I cut a release and publish the package** | `useful-for` | `index.md` | `concepts/release.md`: lines 14–20, `concepts/node-version-and-types.md`: Current policy | release.md is the wiki's page on releasing (it points at RELEASING.md for the runbook); node-version-and-types.md covers the release workflow's Node pin and the gate chain a release runs. |
| **how does init scaffold a wiki into a new project** | `useful-for` | `index.md` | `concepts/init-command.md`: Scaffold steps, `entities/commands.md`: lines 13–20, `concepts/template-system.md`: Directory layout | init-command.md is the flow, commands.md the module it lives in, template-system.md what it copies. |
| **which files does init copy into a consumer project** | `answers` | `index.md` | `concepts/template-system.md`: Directory layout, `entities/templates.md`: Directory layout | templates/ is the scaffold source of truth; template-system.md is why and how it is copied. |
| **what node version must consumers run the CLI on** | `answers` | `index.md` | `concepts/node-version-and-types.md`: Current policy | The runtime floor (engines.node) against the development pin (.nvmrc) is one page. |
| **why must @types/node match the .nvmrc major** | `answers` | `index.md` | `concepts/node-version-and-types.md`: @types/node alignment | Same page as the floor, asked for the rule rather than the number. |
| **what validates wiki pages before a commit lands** | `useful-for` | `index.md` | `concepts/wiki-scripts.md`: Commands, `concepts/dogfooding.md`: Validation | wiki-scripts.md is the lint/build/check subcommands; dogfooding.md is the pre-commit and pre-push hooks that run them. |
| **how does the wiki stay in sync with code changes** | `useful-for` | `index.md` | `concepts/dogfooding.md` | The maintenance-trigger workflow and code_refs are the answer, and dogfooding.md is where it is described. |
| **which tests exercise the compiled CLI binary** | `answers` | `index.md` | `concepts/e2e-tests.md`: Test files, `concepts/unit-tests.md`: Script test coverage | e2e-tests.md runs the built binary; unit-tests.md says which of its script tests invoke dist/bin/cli.js. |
| **how do I run the unit test suite** | `answers` | `index.md` | `concepts/unit-tests.md`: lines 30–36 | One command and the config it reads. |
| **how does upgrade refresh an existing wiki without overwriting user pages** | `answers` | `index.md` | `concepts/dogfooding.md`: Refreshing after template changes, `concepts/template-system.md`: Templates vs consumer output, `entities/commands.md`: lines 13–20 | The refreshed/preserved table is in dogfooding.md, the meta-path list that decides it in template-system.md, and the pipeline in commands.md. |
| **what is the layout of the package source** | `about` | `index.md` | `concepts/repo-layout.md`: Package source | The table of bin/, src/, templates/, test/ and the dogfooded wiki. |
| **why is the entities directory flat with no subdirectories** | `answers` | `index.md` | `AGENTS.md`: §3a Scope-tag convention, `schema.md`: Flat entities/ namespace | The scope-tag convention is stated in AGENTS.md §3a and specified in schema.md's flat namespace section. |
| **what generates the index.md tables** | `answers` | `index.md` | `AGENTS.md`: Gotchas, `concepts/wiki-scripts.md`: Commands | AGENTS.md says never to hand-edit them and names build; wiki-scripts.md documents the subcommand. |
| **how do I add a summary page for an ingested source artifact** | `useful-for` | `index.md` | `AGENTS.md`: Ingest (a new artifact lands in raw/), `schema.md`: Ingest, `raw/raw.md` | The ingest workflow is in AGENTS.md §7 and schema.md, with the raw tree's hub page as where the artifact goes. |
| **where do immutable source artifacts live** | `answers` | `index.md` | `raw/raw.md`, `schema.md`: Directory Layout | raw/raw.md is the hub of the immutable tree; schema.md gives the directory layout and the never-edit rule. |
| **what is llm-wiki-manager for** | `about` | `index.md` | `README.md`, `concepts/repo-layout.md`, `concepts/dogfooding.md` | README.md is the human entry point; repo-layout.md and dogfooding.md say what the package is and what it is a consumer of. |
| **which module dispatches CLI subcommands** | `answers` | `index.md` | `entities/cli.md`: Command dispatch | The dispatch table names the handler module for each subcommand. |
| **how are template variables interpolated during scaffolding** | `answers` | `index.md` | `concepts/template-system.md`: Interpolation, `entities/templates.md`: Directory layout, `concepts/init-command.md`: Prompts and variables | template-system.md has interpolate() and the variable list, init-command.md gathers the values, templates.md has the file set. |
| **what does src/utils/fs.ts handle** | `answers` | `index.md` | `entities/utils.md`: fs.ts — scaffold and config, `concepts/template-system.md`: Interpolation | utils.md is the scope overview with the exports; template-system.md is the part of it that copies and interpolates. |
| **what does the wiki log record** | `answers` | `index.md` | `log.md`, `AGENTS.md`: §7 The three workflows | log.md is the append-only record itself; AGENTS.md documents the entries it takes (ingest, query, lint, maintenance). |

</details>

## Results at a fixed file budget

`--max-files` is the number of files the walk may judge beyond the entry files, which are always visited, and the walk judges that many before the reading list is asked anything. `Visited` counts the files it judged; `Returned` is the list the agent opens — the ones that earn a place on their own, relevance at or above `--threshold` 0.6 or a section at or above it, most relevant first — and the rest, the entry files, hubs and near-misses, are what the JSON reports as `walked`. Recall is the wanted pages in that list over all of the query's wanted pages, and precision is the wanted pages in it over the files in it; precision (visited) is the same over everything the walk judged, which is the number this harness reported while the list was everything the walk had visited, so the two side by side are what the cutoff bought and cost. `Sections` is how many of those wanted pages the list also gave something to read in — the returned ranges overlapping the part of the page the gold entry names, its whole page where it names none — over one part a wanted page, and `Section recall` that count over `Gold`. It is never above recall: a part cannot be returned without its page. At `--max-files 10`: 186 files visited and 126 returned, mean precision 0.27 against 0.19 over everything visited. `read` is what the agent opens — the returned ranges only — and `whole` is those same files read entire; `Lines` is the same reading in line numbers, counted once where ranges overlap.

### `--max-files 10`

| Query | Gold | Visited | Returned | Found | Recall | Sections | Section recall | Precision | Precision (visited) | Read (tok) | Lines | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 7 | 5 | 1 | 0.50 | 0/2 | 0.00 | 0.20 | 0.29 | 85 | 6 | 4677 | $0.002005 | 197 |
| `init-scaffold` | 3 | 11 | 10 | 3 | 1.00 | 2/3 | 0.67 | 0.30 | 0.27 | 2210 | 174 | 7812 | $0.002617 | 183 |
| `init-copies` | 2 | 11 | 10 | 2 | 1.00 | 2/2 | 1.00 | 0.20 | 0.18 | 1992 | 141 | 8008 | $0.002517 | 196 |
| `node-runtime-floor` | 1 | 11 | 6 | 1 | 1.00 | 1/1 | 1.00 | 0.17 | 0.09 | 1491 | 92 | 5886 | $0.002749 | 185 |
| `types-alignment` | 1 | 11 | 6 | 1 | 1.00 | 1/1 | 1.00 | 0.17 | 0.09 | 1231 | 80 | 5886 | $0.002806 | 200 |
| `wiki-validation` | 2 | 11 | 8 | 2 | 1.00 | 2/2 | 1.00 | 0.25 | 0.18 | 3325 | 226 | 9326 | $0.002856 | 199 |
| `wiki-code-sync` | 1 | 11 | 8 | 1 | 1.00 | 1/1 | 1.00 | 0.12 | 0.09 | 5218 | 359 | 8551 | $0.002761 | 206 |
| `compiled-cli-tests` | 2 | 11 | 6 | 2 | 1.00 | 2/2 | 1.00 | 0.33 | 0.18 | 1990 | 113 | 6314 | $0.002670 | 217 |
| `run-unit-tests` | 1 | 11 | 7 | 1 | 1.00 | 1/1 | 1.00 | 0.14 | 0.09 | 1093 | 71 | 7909 | $0.002729 | 218 |
| `upgrade-preserves` | 3 | 8 | 4 | 3 | 1.00 | 2/3 | 0.67 | 0.75 | 0.38 | 1482 | 114 | 2991 | $0.001868 | 201 |
| `source-layout` | 1 | 11 | 8 | 1 | 1.00 | 1/1 | 1.00 | 0.12 | 0.09 | 4665 | 307 | 7124 | $0.002667 | 219 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0/2 | 0.00 | 0.00 | 0.00 | 0 | 0 | 0 | $0.000358 | 225 |
| `index-tables` | 2 | 9 | 6 | 2 | 1.00 | 2/2 | 1.00 | 0.33 | 0.22 | 7420 | 525 | 8502 | $0.002437 | 202 |
| `ingest-summary` | 3 | 10 | 4 | 3 | 1.00 | 3/3 | 1.00 | 0.75 | 0.30 | 4911 | 353 | 5581 | $0.002430 | 158 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 1/2 | 0.50 | 0.50 | 0.50 | 267 | 20 | 884 | $0.000392 | 167 |
| `what-is-it` | 3 | 11 | 9 | 2 | 0.67 | 1/3 | 0.33 | 0.22 | 0.18 | 2611 | 195 | 7255 | $0.002680 | 209 |
| `cli-dispatch` | 1 | 6 | 4 | 1 | 1.00 | 1/1 | 1.00 | 0.25 | 0.17 | 727 | 46 | 3204 | $0.001755 | 170 |
| `template-vars` | 3 | 11 | 8 | 3 | 1.00 | 2/3 | 0.67 | 0.38 | 0.27 | 362 | 26 | 6989 | $0.002551 | 182 |
| `utils-fs` | 2 | 11 | 10 | 2 | 1.00 | 2/2 | 1.00 | 0.20 | 0.18 | 1855 | 90 | 8515 | $0.002549 | 221 |
| `wiki-log` | 2 | 11 | 5 | 0 | 0.00 | 0/2 | 0.00 | 0.00 | 0.00 | 1971 | 116 | 4908 | $0.002548 | 267 |
| **mean** |  |  |  |  | **0.83** |  | **0.74** | **0.27** | **0.19** | **44906** | **3054** | **120322** | **$0.045945** | 203 |

### `--max-files 25`

| Query | Gold | Visited | Returned | Found | Recall | Sections | Section recall | Precision | Precision (visited) | Read (tok) | Lines | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 7 | 5 | 1 | 0.50 | 0/2 | 0.00 | 0.20 | 0.29 | 85 | 6 | 4677 | $0.002005 | 197 |
| `init-scaffold` | 3 | 14 | 13 | 3 | 1.00 | 2/3 | 0.67 | 0.23 | 0.21 | 6714 | 482 | 13768 | $0.003300 | 199 |
| `init-copies` | 2 | 14 | 12 | 2 | 1.00 | 2/2 | 1.00 | 0.17 | 0.14 | 3209 | 227 | 11502 | $0.003179 | 205 |
| `node-runtime-floor` | 1 | 11 | 6 | 1 | 1.00 | 1/1 | 1.00 | 0.17 | 0.09 | 1491 | 92 | 5886 | $0.002749 | 185 |
| `types-alignment` | 1 | 12 | 7 | 1 | 1.00 | 1/1 | 1.00 | 0.14 | 0.08 | 1385 | 87 | 8317 | $0.003053 | 208 |
| `wiki-validation` | 2 | 13 | 9 | 2 | 1.00 | 2/2 | 1.00 | 0.22 | 0.15 | 5591 | 401 | 11592 | $0.003189 | 199 |
| `wiki-code-sync` | 1 | 13 | 10 | 1 | 1.00 | 1/1 | 1.00 | 0.10 | 0.08 | 7913 | 566 | 11374 | $0.003065 | 196 |
| `compiled-cli-tests` | 2 | 13 | 7 | 2 | 1.00 | 2/2 | 1.00 | 0.29 | 0.15 | 1990 | 113 | 7703 | $0.003058 | 211 |
| `run-unit-tests` | 1 | 12 | 7 | 1 | 1.00 | 1/1 | 1.00 | 0.14 | 0.08 | 1093 | 71 | 7909 | $0.002904 | 214 |
| `upgrade-preserves` | 3 | 8 | 4 | 3 | 1.00 | 2/3 | 0.67 | 0.75 | 0.38 | 1482 | 114 | 2991 | $0.001868 | 201 |
| `source-layout` | 1 | 16 | 12 | 1 | 1.00 | 1/1 | 1.00 | 0.08 | 0.06 | 10154 | 712 | 12826 | $0.003623 | 212 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0/2 | 0.00 | 0.00 | 0.00 | 0 | 0 | 0 | $0.000358 | 225 |
| `index-tables` | 2 | 9 | 6 | 2 | 1.00 | 2/2 | 1.00 | 0.33 | 0.22 | 7420 | 525 | 8502 | $0.002437 | 202 |
| `ingest-summary` | 3 | 10 | 4 | 3 | 1.00 | 3/3 | 1.00 | 0.75 | 0.30 | 4911 | 353 | 5581 | $0.002430 | 158 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 1/2 | 0.50 | 0.50 | 0.50 | 267 | 20 | 884 | $0.000392 | 167 |
| `what-is-it` | 3 | 16 | 12 | 2 | 0.67 | 1/3 | 0.33 | 0.17 | 0.12 | 7308 | 534 | 12509 | $0.003623 | 190 |
| `cli-dispatch` | 1 | 6 | 4 | 1 | 1.00 | 1/1 | 1.00 | 0.25 | 0.17 | 727 | 46 | 3204 | $0.001755 | 170 |
| `template-vars` | 3 | 14 | 9 | 3 | 1.00 | 2/3 | 0.67 | 0.33 | 0.21 | 765 | 58 | 9421 | $0.003180 | 189 |
| `utils-fs` | 2 | 11 | 10 | 2 | 1.00 | 2/2 | 1.00 | 0.20 | 0.18 | 1855 | 90 | 8515 | $0.002549 | 221 |
| `wiki-log` | 2 | 11 | 5 | 0 | 0.00 | 0/2 | 0.00 | 0.00 | 0.00 | 1971 | 116 | 4908 | $0.002548 | 267 |
| **mean** |  |  |  |  | **0.83** |  | **0.74** | **0.25** | **0.17** | **66331** | **4613** | **152069** | **$0.051264** | 202 |

## Against grep, and against reading the corpus

The keyword baseline is the harness's own keyword ranker, asked for the same number of hits and read whole: it is what a caller with grep and no model gets. Reading the corpus is the floor no ranking can beat on tokens, counted the way the rows above are — over every query, so reading all 19 pages once per query.

| Budget | s1m recall | s1m precision | s1m read (tok) | grep recall | grep precision | grep read (tok) |
| --- | --- | --- | --- | --- | --- | --- |
| 10 | 0.83 | 0.27 | 44906 | 0.94 | 0.18 | 243070 |
| 25 | 0.83 | 0.25 | 66331 | 1.00 | 0.12 | 326765 |
| whole corpus | 1.00 | 0.10 | 346160 | | | |

Reading every page for every query finds every wanted page and reads 346160 tokens for the gold set, 7.7× s1m's returned ranges. The precision column is the wanted pages over the 19 pages there are, averaged over the queries: that is what an unranked reader reads.

## The pages no walk reached

Wanted pages no walk reached at `--max-files 25`: 6 query/page pairs missed.
The cutoff costs 1 of the 33 wanted pages a walk did reach: those are in the JSON's `walked`, not in the list the agent reads, because reaching a page is not returning it.

| Query | Wanted but not reached |
| --- | --- |
| `flat-entities` | `AGENTS.md`, `schema.md` |
| `raw-immutable` | `schema.md` |
| `what-is-it` | `README.md` |
| `wiki-log` | `AGENTS.md`, `log.md` |

## Calibration: scent against arrival

Every file a link reached carries the scent of that link and the relevance the model then gave the file, which is the plan's curve for free. `gold` is the share of those arrivals the gold set wanted. At `--max-files 25`, over 20 queries:

| Scent | Arrivals | Mean scent | Mean relevance | Wanted |
| --- | --- | --- | --- | --- |
| 0.6–0.7 | 60 | 0.64 | 0.49 | 0.07 |
| 0.7–0.8 | 55 | 0.75 | 0.62 | 0.11 |
| 0.8–0.9 | 64 | 0.85 | 0.74 | 0.22 |
| 0.9–1.0 | 14 | 0.92 | 0.89 | 0.64 |

193 arrivals: a link the threshold followed. The bins below it are empty by construction — a link under `--threshold` is never followed, so nothing arrives by one — which is the next table's question.

Every link the walk judged whose target is a page of this wiki (1100; 0 more left the wiki or are not there, and no label can say what they would have reached):

| Scent | Links | Followed | Wanted when followed |
| --- | --- | --- | --- |
| 0.0–0.1 | 1 | 0 | — |
| 0.1–0.2 | 32 | 0 | — |
| 0.2–0.3 | 88 | 0 | — |
| 0.3–0.4 | 103 | 0 | — |
| 0.4–0.5 | 120 | 0 | — |
| 0.5–0.6 | 146 | 0 | — |
| 0.6–0.7 | 190 | 62 | 0.06 |
| 0.7–0.8 | 170 | 59 | 0.12 |
| 0.8–0.9 | 197 | 65 | 0.23 |
| 0.9–1.0 | 53 | 14 | 0.64 |

| Decision | Links | Wanted |
| --- | --- | --- |
| followed | 200 | 0.17 |
| passed over | 900 | 0.14 |

The walk's own decision, in the same terms: the links it followed reach a wanted page 0.17 of the time, the ones it passed over 0.14. These two rows split on whether the walk followed a link, not on scent, which is why they do not partition the bins above the same way: 410 links clear `--threshold` and were still passed over, for want of depth or because their target had already been reached by a better path. A gold set is not the whole of what is useful — a link can lead to a page worth reading for the query without being one of the pages that query was labelled with — so both numbers are lower than they would be against a label of *relevant*, and it is the gap between them that says where the threshold belongs.

## The default threshold

The same gold set walked at `--max-files 10` with the link and section thresholds moved together, the way the CLI defaults them. These are judgments the runs above already made wherever the threshold never changed which page was worth visiting, so most of this table costs nothing. `Section recall` and `Lines` are the two columns this is tuned against ([#58]): recall says whether the page is in the list at all, and the pair says whether what is returned is the part that answers — a threshold that keeps recall and takes lines without losing parts is reading less of the same pages, and one that loses parts is cutting the answer.

| Threshold | Recall | Section recall | Precision | Read (tok) | Lines | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| 0.5 | 0.86 | 0.74 | 0.21 | 56454 | 3725 | $0.048598 |
| **0.6** (default) | 0.83 | 0.74 | 0.27 | 44906 | 3054 | $0.045945 |
| 0.7 | 0.63 | 0.59 | 0.41 | 25649 | 1731 | $0.035831 |
| 0.8 | 0.52 | 0.50 | 0.40 | 18124 | 1225 | $0.027811 |

## The preview experiment: frontmatter

A preview carries a target's title, its frontmatter and its first paragraph. The spike varied the whole preview as one knob and could not say which part did the work, and left the frontmatter — the part most likely to mislead, since `related:` makes every page look connected to every other — to this milestone. The same gold set, at `--max-files 10`, with the frontmatter dropped and with previews off:

| Preview policy | Recall | Precision | Read (tok) | Input (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- |
| previews on (default) | 0.83 | 0.27 | 44906 | 1093921 | $0.045945 | 203 |
| previews, no frontmatter | 0.77 | 0.25 | 35931 | 733996 | $0.030828 | 157 |
| previews off | 0.35 | 0.20 | 12233 | 183549 | $0.007709 | 165 |

At the scale of one page: `index.md` — the entry file of query `release-and-publish` — judged by that query under each policy, with the scent each policy gave each of its links and whether that scent clears `--threshold` 0.6 so the walk would follow it (bold: it would):

| Target | previews on (default) | previews, no frontmatter | previews off |
| --- | --- | --- | --- |
| `raw/raw.md` | 0.07 | 0.06 | 0.19 |
| `entities/cli.md` | 0.48 | 0.36 | 0.27 |
| `entities/commands.md` | 0.37 | 0.32 | 0.29 |
| `entities/templates.md` | 0.20 | 0.22 | 0.17 |
| `entities/utils.md` | 0.28 | 0.34 | 0.15 |
| `concepts/dogfooding.md` | **0.82** | **0.80** | 0.11 |
| `concepts/e2e-tests.md` | **0.64** | **0.73** | 0.11 |
| `concepts/init-command.md` | 0.52 | 0.39 | 0.18 |
| `concepts/node-version-and-types.md` | **0.70** | **0.67** | 0.19 |
| `concepts/release.md` | **0.90** | **0.63** | **0.91** |
| `concepts/repo-layout.md` | **0.78** | **0.82** | 0.18 |
| `concepts/template-system.md` | 0.51 | **0.63** | 0.14 |
| `concepts/unit-tests.md` | 0.58 | 0.58 | 0.15 |
| `concepts/wiki-scripts.md` | 0.51 | 0.57 | 0.17 |

Judging that one page under each policy is the only measurement here that is not a walk, so it has no row in the tables above; it is in the run's total, three answers.

`previews, no frontmatter` against the default, on this page: 1 of 14 links change whether the walk would follow them, and the mean scent moves by 0.07.
`previews off` against the default, on this page: 4 of 14 links change whether the walk would follow them, and the mean scent moves by 0.32.

## The link context: what the state carries, and what each part earns

A link is judged from one hop: the page it sits on, its anchor, its sentence and its heading, and the target's title, frontmatter and first paragraph. The failure analysis on a private wiki ([#36]) found the queries that reached nothing doing it two or three hops out, behind intermediate pages whose preview says nothing about what lies under them, and [#46] measured what a link needs to carry to reach them. All three parts measured there now ship, so the tables below are ablations of the shipped state rather than additions to it, each at `--max-files 10`:

- **The target's own H2/H3 headings**, in order, at most 40 of them and each cut at 80 characters.
- **The anchor text of the target's own in-root links**, in order, deduped, at most 30 and each cut at 60 characters — one hop of lookahead past the target.
- **The link question asked about two hops** rather than one: what this link reaches directly or through the pages it links to, with the yes-criterion to match. The state is unchanged by this one; only the question is.
- **`before #46`** is the state all of that was measured against — one hop, no headings, no leads — and it is the row every number in this report before the issue was made from.

| Variant | Recall | Precision | Read (tok) | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| what ships (default) | 0.83 | 0.27 | 44906 | 1093921 | $0.045945 | 186 | 1.00 |
| no headings | 0.73 | 0.27 | 36612 | 918350 | $0.038571 | 163 | 1.00 |
| no leads_to | 0.73 | 0.30 | 33964 | 647223 | $0.027183 | 118 | 1.00 |
| one hop | 0.72 | 0.32 | 33342 | 606761 | $0.025484 | 99 | 1.00 |
| before #46 | 0.59 | 0.30 | 30297 | 448709 | $0.018846 | 81 | 1.00 |

`Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged, so 1.00 is a link table that fits one post: a variant above 1.00 is splitting pages the state budget no longer holds ([#37]). A `Requests` column that rose while `Req/answer` stayed at 1.00 is the other cost — a link the model now rates above `--threshold` is a page the walk visits and pays for, which is where the shipped state's recall comes from. It asks 2.3× the requests it asked before [#46].

Recall per query, the queries the shipped walk found least first:

| Query | Gold | what ships (default) | no headings | no leads_to | one hop | before #46 |
| --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `wiki-log` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `raw-immutable` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `what-is-it` | 3 | 0.67 | 0.67 | 0.67 | 0.67 | 0.67 |
| `cli-dispatch` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 0.00 |
| `compiled-cli-tests` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `index-tables` | 2 | 1.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `ingest-summary` | 3 | 1.00 | 0.67 | 0.33 | 0.33 | 0.33 |
| `init-copies` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `init-scaffold` | 3 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `node-runtime-floor` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `run-unit-tests` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `source-layout` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `template-vars` | 3 | 1.00 | 1.00 | 0.67 | 0.67 | 0.33 |
| `types-alignment` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `upgrade-preserves` | 3 | 1.00 | 0.33 | 1.00 | 0.67 | 0.00 |
| `utils-fs` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-validation` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 0.50 |
| **mean** |  | **0.83** | **0.73** | **0.73** | **0.72** | **0.59** |

Requests per query, the same order: what each variant asked of the API, where the split shows up.

| Query | Gold | what ships (default) | no headings | no leads_to | one hop | before #46 |
| --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 1 | 1 | 1 | 1 | 1 |
| `wiki-log` | 2 | 11 | 9 | 3 | 2 | 2 |
| `raw-immutable` | 2 | 2 | 2 | 2 | 2 | 2 |
| `release-and-publish` | 2 | 7 | 5 | 7 | 4 | 5 |
| `what-is-it` | 3 | 11 | 11 | 11 | 11 | 11 |
| `cli-dispatch` | 1 | 6 | 4 | 2 | 2 | 1 |
| `compiled-cli-tests` | 2 | 11 | 11 | 6 | 5 | 4 |
| `index-tables` | 2 | 9 | 1 | 1 | 1 | 1 |
| `ingest-summary` | 3 | 10 | 9 | 5 | 3 | 2 |
| `init-copies` | 2 | 11 | 11 | 9 | 8 | 6 |
| `init-scaffold` | 3 | 11 | 11 | 11 | 11 | 11 |
| `node-runtime-floor` | 1 | 11 | 11 | 3 | 5 | 2 |
| `run-unit-tests` | 1 | 11 | 11 | 4 | 4 | 3 |
| `source-layout` | 1 | 11 | 11 | 11 | 5 | 6 |
| `template-vars` | 3 | 11 | 11 | 7 | 7 | 5 |
| `types-alignment` | 1 | 11 | 8 | 3 | 3 | 2 |
| `upgrade-preserves` | 3 | 8 | 4 | 8 | 4 | 1 |
| `utils-fs` | 2 | 11 | 10 | 6 | 4 | 4 |
| `wiki-code-sync` | 1 | 11 | 11 | 10 | 10 | 9 |
| `wiki-validation` | 2 | 11 | 11 | 8 | 7 | 3 |
| **total** |  | **186** | **163** | **118** | **99** | **81** |

## The relative judge: one Choice over a page's links

The walk as it ships follows a link on the model's own answer about that link — is following it likely to lead somewhere useful — measured against `--threshold`. The same page can be judged as one question instead: which of its links is the best next step, answered as a share per link. A share is followed where it clears a cut that moves with the page — `max(0.02, min(3 / options, 0.5))`, against the options the question actually carried, and never a page whose best option is `none` — and the walk visits at most `--beam` 8 files at each depth. The file's own Score and its section Nouls are asked exactly as the shipping judge asks them, from the same state, so the rows vary the link judgment — and, in the last one, the cut — and nothing else. `choice` describes its options from the page alone; `choice + previews` gives each option the preview the state carries. At `--max-files 10`:

| Links judged | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Returned | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| noul: what ships | 0.83 | 0.27 | 0.19 | 44906 | 120322 | 126 | 1093921 | $0.045945 | 186 | 1.00 |
| choice | 0.54 | 0.35 | 0.33 | 14884 | 35866 | 44 | 370871 | $0.015577 | 103 | 1.94 |
| choice + previews | 0.62 | 0.42 | 0.39 | 26320 | 47093 | 52 | 492666 | $0.020692 | 114 | 1.93 |
| choice + previews, k=1 | 0.77 | 0.45 | 0.41 | 40133 | 65295 | 66 | 643742 | $0.027037 | 152 | 1.92 |

`Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged, so 1.00 is a question set that fits one post: a row above 1.00 is the second request a page's Choice costs. A `choice` row that asks more than the row above it and reads less is the relative judge doing its job — fewer, better files — and one that recalls less is the cut closing pages the Noul would have walked through.

Recall per query, the queries the shipped walk found least first:

| Query | Gold | noul: what ships | choice | choice + previews | choice + previews, k=1 |
| --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 0.00 | 0.00 | 0.00 | 1.00 |
| `wiki-log` | 2 | 0.00 | 0.00 | 0.00 | 0.00 |
| `raw-immutable` | 2 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | 0.50 | 0.50 | 0.50 | 0.50 |
| `what-is-it` | 3 | 0.67 | 0.00 | 0.33 | 0.67 |
| `cli-dispatch` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `compiled-cli-tests` | 2 | 1.00 | 0.50 | 0.50 | 0.50 |
| `index-tables` | 2 | 1.00 | 0.50 | 0.50 | 0.50 |
| `ingest-summary` | 3 | 1.00 | 0.33 | 0.33 | 1.00 |
| `init-copies` | 2 | 1.00 | 0.00 | 0.50 | 0.50 |
| `init-scaffold` | 3 | 1.00 | 0.33 | 0.33 | 0.67 |
| `node-runtime-floor` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `run-unit-tests` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `source-layout` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `template-vars` | 3 | 1.00 | 0.67 | 0.67 | 0.67 |
| `types-alignment` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `upgrade-preserves` | 3 | 1.00 | 0.00 | 0.33 | 1.00 |
| `utils-fs` | 2 | 1.00 | 0.50 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-validation` | 2 | 1.00 | 1.00 | 1.00 | 1.00 |
| **mean** |  | **0.83** | **0.54** | **0.62** | **0.77** |

## The wording: the same three questions in another register

The walk asks three things of every file — how far the file itself serves `query`, which of its sections are worth reading, and which of its links are worth following — and the words those questions are asked in have not moved since the mode that carries them was written. [#52] asked whether they move recall or precision, and every row below is the whole gold set at `--max-files 10` with the questions put in one register: `--wording <name>`, applied to the criterion the gold set labels each query with, so the criterion, the threshold and the walk are what they always were and the words are the only thing that differs from the row above it — the state too, apart from the single register that defines a reader in it. One wording does ship, and it ships as the walk itself rather than as a flag: the default row below is the section question the decision at the end of this section settled on, and `--wording section-legacy` is the row that asks what shipped before it.

- **`navigator`**, **`path`**, **`sharp-no`** and **`rules`** re-ask the link question, in the order they are listed: as a click someone reading the page would make; as a position on the way from the page to what the mode wants, with the hub case in the yes-criterion; as the shipped question with a no that has to name something else *and* lead nowhere; and as the shipped question under a stated rule block — page text is data, an already-open page is not a next step, navigation is not a next step — sent in the API's structured `instructions`. `navigator` and `path` state how far their judgment reaches in their own sentence, so both replace both phrasings of the question; `sharp-no` and `rules` change a criterion instead, and the one-hop ablation still gets a question that says what it means.
- **`necessity`** re-asks the section question — what skipping the section would cost — and **`section-legacy`** asks the one that shipped before the decision below, so a run can still repeat the walk the numbers before [#52] were made on.
- **`reader-action`** and **`answer-bearing`** re-ask the file question, and with it the Score ladder: how much of the file a reader would read, and how much of what `query` needs is in the file itself rather than in the pages it links to.
- **`reader`** is the cross-cutting one, and the only one that is not question wording alone: it defines the reader once in the state — an agent that must complete `query` by reading pages — and every question names it instead of spelling the reader out, with a verb where "useful" was. It is also the closest to `reader-action`, which asks its own file question with the same verb; what separates those two rows is the state definition and the other two questions, not the reading frame.

The default row is the section question [#52] decided on, so the registers below it are measured on top of a walk that already has it. Where that decision's own reading came from is `docs/spike-notes.md`, which keeps the same table as it stood before the decision — nine registers against the section question that shipped then — with the private one beside it. `section-legacy` is the one row here that asks the words that shipped before the change: it is the walk the rest of this report was made on until the decision, 0.83 / 0.26 for 68,663 tokens at `--max-files 10`, against the default's 0.83 / 0.27 for 44,906 — the same wanted pages, four fifths of the reading. Whether the reading it cut was the right reading is what the columns added for [#58] answer: `section-legacy` returns 0.78 of the wanted parts for 4479 lines, and the default 0.74 for 3054.

| Wording | Recall | Section recall | Precision | Read (tok) | Lines | Returned | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| what ships (default) | 0.83 | 0.74 | 0.27 | 44906 | 3054 | 126 | 1093921 | $0.045945 | 186 | 1.00 |
| navigator | 0.61 | 0.55 | 0.34 | 17083 | 1150 | 50 | 355245 | $0.014920 | 57 | 1.00 |
| path | 0.72 | 0.62 | 0.24 | 27956 | 1902 | 99 | 703203 | $0.029535 | 112 | 1.00 |
| sharp-no | 0.82 | 0.72 | 0.25 | 44950 | 3027 | 129 | 1078519 | $0.045298 | 186 | 1.00 |
| rules | 0.72 | 0.62 | 0.21 | 35003 | 2355 | 114 | 903271 | $0.037937 | 131 | 1.00 |
| section-legacy | 0.83 | 0.78 | 0.26 | 68663 | 4479 | 129 | 1057882 | $0.044431 | 181 | 1.00 |
| reader-action | 0.78 | 0.74 | 0.41 | 46782 | 3184 | 79 | 1085297 | $0.045582 | 184 | 1.00 |
| answer-bearing | 0.74 | 0.72 | 0.42 | 47127 | 3214 | 71 | 1088126 | $0.045701 | 184 | 1.00 |
| reader | 0.70 | 0.68 | 0.27 | 37568 | 2464 | 86 | 555427 | $0.023328 | 88 | 1.00 |

`Returned` is the files that earned a place in the reading list, which is the list an agent reads and the one precision is over: a register that leaves recall where it was and returns fewer files is one whose sections and Scores stopped vouching for pages the walk still reached, and that is a cheaper list with the same wanted pages in it. `Section recall` and `Lines` say what that cheaper list kept: the labelled parts of those pages the returned ranges cover, and the lines they span, so a row that returns fewer files and the same parts is reading less of the same pages, and one that loses parts is reading around the answer ([#58]). `Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged; a wording moves the ranking, so a row above the shipped one is asking more questions about the pages the words sent it to. The register each name sends is in `src/jev.rs` (`Wording`), sentence for sentence, and is held there by a test: what is measured here is what a reviewer can read. `--wording` on the CLI is the one way to ask for one.

Recall per query, the queries the shipped walk found least first:

| Query | Gold | Mode | what ships (default) | navigator | path | sharp-no | rules | section-legacy | reader-action | answer-bearing | reader |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | `answers` | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `wiki-log` | 2 | `answers` | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `raw-immutable` | 2 | `answers` | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | `useful-for` | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 | 0.00 | 0.00 | 0.50 |
| `what-is-it` | 3 | `about` | 0.67 | 0.00 | 0.67 | 0.67 | 0.67 | 0.67 | 0.33 | 0.33 | 0.33 |
| `cli-dispatch` | 1 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `compiled-cli-tests` | 2 | `answers` | 1.00 | 0.50 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `index-tables` | 2 | `answers` | 1.00 | 0.00 | 0.00 | 1.00 | 0.00 | 1.00 | 1.00 | 1.00 | 0.00 |
| `ingest-summary` | 3 | `useful-for` | 1.00 | 0.00 | 0.00 | 0.67 | 0.00 | 1.00 | 1.00 | 0.67 | 0.00 |
| `init-copies` | 2 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `init-scaffold` | 3 | `useful-for` | 1.00 | 0.33 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `node-runtime-floor` | 1 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `run-unit-tests` | 1 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `source-layout` | 1 | `about` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `template-vars` | 3 | `answers` | 1.00 | 0.67 | 1.00 | 1.00 | 1.00 | 1.00 | 0.67 | 0.67 | 1.00 |
| `types-alignment` | 1 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `upgrade-preserves` | 3 | `answers` | 1.00 | 0.67 | 0.67 | 1.00 | 0.67 | 1.00 | 1.00 | 0.67 | 0.67 |
| `utils-fs` | 2 | `answers` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | `useful-for` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-validation` | 2 | `useful-for` | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| **mean** |  |  | **0.83** | **0.61** | **0.72** | **0.82** | **0.72** | **0.83** | **0.78** | **0.74** | **0.70** |

Section recall and the same lines per query, the walk that ships against `section-legacy`, the queries the section question lost the most parts on first — a query the two rows agree on is one whose cut reading was not read for:

| Query | Gold | Mode | Section recall: default | Section recall: section-legacy | Lines: default | Lines: section-legacy |
| --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | `useful-for` | 0.00 | 0.50 | 6 | 37 |
| `init-scaffold` | 3 | `useful-for` | 0.67 | 1.00 | 174 | 281 |
| `upgrade-preserves` | 3 | `answers` | 0.67 | 1.00 | 114 | 181 |
| `cli-dispatch` | 1 | `answers` | 1.00 | 1.00 | 46 | 95 |
| `compiled-cli-tests` | 2 | `answers` | 1.00 | 1.00 | 113 | 293 |
| `flat-entities` | 2 | `answers` | 0.00 | 0.00 | 0 | 0 |
| `index-tables` | 2 | `answers` | 1.00 | 1.00 | 525 | 531 |
| `ingest-summary` | 3 | `useful-for` | 1.00 | 1.00 | 353 | 353 |
| `init-copies` | 2 | `answers` | 1.00 | 1.00 | 141 | 263 |
| `node-runtime-floor` | 1 | `answers` | 1.00 | 1.00 | 92 | 195 |
| `raw-immutable` | 2 | `answers` | 0.50 | 0.50 | 20 | 54 |
| `run-unit-tests` | 1 | `answers` | 1.00 | 1.00 | 71 | 229 |
| `source-layout` | 1 | `about` | 1.00 | 1.00 | 307 | 214 |
| `template-vars` | 3 | `answers` | 0.67 | 0.67 | 26 | 272 |
| `types-alignment` | 1 | `answers` | 1.00 | 1.00 | 80 | 119 |
| `utils-fs` | 2 | `answers` | 1.00 | 1.00 | 90 | 228 |
| `wiki-code-sync` | 1 | `useful-for` | 1.00 | 1.00 | 359 | 492 |
| `wiki-log` | 2 | `answers` | 0.00 | 0.00 | 116 | 250 |
| `wiki-validation` | 2 | `useful-for` | 1.00 | 1.00 | 226 | 265 |
| `what-is-it` | 3 | `about` | 0.33 | 0.00 | 195 | 127 |
| **mean** |  |  | **0.74** | **0.78** | **3054** | **4479** |

## What these numbers are not

- **The corpus is thin.** 19 pages, so a budget of 25 can hold the corpus and the wider budget stops being a ranking question. The differences between configurations here are indicative, not a tuning set; nothing in this report should be treated as more than a direction on a wiki this size.
- **The labels are one reader's.** A page that is useful and unlisted counts against precision, so precision is a lower bound and recall is only as good as the list. The wanted sets were written from the wiki's own pages, not from a task run against it.
- **A hit is not an answer.** Recall counts the files the reading list returned, not whether an agent could do the task with them: a wanted page the walk reached but that earned no place on its own is not in that list, so it counts as missed. `read` counts characters at 4, not what a tokeniser would charge.
- **A section hit is not coverage.** Section recall counts a labelled part the returned ranges overlap, so a range that covers one line of a labelled section is counted like the range that covers all of it, and a part is only as good as the label a person wrote. It cannot be above recall, and both are one reader's judgement of what the answer is.
- **One model, one day.** Jev moves its numbers between identical requests, which is why every number here comes from the committed cache: rerun without it and the rankings hold while the numbers underneath them shift (`docs/spike-notes.md`).

## The committed cache

The answers came from `eval/cache`. That directory holds one file per request the runs above made: the query, the mode, the file and its links key the entry, and the entry carries the judgment and what the call cost. It is what makes this report a thing to check rather than a claim — the same command with no key on a machine that has never asked the API reads the same answers and prints the same bytes — and it is committed here because a wiki and a gold set are not: a private wiki is measured with the same command against its own directory, and nothing about it lands in this repository.

