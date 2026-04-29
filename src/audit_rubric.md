# `auto audit` — per-file rubric

You are auditing one file. Your verdict and proposed remediation are
parsed mechanically by the surrounding Rust runner; **follow the output
contract below exactly** or your output will be discarded.

## Verdicts (pick exactly one)

- `CLEAN` — the file conforms to doctrine. No changes.
- `DRIFT-SMALL` — the file has drifted in small, mechanically fixable ways.
  You MUST produce a minimal unified-diff patch.
- `DRIFT-LARGE` — the file has drifted in ways too broad for a single safe
  patch this pass. You MUST write a severity-tagged `worklist-entry.md`.
- `SLOP` — the file violates idiom / style rules in ways safe to fix in
  place. Produce a minimal patch.
- `RETIRE` — the file should be deleted or replaced wholesale. You MUST
  write a `retire-reason.md` explaining why.
- `REFACTOR` — the file needs structural rework too large for this pass.
  Produce a `worklist-entry.md`.

## Output contract

Write all of the following into the artifact directory the prompt names.
**Do not print them on stdout** — only file writes count.

### Required: `verdict.json`

```json
{
  "verdict": "CLEAN | DRIFT-SMALL | DRIFT-LARGE | SLOP | RETIRE | REFACTOR",
  "rationale": "One paragraph, plain prose. Cite the doctrine section(s) or idiom rule(s) you are applying.",
  "touched_paths": ["list", "of", "paths", "your", "patch", "modifies"],
  "escalate": false
}
```

`escalate: true` is a hint to the surrounding runner that this file's
verdict warrants a second-opinion pass by a stronger reviewer.

### Conditional

- For `DRIFT-SMALL` or `SLOP`: write `patch.diff` as a unified diff that
  `git apply` accepts. The patch MUST touch only the file under audit
  (and its colocated tests, if obviously required). No cross-module
  refactors.

- For `DRIFT-LARGE` or `REFACTOR`: write `worklist-entry.md` containing
  exactly one markdown bullet, ready to append to `WORKLIST.md`, with a
  severity tag:

  ```
  - `Required` / `Optional` / `FYI`: short-title — cited doctrine rule;
    1-2 sentences on what needs to change and why it's too big for this
    pass.
  ```

- For `RETIRE`: write `retire-reason.md` with one paragraph explaining
  what supersedes this file, when it became dead, and the confidence level
  (`HIGH` safe to delete, `MEDIUM` needs sign-off, `LOW` needs investigation).

## Hard rules

- Read the doctrine section below carefully. If doctrine says "do not flag
  X", do not flag X. If doctrine designates the repo as Codex-first or
  Rails-first or anything-first, follow that convention.
- Verify every claim. "This file drifted from doctrine section §X" requires
  that §X actually says what you claim. You may use file-reading tools to
  open the doctrine source.
- Patches must be minimal. If you find yourself writing a 300-line diff,
  downgrade to `DRIFT-LARGE` or `REFACTOR` and write a worklist entry
  instead.
- Patches must be stable after commit. Do not fix stale "current HEAD",
  timestamp, run id, local checkout, or other volatile proof drift by
  substituting the value from this audit run; that creates a self-invalidating
  patch as soon as the runner commits it. Replace volatile proof with a stable
  release identity, a command to run, or wording that distinguishes selected
  release evidence from the current checkout without embedding a moving value.
- Do NOT modify files other than the one under audit without explicit
  doctrine authorization.
- Do NOT mix fixing behaviour with formatting churn in the same patch.
- If the file is already `CLEAN`, say so quickly — there is no bonus for
  finding issues that aren't there.

## Output directory

The prompt tells you the artifact directory. All outputs go there.
