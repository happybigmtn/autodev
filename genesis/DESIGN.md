# DESIGN - operator-facing surfaces

## Scope

This repo does not have a browser UI. It does have meaningful user-facing surfaces:

- CLI commands and help text from `src/main.rs`;
- terminal status output from command modules;
- generated markdown under `genesis/`, `gen-*`, root specs, plans, and review docs;
- logs, prompts, stderr captures, and receipts under `.auto/`, `bug/`, and `nemesis/`;
- tmux lane names and status output for `auto parallel`;
- generated Symphony workflow YAML and embedded shell.

Design quality for this repo means information clarity, state coverage, recoverability, accessibility in terminals, and resistance to AI-generated sludge.

## Information Architecture

The command surface currently spans five mental buckets:

1. Planning: `corpus`, `gen`, `reverse`, `steward`.
2. Execution: `loop`, `parallel`, `symphony`.
3. Quality: `review`, `qa`, `qa-only`, `health`, `ship`.
4. Discovery/remediation: `bug`, `nemesis`, `audit`.
5. Accounts/infrastructure: `quota`.

The README and help should present these buckets explicitly. A flat sixteen-command list is discoverable but not teachable.

The active planning surfaces should be named consistently:

- `genesis/` is generated planning corpus.
- `gen-*` is generated implementation output.
- `IMPLEMENTATION_PLAN.md` and `specs/` are active root planning surfaces.
- `ARCHIVED.md` and `WORKLIST.md` are state ledgers, not source-of-truth specs.

## User Journeys

New operator:

1. Run `auto --version`.
2. Run a no-model health/doctor path.
3. Learn which selected command needs which external tools.
4. Run `auto corpus --dry-run` or an equivalent local smoke.
5. See exactly where output will be written and how to recover archives.

Planning operator:

1. Run `auto corpus`.
2. Review generated corpus and Codex review output.
3. Run `auto gen`.
4. Promote only chosen generated tasks into root queue.

Execution operator:

1. Check dirty state and current branch.
2. Run `auto loop`, `auto parallel`, or `auto symphony`.
3. Watch command status without reading raw model logs.
4. Inspect receipts and review handoff before accepting completion.

Quota operator:

1. Capture accounts.
2. Check status without leaking tokens.
3. Run a provider command through `auto quota open`.
4. Trust that previous active credentials are restored.

## State Coverage

Every command that mutates files or credentials should expose:

- current repo branch;
- dirty-state summary;
- planned output directory;
- archive/checkpoint path before destructive changes;
- selected model/backend and dangerous-mode status;
- selected quota provider/account label when applicable;
- completion proof location;
- recovery instructions.

Some commands already do parts of this. The design gap is consistency.

## Accessibility And Terminal Behavior

Terminal output should not depend on color alone. Status words such as `OK`, `FAILED`, `BLOCKED`, `PARTIAL`, and `ARCHIVED` should be present in plain text.

Output should remain legible at common terminal widths. Avoid long unwrapped generated paths when a relative path is sufficient. When paths are long, put them on their own line.

Logs should be searchable. Prefer stable prefixes like `[quota-router]`, `[parallel]`, `[genesis]`, and `[verification]` over prose-only output.

## Responsive Behavior

There is no responsive web layout. The equivalent concern is terminal width and tmux panes. `auto parallel status` and other status commands should avoid tables that become unreadable in narrow panes. Use one fact per line when the data is operationally important.

## AI-Slop Risk

This repo is especially vulnerable to plausible but false markdown because it generates plans, specs, receipts, reviews, and reports. Design countermeasures:

- Require code-linked evidence for current-state claims.
- Separate `Verified`, `Recommendation`, and `Open Question` labels.
- Preserve failing validation results instead of smoothing them into optimistic prose.
- Keep generated plans as subordinate until promoted into root queue.
- Prefer receipts and specific command outputs over narrative claims.

## Copy And Help Principles

Use concrete verbs: `archive`, `write`, `sync`, `commit`, `push`, `restore`, `verify`.

Say when a command may call a live model, mutate the working tree, change credentials, create a checkpoint, or push to a remote.

Avoid saying a command is safe because it is "just planning" if it archives, deletes, or rewrites generated directories first.

## Design Decisions

| Decision | Classification | Rationale |
|---|---|---|
| Treat CLI/log/docs as the design surface | Mechanical | No browser UI exists, but these are operator-facing interfaces. |
| Bucket commands into five mental groups | Taste | The grouping makes onboarding easier without changing CLI names. |
| Prefer extending `health` or adding a doctor path only after design | User Challenge | Adding a new command changes the public surface; extending existing health may be less disruptive. |
| Keep generated markdown evidence-heavy | Mechanical | The product creates markdown for agent execution; false polish is a real failure mode. |

## Not Applicable

Visual layout, mobile responsiveness, iconography, and component styling are not relevant because this repo has no app UI. If a UI is added later, it should be planned separately and should not be implied by this design pass.
