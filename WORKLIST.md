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

## Open review-receipt hardening (pre-existing)

- [Required] Review receipt command synthesis for bin-only Rust crates and shell-sensitive patterns. This batch found stale `cargo --lib` invocations for a crate with only `[[bin]]` targets and a malformed heading grep containing an unescaped backtick; future generated review entries should emit runnable commands such as `cargo test module::tests::` / `cargo clippy --bins` and escape shell metacharacters.
- [Required] Harden generated review verification commands against false-positive proof. This batch found a non-existent cargo test filter that ran zero tests and a directory `grep` command that failed before searching; review harvesting should reject zero-test cargo filters and prefer recursive `rg` commands for directory searches.
- [Required] Make task receipts unambiguous when a failed generated command is corrected in the same task. `AD-018` retained an older failed unquoted `rg` pipeline attempt alongside later passing proof, which makes receipt consumers choose between "any failed command fails the task" and "latest corrected proof wins" without an explicit marker.
