# AGENTS.md

## Build

```bash
cargo check
cargo build
cargo install --path . --root ~/.local
```

## Validate

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Essentials

- CLI binary: `auto`
- Required tools on PATH: `claude`, `codex`, `pi`, `gh`
- Main source tree: `src/`
- Dated specs: `specs/`
- Generated/runtime state: `.auto/`, `bug/`, `nemesis/`
- Checkpoint exclusions are `.auto/`, `bug/`, `nemesis/`, and `gen-*` via `CHECKPOINT_EXCLUDE_RULES` in `src/util.rs`
