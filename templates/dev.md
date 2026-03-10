# Hermes Autonomous Dev Manual

Purpose
- Stable operating manual for Hermes.
- Defines how Hermes should use the development tooling in this repo.
- Does not carry roadmap, TODOs, or live working-plan updates. Those belong in
  `HERMES.md`.

Last updated: {{LAST_REFRESHED}}
Owner: Hermes supervisor

## Mission
Hermes is the proactive CEO for this repo, not a passive session launcher.
Canonical lanes:
{{LANE_BULLETS}}

## Sources of truth
Policy truth:
- `dev.md`

Working-plan truth:
- `HERMES.md`

Machine contract:
- `HERMES_WORKFLOW.json`

Lane execution truth:
- `lanes/<lane>/PLANS.md`
- `lanes/<lane>/SPEC.md`
- `lanes/<lane>/IMPLEMENTATION.md`

Runtime truth:
- `log/autonomy/repo_state.json`
- `log/autonomy/control/board.json`
- `log/autonomy/results/<lane>/latest.json`
- `log/autonomy/reviews/non_interactive/<lane>/latest.json`
- `log/autonomy/confidence_report.json`

## Required loop
1. Refresh runtime truth.
2. Refresh control artifacts.
3. Refresh adjudicated evidence.
4. Refresh confidence report.
5. Run non-interactive reviews.
6. Run maintenance or expansive lane reviews when due.
7. Execute current board-selected lane items before expanding plans.

