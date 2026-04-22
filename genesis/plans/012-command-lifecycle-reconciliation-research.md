# Plan 012 — Research: command-lifecycle reconciliation

This ExecPlan is a living document. Update every section as reality moves. If a root `PLANS.md` is added to the repository root later, maintain this plan in accordance with it.

**Shape: Research-only.** This plan produces a written decision about the command lifecycle surface. It is **also a User Challenge**: the recommendation must be reviewed and accepted by the operator before any implementation follow-on is authored. Implementation, if warranted, is a separate plan.

## Purpose / Big Picture

The repository has sixteen top-level commands (verified by reading `src/main.rs:52-96` and counting `Commands` enum variants, treating `Symphony` with its subcommands as one top-level entry). README (after Plan 002) and `genesis/SPEC.md` each present an inventory, but the inventory alone does not answer the operator question that matters: *when is each command the right tool?* Three of the sixteen -- `steward`, `corpus`, `gen` -- have overlapping-sounding purposes. `auto steward` was introduced on 2026-04-21 with the commit message "new stewardship command for mid-flight repos" (commit `7d60819`). `auto corpus` and `auto gen` together cover seeding and generation. There is now a real possibility that `steward` replaces `corpus + gen` for a meaningful slice of cases, and no written decision about it.

A parallel question: `audit`, `nemesis`, `bug`, and `review` each perform some form of verification-plus-fix. Are their intended trigger conditions disjoint? Is there a slice of situations where two of them would both "work" and the operator is choosing by habit rather than by fit?

A third question: the `auto parallel` command at 7853 lines is the single largest module and offers multi-lane execution of the normal lifecycle. Does its existence change the answers to the first two questions -- i.e., is there implicit lifecycle reconciliation already happening inside `parallel_command.rs` that should be lifted out?

This plan produces a written map of the command lifecycle as it actually exists, a candidate reconciliation that consolidates or re-names where the overlap is genuinely confusing, and a proposal to the operator. The operator's sign-off gates any implementation follow-on. No command is removed, renamed, or merged by this plan.

An operator reading the result knows: the precise decision tree for "which command now?", where the overlaps are, and what they would look like reconciled. The operator then chooses whether to proceed with reconciliation, proceed with a narrower docs-only clarification, or keep the surface as-is.

## Requirements Trace

- **R1.** A written map under `genesis/research/012-command-lifecycle-map.md` describing each of the sixteen commands in one paragraph, with the trigger condition ("when do I use this?") as the first sentence.
- **R2.** A pairwise overlap matrix: for each pair of commands that seem adjacent, a one-line verdict of `{fully disjoint, partial overlap with clear split, confusing overlap}`. The confusing-overlap pairs are the focus.
- **R3.** A candidate reconciliation for each confusing-overlap pair. Candidates are one of: `{merge two commands into one}`, `{keep both, add decision tree to README}`, `{rename one to clarify its trigger}`, `{deprecate one in favor of the other}`.
- **R4.** Each candidate is classified as **Mechanical**, **Taste**, or **User Challenge**. All of them are expected to be User Challenge, which is the point.
- **R5.** A final proposal section with a concrete ask to the operator: "approve reconciliation plan `{summary}`", or "defer all reconciliation", or "approve a subset". Nothing else may happen until the operator signs off in writing (a commit to `genesis/research/012-command-lifecycle-map.md` recording the decision, or a comment on the PR).
- **R6.** An explicit Not Doing list attached to this plan: candidate moves that were considered and rejected in research, so the operator sees the boundary.

## Scope Boundaries

- **Producing:** one research note at `genesis/research/012-command-lifecycle-map.md`; Decision Log entries on this plan.
- **Not producing:** any source file change, any README change (even clarifying), any rename, any deprecation shim.
- **Not promising:** that the operator will approve any of the candidate reconciliations.
- **Not covering:** adding a new command. The Genesis Report's Not Doing list explicitly forbids that.
- **Not covering:** re-architecting `parallel_command.rs`. That's a separate, larger plan.

## Progress

- [ ] Command map written (sixteen entries).
- [ ] Overlap matrix populated.
- [ ] Candidate reconciliations drafted.
- [ ] Each candidate classified.
- [ ] Proposal section written.
- [ ] Not Doing list written.
- [ ] Research note committed.
- [ ] Operator sign-off recorded (external to this plan's completion).

## Surprises & Discoveries

None yet. Anticipated:
- The `parallel_command.rs` module turns out to already encode an implicit decision tree ("run this lane, then that lane") that answers the lifecycle question within a single command. If so, this is a major finding and should be called out.
- `audit` and `nemesis` turn out to have genuinely different trigger conditions that are not documented in the README, making the overlap appear worse than it is.
- `steward` turns out to be a superset of `corpus` plus additional capabilities. If so, a docs-only clarification may be the right reconciliation.

## Decision Log

- **2026-04-21 — User Challenge by default.** User Challenge. Command surface is a product decision, not a mechanical one. Even the candidate reconciliations belong to the operator's judgment.
- **2026-04-21 — Research first, before any rename or deprecation.** Taste. Removing or renaming a command is an expensive operator-facing change; doing it on intuition alone produces regret. A written map forces the overlap to be visible.
- **2026-04-21 — Sign-off recorded in a commit, not in chat.** Mechanical. A commit is durable and auditable; chat approval evaporates. The research file's Decision section records the approved outcome in plain text.

## Outcomes & Retrospective

None yet.

## Context and Orientation

- `src/main.rs:52-96` — authoritative list of sixteen command enum variants.
- `src/bug_command.rs`, `src/nemesis.rs`, `src/audit_command.rs`, `src/review_command.rs` — the four verification-plus-fix commands.
- `src/corpus_command.rs` (or equivalent path) and `src/generation.rs` — the two seeding commands.
- `src/steward_command.rs` — the new "mid-flight repo" entrant.
- `src/parallel_command.rs` — 7853 lines; multi-lane orchestration.
- `README.md` after Plan 002 — per-command detailed guide sections.
- `genesis/SPEC.md` — command inventory table.
- `genesis/GENESIS-REPORT.md` — Finding: command-surface growth is a design risk. Not Doing: no new commands. Not Doing: no refactor of `parallel_command.rs` beyond shared-helper extraction.
- `COMPLETED.md` — historical record of when each command landed, including the 2026-04-21 `steward` / `audit` / `symphony` additions.

## Plan of Work

1. Read `src/main.rs:52-96` and extract the sixteen variants.
2. For each command, read the top of the corresponding module and capture the trigger condition in one sentence.
3. Build the overlap matrix. Mark confusing overlaps.
4. For each confusing overlap, draft a candidate reconciliation.
5. Classify each candidate.
6. Write the proposal section.
7. Write the Not Doing list.
8. Commit the research file.
9. Ask the operator to review and sign off in the same file (append-only) or in a PR comment on the commit.

## Implementation Units

**Unit 1 — Command map.**
- Goal: sixteen paragraphs, one per command, first sentence = trigger condition.
- Requirements advanced: R1.
- Dependencies: none.
- Files to create or modify: `genesis/research/012-command-lifecycle-map.md`.
- Tests to add or modify: none.
- Approach: iterate over `main.rs` variants; read each module's top doc-comment or command-handler function; extract trigger sentence; write paragraph.
- Test expectation: none.

**Unit 2 — Overlap matrix.**
- Goal: pairwise matrix for the sixteen commands, focused on adjacency pairs.
- Requirements advanced: R2.
- Dependencies: Unit 1.
- Files to create or modify: `genesis/research/012-command-lifecycle-map.md` (append).
- Tests to add or modify: none.
- Approach: identify adjacency pairs a priori (e.g., `corpus` vs `gen` vs `steward`; `audit` vs `nemesis` vs `bug` vs `review`; `qa` vs `qa-only`; `loop` vs `parallel`); assign one-line verdict.
- Test expectation: none.

**Unit 3 — Candidate reconciliations.**
- Goal: for each confusing-overlap pair, a candidate and a rationale.
- Requirements advanced: R3, R4.
- Dependencies: Unit 2.
- Files to create or modify: `genesis/research/012-command-lifecycle-map.md` (append).
- Tests to add or modify: none.
- Approach: for each pair marked "confusing overlap", propose one candidate from the four-option set; classify (User Challenge expected); justify in two or three sentences.
- Test expectation: none.

**Unit 4 — Proposal and Not-Doing.**
- Goal: a proposal section addressed to the operator, and an explicit list of candidate moves considered and rejected.
- Requirements advanced: R5, R6.
- Dependencies: Unit 3.
- Files to create or modify: `genesis/research/012-command-lifecycle-map.md` (append).
- Tests to add or modify: none.
- Approach: summarize the reconciliation recommendations; write the Not Doing list.
- Test expectation: none.

**Unit 5 — Operator sign-off.**
- Goal: the operator has written a decision into the research file or into a PR comment.
- Requirements advanced: R5.
- Dependencies: Unit 4 committed.
- Files to create or modify: `genesis/research/012-command-lifecycle-map.md` (append a Decision section, either approving, partially approving, or deferring).
- Tests to add or modify: none.
- Approach: **this unit is owned by the operator, not by the agent**. The agent stops at the end of Unit 4 and surfaces the research note for review.
- Test expectation: none.

## Concrete Steps

From the repository root:

1. Ensure the research directory exists (Plan 008 already created it if executed first):
   ```
   mkdir -p genesis/research
   ```
2. Enumerate commands from source:
   ```
   rg -nE '^\s*(Corpus|Gen|Reverse|Bug|Loop|Parallel|Qa|QaOnly|Health|Review|Steward|Audit|Ship|Nemesis|Quota|Symphony)\b' src/main.rs
   ```
3. For each variant, open the corresponding module and extract the first documented trigger condition.
4. Draft `genesis/research/012-command-lifecycle-map.md` with three sections: Command map, Overlap matrix, Candidate reconciliations. Finish with Proposal and Not Doing.
5. Review the file against the Requirements Trace checklist.
6. Commit:
   ```
   git add genesis/research/012-command-lifecycle-map.md genesis/plans/012-command-lifecycle-reconciliation-research.md
   git commit -m "research(lifecycle): command overlap map and reconciliation candidates"
   ```
7. Surface the file for operator review. Any implementation that follows requires a new ExecPlan.

## Validation and Acceptance

- **Observable 1.** `genesis/research/012-command-lifecycle-map.md` exists with the four declared sections.
- **Observable 2.** The command map has exactly sixteen paragraphs, one per command.
- **Observable 3.** Every adjacency pair (at minimum: `corpus` vs `gen` vs `steward`; `audit` vs `nemesis` vs `bug` vs `review`; `qa` vs `qa-only`; `loop` vs `parallel`) is present in the overlap matrix with a verdict.
- **Observable 4.** Every pair marked "confusing overlap" has a candidate reconciliation and a classification.
- **Observable 5.** The Not Doing list explicitly excludes at least three things the operator might otherwise assume are in scope.
- **Observable 6.** No file under `src/` was modified by this plan.
- **Observable 7.** The research file contains a Decision section that is either unfilled (awaiting operator) or filled by the operator in a subsequent commit. The agent does not fill this section.

## Idempotence and Recovery

- Rerunning the map and matrix against the same codebase produces the same verdicts. If the codebase changes (a command is added or removed), update the map in-place.
- If the operator rejects a candidate, do not reopen the argument in the research file; record the rejection in the Decision section and move on. A future plan may revisit.
- If the operator partially approves (e.g., "rename `audit` and `nemesis` clarification but leave `corpus`/`gen`/`steward` as-is"), a new ExecPlan captures the approved subset. The research file is not re-edited beyond the Decision section.

## Artifacts and Notes

- Sixteen commands as of 2026-04-21 per `src/main.rs:52-96`: Corpus, Gen, Reverse, Bug, Loop, Parallel, Qa, QaOnly, Health, Review, Steward, Audit, Ship, Nemesis, Quota, Symphony (with subcommands Sync, Workflow, Run).
- Confusing-overlap pairs identified in research (to be filled): (expected candidates — `corpus`/`gen`/`steward`, `audit`/`nemesis`, possibly `loop`/`parallel`).
- Recommended reconciliations (to be filled).
- Operator decision (to be filled by operator, not by agent).
- Commit hash: (to be filled).

## Interfaces and Dependencies

- **Depends on:** Plan 009 checkpoint; Plan 002 (README inventory sync) so the map can reference the post-drift command guide.
- **Used by:** potential follow-on implementation plan, authored only after operator sign-off.
- **External:** none. Pure reading of existing source and docs.

## Operator handoff note

When this plan reaches the end of Unit 4, the agent **pauses** and surfaces the research file for review. The agent does not write the Decision section, does not open a follow-on implementation plan, and does not edit any source file. That is the point of the User Challenge classification: the question belongs to the operator.
