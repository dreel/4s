#!/usr/bin/env bash
# Run all automated gates (docs/gates.md) and write a paste-ready report to
# .gates/report.md for the PR description.
#   G2: scripts/check.sh
#   G4: scripts/review.sh (independent fresh-context agent review)
# Exit 0 only if both pass.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "gates.sh: commit your changes first; the gates cover HEAD" >&2
  exit 2
fi
HEAD=$(git rev-parse HEAD)
mkdir -p .gates

echo "==> G2: scripts/check.sh (log: .gates/check.log)"
if scripts/check.sh > .gates/check.log 2>&1; then CHECK=PASS; else CHECK=FAIL; fi
SUMMARY=$(grep -E "checks passed|[0-9]+ passed \(|all checks passed|FAIL|failed" .gates/check.log | grep -v "0 failed" | sed 's/^ *//' | head -8)
echo "    $CHECK"

echo "==> G4: scripts/review.sh"
if scripts/review.sh; then REVIEW=PASS; else REVIEW=FAIL; fi
REVIEW_FILE=.gates/review-$HEAD.md

{
  echo "<!-- gate-report -->"
  echo "### Gate report"
  echo
  echo "Commit: \`$HEAD\`"
  echo
  echo "| Gate | Result |"
  echo "|------|--------|"
  echo "| G2 checks (\`scripts/check.sh\`) | $CHECK |"
  echo "| G4 independent review (\`scripts/review.sh\`) | $REVIEW |"
  echo
  echo '```'
  echo "$SUMMARY"
  echo '```'
  echo
  echo "<details><summary>Independent review</summary>"
  echo
  if [[ -f $REVIEW_FILE ]]; then cat "$REVIEW_FILE"; else echo "(no review output)"; fi
  echo
  echo "</details>"
} > .gates/report.md

echo
echo "G2 checks: $CHECK    G4 review: $REVIEW"
echo "Report: .gates/report.md (paste into the PR description)"
[[ $CHECK == PASS && $REVIEW == PASS ]]
