---
title: Agent docs corpus specification
answers: What shape must an agent-docs corpus have, and what does the refresh job execute?
last_verified: 2026-08-12
verified_ref: 8e39a11315713d53e1e190e3a30f876f75511189
refresh: quarterly
---

# Agent docs corpus specification

This file defines the corpus format. It is repository-independent. Copy it into any
repository that adopts `docs/agents/`.

The corpus exists so an agent can reason about a large codebase without reading all of
it. That only works if a reader can tell what the corpus knows from what it merely
repeats. A corpus that blurs the two is worse than no corpus, because it is confidently
wrong and the reader trusts it.

## The one rule

Every claim states how it is known. The marker is part of the claim, not decoration.

A refresh job re-runs the executable claims each week. Prose that no command can check
is still allowed, but it must say so, and the job must never count it as verified.

## Directory layout

```
docs/agents/
  INDEX.md          entry point; every doc, what it answers, when it was last verified
  CORPUS-SPEC.md    this file
  current-state/    what IS, measured against the tree
  future-state/     what is INTENDED, each gap naming the issue that owns it
  mechanisms/       how one thing works, traced to file and symbol
  hazards/          traps that produced a wrong answer here, and the probe that exposes each
  verification/     how the refresh runs, and the committed ledger of its last run
```

Never name a file in this tree `README.md`. `.github/CODEOWNERS` ends with an unowned
`README.md` rule, and last match wins, so a `README.md` here would drop out of required
review while still carrying commands CI executes.

## Finding the rest of the estate

This corpus answers for one repository. An agent that arrives cold in an estate of
many repositories has a second question first: which repository owns the concern it
is here about.

If your estate publishes a directory answering that, name its location in `INDEX.md`,
in the entry point an agent reads before anything else. One line is enough:

```
Estate directory: <owner>/<repo>/docs/agents/current-state/<file>.md
```

This spec does not name a location, because the location belongs to the estate rather
than to the spec, and this file is copied verbatim into every adopting repository. A
path written here would route every adopter into one organisation's tree.

If your estate publishes no such directory, say so in `INDEX.md` rather than leaving
the question unanswered. "There is no estate directory; this repository stands alone"
takes one line and stops the next agent looking for one.

## Evidence markers

| Marker | Means | Carries |
|---|---|---|
| `MEASURED` | The author ran a command against the tree and read the answer. | `command`, `expect`, `match` |
| `ASSERTED` | A document says this. It is a measurement of the document, never of live infrastructure. | `source`, `quote` |
| `INFERRED` | Reasoned from evidence, not observed. | `basis`, `falsifier` |
| `ASPIRATIONAL` | Described as intended. It may not be built. | `basis`, `falsifier` |
| `CLAIMED` | An issue or a run says this is done. The artifact was not checked. | `command`, `expect`, `match` |
| `LANDED` | Done, and the artifact was confirmed present. | `command`, `expect`, `match` |

`CLAIMED` and `LANDED` differ only in whether the artifact was looked at. Keep them
apart. Today's failures came from treating the first as the second.

## Claim blocks

A claim is a fenced block with the info string `claim`. The body is YAML. The block is
the single source of truth: humans read it in place, and the runner parses it from the
same bytes. There is no sidecar, because two copies of a fact drift apart.

````markdown
```claim
id: infra.terraform-roots.count
kind: MEASURED
statement: mithran-infra defines 49 Terraform roots under live/.
command: git ls-files 'live/**/*.tf' | xargs -n1 dirname | sort -u | wc -l
expect: "49"
match: exact
verified_at: 2026-08-12
verified_ref: 8e39a11315713d53e1e190e3a30f876f75511189
```
````

### Fields

| Field | Required for | Meaning |
|---|---|---|
| `id` | all | Stable dotted identifier. Never reuse one for a different claim. Drift is tracked by `id`, so renaming an id loses its history. |
| `kind` | all | One marker from the table above. |
| `statement` | all | One sentence a human reads. |
| `command` | `MEASURED`, `CLAIMED`, `LANDED` | Read-only shell, run from the repository root. |
| `expect` | with `command` | The answer the command produced when the claim was written. |
| `match` | with `command` | `exact`, `contains`, `regex`, `min`, or `max`. |
| `source` | `ASSERTED` | Repository-relative path of the citing document. |
| `quote` | `ASSERTED` | A literal substring that must appear in `source`. |
| `basis` | `INFERRED`, `ASPIRATIONAL` | What the reasoning rests on. |
| `falsifier` | `INFERRED`, `ASPIRATIONAL` | The observation that would kill the claim. |
| `needs` | optional | `repo` (default), `github`, or `live`. |
| `drift` | optional | `report` (default) or `alert`. |
| `allow_empty` | optional | `true` permits empty output. Default `false`. |
| `verified_at` | all | Date the claim last matched. The runner rewrites this. |
| `verified_ref` | all | Commit the claim last matched at. The runner rewrites this. |

### `regex`, `min`, and `max` follow Python

A `regex` pattern is a Python `re` pattern, compiled with `re.MULTILINE` and searched
unanchored. Look-around and backreferences work. Both runners accept the same dialect, so a
pattern that matches under one matches under the other.

A `min` or `max` bound is read by Python's `float()`. It accepts an underscore between digits
and any Unicode decimal digit, so `"1_000"` is one thousand.

Quote a bound that YAML would otherwise resolve. Written bare, `expect: 1_000` becomes the
integer `1000` before the comparison sees it, which is the same number by a different route.

### Cite by quote, not by line number

An `ASSERTED` claim names a `source` and a `quote`. It does not name a line number.

Line numbers rot silently. A document gains a paragraph, every citation below it now
points at the wrong sentence, and nothing detects it. A quote either survives in the
file or it does not, and the runner can tell which.

Record a line number in prose beside the claim when it helps a reader navigate. Never
make the machine check depend on it.

## Outcomes

The runner reports exactly four outcomes per claim. It never collapses them.

| Outcome | Means |
|---|---|
| `MATCH` | The command ran, exited zero, and the value satisfied `expect`. |
| `DRIFT` | The command ran, exited zero, and the value did not satisfy `expect`. |
| `ERROR` | The command did not run, exited non-zero, timed out, or produced no value where one was required. |
| `UNCHECKABLE` | The claim declares no command, by kind or by `needs: live`. |

`ERROR` is not a kind of `DRIFT`. An absence is not a measurement. A command that fails
to run tells you nothing about the world, and a report that files it under "changed"
claims an observation it never made.

`UNCHECKABLE` is not a kind of `MATCH`. An `INFERRED` claim is never verified by the
job. It is counted in its own column so that "42 verified" cannot quietly mean "31
verified and 11 skipped".

### Exit codes

| Code | Means |
|---|---|
| `0` | The corpus was checked and nothing needs a human. `DRIFT` at `drift: report` still exits `0`. |
| `1` | The corpus was checked and something needs a human: any `ERROR`, or any `DRIFT` at `drift: alert`. |
| `2` | The runner could not produce a trustworthy result. |

Never read a `2` as a `1`. The first says the runner could not look. The second says it
looked and found something.

The runner asserts one invariant on itself: the four outcome counts must sum to the
number of claims parsed. A mismatch exits `2`, because a runner that lost a claim cannot
report on coverage it did not have.

### Changed is not broken

A count that moves from 49 to 51 is information. The repository grew.

`drift: report` is the default and keeps the job green. The job reports the move and
lets a human or an agent judge it. Reserve `drift: alert` for a property that must not
move, such as an invariant a control depends on. A job that fails red on every
legitimate change gets muted within a month, and a muted control is a deleted control.

## Command discipline

The runner executes `command` strings out of committed markdown. Four rules follow.

1. Commands run with `bash -o pipefail`. A pipeline's status is the first failure, not
   the last stage's. `… | tail -1` otherwise reports `tail`'s success as the check's.
2. Commands are read-only. A claim never mutates the tree, the cloud, or an issue.
3. Claims declare what access they need, and the job enables tiers explicitly.

   | `needs` | Reads | Run by the weekly job |
   |---|---|---|
   | `repo` | The checkout only. | Always. |
   | `github` | The GitHub API, read-only, under `GITHUB_TOKEN`. | Yes, via `--enable github`. |
   | `live` | Cloud state, under credentials the job does not hold. | Never. |

   The weekly job holds **no cloud credentials**. A `needs: live` claim is reported
   `UNCHECKABLE` and a human or an agent runs it under the repository's normal access
   rules. That keeps `CLAUDE.md`'s no-manual-mutation rule intact by construction: the
   refresh cannot reach infrastructure even if a claim command tried to.
4. Empty output is `ERROR` unless the claim sets `allow_empty: true`. A grep that
   matches nothing and a grep whose pattern is broken both print nothing.

Write `command` as a YAML block scalar whenever it contains a colon followed by a
space. A bare `command: grep 'workflow: x' file` is not valid YAML, and the runner
exits `2` rather than guessing what you meant.

Quote or block-scalar any command that is a bare YAML keyword. `command: true` loads as
the boolean `True`, not the shell builtin, and the runner rejects it as a missing
command. The same applies to `false`, `yes`, `no`, `on`, and `off`.

Because CI executes these strings, a change under `docs/agents/` is a change to code.
`.github/CODEOWNERS` owns this directory for that reason, under its first arm.

## Document frontmatter

Every corpus document opens with YAML frontmatter.

```yaml
---
title: Guest artifact hydration proof
answers: What does the fast path's serial check assert, and how does it fail?
last_verified: 2026-08-12
verified_ref: 8e39a11315713d53e1e190e3a30f876f75511189
refresh: quarterly
owner_issue: mithran-hq/mithran-infra#3405   # optional
---
```

`INDEX.md` lists every document with its `last_verified`. A document older than its
refresh interval is reported stale, whether or not its claims still match.

### Two freshnesses, and do not conflate them

A corpus has two kinds of age, and one control cannot measure both.

| Kind | What it means | Where it lives | Cadence |
|---|---|---|---|
| Claim freshness | The commands still return what the claims say. | The refresh job's run history, and `verification/last-run.json`. | Weekly, by machine. |
| Document freshness | A human has re-read the prose and still stands behind it. | `last_verified` in the frontmatter. | Quarterly, by hand. |

**`last_verified` is a human signature, not a machine stamp.** The weekly job holds
`contents: read` and never commits, so it cannot move that date. Setting `refresh:
weekly` on a document therefore guarantees it reports stale seven days later and stays
stale forever — a report that is noisy every week and consequential never.

Use a cadence a human will actually meet. `quarterly` is the default here. Move
`last_verified` when you re-read the document, in the same change as whatever you
corrected.

The machine's opinion is already carried by the claims themselves: a stale claim
`DRIFT`s or `ERROR`s. It does not need a second, weaker signal wearing the same name.

Run `--update` to stamp matched claims and fully-matching frontmatter after a manual
re-read. Note that it moves `verified_at` only on claims that **matched**: an
`UNCHECKABLE` claim keeps its old stamp deliberately, because nothing verified it and
a moving date would imply otherwise.

## What the refresh cannot do

The refresh checks claims. It does not check judgment.

`hazards/` is the part no command can verify. A stale clone, a truncated string, an exit
code read from the wrong side of a pipe — those are learned, not measured. Keep the
hazard documents narrative and specific. Each one names the real occasion that produced
the wrong answer, because a hazard stated in the abstract does not transfer.

A hazard may carry a `probe`: the command that exposes the trap on demand. A probe is
not a claim about this repository, so the runner does not execute it. It is there for a
reader who wants to see the trap fire.

## Adopting this in another repository

1. Copy `CORPUS-SPEC.md` and the empty directory skeleton.
2. Write `INDEX.md` first. An entry point that lists nothing is still an entry point.
3. Add the runner and the weekly workflow.
4. Register the workflow against silence on the same day. See
   `verification/REFRESH.md`.
5. Own `docs/agents/` in `CODEOWNERS`, because CI executes its claim blocks.
6. Answer the estate question in `INDEX.md`, either by naming the directory or by
   saying there is none. See `Finding the rest of the estate`.

Start with `hazards/`. It carries the most value per line and needs no infrastructure.
