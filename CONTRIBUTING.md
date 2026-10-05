# Contributing to 4S

Thanks for helping build 4S. Most contributions here are written with a coding
agent, and the process is designed for that: the design principles live in the
repo, every change is checked by machines and by an independent agent, and big
changes are agreed on by a human before code is written.

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE), the project's license.

If you only read one thing: **run `scripts/gates.sh` and paste its report into
your PR.**

## The workflow

1. **Classify your change** (below). If it is RFC-class, stop and write an RFC
   first; implementation PRs for RFC-class changes need an *accepted* RFC.
2. **Build it** following [AGENTS.md](AGENTS.md), the design docs in
   [docs/](docs/), and for extensions the recipes in
   [docs/extending.md](docs/extending.md). Point your agent at AGENTS.md
   first.
3. **Validate it through its real interface**: drive the daemon with the CLI,
   render audio with `4s render`, run the UI tests, use the virtual Livid
   Block. Keep the commands and what you observed; they go in the PR. Add
   end-to-end tests for new behavior, and skip change-detector unit tests;
   see [docs/testing.md](docs/testing.md).
4. **Run the gates**: `scripts/gates.sh`. It runs every check, then has a
   fresh agent -- with none of your agent's context -- review your diff
   against the project's principles. It writes a report to
   `.gates/report.md`.
5. **Open a PR** using the template. Paste the gate report, your validation
   evidence, and the change class. A maintainer reviews and merges.

New commits after the review need a fresh review. The `pr-gates` check
accepts a review only if its `REVIEWED_SHA` is the PR's head commit, or its
`DIFF_SHA256` still matches the PR's diff (e.g. after a clean rebase).

## Change classes

| Class | Examples | RFC? |
|-------|----------|------|
| **Fix** | bug fix, test, docs fix, refactor with no behavior change | No |
| **Extension** | a new or improved drum voice model, new parameters, a MIDI controller mapping, a new CLI command with matching RPC and UI control, a new UI panel that follows existing patterns -- see [docs/extending.md](docs/extending.md) for recipes | No |
| **Architecture / UX** | new instrument types (there is no instrument framework yet; see [docs/extending.md](docs/extending.md#not-an-extension-needs-an-rfc)); how the engine routes or processes audio; the threading or real-time model; the RPC protocol shape or sync model; daemon lifecycle or topology; the project format beyond an additive migration; the UI's overall structure or interaction model (e.g. a windowing system); the core principles; these gates | **Yes** |

If you are unsure, treat it as RFC-class and open an issue to ask. An
Extension that turns out to need new core concepts (say, a voice that needs
a new kind of routing) becomes RFC-class.

## RFCs

Write the RFC as a PR adding `docs/rfcs/NNNN-short-title.md` from the
[template](docs/rfcs/0000-template.md). A maintainer approves it by merging it
with status `accepted`. Then implement it in a separate PR that links the RFC.
See [docs/rfcs/README.md](docs/rfcs/README.md).

## The gates

| Gate | What | Proof in the PR |
|------|------|-----------------|
| **G1 Scope** | Change classified; RFC-class changes link an accepted RFC | Change class + RFC link (or why none is needed) |
| **G2 Checks** | `scripts/check.sh` passes: Rust tests, generated code in sync, CLI end-to-end, Electron end-to-end | Gate report (CI re-runs this on macOS) |
| **G3 Validation** | The change was exercised through its real interface by an agent | Commands and observed output, renders, screenshots |
| **G4 Independent review** | A fresh-context agent reviewed the diff against the principles and passed it | Review block with `REVIEWED_SHA`, `DIFF_SHA256`, `VERDICT: pass` |
| **G5 Docs** | Docs and AGENTS.md updated where behavior, principles, or decisions changed | List of docs touched |
| **G6 Human approval** | A maintainer approved and merged | GitHub review |

Details, including what counts as good evidence: [docs/gates.md](docs/gates.md).

## Keep PRs reviewable

- Aim for under ~800 changed lines, not counting generated code
  (`ui/src/generated/`, `schema/`). Split larger work into steps that each
  pass the gates.
- One change per PR. Don't mix a refactor with a feature.
- Never edit generated files by hand; change `crates/protocol` and run
  `cargo run -p fours-protocol --bin gen-bindings`.

## Setup

Rust (via [rustup](https://rustup.rs)), Node.js 22+, and for the review gate
the [Claude Code](https://claude.com/claude-code) CLI, which is the review
harness. It runs with your Claude login by default, or against Meta's Muse
model with your own key: `REVIEW_PROVIDER=muse META_API_KEY=... scripts/gates.sh`.
`REVIEW_CMD` plugs in any other agent (see [docs/gates.md](docs/gates.md)).

Every PR is also reviewed in CI by the same reviewer (RFC 0003), using
Meta's Muse model at the Contributor tier. Meta may train on that content
(your diff and the repo's docs), which is public in this repository anyway.

```sh
scripts/dev.sh        # build and run everything
scripts/check.sh      # G2 only
scripts/gates.sh      # all automated gates + report
```
