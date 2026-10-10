#!/usr/bin/env bash
# G4: independent review. Starts a fresh agent process -- no conversation,
# plan, or memory from whoever wrote the change -- with read-only tools, the
# reviewer instructions (docs/review/reviewer.md), and the committed diff
# against origin/main. See docs/gates.md.
#
# Env:
#   REVIEW_BASE         base ref (default: origin/<base> of this branch's open
#                       PR, found with gh; else origin/main). Set it to the
#                       parent branch for a stacked change without a PR yet.
#   REVIEW_PROVIDER     claude (default: Claude Code login or ANTHROPIC_API_KEY)
#                       or muse (Meta's Muse model via its Anthropic-compatible
#                       API; needs your own META_API_KEY). Same locked-down
#                       harness either way; see RFC 0003.
#   REVIEW_MODEL        model id (muse default: muse-spark-1.3-contributor)
#   REVIEW_CMD          any other agent command reading the prompt on stdin
#   REVIEW_PROMPT_FILE  reviewer instructions (default: docs/review/reviewer.md;
#                       CI uses the copy from main)
#   REVIEW_DOCS_ROOT    read principles/docs from this checkout instead of the
#                       one under review (CI: the base branch)
#   REVIEW_MAX_TURNS    cap on agent turns (default 40)
#   REVIEW_TIMEOUT      seconds before the agent is stopped (default 1800)
# Writes .gates/review-<sha>.md.
# Exit: 0 VERDICT: pass; 1 any other verdict; 3 invalid review output (wrong or
# missing REVIEWED_SHA / DIFF_SHA256 / VERDICT); 2 usage or setup error;
# 4 the agent process itself failed (e.g. auth error, --max-turns reached,
# REVIEW_TIMEOUT hit).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "review.sh: commit your changes first; the review covers HEAD" >&2
  exit 2
fi

# A stacked PR is reviewed against its parent branch, as CI does.
if [[ -z ${REVIEW_BASE:-} ]] && command -v gh >/dev/null; then
  PR_BASE=$(gh pr view --json baseRefName -q .baseRefName 2>/dev/null || true)
  [[ -n $PR_BASE ]] && REVIEW_BASE=origin/$PR_BASE
fi
BASE_REF=${REVIEW_BASE:-origin/main}
if [[ $BASE_REF == origin/* ]]; then git fetch -q origin "${BASE_REF#origin/}" 2>/dev/null || true; fi
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
  if [[ -n ${REVIEW_DOCS_ROOT:-} ]]; then
    echo "- Judge against the principles and docs of the base branch: read AGENTS.md,"
    echo "  CONTRIBUTING.md, and docs/ from $REVIEW_DOCS_ROOT, not from the checkout under"
    echo "  review (the change may modify them; review those modifications as part of the diff)."
  fi
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
PROVIDER=${REVIEW_PROVIDER:-claude}
rm -f "$OUT" "$OUT.tmp" # never leave an older review behind on failure
echo "==> independent review of $HEAD against $BASE_REF (diff $HASH)" >&2

# Run the agent with a time limit (macOS has no `timeout`): a watchdog stops
# it and its children after REVIEW_TIMEOUT seconds.
TIMEOUT=${REVIEW_TIMEOUT:-1800}
run_agent() {
  rm -f "$OUT.timedout"
  "$@" < "$PROMPT" > "$OUT.tmp" &
  local pid=$! rc=0
  (
    s=
    trap 'kill $s 2>/dev/null; exit 0' TERM
    sleep "$TIMEOUT" &
    s=$!
    wait $s
    touch "$OUT.timedout"
    pkill -TERM -P "$pid" 2>/dev/null || true
    kill -TERM "$pid" 2>/dev/null || true
  ) &
  local watchdog=$!
  wait "$pid" || rc=$?
  kill -TERM "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  if [[ -e $OUT.timedout ]]; then
    rm -f "$OUT.timedout"
    echo "review.sh: agent timed out after ${TIMEOUT}s" >&2
    exit 4
  fi
  return "$rc"
}

if [[ -n ${REVIEW_CMD:-} ]]; then
  REVIEWER="custom ($REVIEW_CMD)"
  run_agent bash -c "$REVIEW_CMD" || { echo "review.sh: review command exited $?" >&2; exit 4; }
else
  command -v claude >/dev/null || {
    echo "review.sh: 'claude' (the Claude Code CLI, used as the review harness) not found; install it or set REVIEW_CMD" >&2
    exit 2
  }
  # Locked down: --restricted removes every command-running tool and WebFetch,
  # ignores user/project/local settings files (so a PR cannot grant its
  # reviewer permissions via .claude/settings.json), and confines file tools
  # to the working directories. --bare additionally skips memory, hooks, and
  # plugins; it needs key-based auth (not the interactive login).
  FLAGS=(--restricted --strict-mcp-config --disable-slash-commands --tools Read Grep Glob --no-session-persistence
    --max-turns "${REVIEW_MAX_TURNS:-40}")
  [[ -n ${REVIEW_DOCS_ROOT:-} ]] && FLAGS+=(--add-dir "$REVIEW_DOCS_ROOT")
  ENV=()
  case $PROVIDER in
    claude)
      REVIEWER="claude/${REVIEW_MODEL:-default}"
      [[ -n ${ANTHROPIC_API_KEY:-} ]] && FLAGS=(--bare "${FLAGS[@]}")
      [[ -n ${REVIEW_MODEL:-} ]] && FLAGS+=(--model "$REVIEW_MODEL")
      ;;
    muse)
      if [[ -z ${META_API_KEY:-} ]]; then
        echo "review.sh: REVIEW_PROVIDER=muse needs META_API_KEY (your own Meta Model API key)" >&2
        exit 2
      fi
      MODEL=${REVIEW_MODEL:-muse-spark-1.3-contributor}
      REVIEWER="muse/$MODEL"
      FLAGS=(--bare "${FLAGS[@]}" --model "$MODEL")
      # Meta's Model API is Anthropic-compatible. Every request, including
      # background ones, goes to Meta with the Muse model.
      ENV=(-u ANTHROPIC_API_KEY
        ANTHROPIC_BASE_URL=https://api.meta.ai
        ANTHROPIC_AUTH_TOKEN="$META_API_KEY"
        ANTHROPIC_MODEL="$MODEL"
        ANTHROPIC_SMALL_FAST_MODEL="$MODEL"
        ANTHROPIC_DEFAULT_HAIKU_MODEL="$MODEL"
        ANTHROPIC_DEFAULT_SONNET_MODEL="$MODEL"
        ANTHROPIC_DEFAULT_OPUS_MODEL="$MODEL")
      ;;
    *)
      echo "review.sh: unknown REVIEW_PROVIDER '$PROVIDER' (claude or muse)" >&2
      exit 2
      ;;
  esac
  run_agent env ${ENV[@]+"${ENV[@]}"} claude -p "${FLAGS[@]}" || { echo "review.sh: agent exited $?" >&2; exit 4; }
fi
{ echo "Reviewer: $REVIEWER"; echo; cat "$OUT.tmp"; } > "$OUT"
rm -f "$OUT.tmp"

field() { { grep -E "^$1:" "$OUT" || true; } | tail -1 | sed -E "s/^$1:[[:space:]]*//" | tr -d '[:space:]'; }
VERDICT=$(field VERDICT)
case $VERDICT in pass | changes-requested | needs-rfc) ;; *) VERDICT= ;; esac
if [[ $(field REVIEWED_SHA) != "$HEAD" || $(field DIFF_SHA256) != "$HASH" || -z $VERDICT ]]; then
  echo "review.sh: review output is missing or has wrong REVIEWED_SHA/DIFF_SHA256/VERDICT lines; see $OUT" >&2
  exit 3
fi
echo "==> VERDICT: $VERDICT ($OUT)" >&2
[[ $VERDICT == pass ]] || exit 1
