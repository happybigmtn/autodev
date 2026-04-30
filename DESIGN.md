# Autodev Operator Design System

Autodev is a terminal-first autonomous development control plane. Its user interface is the command surface, prompts, stdout, markdown reports, receipts, queue files, logs, recovery notes, and release gates. A web UI is not the product truth today.

## Design Thesis

The product should feel forensic, calm, and operationally dense: an expert cockpit for repository work, not a marketing dashboard. Every visible element should answer one of four operator questions:

- What did this command read?
- What did it write?
- What did it prove?
- What should happen next?

## Source Of Truth

- Runtime truth lives in Rust command modules under `src/`.
- Planning truth lives in root queue and review files: `IMPLEMENTATION_PLAN.md`, `REVIEW.md`, `WORKLIST.md`, `ARCHIVED.md`, root `specs/`, and durable receipts.
- `genesis/` is planning input for `auto gen`, not live product truth unless a repo instruction explicitly promotes it.
- `.auto/`, `bug/`, `nemesis/`, and `gen-*` are generated or runtime state. They may contain evidence, but they are not a replacement for source-controlled control inputs.

## Component Vocabulary

Autodev terminal UI should reuse a small set of components:

- Command header: command name, repo root, branch, model or harness, run root, prompt log, and mode.
- Capability matrix: stable `[ok]`, `[warn]`, `[fail]`, or equivalent text labels. Never rely on color alone.
- Artifact list: exact paths for reports, logs, receipts, snapshots, and generated outputs.
- Gate verdict: one line using `Verdict: GO` or `Verdict: NO-GO`, followed by reasons and next command.
- Queue row: task ID, source of truth, runtime owner, UI consumers, generated artifacts, fixture boundary, verification, dependencies, and closeout proof.
- Progress stream: short, grep-friendly lines that remain readable in tmux panes and CI logs.
- Final status block: status, files written, receipts, blockers, and next step.

## Interaction States

Every command that can take meaningful time or write files should make these states visible:

- Empty: say what was checked and why no work was available.
- Loading or running: print durable paths before long model or worker execution begins.
- Partial: name the surviving blocker and the artifact that records it.
- Error: include the failed command, path, or gate plus a concrete recovery step.
- Success: list proof and next command without hiding warnings in prose.

## Typography And Layout

The active UI is monospace terminal output and Markdown artifacts. Prefer compact headings, short labels, and stable field order. Avoid wide tables in stdout; use Markdown tables in durable reports only when they remain readable in code review.

Do not invent decorative type, cards, gradients, icons, or visual previews for acceptance evidence. Concept previews are allowed only when clearly labeled non-authoritative and cannot satisfy runtime/UI proof.

## Color And Accessibility

Color may improve local terminal scanning, but it must never carry meaning alone. Every status must have a text label. Output should be useful when copied into plain text, screen readers, narrow panes, and CI logs.

For any future web UI, follow standard semantic HTML, visible focus states, keyboard access, WCAG AA contrast, user-zoom support, reduced-motion handling, labeled controls, non-color status labels, and URL-addressable state for filters or tabs.

## Runtime/UI Contract

Runtime code owns canonical facts. Presentation surfaces render those facts through existing helpers, generated schemas, or shared parsers. Production UI must not duplicate:

- task status derivation,
- dependency eligibility,
- receipt freshness,
- provider credential state,
- release readiness,
- model routing,
- runtime health,
- fixture/demo/sample data as fallback truth.

If a surface needs new displayed data, the implementation must name the runtime owner, API or schema change, generated artifact, consumer, fixture boundary, and a proof that would fail if the display drifted.

## Responsive Terminal Behavior

Autodev output must work in:

- narrow tmux panes,
- long CI logs,
- copied terminal transcripts,
- markdown review diffs,
- automation that greps for stable labels.

Keep stdout concise and move full explanations into durable files. Prefer artifact paths over transient lane context.

## Product UI Versus Concept Preview

Real product UI: `auto` command help, stdout, generated prompts, root plans, reports, receipts, `.auto/*` manifests, and release gates.

Non-authoritative preview: any mock page, screenshot, proposal, or design sketch not wired to runtime-owned facts. These previews can inform direction, but they cannot close a task or satisfy design QA.
