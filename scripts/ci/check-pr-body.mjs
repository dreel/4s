// Checks a PR description for the gate evidence required by docs/gates.md.
//
// CLI (used by .github/workflows/pr-gates.yml):
//   PR_BODY=... HEAD_SHA=... DIFF_SHA256=... RFC_REF=origin/main CHANGED_FILES=... \
//     node scripts/ci/check-pr-body.mjs
// Exits 1 with a list of problems if any gate evidence is missing.

import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const CLASSES = { fix: /fix/i, extension: /extension/i, architecture: /architecture/i };

/** Which change-class checkboxes are ticked. */
export function checkedClasses(body) {
  const found = [];
  for (const line of body.split(/\r?\n/)) {
    const m = line.match(/^\s*[-*]\s*\[[xX]\]\s*\**\s*(Fix|Extension|Architecture)/);
    if (m) found.push(m[1].toLowerCase());
  }
  return found;
}

function field(body, name) {
  const matches = [...body.matchAll(new RegExp(`^\\s*${name}:\\s*(\\S+)`, "gm"))];
  return matches.length ? matches[matches.length - 1][1].replace(/[`*]/g, "") : null;
}

/**
 * @param {object} p
 * @param {string} p.body        PR description
 * @param {string} p.headSha     PR head commit
 * @param {string} p.diffSha256  hash of scripts/ci/review-diff.sh base..head
 * @param {(num: string) => string | null} p.rfcStatus  status of RFC NNNN on main, or null if absent
 * @param {string[]} [p.changedFiles]  files changed by the PR (to recognize RFC proposals)
 * @returns {{ errors: string[], warnings: string[] }}
 */
export function checkBody({ body, headSha, diffSha256, rfcStatus, changedFiles = [] }) {
  const errors = [];
  const warnings = [];
  body = body ?? "";
  // Template placeholders live in HTML comments; never count them as evidence.
  const text = body.replace(/<!--[\s\S]*?-->/g, "");

  // G1: exactly one change class; RFC-class needs an accepted RFC on main.
  const classes = checkedClasses(text);
  if (classes.length !== 1) {
    errors.push(`G1: tick exactly one change class (found ${classes.length}): Fix, Extension, or Architecture / UX`);
  }
  // An RFC proposal (a PR that only adds or edits RFCs) is approved by being
  // merged, so it cannot already be accepted on main.
  const isRfcProposal =
    changedFiles.length > 0 &&
    changedFiles.every((f) => f.startsWith("docs/rfcs/")) &&
    changedFiles.some((f) => /^docs\/rfcs\/\d{4}-[\w.-]+\.md$/.test(f) && !f.includes("/0000-"));
  if (classes.includes("architecture") && !isRfcProposal) {
    const rfcs = [...new Set([...text.matchAll(/docs\/rfcs\/(\d{4})-[\w.-]+\.md/g)].map((m) => m[1]))].filter(
      (n) => n !== "0000",
    );
    if (rfcs.length === 0) {
      errors.push("G1: Architecture / UX changes must link an accepted RFC (docs/rfcs/NNNN-title.md)");
    } else if (!rfcs.some((n) => rfcStatus(n) === "accepted" || rfcStatus(n) === "implemented")) {
      const states = rfcs.map((n) => `${n}: ${rfcStatus(n) ?? "not on main"}`).join(", ");
      errors.push(`G1: linked RFC must be accepted on main first (${states})`);
    }
  }

  // G4: independent review with a pass verdict, for this code.
  if (!body.includes("<!-- gate-report -->")) {
    errors.push("G2/G4: paste the gate report from scripts/gates.sh (.gates/report.md)");
  }
  const verdict = field(text, "VERDICT");
  const reviewedSha = field(text, "REVIEWED_SHA");
  const reviewedDiff = field(text, "DIFF_SHA256");
  if (!verdict) {
    errors.push("G4: no independent review verdict found (VERDICT: line)");
  } else if (verdict !== "pass") {
    errors.push(`G4: independent review verdict is '${verdict}', must be 'pass'`);
  }
  if (verdict && !(reviewedSha === headSha || (diffSha256 && reviewedDiff === diffSha256))) {
    errors.push(
      `G4: review is stale: it covered ${reviewedSha ?? "?"} but the PR head is ${headSha} and the diff changed; re-run scripts/gates.sh`,
    );
  }

  // G3: validation evidence present (light check; humans judge quality).
  const evidence = text.split(/^##+\s*Validation/im)[1]?.split(/^##\s/m)[0] ?? "";
  if (evidence.trim().length < 40) {
    warnings.push("G3: the Validation section looks empty; include commands and observed output");
  }
  return { errors, warnings };
}

function rfcStatusFromGit(ref) {
  return (num) => {
    try {
      const files = execFileSync("git", ["ls-tree", "--name-only", ref, "docs/rfcs/"], { encoding: "utf8" });
      const file = files.split("\n").find((f) => f.startsWith(`docs/rfcs/${num}-`));
      if (!file) return null;
      const text = execFileSync("git", ["show", `${ref}:${file}`], { encoding: "utf8" });
      return text.match(/^-?\s*Status:\s*(\w+)/im)?.[1]?.toLowerCase() ?? null;
    } catch {
      return null;
    }
  };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { errors, warnings } = checkBody({
    body: process.env.PR_BODY ?? "",
    headSha: process.env.HEAD_SHA ?? "",
    diffSha256: process.env.DIFF_SHA256 ?? "",
    rfcStatus: rfcStatusFromGit(process.env.RFC_REF ?? "origin/main"),
    changedFiles: (process.env.CHANGED_FILES ?? "").split("\n").filter(Boolean),
  });
  for (const w of warnings) console.log(`::warning::${w}`);
  for (const e of errors) console.log(`::error::${e}`);
  if (errors.length) {
    console.log(`\n${errors.length} gate problem(s). See CONTRIBUTING.md and docs/gates.md.`);
    process.exit(1);
  }
  console.log("PR description has the required gate evidence.");
}
