# WORKLIST

- [Required] Review receipt command synthesis for bin-only Rust crates and shell-sensitive patterns. This batch found stale `cargo --lib` invocations for a crate with only `[[bin]]` targets and a malformed heading grep containing an unescaped backtick; future generated review entries should emit runnable commands such as `cargo test module::tests::` / `cargo clippy --bins` and escape shell metacharacters.
- [Required] Harden generated review verification commands against false-positive proof. This batch found a non-existent cargo test filter that ran zero tests and a directory `grep` command that failed before searching; review harvesting should reject zero-test cargo filters and prefer recursive `rg` commands for directory searches.
