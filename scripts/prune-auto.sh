#!/usr/bin/env bash
# prune-auto.sh — reclaim disk from a repo's regenerable autodev artifacts while
# PRESERVING verification receipts.
#
# Usage: scripts/prune-auto.sh [repo_dir]   (defaults to the current directory)
#
# Removes regenerable autodev working directories:
#   <repo>/.auto/{corpus-staging,fresh-input,design,logs,qa,health,book,reverse,nemesis,bug}
#   <repo>/gen-*               (gen snapshots; the operative specs + IMPLEMENTATION_PLAN.md are
#                              synced to the repo root and tracked by git)
#
# Parallel artifacts are delegated to `auto parallel prune --apply`, whose
# host/lease/ledger/marker checks preserve live or resumable runs. The command
# also resolves a configured AUTO_RUN_ROOT safely.
#
# PRESERVES:
#   <repo>/.auto/symphony     (verification receipts; these are also durable as
#                             Auto-Verification-Receipt footers in closeout commits)
#   everything tracked by git (the script refuses a target containing tracked files)
set -eu

REPO="${1:-$(pwd)}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "no such dir: ${1:-}" >&2; exit 2; }
git_root="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "refusing prune: $REPO is not a Git repository" >&2
  exit 2
}
git_root="$(cd "$git_root" 2>/dev/null && pwd)" || exit 2
if [ "$git_root" != "$REPO" ]; then
  echo "refusing prune: $REPO is not the repository root ($git_root)" >&2
  exit 2
fi
name="$(basename "$REPO")"
before=$(df -Pm "$REPO" | awk 'NR==2{print $4}')

prune_untracked_dir() {
  target="$1"
  rel="$2"
  [ -e "$target" ] || return 0
  tracked="$(git -C "$REPO" ls-files -- "$rel")" || {
    echo "refusing prune: failed to inspect tracked files under $rel" >&2
    return 1
  }
  if [ -n "$tracked" ]; then
    echo "refusing tracked prune target: $rel" >&2
    return 1
  fi
  rm -rf -- "$target"
  echo "pruned $rel"
}

for sub in corpus-staging fresh-input design logs qa health book reverse nemesis bug; do
  d="$REPO/.auto/$sub"
  prune_untracked_dir "$d" ".auto/$sub"
done

for d in "$REPO"/gen-*; do
  [ -d "$d" ] || continue
  rel="$(basename "$d")"
  prune_untracked_dir "$d" "$rel"
done

if [ -d "$REPO/.auto/parallel" ] || { [ -n "${AUTO_RUN_ROOT:-}" ] && [ -d "$AUTO_RUN_ROOT/$name/parallel" ]; }; then
  auto_bin="$(command -v auto || true)"
  if [ -z "$auto_bin" ]; then
    echo "refusing parallel cleanup: auto is not on PATH" >&2
    exit 1
  fi
  (cd "$REPO" && "$auto_bin" parallel prune --include-caches --apply)
fi

receipts="$REPO/.auto/symphony/verification-receipts"
if [ -d "$receipts" ]; then
  n=$(find "$receipts" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
  echo "preserved $n verification receipt(s) under .auto/symphony"
fi

after=$(df -Pm "$REPO" | awk 'NR==2{print $4}')
echo "freed ~$((after - before))M on $(df -P "$REPO" | awk 'NR==2{print $1}')"
