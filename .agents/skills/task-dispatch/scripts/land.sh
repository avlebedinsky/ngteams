#!/usr/bin/env bash
# Lands the task worktree's branch on main and pushes it, one worktree at a time.
#
# Run from inside the task's worktree once the work is committed and compiles; review and the gate come later, in the
# review-and-gate pass once the board drains (that pass lands its own fixes with this script too). It
# rebases the branch onto origin/main and checks the whole workspace compiles, then, under a lock shared by every
# worktree of the repo, rebases once more onto whatever landed meanwhile (checking again if that moved it), pushes the
# branch as origin's main and fast-forwards the main checkout to it. Only the task's own commits are pushed: a main
# checkout holding commits origin doesn't, or on another branch, stops it before anything is sent.
#
# Exit codes: 0 landed (prints `LANDED <hash>`); 1 a precondition failed; 2 the rebase conflicts (aborted; resolve it
# with `git rebase origin/main` yourself, rerun `cargo check` on the touched crates, then run this again); 3 the
# workspace doesn't compile on the rebased branch (fix, commit, run again); 4 the main checkout can't take the change
# (it isn't on main, its main holds commits origin doesn't, or its uncommitted files overlap the change): nothing was
# pushed, report it, and never stash, reset, force or touch its files; 5 the fetch or the push failed: nothing changed
# locally, run again; 6 pushed, but the main checkout couldn't be fast-forwarded: the change is on origin, report that
# the checkout is behind.
set -euo pipefail

die() {
  local code=$1
  shift
  echo "land: $*" >&2
  exit "$code"
}

rebase_onto_origin() {
  if ! git rebase --quiet origin/main; then
    git rebase --abort
    die 2 "rebasing $branch onto origin/main conflicts"
  fi
}

check_workspace() {
  echo "land: checking the workspace compiles" >&2
  cargo check --quiet --workspace --all-targets || die 3 "the workspace doesn't compile on $branch"
}

wt=$(git rev-parse --show-toplevel)
common=$(cd "$(git rev-parse --git-common-dir)" && pwd)
main_wt=$(git worktree list --porcelain | awk '/^worktree /{print $2; exit}')
branch=$(git symbolic-ref --short HEAD) || die 1 "HEAD is detached"

[ "$wt" != "$main_wt" ] || die 1 "run from a task worktree, not the main checkout"
[ "$branch" != main ] || die 1 "the worktree is on main; commit on the task branch"
[ -z "$(git status --porcelain)" ] || die 1 "uncommitted changes in $wt; commit them first"

# Outside the lock: the long first check runs here, so the lock is held only for what landed meanwhile.
git fetch --quiet origin main || die 5 "fetching origin/main failed"
[ -n "$(git log --oneline origin/main..HEAD)" ] || die 1 "no commits on $branch beyond origin/main"
# The branch's own first reflog entry is where the worktree started. A start origin/main doesn't hold means the branch
# carries someone else's unpushed (or since dropped) commits, which a rebase would replay and push as the task's.
start=$(git reflog show --format=%H "refs/heads/$branch" | tail -1)
[ -n "$start" ] || die 1 "$branch has no reflog to tell where it started"
git merge-base --is-ancestor "$start" origin/main ||
  die 4 "$branch started from commits origin/main doesn't hold"
rebase_onto_origin
check_workspace

exec 9>"$common/land.lock"
echo "land: waiting for the landing lock" >&2
flock 9

[ "$(git -C "$main_wt" symbolic-ref --short HEAD 2>/dev/null)" = main ] ||
  die 4 "the main checkout isn't on main"
git fetch --quiet origin main || die 5 "fetching origin/main failed"
git merge-base --is-ancestor main origin/main ||
  die 4 "the main checkout's main holds commits origin/main doesn't"

before=$(git rev-parse HEAD)
rebase_onto_origin
[ "$before" = "$(git rev-parse HEAD)" ] || check_workspace

# The fast-forward after the push must not fail on the main checkout's own uncommitted files, so overlap stops it
# before anything is sent.
overlap=$(comm -12 \
  <(git diff --name-only main HEAD | sort -u) \
  <(git -C "$main_wt" status --porcelain --untracked-files=all | cut -c4- | sed 's/.* -> //' | sort -u))
[ -z "$overlap" ] ||
  die 4 "the main checkout has uncommitted changes to files this lands: $(echo "$overlap" | tr '\n' ' ')"

git push --quiet origin HEAD:main || die 5 "pushing $branch to origin/main failed"
git -C "$main_wt" merge --quiet --ff-only "$branch" ||
  die 6 "pushed $(git rev-parse --short HEAD), but the main checkout couldn't fast-forward to it"

echo "LANDED $(git rev-parse --short HEAD)"
