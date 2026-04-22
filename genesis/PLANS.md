# PLANS — autodev corpus

This file is an **index** of the generated ExecPlans under `genesis/plans/`. It is not itself an ExecPlan authoring standard.

## Active planning surface

The repository root has no `PLANS.md`, no `plans/` directory, no `CLAUDE.md`, and an empty `IMPLEMENTATION_PLAN.md` skeleton. The only active control doc is `AGENTS.md`. Therefore the generated corpus under `genesis/` is the active planning surface for this pass.

When `auto gen` is run next, `genesis/plans/*.md` will be promoted into the root `IMPLEMENTATION_PLAN.md` queue. Root-level ExecPlans do not exist yet, so the generated plans here are not subordinate to a root corpus — they are the intended seed for it.

## Numbered plan set

| # | Title | Shape | Dependencies | Gate? |
|---|---|---|---|---|
| 001 | Master plan & sequencing | Index / decision record | — | — |
| 002 | README command inventory sync | Mechanical / small | — | — |
| 003 | Retire `codex_exec.rs` tmux dead code | Mechanical / small | 002 | — |
| 004 | `auto audit` verdict-application test harness | Implementation / medium | — | — |
| 005 | **Checkpoint — Truth pass complete** | Decision gate | 002, 003, 004 | Yes |
| 006 | Quota credential permissions + log scrubbing | Implementation / medium (security) | 005 | — |
| 007 | Shared utilities: branch, reference-repo, prompt-log | Refactor / medium | 005 | — |
| 008 | LlmBackend trait consolidation (research) | Research / bounded | 007 | — |
| 009 | **Checkpoint — Consolidation complete** | Decision gate | 006, 007, 008 | Yes |
| 010 | GitHub Actions CI bootstrap | Implementation / small | 009 | — |
| 011 | End-to-end smoke tests for `qa`, `health`, `ship` | Implementation / medium | 010 | — |
| 012 | Command-lifecycle reconciliation (research) | Research / bounded | 009 | — |

All numbered plans under `genesis/plans/` are full ExecPlans, not task stubs. They are self-contained for a novice with only the current working tree and the plan file.

## Sequencing rationale

**Phase 1 — Truth pass (002-005).** Close the doc-vs-code gap so operators and future planning passes work against an honest inventory. These are small, high-signal, and unblock everything else by eliminating the README-drift noise that otherwise contaminates every subsequent conversation. Remove the dead tmux scaffolding in `codex_exec.rs` at the same time — it is in the same drift category.

Plan 004 (audit tests) belongs in Phase 1 because `auto audit` is the newest command with the weakest coverage, and any further feature work on it without tests risks silent behavior drift.

Plan 005 is a decision gate: nothing in Phase 2 starts until Phase 1 is validated against `cargo test` and `cargo clippy -D warnings`.

**Phase 2 — Consolidation (006-009).** The concrete security gap (plaintext credentials) and the structural debt (duplicated helpers) are both near-at-hand wins that do not require architectural changes. Plan 008 is research-only because a `LlmBackend` trait is a taste-call that should be validated against two commands (`bug` and `nemesis`) before committing. Plan 009 is a decision gate.

**Phase 3 — Foundation (010-012).** CI first (010), then the integration tests it will enforce (011). Plan 012 is research-only because the "when do I use `steward` vs. `corpus + gen`" question is a product-lens decision the operator should weigh in on before code changes.

## Why this slice order is preferable

**Alternative A — Tackle the 7853-LOC `parallel_command.rs` first.** Rejected. `parallel` is the most-used command; restructuring it before cleaning up the surrounding documentation, the duplicated helpers it shares with other commands, and the test scaffolding would destabilize the working path.

**Alternative B — Ship a new command (`auto doctor`, `auto preflight`) first.** Rejected. The repo already added two commands on 2026-04-21 without README updates; adding another compounds drift. The design goal says "this repo should stay small."

**Alternative C — Write the security fix first, before the truth pass.** Tempting, and would be correct if production secrets were at risk. The quota credential storage is a developer-machine concern; the README-drift issue affects every first-run experience. Truth pass first is cheaper and unblocks the corpus itself.

**Alternative D — Skip research plans and implement everything.** Rejected. `LlmBackend` consolidation (008) and command-lifecycle reconciliation (012) both have non-mechanical decision surfaces. Implementing without a scoped research step risks writing the wrong abstraction.

## Not doing (carried forward from Genesis Report)

These are explicitly out of scope for the corpus this index covers. Any of them may become in-scope later via a separate corpus pass.

- Rewrite or workspace-split the crate.
- Introduce a new command.
- Refactor `parallel_command.rs` or `generation.rs` beyond extracting shared helpers.
- Build a web UI or JSON API front end.
- Add cross-repo refactoring features beyond what `--reference-repo` already does.
- Replace `anyhow` with a typed error scheme.
- Encrypt quota credentials at rest (keep it on the backlog, out of this pass).

## Living-document reminder

Every file under `genesis/plans/` must be maintained as a living document. The ExecPlan standard is declared inline in each plan file, matching the conventional header that a root `PLANS.md` would carry if one existed. If a root `PLANS.md` is added later, plans under `genesis/plans/` should be re-reconciled against it.
