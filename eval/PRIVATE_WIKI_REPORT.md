# Evaluation: a reading list against an agent that explores

s1m hands an agent a ranked reading list; a Claude Code Explore agent opens the wiki and looks. This is what each is worth on one wiki, in numbers only: the wiki, the queries and the pages they name stay out of this file, so it can be published about a wiki that cannot be.

## The wiki

| | |
| --- | --- |
| Pages | 1933 |
| Words | 912979 |
| Headings | 18133 |
| Links | 10673 (10673 markdown, 0 wikilink) |
| Link targets | 10250 inside the wiki, 338 broken, 85 outside it |
| Edges | 8802 |
| Out-degree (min/median/mean/max) | 0 / 3.0 / 4.6 / 378 |
| In-degree (min/median/mean/max) | 0 / 3.0 / 4.6 / 67 |
| Out-degree histogram | 0: 141, 1: 336, 2: 429, 3-4: 490, 5-9: 398, 10-19: 91, 20-49: 35, 50+: 13 |
| In-degree histogram | 0: 70, 1: 406, 2: 353, 3-4: 499, 5-9: 412, 10-19: 141, 20-49: 46, 50+: 6 |
| Orphans | 70 |
| Entry page | given: 8 pages, the landing page and the section hubs |
| Depth from the entry page | 0: 8, 1: 168, 2: 497, 3: 927, 4: 204, 5: 7, 6: 2 |
| Unreachable from the entry page | 120 |
| Largest strongly connected component | 1477 |

## Method

20 queries, 3 repeat(s) per condition. A run is one query under one condition; recall and precision are against the gold set's wanted pages, and a run that errored is counted and left out of every average.

| | |
| --- | --- |
| Conditions | explore, s1m, s1m-agent, s1m-t0.4 |
| Agent model | `sonnet` |
| Agent flags | `-p` `--output-format stream-json` `--verbose` `--safe-mode` `--tools Read,Glob,Grep,Task` `--allowedTools Read,Glob,Grep,Task` `--permission-prompts none` |
| s1m flags | `--format json` `--root .` |
| Agent tokens | returned characters at 4 a token |

The Explore agent is dispatched by the parent, and on this build it inherits the session's model rather than declaring one of its own: with no model named it takes whatever Claude Code would have used, and with one named it takes that. What answered is recorded per run.

The agent was asked, with the query and the entry page substituted in:

> Use the Explore agent to answer this query from the wiki in the current directory, starting at these pages: <entry>
> The query: <query>
> Reply with the answer, and then a JSON array of the relative paths of the files the Explore agent relied on.

The `s1m-agent` condition was asked, with the reading list substituted in:

> Here is a reading list for a query, from the wiki in the current directory, most relevant first:
> <files>
> Answer this query, opening only the files you need: <query>
> Reply with the answer, and then a JSON array of the relative paths of the files you relied on.

## Results

| Condition | Runs | Recall | Precision | Agent tokens | Billed tokens | Files opened | Cost (USD) | Wall (ms) | Jev input tokens | Jev cost (USD) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `explore` | 60 | 0.85 ± 0.21 | 0.17 ± 0.09 | 310212 ± 184001 | 315892 ± 186102 | 15.28 ± 6.56 | 0.29 ± 0.11 | 77972 ± 32862 | — | — |
| `s1m` | 60 | 0.59 ± 0.39 | 0.07 ± 0.05 | 9503 ± 8453 | — | 21.75 ± 4.43 | 0.000006 ± 0.000043 | 203 ± 25.93 | 131 ± 1017 | 0.000006 ± 0.000043 |
| `s1m-agent` | 60 | 0.43 ± 0.35 | 0.42 ± 0.37 | 67208 ± 66156 | 68877 ± 67286 | 3.95 ± 2.54 | 0.09 ± 0.07 | 19764 ± 13312 | — | — |
| `s1m-cold` | 20 | 0.59 ± 0.40 | 0.07 ± 0.05 | 9503 ± 8600 | — | 21.75 ± 4.51 | 0.02 ± 0.006367 | 1481 ± 418 | 407036 ± 151596 | 0.02 ± 0.006367 |
| `s1m-t0.4` | 60 | 0.62 ± 0.37 | 0.07 ± 0.04 | 17099 ± 8242 | — | 25.00 ± 0.00 | 0.001107 ± 0.002613 | 327 ± 250 | 26346 ± 62214 | 0.001107 ± 0.002613 |

Every cell is the mean over the runs, with the sample standard deviation after it where there was more than one run.

## What the Explore condition measured

This baseline was run before the harness forced delegation. The parent `claude -p` session was given `Read`, `Glob`, `Grep` and `Task` and asked to use the Explore subagent; every one of the 60 runs did dispatch exactly one `Explore` subagent, and the "Agent tokens" column is that subagent's own usage from its transcript. But the parent also read the wiki itself, about 20 `Read`/`Grep`/`Glob` calls a run (918, 153 and 108 over the 60 runs), so "Billed tokens" and "Cost" include a parent that explored too. The subagent ran on `claude-sonnet-5`, inherited from the parent's `--model sonnet`: on Claude Code 2.1.278 in `-p` mode the built-in Explore agent declares no model of its own and takes the session's. The harness now blocks the parent's reads with a PreToolUse hook and passes an explicit thoroughness, so a rerun would measure the subagent alone; this run is kept as the baseline as it was measured.

## Reading the numbers

The Explore agent finds more and reads far more. Its recall is 0.85 against 0.59 for s1m's reading list at defaults, and it gets there by reading 310k tokens a query at $0.29 and 78 s, against s1m's 9.5k tokens of returned ranges at $0.02 cold and 1.5 s, or 0.2 s warm. Handing that list to an agent (`s1m-agent`) costs a third of Explore's tokens and money and a quarter of its time, and answers with much higher precision (0.42 against 0.17), but its recall falls to 0.43: the agent trusts the list, and on the 5 queries where the walk missed every wanted page (recall 0 for `s1m`) it has nothing to recover from, while Explore greps its way there. Lowering the threshold to 0.4 buys little (recall 0.62, twice the ranges). On this wiki the link scent from the hub pages is the limit, not the budget: the walk stops at 20 to 25 files and the wanted pages sit two or three hops down. Where the list is right it is by far the cheapest route to the answer; where it is wrong, nothing downstream notices.

## Where the private material lives

The wiki clone, the gold set, every raw run row, the agent transcripts and the s1m cache for these runs are under `~/private-eval/` on the machine that ran them (`wiki/`, `gold.json`, `out/`, `cache/`), outside any checkout. The 78 agent runs from the eighth query onward were re-run after a Claude usage-limit outage returned no output; the failed rows were dropped before the resume, so every run above completed. The three hand-written sections are this one and the two above; `eval-agent report` regenerates the rest from `out/aggregates.json` and `out/graph_stats.json`.

## Per query

| Query | Category | Condition | Recall | Precision | Agent tokens | Billed tokens | Files opened | Cost (USD) | Wall (ms) | Jev input tokens | Jev cost (USD) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `Q01` | fact-lookup | `explore` | 0.44 ± 0.19 | 0.10 ± 0.04 | 165491 ± 28925 | 169182 ± 29034 | 13.00 ± 1.00 | 0.22 ± 0.008499 | 87281 ± 58800 | — | — |
| `Q01` | fact-lookup | `s1m` | 1.00 ± 0.00 | 0.15 ± 0.00 | 10972 ± 0.00 | — | 20.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q01` | fact-lookup | `s1m-agent` | 0.67 ± 0.00 | 1.00 ± 0.00 | 19753 ± 15.59 | 20690 ± 86.90 | 2.00 ± 0.00 | 0.03 ± 0.01 | 12541 ± 902 | — | — |
| `Q01` | fact-lookup | `s1m-cold` | 1.00 | 0.15 | 10972 | — | 20.00 | 0.01 | 1402 | 290773 | 0.01 |
| `Q01` | fact-lookup | `s1m-t0.4` | 1.00 ± 0.00 | 0.12 ± 0.00 | 16575 ± 0.00 | — | 25.00 ± 0.00 | 0.000716 ± 0.001240 | 400 ± 346 | 17050 ± 29531 | 0.000716 ± 0.001240 |
| `Q02` | task-oriented | `explore` | 1.00 ± 0.00 | 0.40 ± 0.12 | 137010 ± 4017 | 140813 ± 3392 | 8.00 ± 2.65 | 0.19 ± 0.009630 | 51505 ± 6821 | — | — |
| `Q02` | task-oriented | `s1m` | 1.00 ± 0.00 | 0.12 ± 0.00 | 12309 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q02` | task-oriented | `s1m-agent` | 0.67 ± 0.00 | 1.00 ± 0.00 | 22866 ± 1.53 | 23597 ± 32.23 | 2.00 ± 0.00 | 0.05 ± 0.002752 | 8806 ± 722 | — | — |
| `Q02` | task-oriented | `s1m-cold` | 1.00 | 0.12 | 12309 | — | 25.00 | 0.02 | 1401 | 385603 | 0.02 |
| `Q02` | task-oriented | `s1m-t0.4` | 1.00 ± 0.00 | 0.12 ± 0.00 | 18078 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q03` | fact-lookup | `explore` | 1.00 ± 0.00 | 0.15 ± 0.04 | 210620 ± 51729 | 214722 ± 52052 | 12.00 ± 4.36 | 0.22 ± 0.03 | 54566 ± 4641 | — | — |
| `Q03` | fact-lookup | `s1m` | 0.00 ± 0.00 | 0.00 ± 0.00 | 201 ± 0.00 | — | 18.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q03` | fact-lookup | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 153656 ± 29339 | 157579 ± 29440 | 6.33 ± 0.58 | 0.19 ± 0.007300 | 44575 ± 8694 | — | — |
| `Q03` | fact-lookup | `s1m-cold` | 0.00 | 0.00 | 201 | — | 18.00 | 0.04 | 1402 | 913130 | 0.04 |
| `Q03` | fact-lookup | `s1m-t0.4` | 0.00 ± 0.00 | 0.00 ± 0.00 | 6358 ± 0.00 | — | 25.00 ± 0.00 | 0.000751 ± 0.001301 | 334 ± 232 | 17885 ± 30978 | 0.000751 ± 0.001301 |
| `Q04` | fact-lookup | `explore` | 1.00 ± 0.00 | 0.11 ± 0.02 | 354758 ± 40547 | 359636 ± 40888 | 19.00 ± 4.00 | 0.29 ± 0.02 | 63438 ± 3938 | — | — |
| `Q04` | fact-lookup | `s1m` | 1.00 ± 0.00 | 0.08 ± 0.00 | 10292 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q04` | fact-lookup | `s1m-agent` | 0.83 ± 0.29 | 0.56 ± 0.19 | 27841 ± 9301 | 28835 ± 9494 | 3.00 ± 0.00 | 0.05 ± 0.006246 | 12741 ± 4349 | — | — |
| `Q04` | fact-lookup | `s1m-cold` | 1.00 | 0.08 | 10292 | — | 25.00 | 0.01 | 1601 | 353325 | 0.01 |
| `Q04` | fact-lookup | `s1m-t0.4` | 1.00 ± 0.00 | 0.08 ± 0.00 | 20604 ± 0.00 | — | 25.00 ± 0.00 | 0.002201 ± 0.003813 | 401 ± 349 | 52412 ± 90780 | 0.002201 ± 0.003813 |
| `Q05` | fact-lookup | `explore` | 1.00 ± 0.00 | 0.15 ± 0.02 | 663183 ± 103115 | 670920 ± 103020 | 18.67 ± 0.58 | 0.44 ± 0.02 | 98460 ± 15719 | — | — |
| `Q05` | fact-lookup | `s1m` | 0.67 ± 0.00 | 0.12 ± 0.00 | 1757 ± 0.00 | — | 17.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q05` | fact-lookup | `s1m-agent` | 0.67 ± 0.00 | 1.00 ± 0.00 | 32814 ± 53.23 | 33996 ± 43.21 | 2.00 ± 0.00 | 0.05 ± 0.002227 | 12942 ± 1303 | — | — |
| `Q05` | fact-lookup | `s1m-cold` | 0.67 | 0.12 | 1757 | — | 17.00 | 0.01 | 1201 | 262453 | 0.01 |
| `Q05` | fact-lookup | `s1m-t0.4` | 0.67 ± 0.00 | 0.08 ± 0.00 | 6922 ± 0.00 | — | 25.00 ± 0.00 | 0.002943 ± 0.005098 | 467 ± 462 | 70075 ± 121373 | 0.002943 ± 0.005098 |
| `Q06` | task-oriented | `explore` | 0.67 ± 0.14 | 0.11 ± 0.03 | 565064 ± 179242 | 573788 ± 179755 | 23.33 ± 4.16 | 0.44 ± 0.05 | 115485 ± 2818 | — | — |
| `Q06` | task-oriented | `s1m` | 0.75 ± 0.00 | 0.12 ± 0.00 | 17830 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q06` | task-oriented | `s1m-agent` | 0.50 ± 0.00 | 0.40 ± 0.000000 | 47955 ± 15.37 | 49671 ± 16.86 | 5.00 ± 0.00 | 0.09 ± 0.002484 | 17278 ± 417 | — | — |
| `Q06` | task-oriented | `s1m-cold` | 0.75 | 0.12 | 17830 | — | 25.00 | 0.02 | 1601 | 374474 | 0.02 |
| `Q06` | task-oriented | `s1m-t0.4` | 0.75 ± 0.00 | 0.12 ± 0.00 | 21058 ± 0.00 | — | 25.00 ± 0.00 | 0.000499 ± 0.000864 | 333 ± 231 | 11883 ± 20582 | 0.000499 ± 0.000864 |
| `Q07` | task-oriented | `explore` | 1.00 ± 0.00 | 0.14 ± 0.02 | 348263 ± 135515 | 353668 ± 135903 | 14.33 ± 2.52 | 0.32 ± 0.05 | 75846 ± 6971 | — | — |
| `Q07` | task-oriented | `s1m` | 1.00 ± 0.00 | 0.08 ± 0.00 | 17014 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q07` | task-oriented | `s1m-agent` | 1.00 ± 0.00 | 0.89 ± 0.19 | 39165 ± 13.32 | 40452 ± 120 | 3.00 ± 0.00 | 0.06 ± 0.001312 | 16477 ± 2732 | — | — |
| `Q07` | task-oriented | `s1m-cold` | 1.00 | 0.08 | 17014 | — | 25.00 | 0.02 | 1401 | 510210 | 0.02 |
| `Q07` | task-oriented | `s1m-t0.4` | 1.00 ± 0.00 | 0.08 ± 0.00 | 24923 ± 0.00 | — | 25.00 ± 0.00 | 0.000154 ± 0.000267 | 267 ± 115 | 3672 ± 6360 | 0.000154 ± 0.000267 |
| `Q08` | task-oriented | `explore` | 0.89 ± 0.19 | 0.14 ± 0.02 | 326364 ± 122101 | 333889 ± 123486 | 19.00 ± 6.08 | 0.34 ± 0.07 | 89521 ± 19349 | — | — |
| `Q08` | task-oriented | `s1m` | 0.00 ± 0.00 | 0.00 ± 0.00 | 201 ± 0.00 | — | 25.00 ± 0.00 | 0.000110 ± 0.000191 | 267 ± 116 | 2626 ± 4548 | 0.000110 ± 0.000191 |
| `Q08` | task-oriented | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 177278 ± 54387 | 181163 ± 55029 | 9.00 ± 1.00 | 0.21 ± 0.04 | 43693 ± 13078 | — | — |
| `Q08` | task-oriented | `s1m-cold` | 0.00 | 0.00 | 201 | — | 25.00 | 0.02 | 1401 | 486362 | 0.02 |
| `Q08` | task-oriented | `s1m-t0.4` | 0.00 ± 0.00 | 0.00 ± 0.00 | 12243 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q09` | task-oriented | `explore` | 0.78 ± 0.19 | 0.15 ± 0.03 | 195482 ± 54950 | 201335 ± 55457 | 13.67 ± 5.77 | 0.27 ± 0.04 | 73779 ± 7233 | — | — |
| `Q09` | task-oriented | `s1m` | 0.67 ± 0.00 | 0.12 ± 0.00 | 8716 ± 0.00 | — | 16.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q09` | task-oriented | `s1m-agent` | 0.67 ± 0.00 | 0.67 ± 0.00 | 39158 ± 16.65 | 40492 ± 80.67 | 3.00 ± 0.00 | 0.07 ± 0.001821 | 16677 ± 3421 | — | — |
| `Q09` | task-oriented | `s1m-cold` | 0.67 | 0.12 | 8716 | — | 16.00 | 0.01 | 1201 | 264213 | 0.01 |
| `Q09` | task-oriented | `s1m-t0.4` | 0.67 ± 0.00 | 0.08 ± 0.00 | 15397 ± 0.00 | — | 25.00 ± 0.00 | 0.004221 ± 0.007311 | 467 ± 462 | 100502 ± 174075 | 0.004221 ± 0.007311 |
| `Q10` | task-oriented | `explore` | 1.00 ± 0.00 | 0.09 ± 0.01 | 379395 ± 7090 | 385559 ± 6244 | 23.33 ± 3.51 | 0.37 ± 0.04 | 77316 ± 8704 | — | — |
| `Q10` | task-oriented | `s1m` | 0.50 ± 0.00 | 0.04 ± 0.00 | 24722 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q10` | task-oriented | `s1m-agent` | 0.83 ± 0.29 | 0.21 ± 0.06 | 139063 ± 32091 | 141533 ± 32055 | 7.33 ± 0.58 | 0.19 ± 0.02 | 29620 ± 5636 | — | — |
| `Q10` | task-oriented | `s1m-cold` | 0.50 | 0.04 | 24722 | — | 25.00 | 0.02 | 1401 | 464165 | 0.02 |
| `Q10` | task-oriented | `s1m-t0.4` | 0.50 ± 0.00 | 0.04 ± 0.00 | 28845 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q11` | task-oriented | `explore` | 1.00 ± 0.00 | 0.14 ± 0.03 | 432874 ± 106810 | 440323 ± 106748 | 22.67 ± 4.04 | 0.41 ± 0.005393 | 94064 ± 5192 | — | — |
| `Q11` | task-oriented | `s1m` | 1.00 ± 0.00 | 0.12 ± 0.00 | 29231 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q11` | task-oriented | `s1m-agent` | 0.78 ± 0.19 | 0.64 ± 0.04 | 39555 ± 19498 | 41084 ± 19586 | 3.67 ± 1.15 | 0.09 ± 0.02 | 16677 ± 1272 | — | — |
| `Q11` | task-oriented | `s1m-cold` | 1.00 | 0.12 | 29231 | — | 25.00 | 0.02 | 1601 | 459153 | 0.02 |
| `Q11` | task-oriented | `s1m-t0.4` | 1.00 ± 0.00 | 0.12 ± 0.00 | 35269 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q12` | fact-lookup | `explore` | 0.83 ± 0.14 | 0.34 ± 0.14 | 118597 ± 50809 | 122325 ± 52234 | 10.00 ± 3.46 | 0.18 ± 0.04 | 52658 ± 15846 | — | — |
| `Q12` | fact-lookup | `s1m` | 0.00 ± 0.00 | 0.00 ± 0.00 | 350 ± 0.00 | — | 11.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q12` | fact-lookup | `s1m-agent` | 0.25 ± 0.00 | 0.33 ± 0.00 | 66543 ± 74.70 | 67608 ± 123 | 4.00 ± 0.00 | 0.09 ± 0.001396 | 15743 ± 2470 | — | — |
| `Q12` | fact-lookup | `s1m-cold` | 0.00 | 0.00 | 350 | — | 11.00 | 0.008759 | 801 | 208537 | 0.008759 |
| `Q12` | fact-lookup | `s1m-t0.4` | 0.75 ± 0.00 | 0.12 ± 0.00 | 17507 ± 0.00 | — | 25.00 ± 0.00 | 0.001822 ± 0.003157 | 467 ± 462 | 43391 ± 75155 | 0.001822 ± 0.003157 |
| `Q13` | task-oriented | `explore` | 0.78 ± 0.19 | 0.26 ± 0.07 | 159085 ± 46060 | 162059 ± 46294 | 10.00 ± 4.36 | 0.18 ± 0.02 | 43760 ± 8928 | — | — |
| `Q13` | task-oriented | `s1m` | 0.00 ± 0.00 | 0.00 ± 0.00 | 3422 ± 0.00 | — | 19.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q13` | task-oriented | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 23973 ± 10010 | 24452 ± 10225 | 1.33 ± 0.58 | 0.04 ± 0.02 | 8205 ± 1562 | — | — |
| `Q13` | task-oriented | `s1m-cold` | 0.00 | 0.00 | 3422 | — | 19.00 | 0.02 | 1201 | 437279 | 0.02 |
| `Q13` | task-oriented | `s1m-t0.4` | 0.00 ± 0.00 | 0.00 ± 0.00 | 11017 ± 0.00 | — | 25.00 ± 0.00 | 0.000877 ± 0.001520 | 334 ± 231 | 20891 ± 36184 | 0.000877 ± 0.001520 |
| `Q14` | task-oriented | `explore` | 1.00 ± 0.00 | 0.23 ± 0.06 | 338444 ± 153699 | 343993 ± 155270 | 13.33 ± 2.89 | 0.29 ± 0.06 | 77915 ± 27838 | — | — |
| `Q14` | task-oriented | `s1m` | 0.00 ± 0.00 | 0.00 ± 0.00 | 7133 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 201 ± 0.58 | — | 0.00 ± 0.00 |
| `Q14` | task-oriented | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 250549 ± 17419 | 255035 ± 18809 | 8.67 ± 1.15 | 0.24 ± 0.03 | 49566 ± 14316 | — | — |
| `Q14` | task-oriented | `s1m-cold` | 0.00 | 0.00 | 7133 | — | 25.00 | 0.02 | 1401 | 485010 | 0.02 |
| `Q14` | task-oriented | `s1m-t0.4` | 0.00 ± 0.00 | 0.00 ± 0.00 | 15532 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q15` | task-oriented | `explore` | 0.83 ± 0.29 | 0.16 ± 0.04 | 196726 ± 38653 | 199070 ± 39249 | 10.67 ± 5.03 | 0.18 ± 0.03 | 35994 ± 4770 | — | — |
| `Q15` | task-oriented | `s1m` | 0.50 ± 0.00 | 0.04 ± 0.00 | 1474 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q15` | task-oriented | `s1m-agent` | 0.50 ± 0.00 | 0.67 ± 0.29 | 16762 ± 716 | 17235 ± 748 | 2.00 ± 0.00 | 0.02 ± 0.005942 | 6071 ± 809 | — | — |
| `Q15` | task-oriented | `s1m-cold` | 0.50 | 0.04 | 1474 | — | 25.00 | 0.01 | 3002 | 338529 | 0.01 |
| `Q15` | task-oriented | `s1m-t0.4` | 0.50 ± 0.00 | 0.04 ± 0.00 | 8888 ± 0.00 | — | 25.00 ± 0.00 | 0.002271 ± 0.003934 | 400 ± 347 | 54072 ± 93655 | 0.002271 ± 0.003934 |
| `Q16` | fact-lookup | `explore` | 0.50 ± 0.00 | 0.17 ± 0.03 | 123361 ± 7961 | 126354 ± 8309 | 6.00 ± 1.00 | 0.18 ± 0.007359 | 67726 ± 40692 | — | — |
| `Q16` | fact-lookup | `s1m` | 1.00 ± 0.00 | 0.08 ± 0.00 | 4593 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q16` | fact-lookup | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 32465 ± 50.01 | 33500 ± 71.02 | 2.00 ± 0.00 | 0.04 ± 0.01 | 12475 ± 114 | — | — |
| `Q16` | fact-lookup | `s1m-cold` | 1.00 | 0.08 | 4593 | — | 25.00 | 0.01 | 1801 | 324356 | 0.01 |
| `Q16` | fact-lookup | `s1m-t0.4` | 1.00 ± 0.00 | 0.08 ± 0.00 | 3279 ± 0.00 | — | 25.00 ± 0.00 | 0.002321 ± 0.004020 | 400 ± 347 | 55258 ± 95709 | 0.002321 ± 0.004020 |
| `Q17` | task-oriented | `explore` | 0.67 ± 0.29 | 0.19 ± 0.09 | 120229 ± 58507 | 123286 ± 58472 | 8.00 ± 4.00 | 0.17 ± 0.05 | 48165 ± 12332 | — | — |
| `Q17` | task-oriented | `s1m` | 1.00 ± 0.00 | 0.14 ± 0.00 | 5384 ± 0.00 | — | 14.00 ± 0.00 | 0.00 ± 0.00 | 201 ± 0.58 | — | 0.00 ± 0.00 |
| `Q17` | task-oriented | `s1m-agent` | 0.00 ± 0.00 | 0.00 ± 0.00 | 17208 ± 9.02 | 17876 ± 37.45 | 1.00 ± 0.00 | 0.03 ± 0.002157 | 11007 ± 4362 | — | — |
| `Q17` | task-oriented | `s1m-cold` | 1.00 | 0.14 | 5384 | — | 14.00 | 0.01 | 1601 | 243871 | 0.01 |
| `Q17` | task-oriented | `s1m-t0.4` | 1.00 ± 0.00 | 0.08 ± 0.00 | 11991 ± 0.00 | — | 25.00 ± 0.00 | 0.001757 ± 0.003043 | 401 ± 347 | 41832 ± 72455 | 0.001757 ± 0.003043 |
| `Q18` | task-oriented | `explore` | 0.89 ± 0.19 | 0.17 ± 0.08 | 316140 ± 70858 | 323971 ± 72459 | 19.00 ± 2.65 | 0.34 ± 0.05 | 92457 ± 19803 | — | — |
| `Q18` | task-oriented | `s1m` | 0.67 ± 0.00 | 0.08 ± 0.00 | 16703 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q18` | task-oriented | `s1m-agent` | 0.56 ± 0.19 | 0.50 ± 0.17 | 43472 ± 5026 | 45237 ± 5038 | 3.33 ± 0.58 | 0.07 ± 0.01 | 20547 ± 2533 | — | — |
| `Q18` | task-oriented | `s1m-cold` | 0.67 | 0.08 | 16703 | — | 25.00 | 0.02 | 1602 | 518840 | 0.02 |
| `Q18` | task-oriented | `s1m-t0.4` | 0.67 ± 0.00 | 0.08 ± 0.00 | 27591 ± 0.00 | — | 25.00 ± 0.00 | 0.000007 ± 0.000012 | 267 ± 115 | 159 ± 276 | 0.000007 ± 0.000012 |
| `Q19` | task-oriented | `explore` | 1.00 ± 0.00 | 0.16 ± 0.06 | 406753 ± 135773 | 415045 ± 136710 | 16.67 ± 5.86 | 0.36 ± 0.07 | 103531 ± 16633 | — | — |
| `Q19` | task-oriented | `s1m` | 0.67 ± 0.00 | 0.08 ± 0.00 | 16939 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.58 | — | 0.00 ± 0.00 |
| `Q19` | task-oriented | `s1m-agent` | 0.33 ± 0.00 | 0.44 ± 0.10 | 26616 ± 11155 | 27499 ± 11255 | 2.33 ± 0.58 | 0.05 ± 0.02 | 11074 ± 306 | — | — |
| `Q19` | task-oriented | `s1m-cold` | 0.67 | 0.08 | 16939 | — | 25.00 | 0.02 | 1401 | 435763 | 0.02 |
| `Q19` | task-oriented | `s1m-t0.4` | 0.67 ± 0.00 | 0.08 ± 0.00 | 26339 ± 0.00 | — | 25.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q20` | task-oriented | `explore` | 0.78 ± 0.19 | 0.10 ± 0.03 | 646404 ± 125528 | 657901 ± 126654 | 25.00 ± 6.56 | 0.49 ± 0.07 | 155975 ± 30404 | — | — |
| `Q20` | task-oriented | `s1m` | 0.33 ± 0.00 | 0.05 ± 0.000000 | 819 ± 0.00 | — | 20.00 ± 0.00 | 0.00 ± 0.00 | 200 ± 0.00 | — | 0.00 ± 0.00 |
| `Q20` | task-oriented | `s1m-agent` | 0.33 ± 0.00 | 0.13 ± 0.02 | 127474 ± 15963 | 130007 ± 16167 | 8.00 ± 1.00 | 0.14 ± 0.01 | 28557 ± 4353 | — | — |
| `Q20` | task-oriented | `s1m-cold` | 0.33 | 0.05 | 819 | — | 20.00 | 0.02 | 1201 | 384681 | 0.02 |
| `Q20` | task-oriented | `s1m-t0.4` | 0.33 ± 0.00 | 0.04 ± 0.00 | 13569 ± 0.00 | — | 25.00 ± 0.00 | 0.001589 ± 0.002753 | 400 ± 347 | 37838 ± 65537 | 0.001589 ± 0.002753 |

The query column is the gold set's id for the query, and the category its label: the question itself, the pages it wanted and the files any run opened are in the raw rows under the run directory, which is not committed.

## Caveats

- Recall and precision are against one gold set, written by reading the wiki. A page that is useful and unlisted costs precision, so precision is a lower bound.
- The Explore condition's parent is stopped from exploring by a hook that refuses its own reads, so that what is measured is the subagent. *Parent tools* counts the parent's tool calls and *Parent blocked* the ones the hook refused; a parent that read anything the hook let through would be work counted as the Explore agent's.
- The Explore condition is scored on the files the agent said it relied on. The files it actually opened are counted separately, and the two are not the same set: an agent reads more than it cites.
- The agent's tokens are what Claude Code billed the subagent for, summed over its turns from its own transcript, cache reads and cache writes included. The parent agent's tokens are reported beside them and are not part of the subagent's figure.
- Cost is what the CLI priced the whole run at, parent included. A subagent's share of it is apportioned by tokens, which is an estimate and not a price.
- Jev's input tokens are of the same order as an exploring agent's: it reads every page the walk visits whole, plus a preview of each of that page's links. What differs is the price of a token and the cache — a judgment already bought is not bought again, and a warm run reads nothing at all.
- s1m's tokens are the characters in the ranges it returned, counted at the rate in the method table. Nothing here tokenises, and an agent that opens a returned file whole reads more than that.
- The agent token columns and s1m's are not the same quantity. An agent's are what it was billed for, which includes its system prompt, its tool definitions and every tool result it read; s1m's are the characters of wiki text it asked for. The comparison that puts them on one footing is `s1m-agent` against `explore`.
- The `s1m-agent` condition's cost is the agent's plus what the reading list it was handed cost to buy, so the two agent conditions are priced on the same footing. A warm s1m run has bought nothing and adds nothing; the cold run is where a list's price shows.
- An agent that answered without the list of files it was asked for is counted as unparsed beside the failure count. Those runs are still scored, and they score zero, so a condition with unparsed answers is reading lower than it looked.
- Wall time is one machine on one network, and the agent condition depends on a service whose latency is not ours.
- The two conditions are not the same shape of work: s1m returns a reading list and answers nothing, the agent reads until it can answer. The `s1m-agent` condition is the one that compares like with like.
