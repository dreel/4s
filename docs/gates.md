# Gates

Every PR passes these gates before a maintainer merges it. They exist so the
project can take many contributions -- mostly agent-written -- without losing
the properties that make 4S what it is (see the principles in
[AGENTS.md](../AGENTS.md)).

Run them all with:

```sh
scripts/gates.sh
```

It runs G2, then G4, and writes `.gates/report.md` to paste into the PR. It
exits non-zero if any automated gate fails.

## G1 Scope

Classify the change as **Fix**, **Extension**, or **Architecture / UX** (see
[CONTRIBUTING.md](../CONTRIBUTING.md#change-classes)). Architecture / UX
changes need an RFC with status `accepted` in `docs/rfcs/` before the
implementation PR can merge.

- Proof: the change class checkbox in the PR, plus the RFC link or one line
  on why none is needed.
- Enforced by: the `pr-gates` CI check (an RFC-class PR must link an RFC file
  whose status on `main` is `accepted`; a PR that only changes `docs/rfcs/`
  is an RFC proposal and is exempt, since merging it is the approval), the independent reviewer (which
  flags misclassified changes with `VERDICT: needs-rfc`), and CODEOWNERS on
  `docs/rfcs/`.

## G2 Checks

`scripts/check.sh` passes: Rust unit tests, generated TypeScript/JSON Schema
in sync with the Rust types, the CLI end-to-end script against a real daemon,
and the Electron end-to-end tests.

- Proof: the gate report.
- Enforced by: the `check` CI workflow, which re-runs `scripts/check.sh` on
  macOS for every PR. CI is authoritative; the pasted report just shows you
  ran it before asking for review.

## G3 Validation

The change was exercised through its real interface, by an agent, and the
result was observed. Tests passing is G2; this is about showing the feature
actually works the way a user (or agent) would use it.

Good evidence:

- CLI commands against a running daemon and their output
  (`4s pattern set ...`, `4s state`, `4s watch --count N`).
- `4s render` results showing the audio does what you claim (onsets at the
  right times, peak/RMS in range).
- A Playwright test that drives the UI, plus the screenshot.
- `virtual_block` input/output for controller changes.
- For a bug fix: the reproduction before, and the same steps after.

Not enough: "tests pass", "works on my machine", or a description with no
commands or output.

- Enforced by: the independent reviewer and the human reviewer.

## G4 Independent review

A fresh agent reviews the diff against the project's principles. It has
**none of the authoring agent's context**: no conversation, no plan, no
memory. It sees only the repo (AGENTS.md, docs/, code) and the diff. That
independence is the point: it catches what the author's agent talked itself
into.

`scripts/review.sh` runs it:

- It computes the diff against the merge-base with `origin/main` and its
  SHA-256.
- It starts a new headless agent process with read-only tools and the
  instructions in [docs/review/reviewer.md](review/reviewer.md).
- It saves the review to `.gates/review-<sha>.md`.

The review ends with machine-readable lines:

```
REVIEWED_SHA: <commit>
DIFF_SHA256: <hash of the reviewed diff>
CHANGE_CLASS: fix | extension | architecture
VERDICT: pass | changes-requested | needs-rfc
```

Only `VERDICT: pass` passes the gate. Address the findings and run it again.
If you disagree with a finding, say so in the PR. The human reviewer decides.

- Default agent: Claude Code (`claude -p`, a new process with no
  conversation or session context). With `ANTHROPIC_API_KEY` set it also runs
  `--bare`, which skips personal memory, hooks, and plugins; with the
  interactive login, your user-level instructions may still load. To use another agent, set
  `REVIEW_CMD` to a command that reads the prompt on stdin and prints the
  review, e.g. `REVIEW_CMD="my-agent --read-only --prompt-stdin"`. It must
  start with no prior context.
- Enforced by: the `pr-gates` CI check verifies the evidence is **present
  and current**: a `VERDICT: pass` block whose `REVIEWED_SHA` is the PR head
  or whose `DIFF_SHA256` still matches the PR's diff (so a clean rebase does
  not need a new review). It cannot prove a pasted review is genuine. It
  always checks the PR as it is now against the rules on the current base
  branch, so after the rules change, re-running it on an older PR can fail
  where it passed before. The
  verification is the canonical re-review a maintainer triggers by adding the
  `agent-review` label; run it before merging outside contributions and
  Architecture / UX changes.
- The canonical run is isolated from the PR: the reviewer instructions and
  scripts come from the base branch, the reviewer is instructed to judge
  against the base branch's principles and docs (`REVIEW_DOCS_ROOT`), it can
  only read files, and project settings in the PR (`.claude/`, `.mcp.json`)
  are ignored. It refuses to run on PRs that
  touch those paths; review them by hand.

## G5 Docs

If the change alters behavior, a principle, a design decision, or how to work
in the repo, the matching docs are updated in the same PR: `docs/*.md`,
AGENTS.md, README, CLI help.

- Proof: the list of docs touched in the PR.
- Enforced by: the independent reviewer and the human reviewer.

## G6 Human approval

A maintainer reviews and merges. CODEOWNERS requires the owner's review for
the principles and design docs, RFCs, the protocol crate (the API), CI, and
the gate scripts themselves, so the rules cannot be weakened in passing.

## Changing the gates

The gates are RFC-class: propose changes with an RFC.
