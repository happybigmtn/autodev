# Operator Experience Design

## Design Scope

Autodev has meaningful user-facing surfaces even without a graphical UI. The interface is a terminal product made of command names, help text, prompts, state files, reports, logs, receipts, recovery instructions, and release gates. This design pass therefore covers information architecture, state coverage, user journeys, accessibility, responsive terminal behavior, and AI-slop risk for operator workflows.

## Information Architecture

The command surface should read as one system:

- Plan: `auto corpus`, `auto gen`, `auto spec`, `auto design`, `auto super`.
- Execute: `auto loop`, `auto parallel`, `auto symphony`, quota-backed model execution.
- Verify: `auto qa`, `auto qa-only`, `auto health`, `auto review`, `auto audit`, `auto nemesis`.
- Release: `auto ship`, receipts, QA/health/review reports, install proof, tag evidence.
- Operate: `auto doctor`, `auto quota`, `auto steward`, `auto book`, state files under `.auto/`.

Current IA risk: the code owns this separation better than older specs do. The top of README is mostly current, while dated specs and previous genesis snapshots can pull model prompts back toward an older 16-command product.

## Key Journeys

Fresh operator:

1. Install or run the local binary.
2. Run `auto --version`.
3. Run `auto doctor`.
4. Run `git status --short`.
5. Read the active queue in `IMPLEMENTATION_PLAN.md` and `WORKLIST.md`.

The journey exists, but tool requirements are not presented consistently: `AGENTS.md` calls `claude`, `codex`, `pi`, and `gh` required, while doctor/README treat unavailable tools as capability warnings.

Planning operator:

1. Run `auto corpus` with a focus.
2. Inspect `genesis/ASSESSMENT.md`, `SPEC.md`, `PLANS.md`, and numbered plans.
3. Use `auto gen --snapshot-only` or an explicit sync path after validation.

The gap is rollback safety: a failed corpus run can leave `genesis/` empty, and `auto gen` can later accept the empty root.

Parallel operator:

1. Run `auto parallel status`.
2. Confirm clean worktree, dependency-ready queue rows, safe quota state, and no stale salvage.
3. Launch lanes.
4. Monitor `.auto/parallel/live.log`, receipts, REVIEW handoffs, and lane summaries.

The gap is trust: dependency truth is lossy, salvage notes can outlive lane repos, and credential swaps are not held across the process lifetime.

Release operator:

1. Confirm QA, health, review, design, and audit reports.
2. Confirm verification receipts from the current tree.
3. Confirm installed binary proof and version.
4. Run `auto ship`.

The gap is proof freshness: receipts are not currently bound to commit, dirty state, plan hash, or artifact hashes.

## State Coverage

Autodev should label state by durability:

- Source-controlled control inputs: `AGENTS.md`, `README.md`, `IMPLEMENTATION_PLAN.md`, `WORKLIST.md`, `ARCHIVED.md`, `REVIEW.md`, `specs/`, `docs/decisions/`, `genesis/`.
- Runtime state: `.auto/`, `.auto/parallel/`, `.auto/symphony/verification-receipts/`, `.auto/audit-everything/`.
- Generated investigation surfaces: `bug/`, `nemesis/`.
- Excluded generated workspaces: `gen-*`.

The current code expresses parts of this in `.gitignore`, `src/state.rs`, and `src/util.rs`, but the user-facing explanation is scattered. Production design should make every command say which class it reads and writes.

## Accessibility and Terminal Ergonomics

- Prefer stable labels over color-only status.
- End every long-running command with a compact final status block: status, files written, receipts, next command, and blockers.
- Keep help text grouped by journey, not only alphabetic command lists.
- Avoid long unwrapped paragraphs in terminal output.
- Provide structured output where automation needs it, especially for `auto doctor`, `auto parallel status`, and release gates.
- Ensure dry-run output says whether it wrote prompt logs, state directories, or no files.

## Responsive Behavior

Terminal output should work in narrow logs, tmux panes, and CI:

- Use short line prefixes and avoid table formats that require wide terminals.
- Write full details to durable files and keep stdout summaries compact.
- Make progress logs resumable by stable artifact path, not by transient lane directory.
- Avoid hiding critical warnings behind decorative formatting.

## AI-Slop Risk

Primary AI-slop risks:

- Generated plans that pass prose review but do not match code contracts.
- Report-only commands that write state and then sound dry-run safe.
- Receipts that prove a command passed but not that it passed for the current tree.
- Stale specs that present old command counts or missing-command claims as current truth.
- Worker prompts that encourage broad edits without exact source-of-truth boundaries.

Design remedy: every command that invokes a model should name source truth, write boundaries, validation receipts, and recovery path in deterministic text assembled by code.

## Recommended Design Contract

- `auto doctor`: no-model first-run truth, with optional structured output.
- `auto corpus`: atomic planning-root generation, with a non-empty manifest.
- `auto gen`: snapshot first; root sync only after explicit validated intent.
- `auto parallel status`: queue truth, dependency blockers, stale salvage, receipt drift, quota readiness.
- `auto qa-only`, `auto design`, `auto health`, `auto review`: consistent report-only/write-boundary enforcement.
- `auto ship`: proof summary that names commit, dirty state, installed binary version, receipt freshness, and bypass reason if any.

## 2026-04-30 Design Gate Amendment

The root `DESIGN.md` is now the durable design doctrine for autodev's terminal/operator product surface. This corpus file remains planning input for `auto gen`; it must not become a competing source of live product truth.

Generation should preserve these design requirements:

- Treat terminal help, stdout, prompt logs, plans, reports, receipts, and release gates as the real product UI.
- Reject web-dashboard or mockup work unless it is clearly non-authoritative and not used as acceptance evidence.
- Emit implementation tasks that name runtime owners, UI consumers, generated artifacts, fixture boundaries, contract generation, cross-surface proof, and closeout review.
- Prioritize the `DESIGN-*` repair rows in the root queue before any CEO campaign treats design/runtime integrity as solved.
- Keep `genesis/plans/011-first-run-dx-and-command-output-contracts.md` as the command-output design slice and `genesis/plans/008-auto-loop-auto-review-and-super-schema-parity.md` as the task-contract parity slice.

## 2026-04-30 Pass 02 Amendment

The second design repair pass found that the strongest remaining design risks are not visual styling; they are stale operator truth surfaces:

- `auto parallel status` can show no live host process while stale lane recovery and old host warnings still read as urgent product state.
- Completion receipts prove command history but do not yet bind that proof to the current commit, dirty-state fingerprint, plan hash, or declared artifact hashes.
- Generated verification commands can still be syntactically or semantically unrunnable, including multi-filter cargo tests and shell-sensitive grep examples.
- Open or partial root queue rows must carry the same full runtime/UI task contract that `auto gen`, `auto spec`, and `auto super` now expect.

Generation should keep those findings as design/runtime integrity work. Do not replace them with dashboard, mockup, or purely editorial polish tasks.
