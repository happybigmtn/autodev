# Product Specification

## Product Frame

`auto` is a Rust CLI for autonomous repository work. It turns a repository root into an operator-controlled development system: it reviews the codebase, writes planning corpora, generates root execution queues, runs model-backed workers, checks evidence, audits quality, and gates releases.

The product is not a web app. Its meaningful user-facing surfaces are terminal output, markdown ledgers, generated corpora, receipts, logs, and installed CLI help.

## Primary Behaviors

- `auto corpus` reviews the current repository and writes a planning corpus under `genesis/` by default.
- `auto gen` turns a planning corpus into root specs and implementation ledgers.
- `auto super` composes corpus, design gate, functional review, generation, execution gate, and optional parallel execution.
- `auto parallel` schedules root queue rows into isolated lanes, lands evidence-backed work, and reconciles `IMPLEMENTATION_PLAN.md` and `REVIEW.md`.
- `auto loop` runs a serial queue execution loop.
- `auto design`, `auto qa-only`, and `auto health` produce report-only artifacts with write-boundary checks.
- `auto qa`, `auto review`, `auto audit`, `auto audit --everything`, `auto nemesis`, and `auto book` provide quality, audit, adversarial, and knowledge-corpus workflows.
- `auto quota` manages model account profiles and quota-aware backend execution.
- `auto ship` evaluates release readiness using receipts, QA/health evidence, blockers, rollback notes, and PR state.
- `auto doctor`, `auto --help`, and installed binary smoke are the first-run operator surfaces.

## Current Control Truth

Runtime truth lives in `src/`. Active planning truth lives in `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `ARCHIVED.md`, `COMPLETED.md`, `WORKLIST.md`, root `specs/`, and verification receipts. `genesis/` is planning input and is subordinate until an operator promotes slices into the root queue.

No root `PLANS.md` file exists in this checkout. Numbered plans in this corpus therefore use the requested full ExecPlan envelope, but they do not replace the active root ledger set.

## Near-Term Product Direction

The next 14-day campaign should make `auto` production-trustworthy for autonomous parallel execution:

1. Restore and protect the planning corpus and saved-state boundaries.
2. Harden quota credential profile containment and failover semantics.
3. Make scheduler eligibility fail closed on stale or contradictory plan truth.
4. Bind completion and release evidence to current commit, dirty state, plan hash, artifact hash, and explicit evidence class.
5. Normalize report-only, verdict, QA, health, audit, nemesis, and ship contracts.
6. Improve first-run DX so a new operator can see a meaningful success moment quickly.
7. Add performance and reliability proof around large queues, stale lane recovery, and installed binary behavior.

## Requirements

- R1: Planning roots used by corpus/generation must be inside approved repository-relative locations or explicitly supplied and confirmed.
- R2: `genesis/` must not be left empty after a failed corpus generation.
- R3: Loading a planning corpus must reject an empty numbered plan set.
- R4: Quota account names must be safe slugs and profile paths must remain under the quota profile directory.
- R5: Quota failover must not retry write-capable model work after detected progress without an explicit recovery path.
- R6: Completion evidence must not depend on contradictory ledger conventions.
- R7: Parallel dispatch must fail closed when current plan truth cannot be refreshed unless the operator selects a recovery mode.
- R8: Lane resume must bind to task body, dependency, verification, and base-commit hashes.
- R9: Receipts and declared artifacts must be root-contained and bound to current tree state.
- R10: Release gates must run after branch sync and again after model-driven ship iterations.
- R11: GO/PASS verdicts must use a shared exact parser that rejects mixed verdicts.
- R12: Report-only commands must have explicit write boundaries and honest naming.
- R13: First-run commands must produce clear, copy-pasteable, non-mutating proof.
- R14: CI must keep Rust, installed binary, help, and critical model-free workflow contracts honest.

## Non-Requirements

- Do not build a web UI in this campaign.
- Do not replace all markdown ledgers with a database.
- Do not make archived genesis snapshots authoritative.
- Do not launch `auto parallel` against an empty or unpromoted root queue.
- Do not treat model-written prose as command execution proof when host receipts are required.

## Version And Detail Ledger

- Verified from code: Cargo package version is `0.2.0`.
- Verified from code: Rust edition is `2021`.
- Verified from code: CI uses stable Rust with rustfmt and clippy, runs `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and installed binary help smoke.
- Verified from code: corpus/gen/spec/super default to `gpt-5.5` with `xhigh` planning effort in `src/main.rs`.
- Verified from code: audit file-quality merge floor is `9.0` and aspirational target is `10.0`.
- Recommendation: production dispatch should fail closed on stale plan refresh by default.
- Recommendation: quota account names should use a conservative ASCII slug contract.
- Hypothesis/open question: exact production performance targets for large queue size and lane count need benchmark evidence before becoming requirements.
- Hypothesis/open question: whether Kimi/PI can move prompts off argv depends on backend CLI capabilities and should be researched before implementation promises.
