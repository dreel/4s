#!/usr/bin/env bash
# G4: independent review. Starts a fresh agent process -- no conversation,
# plan, or memory from whoever wrote the change -- with read-only tools, the
# reviewer instructions (docs/review/reviewer.md), and the committed diff
# against origin/main. See docs/gates.md.
#
# Env:
#   REVIEW_BASE         base ref (default: origin/main)
#   REVIEW_CMD          agent command reading the prompt on stdin (default: claude -p ...)
#   REVIEW_PROMPT_FILE  reviewer instructions (default: docs/review/reviewer.md;
#                       CI uses the copy from main)
# Writes .gates/review-<sha>.md. Exit 0 only on VERDICT: pass.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "review.sh: commit your changes first; the review covers HEAD" >&2
  exit 2
fi

BASE_REF=${REVIEW_BASE:-origin/main}
if [[ $BASE_REF == origin/main ]]; then git fetch -q origin main 2>/dev/null || true; fi
BASE=$(git merge-base HEAD "$BASE_REF")
HEAD=$(git rev-parse HEAD)
PROMPT_FILE=${REVIEW_PROMPT_FILE:-docs/review/reviewer.md}

mkdir -p .gates
DIFF=.gates/diff-$HEAD.patch
"$SCRIPTS/ci/review-diff.sh" "$BASE" "$HEAD" > "$DIFF"
if [[ ! -s $DIFF ]]; then
  echo "review.sh: no changes between $BASE_REF and HEAD" >&2
  exit 2
fi
HASH=$(shasum -a 256 < "$DIFF" | cut -d' ' -f1)

PROMPT=.gates/review-prompt-$HEAD.md
{
  cat "$PROMPT_FILE"
  echo
  echo "## This review"
  echo
  echo "- Base commit: $BASE"
  echo "- Head commit (REVIEWED_SHA): $HEAD"
  echo "- Diff SHA-256 (DIFF_SHA256): $HASH"
  echo "- Generated files (ui/src/generated, schema, lockfiles) are excluded from the diff below."
  echo
  echo "### Changed files"
  echo '```'
  git diff --stat "$BASE" "$HEAD"
  echo '```'
  echo
  echo "### Diff"
  echo '```diff'
  cat "$DIFF"
  echo '```'
} > "$PROMPT"

OUT=.gates/review-$HEAD.md
echo "==> independent review of $HEAD (diff $HASH)" >&2
if [[ -n ${REVIEW_CMD:-} ]]; then
  bash -c "$REVIEW_CMD" < "$PROMPT" > "$OUT"
else
  command -v claude >/dev/null || {
    echo "review.sh: 'claude' not found; install Claude Code or set REVIEW_CMD" >&2
    exit 2
  }
  # --bare also skips personal memory, hooks, and plugins, but needs an API
  # key (it does not use the interactive login). Without it the session still
  # starts with no conversation context.
  BARE=()
  [[ -n ${ANTHROPIC_API_KEY:-} ]] && BARE=(--bare)
  claude -p ${BARE[@]+"${BARE[@]}"} --no-session-persistence \
    --allowedTools "Read" "Grep" "Glob" "Bash(git log:*)" "Bash(git show:*)" "Bash(git diff:*)" \
    --disallowedTools "Edit" "Write" "NotebookEdit" "WebFetch" "WebSearch" \
    < "$PROMPT" > "$OUT"
fi

field() { grep -E "^$1:" "$OUT" | tail -1 | sed -E "s/^$1:[[:space:]]*//" | tr -d '[:space:]'; }
VERDICT=$(field VERDICT)
if [[ $(field REVIEWED_SHA) != "$HEAD" || $(field DIFF_SHA256) != "$HASH" || -z $VERDICT ]]; then
  echo "review.sh: review output is missing or has wrong REVIEWED_SHA/DIFF_SHA256/VERDICT lines; see $OUT" >&2
  exit 1
fi
echo "==> VERDICT: $VERDICT ($OUT)" >&2
[[ $VERDICT == pass ]]
