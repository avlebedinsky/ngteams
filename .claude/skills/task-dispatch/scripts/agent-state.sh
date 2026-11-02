#!/usr/bin/env bash
# What a dead dispatch agent left, for resuming its task: its transcript, its worktree, how far its work got, what it
# was told and the last things it said. Reads the session's subagents dir and the worktree; changes nothing but a fetch.
#
# Usage: agent-state.sh <agent name> <session id>...
# Prints `transcript: <path>`, `worktree: <path>` and a `work:` line (or `no agent named …` with exit 1):
#   landed <hash>      the branch moved and origin/main holds it: nothing left but cleanup.sh
#   unlanded           commits, uncommitted edits or a rebase still in progress: continue it
#   none               no worktree, or nothing in it: start over from the brief
set -euo pipefail

[ $# -ge 2 ] || {
  echo "usage: agent-state.sh <agent name> <session id>..." >&2
  exit 2
}
name=$1
shift

# The newest agent of that name holds the latest words. The newest one with a worktree holds the work: an agent that
# took over a task runs in its predecessor's worktree, so its own metadata names none.
newest=""
with_worktree=""
for session; do
  for meta in "$HOME"/.claude/projects/*/"$session"/subagents/*.meta.json; do
    [ -e "$meta" ] && [ "$(jq -r '.name // empty' "$meta")" = "$name" ] || continue
    if [ -z "$newest" ] || [ "$meta" -nt "$newest" ]; then newest=$meta; fi
    [ -n "$(jq -r '.worktreePath // empty' "$meta")" ] || continue
    if [ -z "$with_worktree" ] || [ "$meta" -nt "$with_worktree" ]; then with_worktree=$meta; fi
  done
done
[ -n "$newest" ] || {
  echo "no agent named $name"
  exit 1
}
transcript=${newest%.meta.json}.jsonl
echo "transcript: $transcript"

work=none
if [ -n "$with_worktree" ]; then
  wt=$(jq -r .worktreePath "$with_worktree")
  branch=$(jq -r '.worktreeBranch // empty' "$with_worktree")
  if [ -d "$wt" ] && [ -n "$branch" ]; then
    echo "worktree: $wt"
    git -C "$wt" fetch --quiet origin main
    start=$(git -C "$wt" reflog show --format=%H "refs/heads/$branch" | tail -1)
    tip=$(git -C "$wt" rev-parse "$branch")
    if [ -n "$(git -C "$wt" status --porcelain)" ] ||
      ! git -C "$wt" symbolic-ref --quiet HEAD >/dev/null; then
      work=unlanded
    elif [ "$tip" != "$start" ]; then
      if git -C "$wt" merge-base --is-ancestor "$tip" origin/main; then
        work="landed $(git -C "$wt" rev-parse --short "$tip")"
      else
        work=unlanded
      fi
    fi
  fi
fi
echo "work: $work"

# What it was told: the brief, then each follow-up. The harness's own notices are marked isMeta.
echo
echo "--- told"
jq -r 'select(.type == "user" and (.isMeta | not) and (.message.content | type) == "string")
  | .message.content' "$transcript"

echo
echo "--- last said"
jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text' "$transcript" | tail -n 40
