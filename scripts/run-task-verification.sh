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
stdout_file=$(mktemp)
stderr_file=$(mktemp)
cleanup() {
  rm -f "$stdout_file" "$stderr_file"
}
trap cleanup EXIT

cd "$repo_root"

set +e
bash -lc '"$@"' bash "$@" > >(tee "$stdout_file") 2> >(tee "$stderr_file" >&2)
status=$?
set -e

receipt_args=(scripts/verification_receipt.py record)
for arg in "$@"; do
  receipt_args+=("--argv=$arg")
done
receipt_args+=("--stdout-file=$stdout_file" "--stderr-file=$stderr_file")
receipt_args+=(-- "$task_id" "$command" "$status")

python3 "${receipt_args[@]}" || {
    if [[ "$status" -eq 0 ]]; then
        echo "verification-receipt: error: command passed but receipt recording failed" >&2
        exit 1
    fi
    echo "verification-receipt: warning: failed to record receipt" >&2
}
exit "$status"
