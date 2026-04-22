#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $2 != "--" ]]; then
  echo "usage: scripts/run-task-verification.sh <task-id> -- <exact verification command>" >&2
  exit 1
fi

task_id=$1
shift 2
command="$*"
repo_root=$(git rev-parse --show-toplevel)

cd "$repo_root"

set +e
bash -lc '"$@"' bash "$@"
status=$?
set -e

receipt_args=(scripts/verification_receipt.py record)
for arg in "$@"; do
  receipt_args+=("--argv=$arg")
done
receipt_args+=(-- "$task_id" "$command" "$status")

python3 "${receipt_args[@]}" || {
    echo "verification-receipt: warning: failed to record receipt" >&2
}
exit "$status"
