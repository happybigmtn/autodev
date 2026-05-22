# Focus Brief

## Raw Focus

make auto corpus less focused on documentation and artifacts, and more focused on the highest-priority product, design, and engineering implementation areas for a non-production codebase

## Normalized Focus Themes

- Prefer runnable product slices over new planning inventory.
- Treat reports, audits, and generated snapshots as support evidence, not product value by themselves.
- Rank work by whether it restores the core operator loop: decide work, dispatch agents, verify evidence, recover safely, and ship with proof.
- Keep non-production scope honest: optimize for learning, testability, and operator clarity before release process polish.
- Penalize docs-only work unless stale docs are sending operators or workers into the wrong runtime path.

## Likely Surfaces

- Code: `src/generation.rs`, `src/spec_command.rs`, `src/task_parser.rs`, `src/verification_lint.rs`, `src/super_command.rs`, `src/parallel_command.rs`, `src/completion_artifacts.rs`, `src/loop_command.rs`, `src/doctor_command.rs`, `src/main.rs`, quota modules under `src/quota_*`, and backend execution modules.
- Tests: unit tests embedded in the above modules, integration tests under `tests/`, and CI coverage in `.github/workflows/ci.yml`.
- Product: `auto super`, `auto gen`, `auto parallel`, `auto loop`, `auto doctor`, `auto ship`, `auto quota`, and the operator-facing markdown ledgers they maintain.
- Operations: root control files (`IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `REVIEW.md`, `COMPLETED.md`, `ARCHIVED.md`), `.auto/` runtime state, generated corpus snapshots, verification receipt footers, and model backend credential handling.

## Repo-Wide Review Still Required

- The focus does not excuse ignoring security boundaries, especially credential copy, quota profile isolation, prompt transport, and symlink handling.
- The focus does not excuse ignoring CI and tests; a non-production control plane still needs deterministic proof before dispatching agents.
- The focus does not excuse ignoring docs when stale docs define active control-plane behavior or worker instructions.
- The focus does not excuse ignoring user-facing text; this repository is a terminal product, so command names, help, status output, and recovery hints are part of the design surface.

## Main Questions

- Can an operator trust the checkout enough to run `auto super`, `auto parallel`, or `auto ship` today?
- Which failing tests correspond to real product regressions rather than stale expectations?
- Where does runtime truth disagree with accepted decisions about snapshot promotion, receipts, and operator lanes?
- What is the smallest implementation slice that makes the next autonomous cycle safer and more observable?
- Which docs are only stale inventory, and which stale docs actively misdirect workers or operators?

## Priority Effect

The focus moved documentation cleanup and broad audit expansion below code/test/runtime slices. The top priority is restoring the smallest validation baseline first: rustfmt plus live command-surface truth. Strict generated-plan proof then becomes its own focused slice, followed by source-of-truth control, receipt semantics, operator status clarity, and quota hardening. Stale specs and README drift remain important only where they expose or reinforce those runtime problems.
