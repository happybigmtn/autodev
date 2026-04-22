# WORKLIST

- [Required] Review receipt command synthesis for bin-only Rust crates and shell-sensitive patterns. This batch found stale `cargo --lib` invocations for a crate with only `[[bin]]` targets and a malformed heading grep containing an unescaped backtick; future generated review entries should emit runnable commands such as `cargo test module::tests::` / `cargo clippy --bins` and escape shell metacharacters.
