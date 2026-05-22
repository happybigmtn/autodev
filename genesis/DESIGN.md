# Design Review

## Applicability

This repo has meaningful user-facing surfaces even though it has no graphical UI. The product is a terminal-first operator tool. Its design surface includes command names, help output, status text, markdown ledgers, generated reports, recovery notes, receipt summaries, and error messages.

## Information Architecture

The current command surface has strong components but too many places to ask "what is happening now":

- `auto doctor` answers baseline and readiness questions.
- `auto parallel status` answers execution-host questions.
- `auto quota status` answers account/quota questions.
- `auto audit --everything --everything-phase status` answers audit phase questions.
- `auto health` generates a model-backed quality report.

The design problem is not lack of information. It is that the operator must already know which status command owns the answer. A future no-model `auto status` should aggregate existing facts without replacing the specialized commands.

## Primary User Journeys

### First Run

Desired journey: install or build `auto`, run `auto doctor`, learn whether the checkout is structurally usable, then receive the next safe command for planning or execution.

Current issue: `auto doctor` checks planning health early, so a new or partially initialized repo can fail before the operator has run the planning command that would create the missing surface. `AGENTS.md`, README, and doctor output also differ on whether external model tools are hard requirements or capability warnings.

### Planning And Promotion

Desired journey: generate a reviewable corpus or implementation plan snapshot, inspect it, then explicitly promote root control files only when the operator agrees.

Current issue: accepted decisions describe snapshot-first behavior, but `auto super` still calls generation in root-sync mode. The operator mental model is therefore less safe than the docs imply.

### Execution And Recovery

Desired journey: dispatch a ready task, observe lane state, see whether work landed or is blocked, and recover from partial completion without guessing which file is authoritative.

Current issue: `auto parallel status` is useful, but operator/evidence lane semantics and receipt propagation are currently red in tests. The UI cannot be clearer than the underlying source-of-truth model.

### Completion And Shipping

Desired journey: a completed row has durable proof, `auto ship` can inspect that proof, and stale or failed receipts block release.

Current issue: ship and completion-artifact tests show ambiguity around footer vs JSON receipt fallback, stale receipts, and corrected failed commands.

## State Coverage

The UX should explicitly represent these states:

- Baseline ready: repository root found, binary works, no syntax/help failures.
- Model capable: optional external tools and configured quota accounts are present.
- Planning ready: active control files exist and parse.
- Execution ready: pending rows are dependency-ready and pass strict row validation.
- Running: host/lane state is active and recoverable.
- Partial: worker produced some evidence but not enough durable proof.
- Completed: evidence inspector accepts handoff, artifacts, verification proof, and audit closure.
- Blocked: missing dependency, invalid row, stale receipt, dirty state mismatch, or unsafe credential/config state.

Today these states exist in pieces. The top design need is to surface them consistently, not to add new visual polish.

## Accessibility And Responsiveness

For a terminal CLI, accessibility means predictable output, stable labels, concise summaries, and copy-pasteable commands. Status output should avoid long prose walls, use consistent headings, and make pass/fail/warn categories machine-greppable. Markdown ledgers should keep task rows parseable and avoid decorative formatting that breaks validators.

Responsive behavior is terminal-width behavior. Status lines should degrade cleanly in narrow terminals, and long file paths or receipt messages should wrap without hiding the recommended next action.

## AI-Slop Risk

The main AI-slop risk is generated artifacts that look authoritative while runtime code or tests disagree. The current repo has several examples: stale specs about command count and `doctor`, historical genesis tasks that no longer reflect root state, and accepted decisions that runtime has not fully implemented.

Design response: make generated artifacts visibly subordinate unless promoted, and make status commands render facts from code-owned parsers and receipt inspectors rather than rephrasing generated claims.

## Design Priorities

1. Make validation state visible before adding new planning output.
2. Separate baseline readiness from model/execution readiness in `auto doctor` or a follow-on `auto status`.
3. Use snapshot/promotion language consistently in help, status, and runtime behavior.
4. Make partial completion and stale evidence obvious to an operator.
5. Reconcile the live command list so README, help smoke, tests, and operator mental model agree.

## Not Designing Now

- No web dashboard.
- No marketing-style landing page or documentation-heavy tutorial.
- No new report format until the existing runtime facts are green.
- No decorative terminal UI that makes parseable task rows harder to validate.
