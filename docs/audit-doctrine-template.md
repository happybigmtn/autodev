# Audit doctrine template

Copy this to `audit/DOCTRINE.md` in your repo and edit it. `auto audit`
reads this file verbatim and injects it as the operator-authored judgment
framework into every per-file audit prompt. You own what "clean" means
for your codebase.

The auditor also has the Read tool, so you can reference other documents
(GDDs, specs, invariants, security plans) by relative path and the
auditor will pull them in when needed. That's cheaper and more flexible
than inlining the full GDD here.

Everything below is example content. Replace, delete, or rewrite to match
your repo.

---

## Canonical sources

The auditor should pull these into context when judging a file, based on
path heuristics below. Reference them by relative path; the auditor will
read them on demand.

- **Product doctrine:** `AUTONOMY-GDD.md`. §1-§2 are load-bearing —
  pillars, non-goals, loops. Any file that implements a named primitive
  from §5 must reference the §5.X anchor in its doc comments.
- **Security doctrine:** `SECURITY_PLAN.md`. Arc NS-V items are
  mainnet-gating. Files in bridge / FROST / admin-envelope paths must
  comply with the named invariants.
- **Architecture doctrine:** `INVARIANTS.md` lists the small set of
  architectural invariants every file must respect.
- **Operational rules:** `AGENTS.md` for commands, build, validation.
  `CLAUDE.md` for project-specific instruction to coding agents.
- **Design rules (UI only):** `DESIGN.md`. Non-UI files should not
  reference this.
- **Plan state:** `IMPLEMENTATION_PLAN.md` and `REVIEW.md` are not
  doctrine themselves — they're transient plan state — but a file whose
  contents contradict a live plan claim is DRIFT.

## Path-scoped rules

- **`node/src/bridge_*.rs`, `crates/bridge-signer/**`** — apply
  `SECURITY_PLAN.md` Arc NS-V bridge sections. Any mention of `min_confirmations`,
  FROST key material, or bridge admin envelope shape must match the
  spec byte-for-byte. DRIFT here is always `DRIFT-LARGE` (never SMALL)
  because consequences are mainnet.
- **`crates/bitino-house/src/admin_*`, `crates/bitino-house/src/channel_*`**
  — apply bitino's `SECURITY_PLAN.md` Arc BIT-NS-II and §Remediation
  BIT-NS-01/04/05 sections. Admin envelope shape must match autonomy's
  `bridge_admin_quorum` byte-for-byte — flag any divergence as `DRIFT-LARGE`.
- **`barely-human/src/mortality/**`** — apply `AUTONOMY-GDD.md` §5.8
  Termination Reveal. Any mortality surface referencing retired Heir
  Bond primitives is DRIFT.
- **`observatory-tui/src/**`** — apply `DESIGN.md` §TUI conventions.
  Any hand-rolled widget duplicating a shared component is SLOP.
- **`specs/<date>-*.md`** — these are historical planning artifacts.
  If a newer spec with the same stem exists, the older one is RETIRE
  (confidence: MEDIUM; operator decides).
- **Tests under `tests/` or colocated `*_test.rs`** — apply relaxed
  rules; do not flag tests for line count alone.

## What we consider SLOP

- Any `.rs` file longer than 600 lines without a module-header comment
  explaining why it resists decomposition.
- `#[allow(dead_code)]` on items older than 30 days of commit history.
  If it's still dead after a month, it's RETIRE, not a suppression.
- `TODO` / `FIXME` / `XXX` comments without an author or date.
- `println!` or `eprintln!` in library code (only binaries may print).
- `.unwrap()` / `.expect()` in non-test code paths.
- Hand-rolled error types where a project-standard thiserror / anyhow
  shape exists nearby.
- Copy-pasted doc comments (identical 3+ lines of doc in two places that
  aren't genuine interface-contract mirrors).
- Vendored code under `src/` that should be a dependency.

## What we consider RETIRE-worthy

- Any file under `nemesis/draft-*` — superseded by the current nemesis
  audit spec.
- Any spec with a `-v[1-9]` suffix where a newer version exists.
- Any `xtask/` subcommand whose only caller is a one-off ops script that
  is also being retired.
- Test fixtures referenced by zero tests (empty `git grep` result).
- Generated artifacts accidentally committed (check for `.gitignore`
  omissions first).

## What we do NOT flag (false-positive guard)

- `fixtures/` directories (test data; different rules apply)
- `vendor/` directories
- Auto-generated code clearly marked with a header comment.
- Migration files under `db/migrations/` once merged — these are
  append-only by construction.
- Files explicitly allowlisted via `audit/ALLOWLIST.md` (one path per
  line, optional).

## Severity hints

- Security-adjacent DRIFT (bridge, admin surface, FROST, chain-tx
  signing) is always `DRIFT-LARGE` regardless of line count. A
  `DRIFT-SMALL` patch in these paths is rejected; escalate to worklist.
- Pure-formatting or doc-comment typo drift is `SLOP`, not `DRIFT-SMALL`.
- A file that no longer compiles (the auditor should notice this) is
  `DRIFT-LARGE` even if the fix looks small — never auto-apply a patch
  to a broken build.

## Voice / style preferences

- Imperative mood in Rust doc comments; declarative for TypeScript JSDoc.
- Error messages in library code name the operation + the input; no bare
  "failed".
- Tests are named `test_<subject>_<specific_behavior>`, not prose
  sentences.
- Public APIs get full `///` doc blocks; internal helpers get one-line
  `///` only when the name doesn't already say it.

## Cross-repo doctrine (optional)

If this repo pairs with another (e.g. autonomy ↔ bitino), name the
shared interfaces here:

- `bridge_admin_quorum` admin envelope: both repos must match byte-for-byte.
- FROST signer pubkey registration format: identical.
- Observer JWT handshake (`B-OBS-1b`): HS256, shared secret, same claim
  set.
- Chain-tx wire formats in `types/src/chain_tx.rs`: any new variant must
  also land in bitino's mirror types.

Any file touching these surfaces should reference the other repo's
implementation as the cross-check, and `DRIFT` here means cross-repo
divergence.
