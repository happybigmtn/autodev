# Hermes Autodev Framework

Reusable Hermes/Codex autonomous development scaffold for any repo.

This repo extracts the generic control-plane shape from the current
`/home/r/coding/autonomy` workflow without carrying over its product-specific
lanes, domain logic, or repo-only scripts. It gives you:

- a manual/board/contract doc split
- lane planning templates
- a typed workflow-contract loader
- a generic Hermes memory + skill sync
- a bootstrap script for adopting the framework in another repo
- a compact status reporter for board-style infrastructure checks

## Repo map

- `templates/`: starter `dev.md`, `HERMES.md`, `HERMES_WORKFLOW.json`, and
  lane planning surfaces
- `scripts/hermes_autodev/bootstrap_repo.py`: install the scaffold into a
  target repo
- `scripts/hermes_autodev/doctrine_sync.py`: sync generic autodev memory and
  skill guidance into Hermes
- `scripts/hermes_autodev/status_report.py`: summarize live autodev runtime
  status from generated artifacts
- `scripts/hermes_autodev/workflow_contract.py`: typed loader for the machine
  contract
- `docs/`: operating model and adoption guide

## Quick start

1. Bootstrap a target repo:

```bash
python3 scripts/hermes_autodev/bootstrap_repo.py \
  --target /path/to/target-repo \
  --lanes frontend,backend,ops
```

2. Sync the generic memory/skill into Hermes:

```bash
python3 scripts/hermes_autodev/doctrine_sync.py
```

3. In the adopted repo, fill in lane scopes and proof commands in
   `HERMES_WORKFLOW.json`, then start using the `dev.md` / `HERMES.md` /
   lane-doc split.

## Verification

```bash
python3 -m unittest tests/test_bootstrap_repo.py \
  tests/test_doctrine_sync.py \
  tests/test_status_report.py
```

