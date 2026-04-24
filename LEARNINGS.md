# LEARNINGS

- Review queue validation commands are evidence, not truth. Before archiving, run them against the live package shape: this crate has no library target, so `cargo test --lib ...` and `cargo clippy --lib --bins ...` are invalid even when the equivalent bin-target tests pass.
- Sensitive quota credential/config writes should create or tighten the destination to `0o600` before writing bytes. A write-then-chmod sequence leaves a short exposure window for newly created files under permissive umasks.
- `auto audit` resume logic must compare stored content hashes with current file hashes before skipping. Storing the hash without checking it makes the resumability contract look present while silently missing changed files.
- Quota error redaction must consider the full anyhow chain, not only `err.to_string()`. Callers that render `{err:#}` can otherwise reintroduce token-bearing source errors after a sanitizer appears to pass direct-message tests.
- Specs that record "verified facts" need the same review treatment as code: when a task removes an operator-specific default, adjacent specs and decision docs must be searched for the old literal path before archiving the task.
- Refusal paths should happen before creating task output artifacts. For commands like `auto steward`, tests should assert both the error message and the absence of default output directories so a "refuse to run" path stays side-effect-light.
- Sensitive credential copy paths need the same pre-tightened write discipline as direct writes. Reading the source and writing through the owner-only helper avoids the exposure window left by `fs::copy` followed by chmod.
- Model shorthand flags must not silently override an explicit `--model` value. Regression tests should cover "explicit model plus convenience flag" because default-model tests alone miss precedence bugs.
- CLI default-behavior claims need parser-level tests, not only helper-function tests. A sibling-discovery helper can pass while the exposed command still defaults to not calling it.
- Dirty-state guards should consume Git porcelain `-z` output, not human `--short` status. Quoted paths can otherwise fingerprint as missing and hide mutations to pre-existing dirty files.
- Credential restore guards need absence metadata as well as backup paths. If no original auth file existed, a successful swap must remove the temporary active file during restore instead of leaving the selected profile behind.
