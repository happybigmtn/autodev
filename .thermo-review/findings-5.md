# Thermo-Nuclear Findings — Partition 5: CLI backbone + quota subsystem

Reviewer scope: src/main.rs, src/codex_stream.rs, src/util.rs, src/task_parser.rs,
src/codex_exec.rs, src/claude_exec.rs, src/backend_policy.rs, src/kimi_backend.rs,
src/pi_backend.rs, src/corpus.rs, src/verification_lint.rs, src/state.rs,
src/verdict.rs, src/prompt_ethos.rs, and the 8-file quota family.

Total reviewed: ~14,346 lines across 22 files (full reads).

Verdict: better shape than line counts suggest. Of the five >1k files, four (main.rs,
codex_stream.rs, util.rs, quota_exec.rs) are large mostly due to inline test modules;
only codex_stream.rs has a genuine >1k implementation body and a real god-function.
Structural debt concentrates in three places: copy-pasted process-exec scaffolding
across backend modules, a 414-line dead module (backend_policy.rs), and the
task_parser.rs validation pile.

## P1 — File sprawl

[P1] src/codex_stream.rs:1-1851 — only genuine >1k implementation (tests start 1553,
~1552 impl lines). Four unrelated jobs: async stream pumps, four per-provider JSON
renderers, ANSI sanitization, format primitives. render_codex_stream_line (378-654) is a
277-line ~30-arm match. Split into src/codex_stream/{mod ~120, pump ~280,
render_codex ~330, render_claude ~230, render_pi ~170, format ~250}. Low coupling:
format.rs is leaf, render_* depend only on format, pump depends on renderers.

[P1] src/main.rs:1-1880 — not logic sprawl; ~1300 lines of clap Args structs + 48-line
dispatch. Move structs to src/cli/{mod ~140, args_plan ~330, args_exec ~360,
args_ops ~300, args_quota ~210}. Lower priority — inert declarations.

[P1] src/util.rs:1-1705 — NOT a junk drawer. ~808 impl lines, two cohesive clusters:
git/checkpoint (30-377 ~340) and filesystem/atomic-write (420-807 ~390) plus ~25 lines
of misc. Optional split: src/util/{mod ~30, git ~340, fsutil ~420}. Weak P1; do
codex_stream first.

[P1] src/task_parser.rs:1-1224 — ~903 impl lines. Parsing and execution-row validation
crammed together; validators (418-800 ~380 lines) touch parser only via PlanTask +
task_field_body_until_any. Split src/task_parser/{mod ~30, model ~50, parse ~440,
validate ~390}. File carries module-wide #![allow(dead_code)] at line 1 despite being
live — see P3.

[P1] src/quota_exec.rs:1-1367 — over 1k only via ~790 test lines; real impl ~575,
under the rule. Do NOT carve the implementation. Extract tests via #[path], and fix the
real defect: copy_dir_recursive/copy_file_0o600/remove_path duplicated with quota_config.

## P2 — Spaghetti

[P2] src/codex_stream.rs:378-654 — render_codex_stream_line is a 277-line god-match,
complexity far past 8. Make each event group a named fn; match becomes a dispatch table.
Highest-value P2; folds into the render_codex.rs split.

[P2] src/quota_selector.rs:148-293 — pick_best -> pick_best_by_health ->
pick_best_by_weekly is a 145-line nested-fallback cascade with four near-identical
multi-key comparators. Correct and well-tested (advisory). A single tuple sort key would
collapse three functions and remove the unreachable!() at line 212. RISKY.

[P2] src/quota_exec.rs:489-573 — run_with_quota interleaves retry loop, verdict match,
progress-sentinel bail, and a state-update closure. ~85 lines, within limit but tangled.
Pull verdict handling into handle_verdict returning ControlFlow. Advisory.

[P2] src/codex_stream.rs:102-376 — heartbeat select! loop triplicated across
capture_codex_output_with_heartbeat / capture_opencode_output / capture_pi_output.

## P3 — Code-judo

[P3] src/backend_policy.rs:1-414 — ENTIRE MODULE IS DEAD CODE. rg for backend_policy /
known_backend_policies outside the file returns zero matches. #![allow(dead_code)] at
line 1; the only consumer is its own test. A 414-line doc table compiled into the
binary. DELETE the file and its mod line. Highest-leverage deletion in the partition.

[P3] src/codex_exec.rs:471-521 vs src/claude_exec.rs:249-374 — write_worker_pid,
clear_worker_pid, log_stderr, read_stream are byte-identical duplicates (~110 lines).
Both also have structurally identical run_*_exec -> run_*_exec_with_env -> spawn_* chains
with the same quota branching. Extract src/backend_process.rs (~120 lines). SAFE for the
four leaf helpers.

[P3] src/quota_exec.rs:111-186 vs src/quota_config.rs:457-504 — copy_dir_recursive and
the file-copy primitive (copy_file_0o600 / copy_credential_file) are near-identical;
both re-derive symlink/non-regular-file rejection. Consolidate into quota/credentials.rs.
SAFE.

[P3] src/codex_stream.rs:102-376 — three heartbeat capture fns share the entire
interval/select!/elapsed body. Collapse to one generic capture_with_heartbeat taking the
renderer as a closure (~150 lines removed). RISKY (error-context strings change).

[P3] dead functions, all already #[allow(dead_code)]-tagged: stream_codex_output
(codex_stream.rs:154), capture_opencode_output (codex_stream.rs:273), validate_kimi_model
(kimi_backend.rs:241). Zero external callers. Delete all three.

[P3] src/task_parser.rs:1 — blanket #![allow(dead_code)] on a live module silences future
dead-code warnings forever. Remove it; add targeted allows only where intentional.

[P3] src/task_parser.rs:715-721 — field_value_is_none is defined but never called;
validate_execution_row_completion_artifacts inlines its own none-check (646-652). Masked
by the blanket allow. Delete or use.

[P3] src/codex_exec.rs:492-508 & src/claude_exec.rs:345-361 — log_stderr re-reads the
whole stderr log then atomic_writes it back: O(n^2) on log size. Use OpenOptions::append;
it is append-only telemetry. Fix once when deduping.

## P4 — Types & boundaries

Backend abstraction verdict: NOT a clean trait, NOT pure copy-paste — a 3-variant
SharedExecBackend enum (Codex/KimiCli/Pi) dispatched by match in codex_exec.rs, PLUS a
hand-rolled fourth backend (claude_exec.rs) that sits outside the enum and re-implements
quota branching, worker-pid writes, stderr logging, and read_stream. spawn_codex /
spawn_kimi_cli / spawn_pi / spawn_claude all share one spawn->write-pid->take-streams->
spawn-tasks->wait->clear-pid->join shape, each #[allow(clippy::too_many_arguments)] —
four copies of one process lifecycle. Recommendation: shared spawn_streamed helper in
backend_process.rs so all four collapse onto one lifecycle; keep the enum for selection;
fold Claude into the shared spawn path. Do NOT build a dyn trait — closed small set.

[P4] codex_exec.rs:200-223 / pi_backend.rs:26-35 / kimi_backend.rs:33-60 — three
overlapping lowercase-substring model-routing heuristics. A model can match is_kimi_model
AND PiProvider::detect; is_kimi_model wins by ordering only. A single ModelBackend
resolver would make routing total and testable.

[P4] quota_config.rs Provider — QuotaSelectArgs/QuotaOpenArgs/AccountsAddArgs use
provider: String parsed late in main's dispatch. Provider implements FromStr; making the
fields Provider (derive ValueEnum) moves the error to clap parse time.

Quota family verdict: the 8-file split is MOSTLY SOUND, not arbitrary — patterns/state/
usage/selector/config are genuinely distinct concerns with narrow interfaces. Two real
problems: (1) no quota/ directory — eight top-level quota_*.rs clutter src/; move to
src/quota/ (SAFE, mod rewiring); (2) the credential-copy duplication between quota_exec
and quota_config is the one arbitrary boundary. quota_accounts (109) + quota_status (175)
could merge into quota/commands.rs but are coherent as-is.

## P5 — Canonical layers

Layering largely clean: main.rs is pure parse+dispatch; backend-exec, IO/render, and
util substrate are separated. Violations:

[P5] codex_exec.rs:88 — run_codex_exec_with_env prepends builder-ethos
(with_autodev_prompt_ethos) inside the EXEC layer; claude_exec.rs does NOT. The two
backends are inconsistent about who owns prompt composition — codex callers and claude
callers get different prompts for the same logical phase. Needs a decision.

[P5] quota_exec.rs:644 run_quota_open returns i32 (main.rs:1699 process::exit) while
every other command path returns Result<()> — dispatch-layer contract inconsistency.

## P6 — Cosmetic

verification_lint.rs strip_plan_bullet and task_parser.rs strip_list_bullet are the same
bullet-strip; one shared helper. quota_exec.rs:381 swap_credentials_legacy is a test-only
shim renamed on import — slightly confusing.

## No outright bugs

No panics-on-valid-input or logic inversions. Closest call: log_stderr O(n^2)
read-modify-write is an inefficiency, not a bug. The AuthRestoreGuard Drop path and the
progress-sentinel bail in run_with_quota are careful and well-tested.

## Top refactor moves (ranked)

1. Delete src/backend_policy.rs entirely (414 lines, zero non-test refs). SAFE
2. Split codex_stream.rs into codex_stream/ and break the 277-line
   render_codex_stream_line god-match into per-event helpers. SAFE
3. Extract shared backend-process scaffolding into src/backend_process.rs
   (write_worker_pid, clear_worker_pid, log_stderr, read_stream; then unify spawn_*).
   SAFE for the four leaf helpers; RISKY for unifying spawn_* bodies.
4. Move the 8 quota_*.rs files into a src/quota/ module directory. SAFE
5. Consolidate duplicated credential-copy primitives into quota/credentials.rs. SAFE
6. Split task_parser.rs into task_parser/{model,parse,validate}.rs and remove the blanket
   #![allow(dead_code)] (then delete field_value_is_none). SAFE
7. Delete the three dead #[allow(dead_code)] functions (stream_codex_output,
   capture_opencode_output, validate_kimi_model). SAFE
8. Move clap arg structs out of main.rs into src/cli/. SAFE
9. Collapse the three heartbeat capture fns into one generic capture_with_heartbeat.
   RISKY
10. Resolve the ethos-injection asymmetry between codex_exec and claude_exec. RISKY
