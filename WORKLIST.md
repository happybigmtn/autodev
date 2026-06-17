# WORKLIST

## CI cleanliness gate (closed 2026-06-17)

- [Done] rustfmt drift: ran `cargo fmt` (rustfmt 1.9.0-stable, default config) over
  34 files (import ordering + multi-line wrapping; no logic change). `cargo fmt
  --check` clean. Commit: `style: apply cargo fmt to satisfy the CI fmt gate`.
- [Done] 7 clippy `-D warnings` lints cleared (needless_return, needless_borrow,
  vec_init_then_push, single_element_loop, if_same_then_else, and a justified
  `#[allow(enum_variant_names)]` on `Command::CommandSurface`). `cargo clippy
  --all-targets --all-features -- -D warnings` clean. Commit: `fix: clear all 7
  clippy warnings to satisfy the -D warnings gate`.
- [Done] Flaky `push_branch_with_remote_sync_retries_non_fast_forward_push_race`:
  root cause was `is_non_fast_forward_push_failure` not recognising the
  concurrent ref-lock variant ("cannot lock ref ...: is at X but expected Y" /
  "failed to update ref") a second writer produces when it advances the remote
  ref *during* our push. Classifier now treats these as retryable. This both
  closes a real two-lane concurrent-push gap and makes the test deterministic
  (0/10 full-suite failures, was 3/3). Commit: `fix(git): treat concurrent
  ref-lock failures as retryable non-fast-forward`.
- [Done] Suite-wide ETXTBSY ("Text file busy") flakiness (was ~17/30 full-suite
  runs failing across bug_command::backend, worker_env, quota_usage). Tests
  write-then-exec scripts; under parallel threads a writable fd inherited across
  another thread's fork/posix_spawn transiently poisons an unrelated `execve`.
  Added `util::spawn` ETXTBSY-retry helpers (cargo/rustup do the same) and routed
  the affected spawns through them (also genuine production hardening for the
  written-then-exec'd per-worker git guard shim). Separately, the env race in
  `quota_selector::tests::live_codex_usage_reads_from_live_home` (read `$HOME`
  without `test_process_env_lock`) was serialized via the existing env lock.
  Verified: `cargo test --bin auto` 50/50 consecutive green. Commit: `test:
  eliminate suite-wide ETXTBSY and $HOME spawn races under parallel cargo test`.

Verification (release binary, 2026-06-17): `auto --version` -> 0.2.0, dirty:
clean; `auto --help`, `auto doctor` (exit 0, "doctor ok"), and `auto
command-surface --json` (valid JSON, 24 commands; identical with/without the
flag) all run end-to-end. Full gate green: fmt clean, clippy `-D warnings`
clean, 688/688 tests pass.

## Review-receipt hardening (closed 2026-06-17)

- [Done] Review receipt command synthesis for bin-only Rust crates and shell-sensitive patterns
  AND false-positive-proof hardening. Root cause was scope, not absence: the lint
  (`verification_lint::verify_commands_are_runnable` — rejects stale `cargo --lib`,
  package-wide/no-filter/multi-filter `cargo test`, and directory `grep` → steers to `rg -n`)
  was wired into plan generation (`generation/plan_verify.rs`) and task validation
  (`task_parser/validate.rs`), but `validate_execution_rows` only checks Pending/Partial rows,
  so **Done** rows were copied verbatim into the review queue unlinted. Fix:
  `review_command/harvest::render_completed_plan_review_item` now lints each completed item and
  annotates the handoff (⚠ not directly runnable + how to derive concrete proof) without
  blocking harvest — the receipt machinery already gates the real completion proof. Tests:
  `harvest_review_item_flags_non_runnable_verification_command`,
  `harvest_review_item_leaves_runnable_command_unannotated`. Verified: fmt clean, clippy
  `-D warnings` clean, 690/690 `cargo test --bin auto`, 25/25 full-workspace stress (696 tests).
  Commit: `fix(review): lint completed-task verification commands at review harvest` (58f761a).
- [Done] Task receipts unambiguous when a failed generated command is corrected in the same task.
  Already implemented and tested before this cycle via the explicit `supersedes` marker on
  `VerificationReceiptCommand` (`completion_artifacts/receipt.rs`):
  `verification_receipt_failed_entry_is_superseded` lets a later passing command supersede an
  earlier failed attempt, while `inspect_verification_receipt` still rejects *unsuperseded*
  stray failures. Tests cover accept-superseded, accept-later-pass, and reject-unsuperseded.
