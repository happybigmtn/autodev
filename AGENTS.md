# AGENTS.md

## Build

```bash
cargo check          # type-check only
cargo build          # debug build
cargo install --path . --root ~/.local  # install as ~/.local/bin/auto
```

## Validate

```bash
cargo test           # 135 tests, ~0.3s
cargo clippy --all-targets --all-features -- -D warnings
```

## Binary

The CLI binary is `auto`. Version includes embedded git SHA and dirty flag via `build.rs`.

## Runtime Dependencies

- `claude` on PATH for corpus/gen/reverse commands
- `codex` on PATH for loop/qa/health/review/ship/nemesis-implementation
- `pi` on PATH for bug finder/skeptic/reviewer and nemesis audit/synthesis
- `gh` on PATH for ship PR creation

## Repo Layout

- `src/` — Rust source (single crate, binary target `auto`)
- `specs/` — dated spec snapshots (`ddmmyy-topic.md`)
- `.auto/` — runtime state, logs, archives (gitignored)
- `nemesis/` — nemesis audit outputs (gitignored from checkpoints)
- `bug/` — bug pipeline outputs (gitignored from checkpoints)

## Checkpoint Exclusions

Checkpoint operations exclude `.auto/`, `bug/`, `nemesis/`, and `gen-*` directories.
The exclusion logic is centralized in `CHECKPOINT_EXCLUDE_RULES` in `src/util.rs`.
