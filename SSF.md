# SSF agent guidance

<!-- Session-only rules: ssf gives this file to the main session alone.
Repository-wide policy lives in AGENTS.md. -->

- **Planning.** Clarify on the issue until the acceptance criteria are a
  ticked task list agreed by @mikekelly or the coordinator on #2. Then
  implement.
- **Decisions.** @mikekelly decides language or stack, licence, anything
  costing money beyond TypeSafe calls, and scope changes to the plan. Settle
  everything else yourself and report what you chose.
- **Posting.** Post when you start (what will be delivered), when blocked
  (what you need and from whom), and when delivered (acceptance table, links
  pinned to the commit). Otherwise only when silence would mislead.
- **Review.** Docs- and test-only changes: self-review. Behaviour changes: one
  independent review by a fresh subagent of the pinned diff, at most one
  follow-up round, then simplify.
- **Merging.** Open PRs with `Closes #N`. The coordinator on #2, or
  @mikekelly, merges green PRs that meet acceptance. A session does not merge
  its own PR.
- **Board.** [s1m v1](https://github.com/users/mikekelly/projects/11):
  `In progress` while working, `In review` once the PR is up, `Done` after
  merge.
- **Orchestration.** Keep the main session for planning, the issue thread and
  integration; push execution to subagents.
- **Delivered.** Merged, and the issue closed.

| Harness | Deliberation | Execution |
| --- | --- | --- |
| `omp` | inherits the session | `deepseek/deepseek-flash`, effort `high` |
| `claude` | `claude-fable-5-1`, effort `low` | `claude-opus-5`, effort `medium` |
