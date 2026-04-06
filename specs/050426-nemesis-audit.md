# Specification: Nemesis Audit Findings and Hardening Requirements

## Audit Method

This audit was performed using the Nemesis multi-pass methodology over the `autodev` Rust codebase (11 commands: corpus, gen, reverse, bug, nemesis, loop, qa, qa-only, health, review, ship).

**Phase 0:** Recon identified the target surface: `nemesis.rs`, `util.rs`, `bug_command.rs`, `pi_backend.rs`, `main.rs`.

**Pass 1 (Feynman):** Deep logic audit traced every control-flow path and invariant in the nemesis command implementation.

**Pass 2 (State):** Cross-surface consistency audit checked state desync risks between checkpoint, staging, and commit operations.

**Pass 3 (Feynman):** Re-checked atomic write and temp file cleanup paths.

**Pass 4 (State):** Verified output directory lifecycle and model invocation ordering.

Only evidence-backed findings are retained.

---

## Finding NEM-F1: Output Directory Destructive Wipe Before Model Validation

**Affected surfaces:**
- `src/nemesis.rs`: `prepare_output_dir()` (lines ~560-600)
- `src/nemesis.rs`: `run_nemesis()` orchestration flow

**Triggering scenario:**
1. User runs `auto nemesis --report-only` with existing `nemesis/` containing prior audit outputs
2. `prepare_output_dir()` archives existing directory to `.auto/fresh-input/nemesis-previous-<timestamp>/`
3. `fs::remove_dir_all(output_dir)` deletes the live `nemesis/` directory
4. `fs::create_dir_all(output_dir)` recreates an empty directory
5. PI/Codex model is launched; if it fails, produces empty output, or is interrupted, no files are written
6. `verify_nemesis_spec()` fails with "did not write nemesis/nemesis-audit.md"
7. **Result:** Prior outputs are gone from the worktree; only the archived snapshot survives

**Invariant that breaks:**
The orchestration assumes the model will always successfully produce both required output files after the directory is wiped. There is no pre-invocation validation that the model is available, and no recovery path that restores the archived content on failure.

**Why this matters now:**
This is the primary failure mode for `--report-only` workflows. An operator running successive audits loses their prior work if the model fails or times out. The error is silent until verification, and the recovery requires manual extraction from `.auto/fresh-input/`.

**Discovery path:** `Feynman`

---

## Finding NEM-F2: `--kimi` and `--minimax` Flags Silently Override Explicit `--model`

**Affected surfaces:**
- `src/nemesis.rs`: `NemesisArgs` struct definition
- `src/nemesis.rs`: `run_nemesis()` argument resolution (lines ~193-200)

**Triggering scenario:**
User runs: `auto nemesis --model kimi-coding/k2p5 --kimi`

Resolution logic:
```rust
let auditor = PhaseConfig {
    model: if args.kimi {
        "kimi".to_string()  // Boolean flag wins
    } else if args.minimax {
        "minimax".to_string()
    } else {
        args.model.clone()  // Explicit value ignored if flag is set
    },
    ...
};
```

Result: `auditor.model` is `"kimi"` (the shorthand), not `"kimi-coding/k2p5"` (the explicit value). The `--model` argument is silently discarded.

**Invariant that breaks:**
Explicit `--model` override should take precedence over shorthand convenience flags. The current argument structure inverts this expectation without warning.

**Why this matters now:**
Operators using specific model variants (e.g., `kimi-k2-thinking` or `minimax-m2.5`) cannot combine explicit model selection with the `--kimi` or `--minimax` flag. This breaks CLI predictability and makes scripting fragile.

**Discovery path:** `Feynman`

---

## Finding NEM-F3: Checkpoint Exclusion Logic Inconsistent Across Call Sites

**Affected surfaces:**
- `src/util.rs`: `CHECKPOINT_EXCLUDES` constant (line ~16)
- `src/util.rs`: `checkpoint_status()` using git `:(exclude)` patterns
- `src/util.rs`: `stage_checkpoint_changes()` using `is_checkpoint_excluded_path()`
- `src/nemesis.rs`: `commit_nemesis_outputs_if_needed()` using mixed exclusion strategies

**Triggering scenario:**
The `nemesis/codex.stderr.log` file is created during implementation phase. Examining the exclusion paths:

1. `checkpoint_status()` uses: `git status --short -- . :(exclude).auto :(exclude)bug :(exclude)nemesis :(exclude)gen-*`
   - These git pathspec exclusions operate on directory patterns

2. `stage_checkpoint_changes()` (tracked files) uses the same `:(exclude)` patterns

3. `stage_checkpoint_changes()` (untracked files) uses `is_checkpoint_excluded_path()`:
   ```rust
   path.starts_with("nemesis/")  // Excludes nemesis/codex.stderr.log
   ```

4. `commit_nemesis_outputs_if_needed()` uses `:(exclude)` for tracked files but manual path filtering for untracked:
   ```rust
   !path.starts_with("nemesis/codex.stderr.log")  // Specific file exclusion
   ```

**Result:** The semantic intent (exclude all generated directories) is implemented via three different mechanisms that may diverge:
- Git `:(exclude)` patterns (pathspec syntax)
- Prefix-based Rust string matching
- Specific file path matching

**Invariant that breaks:**
All checkpoint-related operations must apply the same exclusion semantics. The current implementation risks state desync where files are included in status checks but excluded from staging, or vice versa.

**Why this matters now:**
The `nemesis/` directory contains model outputs that should never be committed. Inconsistent exclusion logic creates risk of accidental commits or confusing staging behavior.

**Discovery path:** `State`

---

## Finding NEM-F4: `atomic_write` Leaves Orphaned Temp File on Non-Directory Rename Failures

**Affected surfaces:**
- `src/util.rs`: `atomic_write()` (lines ~231-252)

**Triggering scenario:**
1. Temp file is successfully written to parent directory
2. `fs::rename(&temp, path)` fails with an I/O error
3. Current error handling:
   ```rust
   if let Err(rename_error) = fs::rename(&temp, path) {
       let cleanup_error = match fs::remove_file(&temp) {
           Ok(()) => None,
           Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
           Err(err) => Some(err),
       };
       // ... error construction
   }
   ```

The cleanup runs for ALL rename failures, but the test `atomic_write_removes_temp_file_after_rename_failure` only verifies the directory-conflict case.

**Result:** On permission denied, read-only filesystem, or device-full errors, the temp file is orphaned in the filesystem.

**Invariant that breaks:**
`atomic_write` should guarantee no temp artifact remains on any error path. The current implementation attempts cleanup but does not verify it succeeds.

**Why this matters now:**
Repeated failed atomic writes (e.g., during disk-full conditions) accumulate `.tmp-<pid>-<nanos>` files. The `prune_old_entries` mechanism only targets directories it explicitly manages, not orphaned temp files.

**Discovery path:** `Feynman`

---

## Finding NEM-F5: `commit_nemesis_outputs_if_needed` Non-Atomic Multi-Command Staging

**Affected surfaces:**
- `src/nemesis.rs`: `commit_nemesis_outputs_if_needed()` (lines ~650-700)

**Triggering scenario:**
The commit function stages changes via multiple sequential git commands:

1. `git add -u -- . :(exclude).auto :(exclude)bug :(exclude)gen-*` (tracked files)
2. `git ls-files -z --others --exclude-standard` (query untracked)
3. Filter untracked through `is_checkpoint_excluded_path()` logic
4. `git add -- <chunk1>` (untracked chunk 1, up to 100 files)
5. `git add -- <chunk2>` (untracked chunk 2, if needed)
6. `git commit -m "..."`

**Result:** If the process is interrupted between steps 1 and 6, the repository is left in a partially-staged state. Additionally, pre-existing staged changes from other operations would be included in the commit.

**Invariant that breaks:**
Staging should be atomic relative to the commit operation. The current implementation creates windows for partial state and cross-contamination.

**Why this matters now:**
The nemesis implementation phase runs an external model (Codex) that may take significant time. The longer the window between staging start and commit, the higher the risk of interruption or concurrent modification.

**Discovery path:** `State`

---

## Finding NEM-F6: `ensure_repo_layout` Halts on First Pruning Failure

**Affected surfaces:**
- `src/util.rs`: `ensure_repo_layout()` (lines ~264-283)

**Triggering scenario:**
```rust
prune_old_entries(&repo_root.join(".auto").join("logs"), AUTO_LOG_KEEP_FILES)?;
prune_old_entries(&repo_root.join(".auto").join("fresh-input"), AUTO_FRESH_INPUT_KEEP_ENTRIES)?;
prune_old_entries(&repo_root.join(".auto").join("queue-runs"), AUTO_QUEUE_RUN_KEEP_ENTRIES)?;
prune_pi_runtime_state(repo_root)?;
```

If the first `prune_old_entries` call fails (e.g., permission error on `.auto/logs`), the function returns early via `?`. The remaining directories (`.auto/fresh-input`, `.auto/queue-runs`) are never pruned.

**Invariant that breaks:**
`ensure_repo_layout` claims to ensure the full `.auto/` layout including pruning for all managed subdirectories. It silently leaves some directories unpruned on error.

**Why this matters now:**
In environments with restrictive permissions or disk issues, this creates unbounded growth in log and queue directories without warning.

**Discovery path:** `Feynman`

---

## Finding NEM-F7: Redundant `prune_pi_runtime_state` Called After Every PI Invocation

**Affected surfaces:**
- `src/nemesis.rs`: `run_pi()` (lines ~485-510)
- `src/bug_command.rs`: `run_backend_prompt()` for PI backend (lines ~395-420)

**Triggering scenario:**
Both nemesis and bug commands launch PI with `--no-session --tools read,bash,edit,write,grep,find,ls`. The `--no-session` flag guarantees that each PI invocation starts fresh with no persistent state.

Despite this, `prune_pi_runtime_state(repo_root)` is called after **every** successful PI invocation in `run_pi()`. In `bug_command.rs`, it's called with best-effort error handling.

**Result:**
- Unnecessary I/O overhead (directory removal and recreation) after every PI tool call
- Potential interference with any future session-based PI invocation
- The bug_command comment says "bounds old `.auto/logs/` entries automatically and prunes PI snapshot/session-diff caches after each PI phase" — but this applies to phase boundaries, not individual calls

**Invariant that breaks:**
With `--no-session`, PI should require no cleanup. The redundant pruning adds overhead and risks future session reuse patterns.

**Why this matters now:**
The nemesis audit phase makes multiple PI calls (audit + review). Each call triggers redundant I/O. Over many nemesis runs, this accumulates unnecessary filesystem operations.

**Discovery path:** `Cross-feed`

---

## Finding NEM-F8: `verify_nemesis_spec` and `verify_nemesis_plan` Have Race Window

**Affected surfaces:**
- `src/nemesis.rs`: `verify_nemesis_spec()` (lines ~530-545)
- `src/nemesis.rs`: `verify_nemesis_plan()` (lines ~547-565)
- `src/nemesis.rs`: `run_nemesis()` invocation ordering (lines ~295-305)

**Triggering scenario:**
```rust
let spec_path = verify_nemesis_spec(&output_dir)?;  // Check 1: spec exists
let plan_path = verify_nemesis_plan(&output_dir)?;  // Check 2: plan exists
```

These are sequential, non-atomic checks. The model writes both files, but the verification happens separately. If the process is interrupted between Check 1 and Check 2, or if the model only wrote one file, the error refers to the second missing file while the first may exist.

**Result:** Partial output states are not handled gracefully. The verification assumes atomic completion of both files.

**Invariant that breaks:**
Both output files should be verified as a unit, or the verification should handle partial states with clear recovery guidance.

**Why this matters now:**
Model failures or interruptions can leave the output directory in intermediate states. The current verification doesn't distinguish between "model didn't run" and "model partially completed."

**Discovery path:** `State`

---

## Finding NEM-F9: `sync_nemesis_spec_to_root` Date Collision Risk

**Affected surfaces:**
- `src/nemesis.rs`: `sync_nemesis_spec_to_root()` (lines ~615-645)

**Triggering scenario:**
The function generates destination filenames using:
```rust
let date_prefix = Local::now().format("%d%m%y").to_string();
let mut counter = 1usize;
let destination = loop {
    let candidate = if counter == 1 {
        root_specs_dir.join(format!("{date_prefix}-{slug}.{extension}"))
    } else {
        root_specs_dir.join(format!("{date_prefix}-{slug}-{counter}.{extension}"))
    };
    if !candidate.exists() { break candidate; }
    counter += 1;
};
```

**Result:** In rapid successive runs (e.g., CI or scripted batch operations), multiple nemesis runs on the same calendar day could theoretically collide on the counter increment if they execute in overlapping time windows. The check-then-write pattern is not atomic.

**Invariant that breaks:**
Filename generation should be collision-free. The current implementation relies on filesystem existence checks which are race-prone.

**Why this matters now:**
This is a low-probability but real risk in automated environments. A timestamp-based prefix (including hours/minutes/seconds) would eliminate the collision window.

**Discovery path:** `Feynman`

---

## Finding NEM-F10: Implementation Phase Missing Pre-Flight Task Validation

**Affected surfaces:**
- `src/nemesis.rs`: `run_nemesis()` implementation phase flow (lines ~310-355)
- `src/nemesis.rs`: `verify_nemesis_implementation_results()` (lines ~567-595)

**Triggering scenario:**
1. The implementation plan has zero unchecked NEM- tasks
2. `run_codex_exec()` is invoked anyway with the full implementation prompt
3. Codex runs, produces empty or unexpected results
4. `verify_nemesis_implementation_results()` validates against expected task IDs
5. Error: "Nemesis implementation results missing task NEM-XXX"

**Result:** The implementation phase runs even when there's no work to do, wasting model tokens and producing confusing error messages.

**Invariant that breaks:**
The implementation phase should validate that there's actual work to do before invoking the model. Zero-task plans should short-circuit cleanly.

**Why this matters now:**
Running expensive Codex calls when there's nothing to implement wastes resources and creates unnecessary failure modes.

**Discovery path:** `Feynman`
