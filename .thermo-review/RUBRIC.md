# Thermo-Nuclear Code Quality Rubric

A harsh maintainability audit. Tone: direct, high-conviction. Skip cosmetic nits
when structural issues exist. Output findings in **priority order** (structural
first). Every finding cites `file:line` and gives a concrete, actionable fix.

## Priority 1 — File sprawl (the 1k-line rule)

Any file past ~1,000 lines is a defect: it stops fitting in a reviewer's
working memory, so cross-cutting bugs hide in the gaps. For every oversized
file, do not just say "split it" — propose a concrete module decomposition:
the submodule names, what types/functions move into each, and the natural
seams (cohesive clusters that already barely interact). A good split has low
coupling across submodule boundaries and each piece under ~1k lines.

## Priority 2 — Spaghetti / control-flow rot

- Functions over ~100 lines or cyclomatic complexity over ~8.
- Deep nesting; arrow-shaped code that should use `let ... else` early returns.
- Ad-hoc branch growth: special cases and feature flags bolted onto a function
  instead of refactored into a clean abstraction.
- Tangled data flow where a value is mutated across many scopes.

## Priority 3 — Code-judo (aggressive simplification)

Where does deleting or collapsing code remove more than it costs?
- Duplicated logic that should be one function.
- Parallel structures (e.g. near-identical command handlers) that should unify.
- Abstractions used exactly once — inline them.
- Dead code: unreferenced functions, unreachable branches, stale flags.
- Over-defensive error handling for impossible states.

## Priority 4 — Types & boundaries

- Primitive obsession: stringly-typed values where a newtype or enum belongs.
- Boolean flags that encode state better expressed as an enum.
- Leaky module boundaries: internals reached across modules instead of via a
  narrow public API.
- Missing or vague error types; `anyhow` where a typed error would catch bugs.

## Priority 5 — Canonical layers

Is there a clear layering (CLI parse -> command orchestration -> backend exec
-> IO/process)? Or is parsing, business logic, and process spawning tangled in
one place? Name the layer violations.

## Priority 6 — Cosmetic

Only if nothing structural remains. Naming, formatting, comment hygiene.

## Output format

Write a findings report. For each finding:
`[P1-P6] file:line — <one-line problem> -> <concrete fix>`
For oversized files, include a proposed module tree. End with a ranked
"Top refactor moves" list: the 5-10 highest-leverage changes, each marked
SAFE (pure code movement, mechanically verifiable) or RISKY (changes behavior).
