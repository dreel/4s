# RFC 0003: Automatic CI agent review (Muse Contributor tier)

- Status: implemented
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-04
- Discussion: the PR that introduces this RFC. Merging that PR is the
  maintainer's approval.
- Amends: [RFC 0001](0001-contribution-gates.md) (resolves its open question
  on running the canonical review automatically)

## Summary

Run the independent agent review (G4) in CI automatically on every PR, on
the latest commit, and post it as one comment that is updated in place. Power
it with Meta's Muse model at the Contributor tier, using the maintainer's
`META_API_KEY`, through the same locked-down review harness we use today.
Contributors may run the local review with Muse too, with their own key.

## Motivation

Today the canonical review only runs when a maintainer adds a label, and the
G4 evidence in a PR is whatever the contributor pasted. A pasted review can
be faked; a CI review cannot. The Contributor tier makes a review on every PR
cheap enough to run by default, so every PR gets a trustworthy, independent
review without a maintainer having to ask.

## Design

### Model and harness

- Meta's Model API is Anthropic-compatible
  (`ANTHROPIC_BASE_URL=https://api.meta.ai`, key as bearer token), and Meta
  documents driving Claude Code with Muse models. The Contributor tier is
  chosen by model id: `muse-spark-1.3-contributor`.
- We keep the review harness we already validated: the Claude Code CLI with
  `--restricted` (no command-running tools, no WebFetch, repository
  settings ignored, file tools confined), `--tools Read Grep Glob`,
  `--strict-mcp-config`, `--bare`. The model endpoint and credentials
  change:
  - `ANTHROPIC_BASE_URL=https://api.meta.ai`;
  - `ANTHROPIC_AUTH_TOKEN=$META_API_KEY` (bearer), with `ANTHROPIC_API_KEY`
    unset;
  - `--model muse-spark-1.3-contributor`, also used for any background or
    small-model requests, so nothing asks for a Claude model id;
  - `--bare` stays on for this provider. It is what skips personal memory,
    hooks, and plugins, and the current "only with `ANTHROPIC_API_KEY`"
    condition must cover the bearer-token case;
  - redaction in CI covers `META_API_KEY` (and any Anthropic key).
- Not the `muse exec` CLI: it loads project-local rules, skills, and hooks
  for trusted workspaces, and documents no way to disable that. In CI it
  would run on untrusted PR content with the key present. Revisit if Meta
  documents an isolated read-only mode.
- `scripts/review.sh` gets `REVIEW_PROVIDER=claude|muse`; the default stays
  `claude`. With `muse`, the contributor supplies `META_API_KEY`.

### Workflow

- `pull_request_target` on opened, synchronize, reopened, and
  ready_for_review. Drafts are skipped. Adding the `agent-review` label
  forces a re-run.
- **Volume limit**: `pull_request_target` is not covered by GitHub's
  "require approval for fork PR workflows" setting, so automatic runs are
  limited to authors who already have standing in the repository
  (`author_association` of OWNER, MEMBER, COLLABORATOR, or CONTRIBUTOR).
  For first-time contributors (FIRST_TIME_CONTRIBUTOR, FIRST_TIMER, NONE),
  the workflow posts a note, and a maintainer runs the review by adding the
  `agent-review` label, which only users with write access can do. The
  maintainer should also set a spending limit on the Meta key.
- One run per PR at a time; a new push cancels the old run, so only the
  latest commit is reviewed.
- Unchanged isolation: the reviewer instructions, scripts, and principles
  come from the current base branch; the PR's code is read but never
  executed; the run refuses PRs that change agent configuration
  (`.claude/`, `.mcp.json`, `CLAUDE.md`, `.muse/`) until a human has looked;
  the key is redacted from output.
- Cost and abuse guards: skip diffs over about 4000 lines (excluding
  generated code) with a note (the label still forces a run); cap agent
  turns.
- Output: one sticky PR comment with the verdict, reviewed SHA, model, and
  the full review, updated on each run.

### Data use

At the Contributor tier, Meta may train on prompts and completions. The
review sends the PR diff and repository docs. 4S is a public repository, so
this content is already public. This is disclosed in CONTRIBUTING.md and
docs/gates.md.

## Impact on the principles

- Validation and the gates: G4 becomes trustworthy by default, not
  self-reported.
- Agent-drivable, multiplayer, real-time safety, modularity: no impact (no
  runtime change).

## Alternatives

- Keep label-only: cheaper, but G4 stays self-reported for most PRs.
- Anthropic API: works today with the same harness, but costs more per
  review. Remains available locally (default provider).
- `muse exec`: native, but not isolatable in CI today (see above).

## Migration and compatibility

None for code. The maintainer adds a `META_API_KEY` repository secret.
Without it, the workflow posts a note and does nothing.

## Validation plan

After merge, on a live PR:
- the sticky comment appears with `Reviewer: muse/muse-spark-1.3-contributor`
  and SHA lines matching the head;
- a new push updates the comment rather than adding a second one;
- the key is absent from logs and comments.

If Meta's endpoint rejects the harness's requests, report it and keep the
label-triggered Claude path until it is fixed.

## Open questions

- Should `pr-gates` accept the CI review as G4 evidence instead of a pasted
  one (a later RFC)?
- Re-evaluate `muse exec` if Meta adds an isolated read-only mode.
