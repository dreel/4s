#!/usr/bin/env bash
# Print the diff under review: BASE..HEAD, excluding generated code. Shared by
# scripts/review.sh and CI so both compute the same DIFF_SHA256.
# Usage: scripts/ci/review-diff.sh BASE HEAD
set -euo pipefail
git diff --no-color --no-ext-diff "$1" "$2" -- . \
  ':(exclude)ui/src/generated' ':(exclude)schema' \
  ':(exclude)Cargo.lock' ':(exclude)ui/package-lock.json'
