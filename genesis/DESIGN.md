# DESIGN — autodev

## Design scope

`autodev` has no graphical UI. Its user-facing surfaces are the command-line interface (`auto --help`, error messages, streamed stdout) and the persistent artifact file formats (`specs/*.md`, `IMPLEMENTATION_PLAN.md`, `QA.md`, `HEALTH.md`, `SHIP.md`, `REVIEW.md`, `ARCHIVED.md`, `WORKLIST.md`, `LEARNINGS.md`, `COMPLETED.md`, `audit/MANIFEST.json`, `genesis/*.md`, `bug/`, `nemesis/`). These are developer-facing surfaces, and they count as design — operators read them, hand-edit them, and diff them across runs.

This document captures the design principles actually in the code today, the gaps against those principles, and the stance recommended for the next iteration.

## Information architecture of the CLI surface

Commands are organized by lifecycle, not by model or by subsystem. The README lays out seven lifecycle commands (`corpus → gen → loop → qa → health → review → ship`) and six side lanes (`reverse`, `bug`, `nemesis`, `quota`, `parallel`, `qa-only`). The code adds three more commands (`steward`, `audit`, `symphony`) that do not fit cleanly into that split without editorial effort.

Pragmatic clustering for the current code:

- **Planning.** `corpus`, `gen`, `reverse`, `steward`.
- **Execution.** `loop`, `parallel`.
- **Quality.** `qa`, `qa-only`, `health`, `review`, `audit`.
- **Hardening / investigation.** `bug`, `nemesis`.
- **Release.** `ship`.
- **Infrastructure.** `quota`, `symphony`.

This clustering is not expressed anywhere in `--help`, only in the prose of the README (which is also out of date). An operator running `auto --help` sees a flat list.

**Design gap.** The CLI help should group commands by cluster (clap supports `[command(next_help_heading)]`) so operators can triage by intent rather than reading a flat 16-item list.

## State coverage (artifacts as an information system)

The artifact set forms a small but coherent information system. Each file has a role and a lifecycle:

| Artifact | Written by | Read by | Lifecycle |
|---|---|---|---|
| `genesis/` corpus | `corpus` | Operator, `gen` | Disposable; wiped on fresh `corpus` run (archived to `.auto/fresh-input/`) |
| `specs/` | `gen`, `reverse`, `nemesis` (snapshot) | Operator, `loop`, `parallel`, `qa`, `review`, `ship` | Durable; dated slugs; dedupes by stem within a day |
| `IMPLEMENTATION_PLAN.md` | `gen`, `loop` (removes done), `nemesis` (appends) | `loop`, `parallel`, `review`, `steward` | Durable; append/consume queue |
| `COMPLETED.md` | `loop` (appends), `review` (moves to REVIEW) | `review` | Durable; transitional |
| `REVIEW.md` | `loop` (appends handoff), `review` (consumes) | `review` | Durable; queue |
| `ARCHIVED.md` | `review` | Operator | Durable; append-only |
| `WORKLIST.md` | `review`, `audit`, `qa`, `ship` | Operator | Durable; append + sometimes consume |
| `LEARNINGS.md` | `review`, `qa`, `ship` | Operator | Durable; append-only |
| `QA.md` | `qa`, `qa-only` | Operator, `ship` | Durable; overwritten each pass |
| `HEALTH.md` | `health` | Operator, `ship` | Durable; overwritten each pass |
| `SHIP.md` | `ship` | Operator | Durable; overwritten on ship-prep |
| `bug/` | `bug` | Operator, `bug --resume` | Disposable; archived on fresh run |
| `nemesis/` | `nemesis` | Operator, `nemesis --resume` is **not** implemented explicitly | Disposable; archived on fresh run |
| `audit/` | `audit` | `audit` (resume via manifest hash) | Durable between runs |
| `.auto/` | All commands | All commands | Runtime state/logs |

**Strength.** The lifecycle of each file is consistent across commands. `atomic_write` is applied uniformly.

**Gap.** Three artifact sets use three different resume mechanisms: `bug/` resumes via chunk directories, `audit/` resumes via `MANIFEST.json` hashes, `nemesis/` has no resume. There is no shared pattern; each command invented its own.

## User journeys (what operators actually do)

### Journey 1 — Fresh planning on a drifting repo
`auto corpus` → inspect `genesis/ASSESSMENT.md` → `auto gen` → review generated `specs/` + `IMPLEMENTATION_PLAN.md` → `auto loop`.

Observable artifacts tell the story without requiring re-running the tool. Strong.

### Journey 2 — Mid-flight reconciliation
`auto steward`. The command writes `DRIFT.md`, `HINGES.md`, `RETIRE.md`, `HAZARDS.md`, `STEWARDSHIP-REPORT.md`, `PROMOTIONS.md`. These are not in the README artifact list.

**Gap.** Six new artifact types introduced by a command that is not in the README. Operators discover them only by running the command.

### Journey 3 — File-by-file audit
`auto audit` after authoring `audit/DOCTRINE.md`. Manifest-driven; partial-run resume via content-hash matching.

**Gap.** `audit/DOCTRINE.md` shape is documented in `docs/audit-doctrine-template.md` but not linked from the README. The path from "I want to use `auto audit`" to "I wrote my own `DOCTRINE.md`" is undocumented.

### Journey 4 — Release the branch
`auto qa` → `auto health` → `auto review` → `auto ship`. Each step writes a durable report. `ship` reconciles docs, version, and changelog, and creates or refreshes a PR via `gh`.

Strong — this is the best-documented and most linear path.

### Journey 5 — Quota-aware long run
`auto quota select codex` → `auto loop` or `auto parallel`. Quota router handles rotation silently during the run.

**Gap.** On-disk credentials are plaintext with default umask. An operator with shared access to `~/.config/quota-router/` could read tokens.

## Accessibility and output behavior

The CLI emits colored output via the `console` crate. There is no explicit `--no-color` honoring of `NO_COLOR` env var in the code surveyed, but `console::Term` honors it through its detection logic by default.

Progress is streamed from the agent CLIs in real time (`codex_stream.rs`), including phase markers, token usage, and tool-call summaries. This is the right choice for long-running commands.

**Design gap.** There is no machine-readable output mode. Every command emits prose to stdout interleaved with agent output. A second operator or CI job consuming `auto qa` results would need to parse `QA.md`, not the run's stdout.

## Responsive behavior / scaling

The tool is a CLI; responsiveness is about keeping the operator informed during long runs, not viewport adaptation.

- **Phase markers** emitted for planning commands (README:138-141) are implemented in `corpus.rs` and the generation loop.
- **Lane logs** for `auto parallel` tail `.auto/parallel/<session>/lane-*/stdout.log` into tmux windows.
- **Elapsed timings** are logged for each phase in `bug_command.rs`, `nemesis.rs`, `generation.rs`.

These are sufficient for a single-operator terminal use case. They would be insufficient for embedding `auto` into a CI job without additional structured output.

## AI-slop risk

This is specifically a risk for a tool whose artifacts are agent-generated.

- **Spec duplication.** `generation.rs` guards against duplicate specs via dated filename slugs and a `SpecSyncSummary` that counts skips, but spot-checks suggest that the `skipped_count` is advisory; two semantically identical specs emitted in the same run may both be written if they differ in whitespace.
- **Plan re-introduction.** `review_command.rs` has stale-batch detection (`StaleBatchTracker`), but if the reviewer re-adds an item that was just archived, the tracker cannot prevent it.
- **Generated-and-forgotten docs.** `auto audit` can produce `patch.diff`, `worklist-entry.md`, `retire-reason.md` per file. On a large repo, this is hundreds of artifacts the operator must review; there is no summary page listing what was produced in this run.
- **Model-output quality.** No built-in check that the agent's final output is coherent — only the final-text extraction (`kimi_backend.rs::extract_final_text`, `codex_stream.rs` renderers). A degenerate run that produces empty-but-valid JSON writes empty artifacts.

**Mitigation posture.** Rely on review-style commands (`auto review`, `auto audit`, `auto nemesis`) to catch slop in downstream code, and on the evidence-status sections required by `auto gen` / `auto reverse` specs to catch slop in planning artifacts.

## Decisions to recommend

1. **Group `auto --help` by cluster.** Use clap's `next_help_heading` to present Planning / Execution / Quality / Hardening / Release / Infrastructure, matching the README.
2. **Add a one-line "what is this" column to the README inventory.** Sixteen rows, one sentence each, before the detailed per-command sections. This is a smaller change than rewriting the detailed guide and fixes the primary discovery gap.
3. **Document the steward / audit / symphony artifact inventories.** Each command that writes files beyond the standard set deserves an "Artifacts" callout in its README section.
4. **Restrict credential file permissions.** `chmod 0o600` on every file under `~/.config/quota-router/profiles/*` at write time. Defer "encryption at rest" as a separate product decision.
5. **Provide a `--json` output mode for `qa`, `health`, `ship`, `audit`.** Enables CI integration and external dashboards without parsing markdown.
6. **Offer an `audit/DOCTRINE.md` scaffold command.** `auto audit --init` copies `docs/audit-doctrine-template.md` into `audit/DOCTRINE.md` for the operator to edit. Shortens first-run time.

## What we are explicitly not designing for right now

- A web UI or desktop front-end.
- Multi-user concurrency within a single repo.
- Non-Git VCSes.
- An auto-updater for the tool itself.
- Embedding `auto` as a library; it remains a binary CLI.
