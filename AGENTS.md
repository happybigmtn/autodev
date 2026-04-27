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

## Loom Access

- Loom operator SSH should use Tailscale: `ssh -i ~/.ssh/id_ed25519_hetzner root@100.124.18.111`.
- Public SSH on the Loom IPv4 may be firewalled even while the HTTPS edge is healthy.
