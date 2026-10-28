#!/usr/bin/env bash
# Removes a finished task's worktree and everything it left: processes still running in it, the worktree with its
# `target/`, its branch and git's record of it.
#
# Usage: cleanup.sh <worktree path> [--discard]
# Without --discard it refuses while the worktree has uncommitted or untracked (not ignored) files, or while its
# branch has a commit origin/main doesn't hold, so landed work is the only work it removes. --discard drops unlanded
# work too; pass it only when the user said to drop that task.
set -euo pipefail

die() {
  echo "cleanup: $*" >&2
  exit 1
}

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: cleanup.sh <worktree path> [--discard]"
discard=0
[ "${2:-}" = --discard ] && discard=1
[ $# -eq 1 ] || [ $discard -eq 1 ] || die "unknown option ${2}"

wt=$(cd "$1" && pwd)
main_wt=$(git -C "$wt" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
[ "$wt" != "$main_wt" ] || die "$wt is the main checkout"
git -C "$main_wt" worktree list --porcelain | grep -qxF "worktree $wt" || die "$wt isn't a worktree of this repo"
branch=$(git -C "$wt" symbolic-ref --short HEAD 2>/dev/null || true)

if [ $discard -eq 0 ]; then
  [ -n "$branch" ] || die "$wt has a detached HEAD"
  [ -z "$(git -C "$wt" status --porcelain)" ] || die "$wt has uncommitted or untracked files"
  git -C "$main_wt" fetch --quiet origin main
  git -C "$main_wt" merge-base --is-ancestor "$branch" origin/main ||
    die "$branch has commits origin/main doesn't hold"
fi

# Leftover processes of the task (a cargo build, a test binary, a wait loop): anything whose working directory is
# inside the worktree.
pids=()
for p in /proc/[0-9]*; do
  cwd=$(readlink "$p/cwd" 2>/dev/null) || continue
  case "$cwd" in "$wt" | "$wt"/*) [ "${p#/proc/}" != $$ ] && pids+=("${p#/proc/}") ;; esac
done
if [ ${#pids[@]} -gt 0 ]; then
  echo "cleanup: stopping ${#pids[@]} leftover process(es): ${pids[*]}" >&2
  kill "${pids[@]}" 2>/dev/null || true
  sleep 2
  kill -9 "${pids[@]}" 2>/dev/null || true
fi

git -C "$main_wt" worktree unlock "$wt" 2>/dev/null || true
git -C "$main_wt" worktree remove --force --force "$wt" 2>/dev/null || rm -rf "$wt"
git -C "$main_wt" worktree prune
[ -z "$branch" ] || git -C "$main_wt" branch --quiet -D "$branch"
echo "removed $wt${branch:+ and $branch}"
