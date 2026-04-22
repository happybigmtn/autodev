# LEARNINGS

- Review queue validation commands are evidence, not truth. Before archiving, run them against the live package shape: this crate has no library target, so `cargo test --lib ...` and `cargo clippy --lib --bins ...` are invalid even when the equivalent bin-target tests pass.
- Sensitive quota credential/config writes should create or tighten the destination to `0o600` before writing bytes. A write-then-chmod sequence leaves a short exposure window for newly created files under permissive umasks.
- `auto audit` resume logic must compare stored content hashes with current file hashes before skipping. Storing the hash without checking it makes the resumability contract look present while silently missing changed files.
