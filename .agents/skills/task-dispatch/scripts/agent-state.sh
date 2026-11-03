#!/usr/bin/env bash
# What a dead dispatch agent left, for resuming its task: its transcript, its worktree, how far its work got, what it
# was told and the last things it said. Reads the session's subagents dir and the worktree; changes nothing but a fetch.
#
# Usage: agent-state.sh --ledger <progress.json> --task <number>
# Legacy Claude boards: agent-state.sh <agent name> <session id>...
# Prints `transcript: <path>`, `worktree: <path>` and a `work:` line (or `no agent named …` with exit 1):
#   landed <hash>      the branch moved and origin/main holds it: nothing left but cleanup.sh
#   unlanded           commits, uncommitted edits or a rebase still in progress: continue it
#   none               no worktree, or nothing in it: start over from the brief
set -euo pipefail

usage() {
  echo "usage: agent-state.sh --ledger <progress.json> --task <number> | <agent name> <session id>..." >&2
  exit 2
}
[ $# -ge 2 ] || usage
wt="" branch="" start="" transcript="" task=""
if [ "$1" = --ledger ]; then
  [ $# -eq 4 ] && [ "$3" = --task ] || usage
  [[ "$4" =~ ^[0-9]+$ ]] || usage
  ledger=$2
  task=$(jq -c --argjson num "$4" '.tasks[] | select(.num == $num)' "$ledger") || exit 1
  [ -n "$task" ] || { echo "no task $4 in $ledger" >&2; exit 1; }
  wt=$(jq -r '.worktree // empty' <<<"$task")
  branch=$(jq -r '.branch // empty' <<<"$task")
  start=$(jq -r '.base_commit // empty' <<<"$task")
  transcript=$(jq -r '.transcript // empty' <<<"$task")
  name=$(jq -r '.agent // empty' <<<"$task")
  mapfile -t sessions < <(jq -r '.sessions[]?' "$ledger")
  set -- "$name" "${sessions[@]}"
fi
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
if [ -z "$newest" ] && [ -z "$task" ]; then
  echo "no agent named $name"
  exit 1
fi
[ -n "$transcript" ] || [ -z "$newest" ] || transcript=${newest%.meta.json}.jsonl
echo "transcript: $transcript"

work=none
if [ -z "$wt" ] && [ -n "$with_worktree" ]; then
  wt=$(jq -r .worktreePath "$with_worktree")
  branch=$(jq -r '.worktreeBranch // empty' "$with_worktree")
fi
if [ -n "$wt" ]; then
  if [ -d "$wt" ]; then
    echo "worktree: $wt"
    actual_branch=$(git -C "$wt" symbolic-ref --short HEAD 2>/dev/null || true)
    [ -n "$branch" ] || branch=$actual_branch
    git -C "$wt" fetch --quiet origin main
    [ -n "$start" ] || start=$(git -C "$wt" reflog show --format=%H HEAD | tail -1)
    tip=$(git -C "$wt" rev-parse HEAD)
    if [ -n "$(git -C "$wt" status --porcelain)" ] ||
      [ "$actual_branch" != "$branch" ] ||
      ! git -C "$wt" symbolic-ref --quiet HEAD >/dev/null ||
      [ -d "$(git -C "$wt" rev-parse --git-path rebase-merge)" ] ||
      [ -d "$(git -C "$wt" rev-parse --git-path rebase-apply)" ]; then
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
if [ -n "$task" ]; then
  jq -r '.text // .result // empty, .question // empty' <<<"$task"
fi
if [ -n "$transcript" ] && [ -f "$transcript" ]; then
jq -r 'select(.type == "user" and (.isMeta | not) and (.message.content | type) == "string")
  | .message.content' "$transcript"

echo
echo "--- last said"
jq -r 'if .type == "dispatch" then .text else
  select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text end' "$transcript" | tail -n 40
fi
