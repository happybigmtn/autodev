# Decision: parallel host reconciliation preserves queue truth

Status: accepted
Date: 2026-05-01

## Context

`auto parallel` coordinates isolated worker lanes against shared queue files.
The host owns `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, legacy queue files, and
run-level triage artifacts. Lane workers may commit code, tests, and local
evidence, but they must not race the host on shared queue state.

Recent production-style runs exposed two scheduler foot-guns:

- Canonical repos with dirty tracked dispatch paths could behave differently
  between repos. One run would checkpoint pre-existing work; another would
  refuse to launch, forcing manual stash/restore/relaunch steps.
- Receipt freshness drift could demote already completed `[x]` rows back to
  `[~]`, even when the implementation had landed on trunk and the stale receipt
  was only a proof-trail repair issue.

## Decision

`auto parallel` uses one host reconciliation rule across repos:

- Before dispatch, the host auto-checkpoints stageable canonical repo changes
  with an `auto parallel checkpoint` commit. It refuses only if checkpointing
  cannot clear the dirty dispatch paths.
- Receipt drift is warning-only. The host writes `RECEIPTS-DRIFT.md` naming
  affected completed tasks, exact evidence gaps, and manual closeout candidates,
  but it does not mutate `IMPLEMENTATION_PLAN.md` during a drift audit.
- `RECEIPTS-DRIFT.md` is host-owned queue state. It is included with host queue
  sync files and protected from direct lane edits.
- Operator work is first-class. Rows can declare `Lane kind: operator`; those
  rows are routed to `.auto/parallel/operator-actions.md` instead of code
  workers. Rows can also declare `Lane kind: evidence` for proof-only closeout
  work.

## Why

The root queue is the product/control-plane source of truth. Receipts are
execution evidence. When those diverge, the right behavior is to preserve
landed work and expose the proof gap, not to reschedule already completed code
work as if the implementation disappeared.

Auto-checkpointing before dispatch also matches the practical operator path:
preserve pre-existing work with a clear commit message, then keep the scheduler
moving. Refusing remains appropriate only when Git cannot produce a clean
checkpoint.

## Consequences

- Operators should inspect `RECEIPTS-DRIFT.md` when `auto parallel` reports
  drift. Repair the proof trail or close out candidates deliberately instead of
  assuming the queue status changed.
- Worker lanes should continue to avoid shared queue files and should preserve
  task proof in committed code/tests and logs.
- Future receipt-footer work should regenerate or derive proof from landed
  commits without changing this rule: drift can block a ship/release gate, but
  it must not silently rewrite queue status during host sync.

## Receipt Footer Update

Implemented follow-on: `auto parallel` now promotes staged JSON receipts into
compact `Auto-Verification-Receipt-*` commit-message footers on task closeout
commits. Evidence readers and the ship gate prefer reachable footers and keep
JSON as compatibility/staging input. Footer receipts use containing-commit
semantics, so a proof commit can remain valid after later branch movement
without requiring the receipt's pre-closeout `commit` field to equal current
`HEAD`.
