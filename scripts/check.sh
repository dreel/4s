#!/usr/bin/env bash
# Run every validation: Rust tests, generated-code freshness, UI typecheck,
# CLI end-to-end, and Electron end-to-end.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"

echo "== cargo test"; cargo test -q
echo "== cargo build"; cargo build -q
echo "== generated bindings are up to date"
cargo run -q -p fours-protocol --bin gen-bindings >/dev/null
if ! git diff --quiet -- ui/src/generated schema || [ -n "$(git ls-files --others --exclude-standard ui/src/generated schema)" ]; then
  echo "generated files changed; commit the output of: cargo run -p fours-protocol --bin gen-bindings"
  git status --short ui/src/generated schema
  exit 1
fi
echo "== ui typecheck"; (cd ui && npx tsc --noEmit -p .)
echo "== gate tooling tests"; node --test --test-reporter=dot scripts/ci/*.test.mjs
echo "== cli e2e"; scripts/e2e-cli.sh
echo "== electron e2e"; (cd ui && npm run -s test:e2e)
echo "all checks passed"
