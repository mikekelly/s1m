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
| Requests | 2046 behind those answers; more than one per answer means a file whose sections and links did not fit one post |
| Cost | $0.044431 for the gold set at `--max-files 10`, $0.048529 at `--max-files 25`; every answer this report used, at the list price above, $0.457720 |

## Headline

- **Recall and precision at `--max-files 10`**: mean recall 0.83, mean precision 0.26 — 32 of the 39 wanted pages are in the list the agent opens, over 129 files returned, 6.5 a query — against 0.19 over everything the walk visited: 181 files judged, 129 of them earned a place, and the rest are what the JSON reports as `walked`. The budget is not what binds: the walk runs out of links above `--threshold` first, and `--max-files 25` visits 202 files for the same mean recall (0.83), 141 of which earn a place, so everything below is a statement about the link graph and the threshold, not about the budget.
- **The keyword ranker finds more and reads far more**: recall 0.94 against s1m's 0.83, at 243070 tokens against 68663 — 3.5× the reading for 0.11 more of the wanted pages. On a wiki whose pages share their vocabulary with the queries, grep is the stronger recaller and s1m the cheaper reader.
- **What an agent reads**: 68663 tokens for the returned ranges, against 122023 for the same files whole and 346160 for every page on every query. Reading the returned files whole costs 35% of the corpus's text; the section scores take 44% off that, and the ranking 80% off reading everything.
- **What it costs**: $0.044431 for the gold set at `--max-files 10` — $0.002222 a query, at 0.19 s an answer, $0.048529 at `--max-files 25`; every answer this report used, at the price above, $0.457720. The figures are the input tokens the answers spent, priced at the list rate in the header: the cache fixes the tokens, and a rate change re-prices every row, so a rerun reproduces them only while that constant stands.
- **Where `--threshold` sits**: this report walked at 0.6. Against that walk, the swept thresholds move recall and reading by: 0.5: recall +0.05 and reading +50%; 0.6: recall +0.00 and reading +0%; 0.7: recall -0.17 and reading -40%; 0.8: recall -0.30 and reading -70%. The calibration says the same from the other side — the links the walk followed reach a wanted page 0.18 of the time, the ones it passed over 0.14, and 394 links clear the threshold and are still not followed.
- **The frontmatter earns its tokens**: dropping it from the preview costs 0.08 of recall (0.83 → 0.75) for -33% of the input tokens, and dropping previews altogether costs 0.47. It is the larger half of what a preview buys, and `related:` is why — on the hub page it is what lifts the links to `dogfooding.md` and `node-version-and-types.md` over the threshold. [#10]'s worry that the frontmatter misleads is the wrong way round on this wiki.

## How to reproduce

```bash
cargo run --release --bin eval -- \
  --wiki eval/wikis/llm-wiki-manager/wiki \
  --gold eval/gold/llm-wiki-manager.json \
  --cache eval/cache \
  --relative-judge \
  --out PATH
```

This report goes to stdout without `--out`, and `--out PATH` writes it to a file instead. Every judgment is cached on the request that produced it, and the cache stores the tokens each call spent beside its answer, so the cache committed under that directory reproduces this report byte for byte with no `TYPESAFE_API_KEY` at all. The cost columns are those stored tokens at the list rate in the header — the cache fixes the tokens, not the rate — and `--no-cache` with a key buys every judgment again. `--wiki` and `--gold` are the only thing a private wiki needs, and nothing about either is committed here. `--relative-judge` is what adds the relative judge's rows below: those asks are this report's own, so a run without the flag prints the report without that table, and the cache answers the rest either way.

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

`--max-files` is the number of files the walk may judge beyond the entry files, which are always visited, and the walk judges that many before the reading list is asked anything. `Visited` counts the files it judged; `Returned` is the list the agent opens — the ones that earn a place on their own, relevance at or above `--threshold` 0.6 or a section at or above it, most relevant first — and the rest, the entry files, hubs and near-misses, are what the JSON reports as `walked`. Recall is the wanted pages in that list over all of the query's wanted pages, and precision is the wanted pages in it over the files in it; precision (visited) is the same over everything the walk judged, which is the number this harness reported while the list was everything the walk had visited, so the two side by side are what the cutoff bought and cost. At `--max-files 10`: 181 files visited and 129 returned, mean precision 0.26 against 0.19 over everything visited. `read` is what the agent opens — the returned ranges only — and `whole` is those same files read entire.

### `--max-files 10`

| Query | Gold | Visited | Returned | Found | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 6 | 4 | 1 | 0.50 | 0.25 | 0.33 | 668 | 3419 | $0.001760 | 178 |
| `init-scaffold` | 3 | 11 | 10 | 3 | 1.00 | 0.30 | 0.27 | 4335 | 7812 | $0.002598 | 170 |
| `init-copies` | 2 | 11 | 9 | 2 | 1.00 | 0.22 | 0.18 | 4107 | 7634 | $0.002517 | 200 |
| `node-runtime-floor` | 1 | 11 | 6 | 1 | 1.00 | 0.17 | 0.09 | 3195 | 5886 | $0.002721 | 154 |
| `types-alignment` | 1 | 11 | 6 | 1 | 1.00 | 0.17 | 0.09 | 1817 | 5886 | $0.002768 | 184 |
| `wiki-validation` | 2 | 11 | 7 | 2 | 1.00 | 0.29 | 0.18 | 3809 | 8067 | $0.002737 | 166 |
| `wiki-code-sync` | 1 | 11 | 9 | 1 | 1.00 | 0.11 | 0.09 | 7439 | 8925 | $0.002738 | 146 |
| `compiled-cli-tests` | 2 | 11 | 8 | 2 | 1.00 | 0.25 | 0.18 | 4891 | 8765 | $0.002679 | 197 |
| `run-unit-tests` | 1 | 11 | 7 | 1 | 1.00 | 0.14 | 0.09 | 3708 | 7909 | $0.002745 | 200 |
| `upgrade-preserves` | 3 | 8 | 5 | 3 | 1.00 | 0.60 | 0.38 | 2609 | 3847 | $0.001846 | 206 |
| `source-layout` | 1 | 11 | 7 | 1 | 1.00 | 0.14 | 0.09 | 3562 | 6135 | $0.002669 | 193 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000355 | 195 |
| `index-tables` | 2 | 8 | 6 | 2 | 1.00 | 0.33 | 0.25 | 7706 | 8502 | $0.002152 | 196 |
| `ingest-summary` | 3 | 10 | 4 | 3 | 1.00 | 0.75 | 0.30 | 4911 | 5581 | $0.002407 | 184 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 0.50 | 0.50 | 841 | 884 | $0.000389 | 207 |
| `what-is-it` | 3 | 11 | 10 | 2 | 0.67 | 0.20 | 0.18 | 1567 | 7812 | $0.002517 | 200 |
| `cli-dispatch` | 1 | 5 | 4 | 1 | 1.00 | 0.25 | 0.20 | 1370 | 3204 | $0.001563 | 191 |
| `template-vars` | 3 | 11 | 10 | 3 | 1.00 | 0.30 | 0.27 | 4356 | 8515 | $0.002517 | 199 |
| `utils-fs` | 2 | 11 | 10 | 2 | 1.00 | 0.20 | 0.18 | 3750 | 8515 | $0.002515 | 234 |
| `wiki-log` | 2 | 9 | 5 | 0 | 0.00 | 0.00 | 0.00 | 4022 | 4725 | $0.002239 | 307 |
| **mean** |  |  |  |  | **0.83** | **0.26** | **0.19** | **68663** | **122023** | **$0.044431** | 194 |

### `--max-files 25`

| Query | Gold | Visited | Returned | Found | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `release-and-publish` | 2 | 6 | 4 | 1 | 0.50 | 0.25 | 0.33 | 668 | 3419 | $0.001760 | 178 |
| `init-scaffold` | 3 | 12 | 11 | 3 | 1.00 | 0.27 | 0.25 | 4335 | 9070 | $0.002829 | 175 |
| `init-copies` | 2 | 14 | 11 | 2 | 1.00 | 0.18 | 0.14 | 5003 | 9070 | $0.003129 | 201 |
| `node-runtime-floor` | 1 | 11 | 6 | 1 | 1.00 | 0.17 | 0.09 | 3195 | 5886 | $0.002721 | 154 |
| `types-alignment` | 1 | 11 | 6 | 1 | 1.00 | 0.17 | 0.09 | 1817 | 5886 | $0.002768 | 184 |
| `wiki-validation` | 2 | 13 | 8 | 2 | 1.00 | 0.25 | 0.15 | 6075 | 10333 | $0.003158 | 169 |
| `wiki-code-sync` | 1 | 14 | 11 | 1 | 1.00 | 0.09 | 0.07 | 10133 | 11749 | $0.003268 | 175 |
| `compiled-cli-tests` | 2 | 12 | 8 | 2 | 1.00 | 0.25 | 0.17 | 4891 | 8765 | $0.002843 | 198 |
| `run-unit-tests` | 1 | 11 | 7 | 1 | 1.00 | 0.14 | 0.09 | 3708 | 7909 | $0.002745 | 200 |
| `upgrade-preserves` | 3 | 8 | 5 | 3 | 1.00 | 0.60 | 0.38 | 2609 | 3847 | $0.001846 | 206 |
| `source-layout` | 1 | 16 | 11 | 1 | 1.00 | 0.09 | 0.06 | 8995 | 11764 | $0.003547 | 196 |
| `flat-entities` | 2 | 1 | 0 | 0 | 0.00 | 0.00 | 0.00 | 0 | 0 | $0.000355 | 195 |
| `index-tables` | 2 | 8 | 6 | 2 | 1.00 | 0.33 | 0.25 | 7706 | 8502 | $0.002152 | 196 |
| `ingest-summary` | 3 | 10 | 4 | 3 | 1.00 | 0.75 | 0.30 | 4911 | 5581 | $0.002407 | 184 |
| `raw-immutable` | 2 | 2 | 2 | 1 | 0.50 | 0.50 | 0.50 | 841 | 884 | $0.000389 | 207 |
| `what-is-it` | 3 | 15 | 12 | 2 | 0.67 | 0.17 | 0.13 | 6265 | 12509 | $0.003317 | 193 |
| `cli-dispatch` | 1 | 5 | 4 | 1 | 1.00 | 0.25 | 0.20 | 1370 | 3204 | $0.001563 | 191 |
| `template-vars` | 3 | 13 | 10 | 3 | 1.00 | 0.30 | 0.23 | 4356 | 8515 | $0.002978 | 204 |
| `utils-fs` | 2 | 11 | 10 | 2 | 1.00 | 0.20 | 0.18 | 3750 | 8515 | $0.002515 | 234 |
| `wiki-log` | 2 | 9 | 5 | 0 | 0.00 | 0.00 | 0.00 | 4022 | 4725 | $0.002239 | 307 |
| **mean** |  |  |  |  | **0.83** | **0.25** | **0.18** | **84650** | **140133** | **$0.048529** | 196 |

## Against grep, and against reading the corpus

The keyword baseline is the harness's own keyword ranker, asked for the same number of hits and read whole: it is what a caller with grep and no model gets. Reading the corpus is the floor no ranking can beat on tokens, counted the way the rows above are — over every query, so reading all 19 pages once per query.

| Budget | s1m recall | s1m precision | s1m read (tok) | grep recall | grep precision | grep read (tok) |
| --- | --- | --- | --- | --- | --- | --- |
| 10 | 0.83 | 0.26 | 68663 | 0.94 | 0.18 | 243070 |
| 25 | 0.83 | 0.25 | 84650 | 1.00 | 0.12 | 326765 |
| whole corpus | 1.00 | 0.10 | 346160 | | | |

Reading every page for every query finds every wanted page and reads 346160 tokens for the gold set, 5.0× s1m's returned ranges. The precision column is the wanted pages over the 19 pages there are, averaged over the queries: that is what an unranked reader reads.

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
| 0.6–0.7 | 57 | 0.64 | 0.52 | 0.09 |
| 0.7–0.8 | 55 | 0.75 | 0.65 | 0.15 |
| 0.8–0.9 | 57 | 0.85 | 0.74 | 0.23 |
| 0.9–1.0 | 13 | 0.92 | 0.90 | 0.54 |

182 arrivals: a link the threshold followed. The bins below it are empty by construction — a link under `--threshold` is never followed, so nothing arrives by one — which is the next table's question.

Every link the walk judged whose target is a page of this wiki (1071; 0 more left the wiki or are not there, and no label can say what they would have reached):

| Scent | Links | Followed | Wanted when followed |
| --- | --- | --- | --- |
| 0.0–0.1 | 1 | 0 | — |
| 0.1–0.2 | 32 | 0 | — |
| 0.2–0.3 | 84 | 0 | — |
| 0.3–0.4 | 97 | 0 | — |
| 0.4–0.5 | 116 | 0 | — |
| 0.5–0.6 | 160 | 0 | — |
| 0.6–0.7 | 177 | 59 | 0.08 |
| 0.7–0.8 | 157 | 57 | 0.14 |
| 0.8–0.9 | 199 | 58 | 0.24 |
| 0.9–1.0 | 48 | 13 | 0.54 |

| Decision | Links | Wanted |
| --- | --- | --- |
| followed | 187 | 0.18 |
| passed over | 884 | 0.14 |

The walk's own decision, in the same terms: the links it followed reach a wanted page 0.18 of the time, the ones it passed over 0.14. These two rows split on whether the walk followed a link, not on scent, which is why they do not partition the bins above the same way: 394 links clear `--threshold` and were still passed over, for want of depth or because their target had already been reached by a better path. A gold set is not the whole of what is useful — a link can lead to a page worth reading for the query without being one of the pages that query was labelled with — so both numbers are lower than they would be against a label of *relevant*, and it is the gap between them that says where the threshold belongs.

## The default threshold

The same gold set walked at `--max-files 10` with the link and section thresholds moved together, the way the CLI defaults them. These are judgments the runs above already made wherever the threshold never changed which page was worth visiting, so most of this table costs nothing.

| Threshold | Recall | Precision | Read (tok) | Cost |
| --- | --- | --- | --- | --- |
| 0.5 | 0.88 | 0.21 | 103134 | $0.049672 |
| **0.6** (default) | 0.83 | 0.26 | 68663 | $0.044431 |
| 0.7 | 0.67 | 0.30 | 40943 | $0.034641 |
| 0.8 | 0.53 | 0.31 | 20587 | $0.026314 |

## The preview experiment: frontmatter

A preview carries a target's title, its frontmatter and its first paragraph. The spike varied the whole preview as one knob and could not say which part did the work, and left the frontmatter — the part most likely to mislead, since `related:` makes every page look connected to every other — to this milestone. The same gold set, at `--max-files 10`, with the frontmatter dropped and with previews off:

| Preview policy | Recall | Precision | Read (tok) | Input (tok) | Cost | ms/answer |
| --- | --- | --- | --- | --- | --- | --- |
| previews on (default) | 0.83 | 0.26 | 68663 | 1057882 | $0.044431 | 194 |
| previews, no frontmatter | 0.75 | 0.25 | 61900 | 711375 | $0.029878 | 166 |
| previews off | 0.37 | 0.23 | 15383 | 170764 | $0.007172 | 191 |

At the scale of one page: `index.md` — the entry file of query `release-and-publish` — judged by that query under each policy, with the scent each policy gave each of its links and whether that scent clears `--threshold` 0.6 so the walk would follow it (bold: it would):

| Target | previews on (default) | previews, no frontmatter | previews off |
| --- | --- | --- | --- |
| `raw/raw.md` | 0.07 | 0.06 | 0.19 |
| `entities/cli.md` | 0.51 | 0.39 | 0.25 |
| `entities/commands.md` | 0.38 | 0.32 | 0.27 |
| `entities/templates.md` | 0.21 | 0.23 | 0.16 |
| `entities/utils.md` | 0.28 | 0.41 | 0.14 |
| `concepts/dogfooding.md` | **0.80** | **0.82** | 0.13 |
| `concepts/e2e-tests.md` | 0.58 | **0.73** | 0.11 |
| `concepts/init-command.md` | 0.44 | 0.35 | 0.22 |
| `concepts/node-version-and-types.md` | **0.67** | **0.69** | 0.21 |
| `concepts/release.md` | **0.89** | **0.65** | **0.91** |
| `concepts/repo-layout.md` | **0.75** | **0.82** | 0.20 |
| `concepts/template-system.md` | 0.55 | **0.60** | 0.15 |
| `concepts/unit-tests.md` | 0.55 | 0.55 | 0.14 |
| `concepts/wiki-scripts.md` | 0.49 | 0.51 | 0.17 |

Judging that one page under each policy is the only measurement here that is not a walk, so it has no row in the tables above; it is in the run's total, three answers.

`previews, no frontmatter` against the default, on this page: 2 of 14 links change whether the walk would follow them, and the mean scent moves by 0.07.
`previews off` against the default, on this page: 3 of 14 links change whether the walk would follow them, and the mean scent moves by 0.30.

## The link context: what the state carries, and what each part earns

A link is judged from one hop: the page it sits on, its anchor, its sentence and its heading, and the target's title, frontmatter and first paragraph. The failure analysis on a private wiki ([#36]) found the queries that reached nothing doing it two or three hops out, behind intermediate pages whose preview says nothing about what lies under them, and [#46] measured what a link needs to carry to reach them. All three parts measured there now ship, so the tables below are ablations of the shipped state rather than additions to it, each at `--max-files 10`:

- **The target's own H2/H3 headings**, in order, at most 40 of them and each cut at 80 characters.
- **The anchor text of the target's own in-root links**, in order, deduped, at most 30 and each cut at 60 characters — one hop of lookahead past the target.
- **The link question asked about two hops** rather than one: what this link reaches directly or through the pages it links to, with the yes-criterion to match. The state is unchanged by this one; only the question is.
- **`before #46`** is the state all of that was measured against — one hop, no headings, no leads — and it is the row every number in this report before the issue was made from.

| Variant | Recall | Precision | Read (tok) | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| what ships (default) | 0.83 | 0.26 | 68663 | 1057882 | $0.044431 | 181 | 1.00 |
| no headings | 0.76 | 0.26 | 62512 | 923212 | $0.038775 | 165 | 1.00 |
| no leads_to | 0.73 | 0.29 | 48228 | 645438 | $0.027108 | 118 | 1.00 |
| one hop | 0.69 | 0.29 | 47685 | 596166 | $0.025039 | 97 | 1.00 |
| before #46 | 0.64 | 0.30 | 35771 | 457709 | $0.019224 | 85 | 1.00 |

`Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged, so 1.00 is a link table that fits one post: a variant above 1.00 is splitting pages the state budget no longer holds ([#37]). A `Requests` column that rose while `Req/answer` stayed at 1.00 is the other cost — a link the model now rates above `--threshold` is a page the walk visits and pays for, which is where the shipped state's recall comes from. It asks 2.1× the requests it asked before [#46].

Recall per query, the queries the shipped walk found least first:

| Query | Gold | what ships (default) | no headings | no leads_to | one hop | before #46 |
| --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `wiki-log` | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| `raw-immutable` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | 0.50 | 0.50 | 0.50 | 0.50 | 0.50 |
| `what-is-it` | 3 | 0.67 | 0.67 | 0.67 | 0.67 | 0.67 |
| `cli-dispatch` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `compiled-cli-tests` | 2 | 1.00 | 1.00 | 1.00 | 0.50 | 1.00 |
| `index-tables` | 2 | 1.00 | 0.50 | 0.00 | 0.00 | 0.00 |
| `ingest-summary` | 3 | 1.00 | 0.67 | 0.33 | 0.33 | 0.33 |
| `init-copies` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `init-scaffold` | 3 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `node-runtime-floor` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `run-unit-tests` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `source-layout` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `template-vars` | 3 | 1.00 | 1.00 | 1.00 | 0.67 | 0.33 |
| `types-alignment` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `upgrade-preserves` | 3 | 1.00 | 0.33 | 0.67 | 0.67 | 0.00 |
| `utils-fs` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-validation` | 2 | 1.00 | 1.00 | 1.00 | 1.00 | 0.50 |
| **mean** |  | **0.83** | **0.76** | **0.73** | **0.69** | **0.64** |

Requests per query, the same order: what each variant asked of the API, where the split shows up.

| Query | Gold | what ships (default) | no headings | no leads_to | one hop | before #46 |
| --- | --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 1 | 1 | 1 | 1 | 1 |
| `wiki-log` | 2 | 9 | 7 | 4 | 2 | 3 |
| `raw-immutable` | 2 | 2 | 2 | 2 | 2 | 2 |
| `release-and-publish` | 2 | 6 | 5 | 6 | 5 | 5 |
| `what-is-it` | 3 | 11 | 11 | 11 | 11 | 11 |
| `cli-dispatch` | 1 | 5 | 3 | 2 | 2 | 2 |
| `compiled-cli-tests` | 2 | 11 | 11 | 6 | 3 | 4 |
| `index-tables` | 2 | 8 | 6 | 1 | 1 | 1 |
| `ingest-summary` | 3 | 10 | 8 | 5 | 2 | 4 |
| `init-copies` | 2 | 11 | 11 | 9 | 9 | 7 |
| `init-scaffold` | 3 | 11 | 11 | 11 | 11 | 11 |
| `node-runtime-floor` | 1 | 11 | 11 | 3 | 5 | 2 |
| `run-unit-tests` | 1 | 11 | 11 | 6 | 4 | 3 |
| `source-layout` | 1 | 11 | 11 | 11 | 4 | 4 |
| `template-vars` | 3 | 11 | 11 | 9 | 7 | 5 |
| `types-alignment` | 1 | 11 | 9 | 3 | 3 | 3 |
| `upgrade-preserves` | 3 | 8 | 4 | 4 | 4 | 1 |
| `utils-fs` | 2 | 11 | 10 | 6 | 5 | 4 |
| `wiki-code-sync` | 1 | 11 | 11 | 11 | 10 | 9 |
| `wiki-validation` | 2 | 11 | 11 | 7 | 6 | 3 |
| **total** |  | **181** | **165** | **118** | **97** | **85** |

## The relative judge: one Choice over a page's links

The walk as it ships follows a link on the model's own answer about that link — is following it likely to lead somewhere useful — measured against `--threshold`. The same page can be judged as one question instead: which of its links is the best next step, answered as a share per link. A share is followed where it clears a cut that moves with the page — `max(0.02, min(3 / options, 0.5))`, against the options the question actually carried, and never a page whose best option is `none` — and the walk visits at most `--beam` 8 files at each depth. The file's own Score and its section Nouls are asked exactly as the shipping judge asks them, from the same state, so the rows vary the link judgment — and, in the last one, the cut — and nothing else. `choice` describes its options from the page alone; `choice + previews` gives each option the preview the state carries. At `--max-files 10`:

| Links judged | Recall | Precision | Precision (visited) | Read (tok) | Whole (tok) | Returned | Input (tok) | Cost | Requests | Req/answer |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| noul: what ships | 0.83 | 0.26 | 0.19 | 68663 | 122023 | 129 | 1057882 | $0.044431 | 181 | 1.00 |
| choice | 0.57 | 0.38 | 0.34 | 24649 | 39239 | 50 | 395339 | $0.016604 | 115 | 1.95 |
| choice + previews | 0.62 | 0.41 | 0.39 | 29123 | 43458 | 51 | 483671 | $0.020314 | 113 | 1.95 |
| choice + previews, k=1 | 0.72 | 0.43 | 0.40 | 40504 | 59763 | 64 | 590513 | $0.024802 | 141 | 1.93 |

`Requests` is what the API was asked over the whole gold set and `Req/answer` the same over the files it judged, so 1.00 is a question set that fits one post: a row above 1.00 is the second request a page's Choice costs. A `choice` row that asks more than the row above it and reads less is the relative judge doing its job — fewer, better files — and one that recalls less is the cut closing pages the Noul would have walked through.

Recall per query, the queries the shipped walk found least first:

| Query | Gold | noul: what ships | choice | choice + previews | choice + previews, k=1 |
| --- | --- | --- | --- | --- | --- |
| `flat-entities` | 2 | 0.00 | 0.00 | 0.00 | 0.00 |
| `wiki-log` | 2 | 0.00 | 0.00 | 0.00 | 0.00 |
| `raw-immutable` | 2 | 0.50 | 0.50 | 0.50 | 0.50 |
| `release-and-publish` | 2 | 0.50 | 0.50 | 0.50 | 0.50 |
| `what-is-it` | 3 | 0.67 | 0.00 | 0.00 | 0.67 |
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
| `upgrade-preserves` | 3 | 1.00 | 0.67 | 0.67 | 1.00 |
| `utils-fs` | 2 | 1.00 | 0.50 | 1.00 | 1.00 |
| `wiki-code-sync` | 1 | 1.00 | 1.00 | 1.00 | 1.00 |
| `wiki-validation` | 2 | 1.00 | 1.00 | 1.00 | 1.00 |
| **mean** |  | **0.83** | **0.57** | **0.62** | **0.72** |

## What these numbers are not

- **The corpus is thin.** 19 pages, so a budget of 25 can hold the corpus and the wider budget stops being a ranking question. The differences between configurations here are indicative, not a tuning set; nothing in this report should be treated as more than a direction on a wiki this size.
- **The labels are one reader's.** A page that is useful and unlisted counts against precision, so precision is a lower bound and recall is only as good as the list. The wanted sets were written from the wiki's own pages, not from a task run against it.
- **A hit is not an answer.** Recall counts the files the reading list returned, not whether an agent could do the task with them: a wanted page the walk reached but that earned no place on its own is not in that list, so it counts as missed. `read` counts characters at 4, not what a tokeniser would charge.
- **One model, one day.** Jev moves its numbers between identical requests, which is why every number here comes from the committed cache: rerun without it and the rankings hold while the numbers underneath them shift (`docs/spike-notes.md`).

## The committed cache

The answers came from `eval/cache`. That directory holds one file per request the runs above made: the query, the mode, the file and its links key the entry, and the entry carries the judgment and what the call cost. It is what makes this report a thing to check rather than a claim — the same command with no key on a machine that has never asked the API reads the same answers and prints the same bytes — and it is committed here because a wiki and a gold set are not: a private wiki is measured with the same command against its own directory, and nothing about it lands in this repository.

