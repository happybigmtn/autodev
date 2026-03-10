# Adoption Guide

## 1. Bootstrap a target repo

Run:

```bash
python3 scripts/hermes_autodev/bootstrap_repo.py \
  --target /path/to/repo \
  --lanes frontend,backend,ops
```

That creates:
- `dev.md`
- `HERMES.md`
- `HERMES_WORKFLOW.json`
- lane planning surfaces under `lanes/<lane>/`
- empty `log/autonomy/` directories for generated artifacts

## 2. Fill in repo-specific lane scopes

Edit `HERMES_WORKFLOW.json`:
- replace placeholder `owned_roots`
- replace placeholder `adjacent_roots`
- replace placeholder `product_truth_surfaces`

## 3. Teach Hermes the generic doctrine

Run:

```bash
python3 scripts/hermes_autodev/doctrine_sync.py
```

This adds a generic framework memory entry to `~/.hermes/memories/MEMORY.md`
and installs a reusable `repo-autodev-supervisor` skill.

## 4. Use the runtime

After an adopted repo begins writing `log/autonomy/` artifacts, inspect it with:

```bash
python3 scripts/hermes_autodev/status_report.py --repo-root /path/to/repo
```

