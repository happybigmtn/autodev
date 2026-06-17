# `auto orchestrate` — Definition-of-Done supervisor (design)

Productizes the super-orchestrator pattern run by hand across the fleet. The one
concept absent from `super`/`loop`/`parallel`/`pilot`: an explicit **Definition-of-Done
acceptance gate** that (a) verifies the repo *meets* a stated DoD, (b) emits a live
dashboard artifact, and (c) loops execution until every criterion is met.

`super` = plan→execute (gates the *plan*). `orchestrate` = assess→execute→re-assess
(gates the *repo against a DoD*), reusing `super`/`loop` as the execution engine.

## DoD spec — `.auto/dod.json` (or `--dod <path>`)
```json
{
  "statement": "A real user can <the promise> ...",
  "criteria": [
    { "id": "fmt",       "label": "fmt gate green",     "verify": "cargo fmt --check" },
    { "id": "clippy",    "label": "clippy -D warnings",  "verify": "cargo clippy --all-targets --all-features -- -D warnings" },
    { "id": "tests",     "label": "test suite green",    "verify": "cargo test --bin auto" },
    { "id": "first-run", "label": "turn-key first-run",  "gate": "operator", "note": "user supplies own model creds" }
  ]
}
```
- `verify`: shell command run in repo root; exit 0 ⇒ criterion `done`, else `todo`
  (capture tail of output as the note).
- `gate: "operator"`: not auto-verifiable / human-gated ⇒ status `blocked`; surfaced
  separately and excluded from the auto denominator (honest about what a tool can't self-certify).

## Phases
1. **ASSESS** — run each criterion's `verify` (skip `operator`). Compute
   `pct = round(100 * done / (total - blocked))`.
2. **EMIT** — write dashboard-shape `dod.json` (`{statement, criteria:[{label,status,note}]}`,
   the exact shape the orchestration-dashboard already consumes), a human `DOD-STATUS.md`
   (pct + ✓/○/🔒 per criterion + last failing line), and, if `--dashboard <dir>`, merge there.
3. **EXECUTE** (only with `--execute`, only if pct < 100) — build a focus string from the
   unmet criteria + their failing output; invoke the existing engine
   (`auto loop` default, `auto super` with `--engine super`), bounded by `--max-loops`.
   Re-assess + re-emit after each pass. Stop at 100%, at `--max-loops`, or on no-progress
   (pct unchanged two passes running). Code commit/push is the engine's job (existing
   per-repo policy: push after every commit, never force-push).

## Exit code
0 iff every non-blocked criterion is `done` (lets CI/automation gate on real done-ness).

## Scope for this build (minimal, testable)
- Phase 1+2 fully (spec parse, run verifies, compute pct, emit JSON + markdown, exit code) — unit tested.
- Phase 3 as a thin, bounded wrapper over `run_loop`/`run_super` (focus-string builder unit-tested;
  the model run itself is integration/manual).

## Integration points (verified)
- `src/cli/mod.rs`: add `Orchestrate(OrchestrateArgs)` to `enum Command` + an args struct (clap derive).
- `src/main.rs`: `Command::Orchestrate(args) => orchestrate_command::run_orchestrate(args).await,`
- New `src/orchestrate_command/` module; reuses `util::{git_repo_root, atomic_write}`,
  `super_command::run_super`, `loop_command::run_loop`.
