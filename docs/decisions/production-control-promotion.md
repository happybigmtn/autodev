# Decision: Production-Control Promotion Artifact

Root queue truth remains the production-control artifact.

Generated snapshots, design reports, super reports, and audit reports are
proposal or evidence artifacts until an explicit promotion command updates the
root ledger:

- `IMPLEMENTATION_PLAN.md`
- `REVIEW.md`
- promoted `specs/*.md`
- release evidence in `SHIP.md`, `QA.md`, and `HEALTH.md`

Promotion must be explicit. `auto gen --snapshot-only`, `auto design`, and
`auto super` may create reviewable snapshots, but they must not silently replace
root queue truth. Operators promote accepted generated work with
`auto gen --sync-only --output-dir <gen-dir>` or with a checked-in patch that
updates the root ledger directly.

Waivers must be durable. If generated output is rejected, leave the snapshot as
evidence only or add a tombstone note in the relevant report. Do not treat an
unpromoted generated snapshot as active doctrine.
