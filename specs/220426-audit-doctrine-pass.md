# Specification: `auto audit` — file-by-file doctrine audit

## Objective

Keep `auto audit` a deterministic file-by-file check against an operator-authored `audit/DOCTRINE.md`, producing per-file verdicts, safe patches applied atomically, a manifest that supports content-hash resume, and worklist handoff for items that cannot be auto-fixed. The command must stay agnostic about what "good" means (the doctrine decides), must never silently widen scope beyond declared files, and must have test coverage on verdict application, manifest reconciliation, and escalation pathways before any further feature expansion.

## Evidence Status

### Verified facts (code)

- `src/main.rs:81-87` doc-comments `Audit` as "File-by-file audit of a mature codebase against an operator-authored doctrine. Produces per-file verdicts (CLEAN / DRIFT / SLOP / RETIRE / REFACTOR), applies safe fixes atomically, batches large work into WORKLIST.md, and resumes cleanly from partial runs via a manifest."
- `src/main.rs:850-912` `AuditArgs` defaults: `model = "k2.6"`, `reasoning_effort = "high"`, `escalation_model = "gpt-5.4"`, `escalation_effort = "high"`, `codex_bin = "codex"`, `kimi_bin = "kimi-cli"`, `pi_bin = "pi"`, `use_kimi_cli = true`.
- `AuditResumeMode` variants (`src/main.rs:914-925`): `Resume` (default), `Fresh`, `OnlyDrifted`.
- `audit_command.rs:1050` bails with "auto audit currently requires --use-kimi-cli" when `use_kimi_cli` is false.
- `FileVerdict` struct at `audit_command.rs:131` fields: `verdict: String`, `patch_diff: Option<String>`, `worklist_entry: Option<String>`, `retire_reason: Option<String>`.
- Verdict variants in the auditor doctrine (`audit_command.rs:9` or similar const list): `CLEAN`, `DRIFT-SMALL`, `DRIFT-LARGE`, `SLOP`, `RETIRE`, `REFACTOR`.
- `touched_paths` and `escalate` fields exist on the parsed verdict JSON but are not currently consumed downstream (`corpus/ASSESSMENT.md` §"Half-built" and §"Tech debt inventory": "`FileVerdict::touched_paths` and `FileVerdict::escalate` are parsed but never consumed").
- Manifest hash/resume tracked fields include `content_hash`, `doctrine_hash`, and `rubric_hash` per file under `audit/MANIFEST.json` (per corpus and current audit implementation inventory).
- `audit_command.rs` has 3-5 tests covering glob match and sha256 only (corpus ASSESSMENT table); no test covers verdict application, manifest reconciliation, or escalation.
- Per-file artifacts land under `audit/files/<hash-prefix>/{verdict.json,patch.diff,response.log,prompt.md,worklist-entry.md,retire-reason.md}` per `corpus/SPEC.md` §"Artifact shapes".

### Verified facts (docs)

- `docs/audit-doctrine-template.md` exists as the doctrine file template (per corpus).
- README does not document `auto audit` at all in the current inventory (`corpus/SPEC.md` §"Command surface").
- `corpus/plans/004-audit-verdict-test-harness.md` is the plan that adds the test harness required below.

### Recommendations (corpus)

- Add an `auto audit --init` scaffold command that copies `docs/audit-doctrine-template.md` into `audit/DOCTRINE.md` for first-run operators (`corpus/DESIGN.md` §"Decisions to recommend").
- `--json` output shape for per-run summary to support CI dashboards (`corpus/DESIGN.md` §"Decisions to recommend").
- Wire `FileVerdict::touched_paths` and `FileVerdict::escalate` into the apply pipeline once Plan 004 locks existing behavior with tests (`corpus/ASSESSMENT.md` §"Tech debt inventory"; `corpus/plans/004-*`).

### Hypotheses / unresolved questions

- Whether `DRIFT-LARGE` ever triggers an automatic Codex escalation today or always routes to `WORKLIST.md` is unverified; the escalation-model flag exists but the wiring is partially implemented.
- Whether a doctrine change detected via `doctrine_hash` drift forces a full re-audit of all files or only flags the manifest as stale is not source-verified in this pass.

## Acceptance Criteria

- `auto audit` requires a readable `audit/DOCTRINE.md` in the repo root; missing or empty doctrine yields a clear non-zero exit.
- `auto audit` defaults to Kimi (`k2.6` at `high`) via `kimi-cli --yolo`; explicit `--no-use-kimi-cli` requires the operator to set up the PI fallback or the command exits non-zero with the "currently requires --use-kimi-cli" message.
- `auto audit --resume` (default) reads `audit/MANIFEST.json` and re-audits only files where `content_hash` or `doctrine_hash` has drifted since the previous run.
- `auto audit --resume-mode fresh` (or equivalent flag name) archives the current manifest and starts a fresh full pass.
- `auto audit --resume-mode only-drifted` re-audits only files with drifted hashes and skips all files never audited.
- For each audited file, the command writes `audit/files/<hash-prefix>/verdict.json` plus the relevant artifact files: `patch.diff` when `patch_diff` is set, `worklist-entry.md` when `worklist_entry` is set, `retire-reason.md` when `retire_reason` is set; `response.log` and `prompt.md` always.
- Verdict application is atomic: either both the verdict JSON and any produced patch land on disk, or neither.
- `CLEAN` verdicts produce a verdict JSON but no patch, no worklist entry, no retire reason.
- `DRIFT-SMALL` verdicts with a patch apply the patch to the tracked file through `git apply` (or equivalent) under a checkpoint commit; verification is re-auditing does not re-flag the same drift.
- `DRIFT-LARGE` and `REFACTOR` verdicts emit `worklist-entry.md` and append to `WORKLIST.md` rather than auto-applying.
- `RETIRE` verdicts emit `retire-reason.md` and queue the file for operator review; `auto audit` does not delete files on its own.
- `SLOP` verdicts emit a worklist entry describing the slop and never auto-patch.
- `FileVerdict::touched_paths` and `FileVerdict::escalate` are either (a) consumed by the apply pipeline (future) or (b) explicitly marked as reserved-for-future so test coverage does not depend on them yet.
- Manifest reconciliation test: after an intentional doctrine edit, `auto audit --resume` re-audits every file whose per-file doctrine hash has drifted.
- Test harness covers: manifest load/save, hash computation, verdict application for `CLEAN` / `DRIFT-SMALL` / `REFACTOR`, glob matching, and the `--use-kimi-cli` bail.

## Verification

- `cargo test -p autodev audit_command` passes existing glob/sha256 tests and the new verdict-application / manifest-reconcile / escalation tests specified by `corpus/plans/004-audit-verdict-test-harness.md`.
- Fixture repo with `audit/DOCTRINE.md` + a single file whose doctrine requires a specific import ordering; run `auto audit` with a stubbed Kimi CLI that produces a pre-recorded `DRIFT-SMALL` verdict + patch; assert the patch is applied and the manifest updated.
- Re-run `auto audit --resume` on the same repo without doctrine or code changes; assert no files are re-audited.
- Edit `audit/DOCTRINE.md`; re-run `auto audit --resume`; assert all files are re-audited (or the ones whose per-file doctrine hash now differs).
- Negative test: run `auto audit --no-use-kimi-cli` without PI fallback; assert the documented exit message and non-zero exit.

## Open Questions

- Should `auto audit --init` ship as part of this spec, or be deferred? `corpus/DESIGN.md` recommends it; current code does not implement it.
- Does a `retire-reason.md` ever trigger a delete in a later `auto review` pass, or does the operator always perform the deletion manually?
- Is there a limit on total per-file artifacts written in one run (for example, 1,000-file repos produce 1,000+ artifact dirs)? `corpus/DESIGN.md` §"AI-slop risk" notes there is no summary page; should one be required?
- How should verdict JSON shape evolve to match the declared `touched_paths` / `escalate` fields — as optional fields that tests tolerate, or as required fields that break schema compatibility?
