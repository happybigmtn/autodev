# AGENTS.md

This repo is the reusable Hermes/Codex autodev framework scaffold.

## Start here

1. Read `README.md`
2. Read `docs/README.md`
3. Read `docs/operating-model.md`
4. Read `docs/adoption-guide.md`

## Editing rules

- Keep this repo generic; do not add autonomy-specific lane names or domain
  logic.
- Prefer templates and bootstrap tooling over hard-coded per-repo rules.
- When behavior changes, update templates/docs and scripts together.

## Verification

- `python3 -m unittest tests/test_bootstrap_repo.py tests/test_doctrine_sync.py tests/test_status_report.py`

