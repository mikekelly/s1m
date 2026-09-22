# The Trestle handbook

> A fixture, not documentation: nothing below describes software that exists.
> This page is written for `s1m`'s tests — it is over the 40,000-character cap
> a page is split at, and its last section, `Development`, opens past that cap —
> so that a live call can ask about the tail without a document that has to stay
> true being the thing measured. No page anyone reads as documentation is this
> one, and no other test reads it either.

Trestle imports a markdown vault into a hosted workspace. It reads the vault as
it lies on disk, keeps the pages' own links pointing at each other, and leaves
the vault as the only copy anyone has to edit. This handbook is what an operator
reads before a first import and returns to when one goes wrong.

## What Trestle is

Trestle is a one-way importer. A vault of markdown files goes in; pages,
attachments and a link graph come out the other side, held in a workspace you
can search and share. The vault stays where it is, in whatever editor its
authors already use, and nothing Trestle writes ever lands back in it. That is
the whole design: no round trip, no merge, no second source of truth.

The unit of the import is the page, which is one markdown file. A page carries
its own title, its own links and its own attachments, and the importer judges
each of them on its own: a page with unreadable frontmatter is reported and
skipped, and the rest of the vault still lands. An import that stops on the
first bad file is an import nobody can finish, so Trestle counts failures and
carries on, and the report at the end says which pages it left behind.

Imports are idempotent. Running the same import twice does not produce two
copies of anything, and it does not rewrite pages whose sources have not
changed. The second run reads the state directory beside the vault, compares
what each page hashes to now, and touches only what moved. A nightly import of a
vault that changed in three places costs three pages of work, whatever the vault
weighs.

Trestle is not a wiki engine. It does not resolve links at read time, keep a
history of edits, or offer a comment box. Once a page is in the workspace, the
workspace owns it until the next import; the importer's job is to decide what
the page is, not what to do with it afterwards. Teams that want to edit in the
workspace use the workspace's own tools, and Trestle simply stops paying
attention to the pages they take over.

The importer is deliberately boring about what it accepts. Markdown is markdown,
frontmatter is YAML, links are the two syntaxes every editor has agreed on for
years. A vault that reads correctly in a plain text editor imports without
surprises, and a vault that needs an extension to be understood is a vault that
will import as plain text and lose that extension's meaning.

## Installing Trestle

Trestle ships as a single binary. There is no runtime, no service to start, and
nothing to install beside it on the machine that runs the import. The binary
does the reading, the parsing and the network calls, and it exits when the
import is done.

The install is one of three shapes, depending on the machine:

| Shape | Command | When it is the right one |
| --- | --- | --- |
| Cargo | `cargo install trestle --locked` | A Rust toolchain is already there, and a pinned version matters |
| Release archive | `tar -xzf trestle-x.y.z-linux-x86_64.tar.gz` | A build machine or a container with no toolchain |
| Package manager | `brew install trestle` | A laptop whose owner does not want to think about it |

Whatever the shape, the binary lands on `PATH` as `trestle`, and it reports the
version it was built at: `trestle --version`. Pin that version where the import
is scheduled from. An import is a program that reads a vault once and writes a
report nobody reads twice, so a version that changes without anyone noticing is
a version whose changes nobody can explain.

The first command to run on any machine is `trestle doctor`. It answers three
questions in one page of output: which binary is on `PATH` and what version it
is, whether a workspace token is present and for which workspace, and whether
the vault path the caller is about to import is readable, walked and counted.
Doctor makes no network call and writes nothing. It is the command to run before
the first import and the command to run again when an import fails for a reason
that does not look like the vault's fault.

A workspace token is the only credential Trestle needs. It is read from
`TRESTLE_TOKEN`, or from the file named in `TRESTLE_TOKEN_FILE` when a scheduler
would rather not put a secret in an environment block. There is nothing else:
no user name, no workspace id, no endpoint override in the normal case. A token
is scoped to one workspace, and the import refuses to run when the vault's
state directory says it belongs to a different one, because the alternative is a
workspace quietly filling with a second copy of somebody else's pages.

Install once, then leave it alone. Trestle does not check for updates, does not
phone home for telemetry, and does not need a daemon to stay warm. Upgrading is
replacing the binary, and the state directory is forward-compatible: a newer
binary reads a state directory written by an older one and rewrites it in place,
and the older binary refuses a state directory that a newer one has touched
rather than guessing at what it does not understand.

## The vault layout it reads

A vault is a directory. Everything under it is markdown, an attachment, or a
directory that holds one of those, with one exception: the state directory,
which Trestle writes and owns. Nothing else is special.

The walk starts at the root the caller names and goes down. Directory names do
not matter, except that a few are skipped by convention and can be unskipped
with a flag. File names matter in one way only: the name of a file is the last
fallback for the title of the page it defines, used when the frontmatter says
nothing and the first heading is missing. `releases/2026-04-rollout.md` becomes
`2026 04 rollout` when nothing else claims the page's name.

There are four conventions worth knowing before the first import:

- Files and directories whose names begin with a dot are skipped. A vault that
  keeps drafts in `.scratch/` will not import them, and that is usually what its
  author wanted.
- `node_modules`, `vendor`, `target` and `dist` are skipped by default, because
  a vault that happens to live inside a repository would otherwise import the
  dependency tree it contains. `--no-vendor-skip` puts them back.
- A directory named `.trestle/` is the state directory and never part of the
  walk. It holds the manifest, the hash of every page as it was imported, and
  the log of the last run.
- A file named `README.md` inside a directory is a page like any other. It is
  not treated as a directory index, and it does not shadow the pages beside it.

Depth is unlimited and cycles through symbolic links are not followed. A symlink
that points inside the vault is resolved and the target is imported once, under
its real path; a symlink that points outside the root, or back to a directory
the walk has already entered, is reported as skipped. The alternative — walking
a link out of the vault and importing a home directory — is exactly the kind of
accident that makes an importer untrustworthy.

Case is preserved and never normalized. `Notes/Plan.md` and `notes/plan.md` are
two pages on a case-sensitive filesystem, and Trestle imports both and reports
the collision, because the workspace will hold them under one name and something
has to be said about which. Unicode in file names is fine, and the importer
normalizes nothing: the name on disk is the name in the report, byte for byte,
so a search for what `ls` showed finds it.

An empty vault is a valid vault. The import walks it, finds nothing, writes a
report that says so, and exits zero. A vault with one page and a vault with
fifty thousand are the same code path, and the walk is bounded by memory for the
manifest rather than by anything the caller has to configure.

## Page frontmatter

A page may open with a YAML block, and most do. The block is delimited by a line
of three dashes on either side, and it is read before anything else on the page.

```yaml
---
title: Rolling out a release
slug: rollouts/release
status: final
audience: support
owner: payments
updated: 2026-04-02
tags: [rollout, release, support]
aliases: [release rollout, how we roll out]
---
```

Seven keys are understood, and the rest are kept as metadata and passed through
untouched. An import that drops a key the vault's author wrote is an import that
silently loses information, so Trestle keeps what it does not understand and
reports the key the first time it meets one.

- `title` is what the workspace shows. Without it, Trestle takes the first
  heading of the page, then the file name.
- `slug` is the page's address. Two pages that claim the same slug are a
  conflict, reported and resolved in the order the walk met them.
- `status` is one of `draft`, `review`, `final` or `retired`. Anything else is
  kept as written and flagged in the report.
- `audience` and `owner` are free text, and they are what the workspace's
  filters use.
- `updated` is a date. It is not the import time; the import time is in the
  report, and the two are different questions.
- `tags` is a list. A single string is read as a one-element list, because that
  is what half the vaults in the world write.
- `aliases` is what other names this page answers to, and it is the key the
  redirect logic reads.

Values are coerced where the coercion is obvious and reported where it is not.
`updated: 2026-04-02` is a date, `updated: "2026-04-02"` is the same date, and
`updated: someday` is a string that fails the page. A frontmatter block that
does not parse fails its page and only its page: the walk reports the file and
line, skips the page, and imports the rest of the vault, because a vault of two
thousand pages whose import dies on one unclosed quote is a vault nobody
imports twice.

Frontmatter is not rewritten. Trestle reads it and never writes the file back.
An import that normalizes the vault it reads is an import that will one day
reformat somebody's quoting conventions in a commit they did not ask for.

## Links between pages

Two link syntaxes are read, and both mean the same thing: the page the author
wrote is the page the reader lands on.

```markdown
See [the rollout checklist](rollouts/checklist.md) for the order.
See [[rollouts/checklist]] for the order.
```

A link is resolved against the file that holds it, so `rollouts/checklist.md`
written in `rollouts/index.md` names `rollouts/rollouts/checklist.md`, which is
almost never what the author meant, and the report says the target is missing
rather than quietly looking somewhere else. A target starting with `/` is
resolved against the vault root, and that is how a page links to another part of
the vault without counting directories.

A `#fragment` is kept but not used for resolution. `checklist.md#rollback` names
the page `checklist.md`; whether that page has a heading called `rollback` is a
question for the workspace, not for the importer, and the importer is not in the
business of judging headings it did not write.

A link whose target does not exist is imported as a link with a missing target,
not as plain text. The page keeps saying what its author wrote, the workspace
shows the link as broken, and the report counts it. Silently flattening a link
would hide the one signal that tells a vault owner something is out of date.

The link's own text is the target page's title when the target page claims no
title itself. `[[rollouts/checklist|the rollout checklist]]` makes the target
page's title `the rollout checklist` if its frontmatter and its first heading
say nothing, and the first link to reach a title-less page wins. That is a
fallback and not a rename: the target's own frontmatter always wins when it has
any.

Backlinks are computed in the same pass, and they are what makes a vault worth
importing rather than merely exporting. Every page knows what links to it, the
workspace can show that list, and the report prints the pages nothing links to
and the pages that link to nothing, which together are usually the answer to
"why did this become a graveyard".

Anchors written as HTML are read as HTML and left alone. Trestle does not parse
the markup inside a page beyond the frontmatter, the headings and the links, and
a page that builds its own table of contents with anchor tags keeps them exactly
as written. The importer's job is the graph, not the rendering.

## Aliases and redirects

An alias is another name a page answers to. It is how a vault handles the page
someone renamed last year without breaking every link to it.

Aliases come from frontmatter, one per entry in the `aliases` list, and each
alias is registered against the page that claims it. A link whose target matches
an alias resolves to the page holding it, so `[[release rollout]]` lands on
`rollouts/release.md` when that page lists `release rollout` among its aliases.
Resolution is tried against paths first and aliases second, so a file that
happens to be named like an alias always wins over the alias.

Two pages claiming one alias is a conflict. The walk resolves it in the order it
meets the pages, keeps the first claim, and reports the second, which is the
same rule slugs use and for the same reason: an import has to be able to place
every page, and a rule that changes with the order of a directory listing is a
rule nobody can predict. The report names both files, so the fix is a one-line
edit in whichever page was wrong.

Redirects are the second half of the same idea, and they are what a renamed page
needs after the fact. A page whose frontmatter carries `redirect: old/path.md`
is imported as a redirect: the workspace shows the new page, and links to the
old path land on it. A redirect chain is followed to its end, with a depth limit
of eight, and a chain that loops is reported and broken at the point the loop
closes. Redirects do not appear in the page list; they exist so that the links
that name them keep working.

A redirect to a page that does not exist is a broken redirect, reported like a
broken link, and the page it was written on still imports. Deleting a page and
leaving the redirect behind is the common mistake, and the report is where it
surfaces: the redirect count and the missing-target count in the same table is
usually enough for someone to see what happened.

Aliases and redirects are also what makes an import repeatable across a
reorganization. Moving fifty pages into a new directory tree and importing again
leaves every link that used the old paths working, as long as the moved pages
claim their old names as aliases. Nothing about the workspace's own copy has to
change for that to be true.

## Attachments and other assets

An attachment is any file in the vault that is not markdown. Images, PDFs,
spreadsheets, exports: whatever a page links to and a person wants to open.

Attachments are imported as files, under the path they hold in the vault, and
they are deduplicated by content hash rather than by name. Two copies of the
same diagram in two directories import as one attachment, and both pages that
show it point at the same asset. A vault whose authors copy images around
instead of linking to them shrinks by whatever the duplicates weighed, and the
report prints the bytes saved, because that number is the only thanks an
importer ever gets.

A linked attachment that is missing is reported and the link is kept. The
alternative — dropping the link — would make a page that still reads correctly
in the vault read as a page with a gap in it in the workspace, and the gap is
the more confusing of the two.

Attachment paths follow the same rules as page paths: relative to the file that
links them, resolved against the vault root when they start with `/`, and
refused when they escape the root entirely. A link to `../../elsewhere/secret.png`
imports nothing and is reported, which is the behaviour a vault that shares
files with a sibling directory needs.

Size limits exist and are configurable. An attachment over the limit is
reported, skipped, and its page still imports with the link kept. The default
limit is generous — large enough for the largest sane screenshot, small enough
that a stray database dump in a vault does not turn a nightly import into a
bandwidth bill — and `--max-asset-bytes` moves it in either direction.

File types are not filtered by default. A vault may hold whatever it likes, and
an importer that quietly drops files it did not recognize would be an importer
whose report nobody can trust. The workspace may refuse a type it cannot show;
that refusal appears in the report as a skipped asset with the reason the
workspace gave, which is a better explanation than a silence.

## Keeping paths out of an import

Not everything in a vault belongs in a workspace. Drafts, personnel notes,
customer exports and the raw material of a half-finished page are all things a
vault holds and an import should not carry.

`.trestleignore` is the file that says so. It sits at the vault root, one
pattern per line, and uses the same syntax as the ignore files every developer
already knows: a bare name matches at any depth, a leading slash anchors to the
root, a trailing slash matches only directories, `!` unignores something an
earlier line ignored, and `#` starts a comment.

```gitignore
# Personnel and anything under it.
/people/
# Drafts anywhere, but keep the shared template.
drafts/
!drafts/template.md
*.local.md
```

An ignored path is never read. It is not parsed, not hashed, not sent anywhere,
and it does not appear in the link graph: a link to an ignored page is reported
as a link to a path outside the import, which is a different thing from a broken
link and is counted separately. The distinction matters when a vault keeps a
private page and a public page with the same name; ignoring the private one
leaves the public one importing normally, and nothing about the private page's
existence reaches the workspace.

Ignore rules are read once, at the start of the walk, and applied to directories
before they are entered. A directory that is ignored is not descended into at
all, which is what makes ignoring a large tree cheap rather than merely
effective.

The state directory ignores itself. So does `.git`, always, whatever the ignore
file says, because a vault that lives in a repository would otherwise import its
own history and every object in it. Everything else is the caller's decision,
and the report prints the number of paths ignored and the bytes never read, so a
rule with a typo is visible as a number that did not change.

`.s1mignore` is not this file and has nothing to do with it. Trestle reads
`.trestleignore` only.

## Incremental imports

The first import of a vault reads everything. Every import after it reads what
changed, and the difference is the whole reason a nightly import is cheap.

Change is decided by content hash, page by page. The manifest beside the vault
holds the hash of every page as it was last imported, so an import compares what
each file hashes to now against what it hashed to then. A file whose bytes are
the same is not parsed, not sent, and not written; a file whose bytes differ is
a page to import, whatever changed inside it. Timestamps are not consulted,
because a vault that is copied, restored from a backup or checked out again has
new timestamps and identical contents, and an importer that trusted mtime would
re-import all of it.

Three things make a page dirty: its own bytes changed, its frontmatter names an
attachment whose bytes changed, or a page it links to was added or removed. The
third is what keeps the link graph honest. A page whose own text never changed
still needs rewriting when the page it links to disappears, because the link
that used to resolve no longer does, and a workspace showing a link that works
is showing something the vault no longer says.

Deletions are part of the same comparison. A page in the manifest that is no
longer on disk is a page the next import removes, unless `--keep-deleted` says
otherwise, which is what a vault that moves files between machines needs while
the other machine is still syncing.

A run can also be told to look at less than the whole vault. `--since` takes a
commit, a date or a marker file and imports only what changed after it, which is
how a vault in a repository ties an import to a merge. `--only` takes a path or
a pattern and imports only what matches, and it is what a page's author runs
while they are working on that page. Both narrow the walk; neither changes what
a dirty page means.

The manifest is written at the end of a successful run, and only then. A run
that fails halfway leaves the previous manifest alone, so the next import sees
the same set of dirty pages the failed one did and tries them again. Marking
pages clean as they are imported would mean a crash silently skips whatever was
in flight, which is the failure mode that makes people stop trusting a nightly
job.

## Conflicts and how they resolve

A conflict is two pieces of the vault claiming the same thing. The importer
cannot ask anyone, so it applies a rule, and the report says which rule fired.

There are four conflicts worth knowing:

| Conflict | Rule | What the report shows |
| --- | --- | --- |
| Two pages claim one slug | The page the walk met first keeps it | Both paths, the winner marked |
| Two pages claim one alias | The first claim wins, as with slugs | Both paths and the alias |
| Two pages claim one title | No rule; titles are not unique | Both paths, as a notice |
| A page and a redirect claim one path | The page wins | The redirect is reported as shadowed |

The order the walk meets files is deterministic: directories are read in name
order, byte for byte, and files within a directory likewise. That is what makes
the first-claim rule stable across machines, filesystems and runs. A rule that
depended on the order a filesystem happened to return entries would give two
operators two different workspaces from one vault, and nobody would ever be able
to explain the difference.

Case collisions get their own line. On a filesystem where `Plan.md` and
`plan.md` are one file there is no conflict to report; where they are two, the
workspace holds one name and the report names both files. Trestle does not try
to guess which one the author meant, and it does not merge them.

Conflicts never stop an import. Every conflict has an answer, and the answer is
always the deterministic one, so the run finishes and the report carries the
work. The one thing a caller can do about a conflict is fix the vault, and the
report is written to make that a two-minute job: both paths, both lines, and the
rule that applied.

## The import report

Every run writes a report, and the report is the run's output. It goes to
stdout as text by default, to a file when `--report` names one, and to the
workspace when `--record` is set. Nothing is hidden: a number in the report is
either something the run counted or something it was told.

The report opens with what it read and what it wrote: the vault path, the
workspace, the binary's version, the started and finished times, and the counts
that matter.

```text
read       4,182 pages in 2.6s
imported     117 pages, 46 assets, 881 kB sent
unchanged  4,065 pages
skipped        2 pages, 1 asset
links      12,904 in, 41 broken, 6 redirects
```

Then come the details, grouped by what they are about and in a stable order:
pages skipped with the reason, links whose target is missing, attachments over
the size limit, ignore rules that matched nothing, and every conflict the walk
resolved. Each line names a path and, where it has one, a line number. A report
that says "3 pages failed" and stops is a report that gets a vault owner to open
the vault themselves, which is exactly the work the importer was supposed to
save them.

The report's exit code is the summary in one number: zero when every page that
could import did, two when the vault had pages that could not, and one when the
run could not reach the workspace, read the vault, or write its state. Two is
not a failure; it is a run that finished with something to say. A nightly job
that treats two as a failure will page someone about a draft with a broken
link, and a job that treats it as success will never tell anyone about it, so
the code is there for the caller to decide.

`--quiet` prints the summary line and nothing else, and `--json` prints the
whole report as one object for a machine to read. The JSON form is the one a
dashboard should use: its field names are the report's own, and it carries the
counts, the paths and the reasons the text form carries.

## Scheduling an import

Most vaults are imported by a schedule rather than by a person. Trestle is built
to be a scheduled program: it takes no input, needs no terminal, and its exit
code means something.

A cron line is usually the whole of it:

```cron
17 3 * * * /usr/local/bin/trestle import /srv/vault --quiet --report /var/log/trestle.json --json
```

Three details make a scheduled import kinder to the machine it runs on:

- `--lock` takes a file lock for the run, so a slow import is not joined by the
  next one. A second run that cannot take the lock exits zero and says so, which
  is what a schedule wants: nothing to fix, nothing to alert on.
- `--timeout` bounds the run. An import that hangs is worse than one that fails,
  because a hang holds the lock and the workspace's attention forever.
- `--retry` re-runs the workspace calls a set number of times with backoff, for
  the case where the workspace is briefly unavailable and the vault is fine.

A scheduled import should write its report somewhere that outlives the process.
The report is the only record of what a nightly run did, and a run whose report
went to a terminal nobody read is a run nobody can audit. `--report` with a path,
and a rotation rule beside it, is the whole of the advice.

Concurrency within one run is bounded by `--jobs`, which defaults to four. The
bottleneck is almost always the workspace calls rather than the disk, and four
in-flight pages is enough to keep a link fast while leaving a small machine
usable. Large vaults on fast machines do better with more; a laptop does worse.

## Backups and restoring a vault

Trestle never writes to the vault, so a vault needs no backup on Trestle's
account. The state directory is the exception, and it is small: a manifest, the
hash of each page, and the last report, all of which can be rebuilt by importing
the vault again.

Rebuilding is the recovery story. Delete `.trestle/`, run an import, and the
result is the same workspace the previous import produced — slower, because
every page is dirty, and identical otherwise. That property is deliberate:
nothing in the workspace depends on the state directory, and nothing in the
state directory is the only copy of anything.

A workspace is not a backup either, and it should not be treated as one. Pages
in the workspace are copies; the vault is the source; a page deleted from the
vault is deleted from the workspace by the next import. A team that wants a
history of what was imported should keep one by copying the vault, not by
relying on an importer to remember.

Two things are worth backing up when a machine is replaced. The first is the
token, if the replacement machine is expected to import the same workspace. The
second is `.trestle/`, if the vault is large and the first import would be
expensive; restoring it turns a full import back into the incremental one the
next run would have been. Everything else is the vault's own problem, and the
vault presumably already has an answer.

The restore path is the same as the fresh path. A new machine reads the vault,
reads or does not read the state directory, and imports. Nothing about the
import depends on where it ran last, and nothing about the workspace depends on
the machine that filled it.

## Troubleshooting an import

Most failed imports are one of six things, and the report usually says which.

**A page did not import.** The report names the path and the reason. The three
reasons are frontmatter that does not parse, a link that escapes the root, and a
file the importer was told to skip. The first is a line number; the second is a
line number; the third is the ignore rule that matched.

**A page imported and should not have.** A path the caller expected to be
ignored was not. Ignore matching is case-sensitive, a bare name matches at any
depth, and a pattern with a trailing slash matches directories only. A rule like
`notes` does not match `notes.md`, which surprises people once and never again.

**A link is broken in the workspace and fine in the vault.** The two usual
causes are a target that is ignored — a link to an ignored page is counted as
outside the import, not as broken — and a target that differs only in case. The
report distinguishes the two, and the distinction is the answer.

**Nothing imported on the second run.** Either nothing changed, which the report
says in one line, or the manifest was restored from a backup whose pages have
different hashes, which makes every page dirty again in the opposite direction.
The report's unchanged count is the number to read first.

**The run is slow.** `--jobs` is the knob. A run that is slow with a fast disk
and an idle link is usually waiting on the workspace, and a run that is slow
with a busy link is usually the vault's size rather than anything else. The
report's timing line separates reading from sending.

**The run cannot reach the workspace.** The exit code is one, the report has the
endpoint and the error, and nothing in the vault was changed. This is the one
failure that is never the vault's fault, and retrying is what `--retry` is for.

`trestle doctor` answers adjacency questions before an import: which binary,
which token, which vault, how many pages and attachments the walk would see. It
is the first command to run when a report does not explain itself, and it costs
nothing.

## Large vaults

The importer is built for vaults much larger than the ones it is usually pointed
at, and the numbers are the reason.

The walk holds one entry per file, not one page of content: forty thousand pages
is a few megabytes of paths and hashes, and the content of a page is read, sent
and released as the page's turn comes. A vault whose total size is fifty
gigabytes imports on a machine with a gigabyte of free memory, as long as the
largest single attachment fits.

Imports are concurrent up to `--jobs` and no further. Concurrency past four is
worth less than it looks: the workspace's link is the limit, and four requests
in flight already keep a link busy. `--jobs 1` is a supported and sometimes
sensible setting, and it is what a run over a metered connection should use.

The first import of a large vault is the expensive one, and it is the only one.
A vault of forty thousand pages takes minutes to an hour depending on the link;
the nightly imports after it read a few hundred pages and finish in seconds.
Anyone budgeting for Trestle should budget for the first run and then forget
about the rest.

Attachment deduplication is what keeps the second import small as well as the
first. A vault that has a diagram duplicated in ninety pages sends that diagram
once, hashes it once, and links it ninety times. The report prints the saved
bytes, and on a vault built by copying pages around, that number is usually in
the tens of megabytes.

`--resume` exists for the first import of a large vault. A run that is
interrupted writes progress into the state directory as it goes, and a resumed
run starts from the page after the last one it finished. It does not change what
a finished import produces; it only means that losing a connection an hour into
a first import does not cost the hour.

## Permissions and ownership

Trestle reads as the user it runs as, and it needs nothing more than that. No
setuid, no root, no service account with special rights. The vault directory
must be readable and traversable by that user; the state directory must be
writable by that user; nothing else is required.

The state directory is created mode `0700` and its files mode `0600`. It holds
the hash of every page in the vault, which is a fingerprint of a vault's
contents, and a file listing that is nobody else's business. A shared machine
whose `/tmp` holds a vault's manifest is a machine where one user can learn what
another user's vault contains, page by page, from the hashes.

The token deserves the same care and gets less. It is read from an environment
variable or a file, and a file should be mode `0600` and owned by the user the
import runs as. A token in a crontab is a token in a file that is world-readable
on some systems; `TRESTLE_TOKEN_FILE` exists so that the secret can live
somewhere with an owner and a mode.

Ownership in the workspace is per page, and it comes from the frontmatter's
`owner` when the vault writes one. It is a label for filtering, not an
authorization control: Trestle does not decide who may read a page, and it has
no opinion about a workspace's own permissions. A vault whose pages are
sensitive in the workspace's sense should be split, ignored, or imported into a
workspace only the right people can see.

## What leaves the vault

An import sends page text and attachment bytes to a workspace, and that is worth
being plain about. Everything the walk reads, except what the ignore rules kept
out, is sent to the endpoint named in the token's configuration, over TLS, and
held there.

Four things never leave:

- Anything an ignore rule matched. Ignored paths are not read at all, so they
  cannot be sent by accident.
- The state directory. Hashes and a manifest are local, and the workspace never
  sees them.
- Anything outside the vault root. A link that escapes the root is resolved far
  enough to report it and no further; the target is never read.
- File metadata beyond the path. Modification times, ownership and inode numbers
  are not part of an import; the path is the identity.

Nothing else about the machine is read. No environment variables beyond the
token's, no home directory contents, no git history, no editor state. The import
is a walk of one directory and a sequence of calls to one endpoint.

A vault that must not leave a machine at all is a vault Trestle cannot import,
and no flag changes that. The importer's whole job is to send what it read, and
a mode that did not send it would be an importer that computes a link graph
nobody can see.

`--dry-run` is the closest thing to a safe first look. It walks, parses, resolves
links and prints the report, and sends nothing. It is what to run on a vault
whose ignore rules were written from memory rather than from the tree.

## Upgrading a workspace

A workspace's pages belong to the importer as long as the importer keeps
importing them, and an upgrade is just a run that changes more than usual.

The version to watch is the state directory's, not the binary's. A newer binary
reading an older state directory migrates it in place on the first run: the
manifest gains the fields the new version needs with the values the old version
implies, and the run proceeds as an incremental one. The migration is one-way
and it is recorded in the directory, so an older binary that meets a newer
directory refuses to run rather than writing something the newer binary will
misread.

Page renames are the other thing an upgrade brings. A version that changes how a
title is derived makes pages whose titles it derived dirty, and the next import
rewrites them. That is a large diff in the workspace for a small change in the
binary, and it is the honest one: the pages really are different now, and a
workspace that kept the old titles would be a workspace that disagrees with the
importer that filled it.

Between two versions of the same major line, an upgrade is safe to run on a
schedule. Across a major line, read the changes first: the shapes worth breaking
are the exit codes, the report's fields and the state directory's version, and
each of those is in the release notes precisely because a caller depends on it.

A workspace can also be rebuilt from nothing at any time. Delete its pages, or
point a run at a new workspace, and import the vault again: same pages, same
links, same titles, and the next import goes back to being incremental. A
workspace is a derived artefact, and the only irreplaceable thing in this
handbook is the vault.

## Commands and flags

The commands are few, and each does one thing.

| Command | What it does |
| --- | --- |
| `trestle import <vault>` | Imports the vault into the token's workspace |
| `trestle doctor` | Prints the binary, the token, the workspace and the vault it would walk |
| `trestle report <file>` | Renders a JSON report as the text form |
| `trestle forget <path>` | Removes one page from the manifest, so the next run re-imports it |
| `trestle version` | Prints the version and the state directory's format version |

The flags worth knowing, in the order they tend to come up:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--jobs N` | 4 | Pages in flight at once |
| `--dry-run` | off | Walk, parse and report, sending nothing |
| `--only <pattern>` | none | Import only paths matching the pattern |
| `--since <marker>` | none | Import only what changed since |
| `--keep-deleted` | off | Do not remove pages the vault no longer holds |
| `--max-asset-bytes N` | 64 MiB | Skip larger attachments, with a report line |
| `--report <file>` | stdout | Where the report goes |
| `--json` | off | Print the report as one JSON object |
| `--quiet` | off | Print the summary line only |
| `--lock <file>` | none | Take a lock so two runs never overlap |
| `--timeout <seconds>` | none | Bound the run |
| `--retry N` | 0 | Re-try failed workspace calls with backoff |
| `--no-vendor-skip` | off | Do not skip `node_modules`, `vendor`, `target`, `dist` |
| `--resume` | off | Continue an interrupted run from its progress file |

Every flag has a long form and no short form. Short flags are a courtesy to
someone at a terminal and a hazard in a cron line, where `-j` and `-J` are one
keystroke apart and the difference is a throttled link. The long forms are what
the handbook documents and what the examples use.

## Frequently asked questions

**Can two vaults import into one workspace?** Yes, if they do not claim the same
paths. Each vault keeps its own state directory and its own manifest, and both
import as their own pages. Two vaults that both hold `index.md` import as one
page, whichever ran last, and the report of the later run shows the collision.

**Does Trestle delete pages?** It removes pages the vault no longer holds, which
is the point of a one-way import. `--keep-deleted` suppresses that, and a run
that only ever uses `--keep-deleted` is a run whose workspace accumulates pages
nobody can find the source of.

**Can I edit a page in the workspace?** Yes, and the next import overwrites the
edit if the vault's copy of the page is dirty. A page whose vault copy is
unchanged is left alone, so an edit to a quiet page survives — which is a
property nobody should rely on and everybody eventually notices.

**What happens to a page I rename in the vault?** It imports under its new path,
the old path stops being a page, and every link to the old path is broken unless
the renamed page claims the old name as an alias. Aliases are the answer, and
the report is where the missing ones show up.

**Is the import deterministic?** Yes, for a given vault and a given binary
version. Any two runs over identical bytes produce the same pages, the same
links and the same report modulo timings, which is what makes an import
reviewable and what makes the manifest comparable between two machines.

**What if the workspace is empty and the vault is empty?** The run imports
nothing, reports nothing, and exits zero. An empty vault is a valid vault.

**Does Trestle watch the vault?** No. There is no filesystem watcher and no
long-running mode. Something has to run the import, whether that is a person, a
schedule or a repository hook, and a product that ran itself in the background
would be a product nobody could reason about.

**Why is the report not a page in the workspace?** Because a report is about a
run and a page is about a vault. A page of reports would need its own retention
rule, its own ignore rules and its own answer to "who edited this", and the file
it writes is already readable by everything that reads files.

## Glossary

**Alias.** Another name a page answers to, from its frontmatter. Links that name
an alias land on the page that claims it.

**Attachment.** A file in the vault that is not markdown. Imported as a file and
deduplicated by content hash.

**Dirty.** A page whose bytes, attachment or links changed since the last import,
and which the next run therefore rewrites.

**Import.** One run of `trestle import`, which walks a vault, resolves its links
and writes its pages into a workspace.

**Ignore rule.** A line of `.trestleignore` that keeps a path out of the import
entirely: not read, not hashed, not sent.

**Manifest.** The state directory's record of every page as the last import left
it: path, slug, title and content hash.

**Page.** One markdown file in the vault, with its frontmatter, its headings, its
links and its attachments.

**Redirect.** A page that names another page to be shown in its place, so that a
link to a former path keeps working.

**Slug.** A page's address in the workspace, from its frontmatter and otherwise
derived from its path.

**State directory.** `.trestle/` at the vault root: the manifest, the progress
of an interrupted run, and the last report. Safe to delete.

**Title.** What the workspace shows for a page: its frontmatter's `title`, else
its first heading, else the link text that reached it, else its file name.

**Vault.** A directory of markdown files and attachments, read by Trestle and
never written to.

**Workspace.** What a vault becomes once imported: pages, attachments and a link
graph, held by the endpoint the token names.

## Development

This section is the one a live test asks about: it is the tail of the page, it
opens past the cap, and it says how to run the tests, the lints and CI.

Everything here is built with Cargo, from the repository root, and every command
below is the one CI runs. Run all of them before opening a pull request.

| Command | What it does |
| --- | --- |
| `cargo test --locked` | The whole suite: unit tests, the parser, the cache, the importer, the walk, the report renderer and the end-to-end CLI tests |
| `cargo fmt --all --check` | Formatting, verified rather than applied; `cargo fmt --all` to fix it |
| `cargo clippy --locked --all-targets -- -D warnings` | Lints, with warnings as errors, over the library, the binaries, the tests and the examples |
| `cargo build --locked` | A debug build, which is what the smoke test runs |

Write the test first. A bug fix starts with a test that fails before the fix and
passes after it, and a feature starts with the smallest test that describes what
the feature has to do; a change with no test is a change nobody can check
without reading all of it. Where a test would pin an implementation rather than
a behaviour — a field copy, a default, a forwarded argument — the right answer
is usually no test at all.

The live tests are the exception to running everything locally. They call a real
API and they skip themselves unless `TYPESAFE_API_KEY` is set, so `cargo test`
runs offline and costs nothing; with the key set they import the vendored
fixture vault and this repository's own fixture page, which is the file you are
reading. CI has no key and therefore runs the rest of the suite only.

CI runs the four commands above on the stable toolchain, then a smoke test on
the binary it built: `--help` prints usage, no arguments exits with a usage
error, and an unknown flag is refused. `cargo build --locked` comes first
because the smoke test needs a binary, and the smoke test is what catches a flag
that was documented but never registered.

The cycle for a change, in order:

1. Branch from the main branch, with the change's name in the branch.
2. Write the failing test, if the change is a bug fix, and watch it fail.
3. Make the smallest change that passes it, and nothing else.
4. Run `cargo fmt --all`, then `cargo clippy --locked --all-targets -- -D
   warnings`, then `cargo test --locked`.
5. With a key, run the live tests too: `cargo test --locked --test jev_live`.
6. Open a pull request that says what changed, what it costs and how it was
   checked: the commands, and their result.

A pull request nobody can check is a pull request that waits. Say what to run,
say what it printed, and leave the reviewer the smallest diff that does the job.
A change that fixes a symptom while leaving the cause alone will be sent back,
and the person who sends it back will be right.
