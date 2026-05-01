# Decision: Auto Super Snapshot Promotion Default

`auto super` should default to snapshot-first production planning.

The CEO run may use unlimited compute to review design, security, reliability,
product, architecture, and release posture, but generated outputs are still
proposals until the deterministic gate passes. The default order is:

1. Snapshot the current planning state.
2. Run design first when enabled because runtime/UI drift is a blocking product
   risk.
3. Run functional reviews and generate queue-ready tasks.
4. Verify the generated implementation plan with the shared execution-row
   validator and deterministic gate.
5. Only then run `auto parallel` when `--execute` is set.

`--sync-only` and explicit promotion remain the durable root-sync mechanism.
This keeps `auto super` powerful without allowing a partially generated CEO
plan to overwrite the root ledger before it is reviewable and executable.
