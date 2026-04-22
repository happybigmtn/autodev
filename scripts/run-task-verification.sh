#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $2 != "--" ]]; then
  echo "usage: scripts/run-task-verification.sh <task-id> -- <exact verification command>" >&2
  exit 1
fi

task_id=$1
shift 2
command=$*
repo_root=$(git rev-parse --show-toplevel)

cd "$repo_root"

set +e
bash -lc "$command"
status=$?
set -e

python3 scripts/verification_receipt.py record -- "$task_id" "$command" "$status"
exit "$status"
