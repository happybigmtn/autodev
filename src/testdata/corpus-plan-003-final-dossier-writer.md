# Implement fail-closed final go/no-go dossier writer

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

The repository root `PLANS.md` is checked in. This document must be maintained in accordance with that standard.

## Purpose / Big Picture

After this plan, the autonomy repo has a single deterministic file at `ops/evidence/rsociety/final-go-no-go-current.json` (and `.md`) that an operator can hand to release. The file is produced by a Python script `scripts/ops/write-rsociety-final-go-no-go-dossier.py` that reads the eight input JSON files contracted in Plan 002, applies the decision precedence rules in order, and emits the dossier with `decision`, `blockers`, `carries`, `inputs`, and `source_refs`.

An operator runs the writer with `python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py` and gets the current dossier. Or runs `python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py --self-test` to verify the reducer logic against canned inputs.

The user-visible behavior: instead of reading 8+ JSON files and 6+ prose surfaces to decide release, the operator opens one file. The file fails closed: any red or stale input produces `no_go`. There is no path by which a fixture or local row promotes to Go.

## Requirements Trace

R2 (master) — Produce a current `ops/evidence/rsociety/final-go-no-go-current.{json,md}` artifact that represents the production-go decision.

R2c — Fail-closed on any red or stale input.

R2d — Reject fixture/local rows from promoting to Go.

R2e — Retired bridge / cash-out scope is not an active blocker.

## Scope Boundaries

This plan does **not** change the contract decided in Plan 002. If the contract is incomplete, return to Plan 002.

This plan does **not** change the input writers (composition packet, strict audit, readiness, code creation, release proof, external CI, no-ship ledger). Each is owned by its existing writer.

This plan does **not** render the dossier in any UI surface. Plan 022 (forward-look) carries the rendered widget.

This plan does **not** broaden Code Creation production-deploy authority.

This plan does **not** modify NS-RSOCIETY-001 or other no-ship ledger rows.

## Progress

- [x] (2026-05-14 13:02Z) Plan authored.
- [ ] Writer script scaffolded with argument parser and `--self-test` flag.
- [ ] Decision precedence rules implemented as ordered functions.
- [ ] JSON emission and `.md` companion implemented.
- [ ] Self-test cases written: all-green, strict-audit-red, readiness-stale, missing-input, fixture-row, broadened-scope.
- [ ] Writer run against current inputs; current dossier `decision=no_go` due to strict audit and stale readiness.
- [ ] Docs/process guard run.

## Surprises & Discoveries

- Observation: The strict audit's nested live snapshot is the canonical source for `success`; the envelope-level `success` may emit a different value in some run modes.
  Evidence: `ops/evidence/autonomous-completion/final.json` `.evidence.autonomous_completion_progress_history.summary.latest_snapshot.audit_success=false`.
  Implication: The reducer must read the nested live snapshot.

## Decision Log

- Decision: Implement the writer in Python 3 to match the existing `scripts/ops/write-rsociety-*.py` family.
  Rationale: Consistency with the existing writer style; the harness engineering standards guard runs against Python writers; the operator already has Python 3 in the toolchain.
  Date/Author: 2026-05-14 / corpus author

- Decision: Emit the dossier as both JSON and Markdown. JSON is the machine-readable truth; Markdown is the human-readable companion that an operator can read at a glance.
  Rationale: Matches `ops/evidence/rsociety/full-gdd-composition-current.{json,md}` and other paired artifacts.
  Date/Author: 2026-05-14 / corpus author

- Decision: Implement `--self-test` with at least seven canned input fixtures.
  Rationale: The reducer's correctness is the whole point. Self-test catches regression. PLAN-120526-03 requires `write-rsociety-final-go-no-go-dossier.py self_test`.
  Date/Author: 2026-05-14 / corpus author

## Outcomes & Retrospective

None yet.

## Context and Orientation

The writer script lives at `scripts/ops/write-rsociety-final-go-no-go-dossier.py`. It follows the same shape as `scripts/ops/write-rsociety-full-gdd-composition-dossier.py` (already in the repo). Common imports: `argparse`, `dataclasses`, `json`, `pathlib`, `datetime`, `hashlib`, `sys`.

Inputs read from `ops/evidence/rsociety/`, `ops/evidence/autonomous-completion/`, `ops/evidence/readiness/`, `ops/evidence/code-creation/`, `.auto/autonomous-remediation/`, and `ops/no-ship-ledger.md`.

Outputs written to `ops/evidence/rsociety/final-go-no-go-current.json` and `ops/evidence/rsociety/final-go-no-go-current.md`.

Key terms:

- **Reducer**: a pure function that takes a dict of inputs and returns a dict (the dossier).
- **Fail closed**: when in doubt, return `no_go`.
- **Conditional-Go**: would be Go except for one or more named carries that the operator has approved via an explicit carry artifact.
- **Fixture row**: an input record marked `evidence_profile: "fixture"` or `"local"`. Cannot promote to Go.

## Plan of Work

1. Read `scripts/ops/write-rsociety-full-gdd-composition-dossier.py` to copy the style: top docstring, argparse, dataclasses, main, `if __name__ == "__main__"`.

2. Author `scripts/ops/write-rsociety-final-go-no-go-dossier.py` with:
   - Top docstring naming the contract section in `docs/contracts/runtime-authority-matrix.md`.
   - Argparse with flags: `--self-test`, `--output-json`, `--output-md`, `--evidence-root`, `--verbose`.
   - A `Reducer` dataclass holding the precedence rules.
   - A `compute_decision(inputs)` function returning a dossier dict.
   - A `render_markdown(dossier)` function returning the markdown text.
   - Helpers for reading each input safely (missing file → blocker; bad JSON → blocker).
   - A `main()` that loads inputs, computes the decision, writes JSON and MD, prints the decision.
   - A `run_self_test()` that exercises seven cases and asserts the resulting decisions.

3. Run the writer against current evidence. With strict audit red and readiness stale, expected: `decision=no_go` with blockers naming both.

4. Run the self-test. Expected: every case passes.

5. Add the writer name to `scripts/README.md` (decision in Plan 002 already named the file; this step adds the run command).

6. Verify the dossier file paths exist.

7. Run `scripts/check-harness-engineering-standards.sh`.

The decision precedence rules (from Plan 002) are implemented as a list of `(label, predicate, blocker_message)` triples that the reducer iterates in order; the first matching rule wins.

## Implementation Units

U1. Scaffold the writer script with arg parser, dataclasses, and main entry point.
   Requirements advanced: R2.
   Dependencies: Plan 002 (contract).
   Files to create: `scripts/ops/write-rsociety-final-go-no-go-dossier.py`.
   Files to modify: none.
   Tests to add: a self-test inside the script (`--self-test`).
   Approach: Copy the structure of `scripts/ops/write-rsociety-full-gdd-composition-dossier.py`, replace the body. Define dataclasses for `Input`, `Dossier`, `Blocker`, `Carry`. Use stdlib only.

U2. Implement the decision precedence rules.
   Requirements advanced: R2b, R2c.
   Dependencies: U1.
   Files to modify: the writer script.
   Tests to add: self-test cases for each rule.
   Approach: Encode each rule from Plan 002 as a function that receives the loaded inputs and returns either `None` (rule did not match) or a `(decision, blockers, carries)` tuple.

U3. Implement JSON and Markdown emission.
   Requirements advanced: R2a (schema fields).
   Dependencies: U2.
   Files to modify: the writer script.
   Tests to add: self-test asserts both files are written and the JSON validates against schema id `autonomy.rsociety_final_go_no_go.v1`.
   Approach: Write JSON with `json.dumps(..., indent=2, sort_keys=True)`. Write MD as a templated string interpolating decision, blockers, carries, inputs.

U4. Write seven self-test cases.
   Requirements advanced: R2 reducer correctness.
   Dependencies: U2, U3.
   Files to modify: the writer script.
   Tests to add: each self-test case asserts the resulting decision.
   Test scenarios:
   - All-green: every input fresh and `state=go` → `decision=go`.
   - Strict audit red: strict audit `audit_success=false` → `decision=no_go`, blocker `strict_autonomous_completion_audit_red:next_action_runnable_run_stale`.
   - Readiness stale: readiness `observed_at` older than `freshness_sla_seconds` → `decision=no_go`, blocker `readiness_bundle_stale:<observed_at>`.
   - Missing input: composition file absent → `decision=no_go`, blocker `input_missing:ops/evidence/rsociety/first-earned-cycle-live-composition-current.json`.
   - Fixture row promoted: composition `evidence_profile=fixture` → `decision=no_go`, blocker `fixture_row_promoted`.
   - Broadened scope: code creation scope `production:rsociety-web,rsociety-tui` → `decision=no_go`, blocker `code_creation_scope_broadened`.
   - Conditional-go: composition green, strict audit red, with explicit-carry artifact present → `decision=conditional_go`, carries listing the strict-audit carry.

U5. Run the writer against current evidence and capture the produced dossier as the current artifact.
   Requirements advanced: R2.
   Dependencies: U1-U4 complete.
   Files to create: `ops/evidence/rsociety/final-go-no-go-current.json`, `ops/evidence/rsociety/final-go-no-go-current.md`.
   Tests to add: none (file existence is the proof).
   Approach: `python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py`.

U6. Update `scripts/README.md` and run docs guard.
   Requirements advanced: discoverability.
   Dependencies: U5.
   Files to modify: `scripts/README.md`.
   Tests to add: none.
   Approach: Add the run command and the self-test command.

## Concrete Steps

Run from the repository root.

    cat scripts/ops/write-rsociety-full-gdd-composition-dossier.py | head -80

Read the style of the closest existing writer.

    sed -n '1,60p' docs/contracts/runtime-authority-matrix.md | rg -n 'final-go-no-go'

Confirm the contract section from Plan 002 exists.

Now author the writer script. After the script is written, run:

    python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py --self-test

Expected: exit 0 with output like `final-go-no-go self_test passed (7/7 cases)`.

    python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py

Expected: writes `ops/evidence/rsociety/final-go-no-go-current.{json,md}`. With current inputs, expected outcome is `decision=no_go` with at least two blockers: `strict_autonomous_completion_audit_red:next_action_runnable_run_stale` and `readiness_bundle_stale:2026-05-08T01:43:52Z`.

    jq '{decision, blockers, carries, inputs}' ops/evidence/rsociety/final-go-no-go-current.json

Expected: a structured object with the decision, blockers, carries, and input file paths.

    scripts/check-harness-engineering-standards.sh

Expected: exits 0 and prints "harness engineering standards check passed".

## Validation and Acceptance

Acceptance phrased as observable behavior:

- `python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py --self-test` exits 0 and reports all 7 cases pass. This test must fail before this plan (the script does not exist) and pass after.
- `python3 scripts/ops/write-rsociety-final-go-no-go-dossier.py` writes `ops/evidence/rsociety/final-go-no-go-current.json` and `ops/evidence/rsociety/final-go-no-go-current.md`. The JSON validates against schema id `autonomy.rsociety_final_go_no_go.v1`.
- The current dossier (against today's inputs) emits `decision=no_go` with blockers naming the strict audit and the stale readiness.
- A reviewer can read the dossier and trace every blocker to a concrete file path.
- `scripts/check-harness-engineering-standards.sh` exits 0.

## Idempotence and Recovery

The writer is idempotent. Re-running overwrites the dossier files. The `--self-test` flag exercises canned inputs and does not touch the output files.

Partial completion recovery: if U1-U4 ship but U5 fails (e.g., a missing input causes the run to abort), the partially-built writer can still self-test. Once U5 succeeds, the dossier files appear.

No destructive operations. The writer reads inputs and writes outputs in a known directory.

## Artifacts and Notes

Evidence consulted while authoring this implementation plan:

- `scripts/ops/write-rsociety-full-gdd-composition-dossier.py` (style reference; already in repo).
- `specs/120526-final-go-no-go-dossier.md` (spec).
- `docs/contracts/runtime-authority-matrix.md` (contract authored in Plan 002).
- `ops/evidence/autonomous-completion/final.json` (red strict audit).
- `ops/evidence/readiness/latest.json` (stale readiness).
- `ops/evidence/rsociety/full-gdd-composition-current.json` (green composition).
- `ops/evidence/code-creation/production-authority-current.json` (scoped Go).

After the writer runs, capture the dossier head:

    head -30 ops/evidence/rsociety/final-go-no-go-current.md

and paste it into this section so reviewers can verify the format.

## Interfaces and Dependencies

Module / function names introduced:

- `scripts.ops.write_rsociety_final_go_no_go_dossier.compute_decision(inputs: dict) -> dict`
- `scripts.ops.write_rsociety_final_go_no_go_dossier.render_markdown(dossier: dict) -> str`
- `scripts.ops.write_rsociety_final_go_no_go_dossier.run_self_test() -> None`
- `scripts.ops.write_rsociety_final_go_no_go_dossier.main() -> int`

Schema:

- `autonomy.rsociety_final_go_no_go.v1`.

Input paths (read-only):

- `ops/evidence/rsociety/first-earned-cycle-live-composition-current.json`
- `ops/evidence/rsociety/full-gdd-composition-current.json`
- `ops/evidence/autonomous-completion/final.json`
- `ops/evidence/readiness/latest.json`
- `ops/evidence/code-creation/production-authority-current.json`
- `.auto/autonomous-remediation/latest-release-proof.json`
- `.auto/autonomous-remediation/latest-external-ci-dossier.json`
- `ops/no-ship-ledger.md`

Output paths (write-only):

- `ops/evidence/rsociety/final-go-no-go-current.json`
- `ops/evidence/rsociety/final-go-no-go-current.md`

External tools used:

- `python3` 3.13.
- `jq` for evidence-file inspection.
- `scripts/check-harness-engineering-standards.sh`.

Plans that depend on this one:

- Plan 005 (checkpoint): reviewer reads the dossier.
- Plan 015 (auth metadata to consumers): not directly dependent, but auth posture is one of the dossier inputs (via strict audit).
- Plan 018 (checkpoint): cross-surface First Earned full-go reads the dossier.
- Plan 022 (forward-look): rendered dossier widget.
