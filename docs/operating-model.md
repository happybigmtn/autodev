# Operating Model

The framework uses four persistent truth surfaces:

1. `dev.md`
   - stable operator manual
   - explains how Hermes should use the framework
2. `HERMES.md`
   - live board and working queue
   - tracks portfolio-level priorities and progress ledger updates
3. `HERMES_WORKFLOW.json`
   - machine-readable policy contract
   - review modes, lanes, cadence, lane scopes, and allowed write surfaces
4. `lanes/<lane>/PLANS.md`, `SPEC.md`, `IMPLEMENTATION.md`
   - executable lane truth

Default operating rule:
- refresh infrastructure truth first
- execute current board-selected item second
- expand or rewrite the plan only when a fresh review proves the current
  priority is wrong

Interactive steering is recovery-only, not the default engine.

