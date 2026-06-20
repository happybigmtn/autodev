#!/usr/bin/env bash
# prune-auto.sh — reclaim disk from a repo's regenerable autodev artifacts while
# PRESERVING verification receipts.
#
# Usage: scripts/prune-auto.sh [repo_dir]   (defaults to the current directory)
#
# Removes regenerable autodev working directories:
#   <repo>/.auto/{corpus-staging,fresh-input,design,parallel,logs,qa,health,book,reverse,nemesis,bug}
#   <repo>/gen-*               (gen snapshots; the operative specs + IMPLEMENTATION_PLAN.md are
#                              synced to the repo root and tracked by git)
#   $AUTO_RUN_ROOT/<repo>/*    (off-volume run dirs, if AUTO_RUN_ROOT is set) — except symphony
#
# PRESERVES:
#   <repo>/.auto/symphony     (verification receipts; these are also durable as
#                             Auto-Verification-Receipt footers in closeout commits)
#   everything tracked by git
set -u

REPO="${1:-$(pwd)}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "no such dir: ${1:-}" >&2; exit 2; }
name="$(basename "$REPO")"
before=$(df -Pm "$REPO" | awk 'NR==2{print $4}')

for sub in corpus-staging fresh-input design parallel logs qa health book reverse nemesis bug; do
  d="$REPO/.auto/$sub"
  [ -e "$d" ] && rm -rf "$d" && echo "pruned .auto/$sub"
done

for d in "$REPO"/gen-*; do
  [ -d "$d" ] && rm -rf "$d" && echo "pruned $(basename "$d")"
done

if [ -n "${AUTO_RUN_ROOT:-}" ] && [ -d "$AUTO_RUN_ROOT/$name" ]; then
  find "$AUTO_RUN_ROOT/$name" -mindepth 1 -maxdepth 1 ! -name symphony -exec rm -rf {} + 2>/dev/null || true
  echo "pruned $AUTO_RUN_ROOT/$name/* (kept symphony)"
fi

receipts="$REPO/.auto/symphony/verification-receipts"
if [ -d "$receipts" ]; then
  n=$(find "$receipts" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
  echo "preserved $n verification receipt(s) under .auto/symphony"
fi

after=$(df -Pm "$REPO" | awk 'NR==2{print $4}')
echo "freed ~$((after - before))M on $(df -P "$REPO" | awk 'NR==2{print $1}')"
