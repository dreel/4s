import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { checkBody, checkedClasses } from "./check-pr-body.mjs";

const HEAD = "a".repeat(40);
const DIFF = "d".repeat(64);
const report = (verdict = "pass", sha = HEAD, diff = DIFF) =>
  `<!-- gate-report -->\nREVIEWED_SHA: ${sha}\nDIFF_SHA256: ${diff}\nCHANGE_CLASS: fix\nVERDICT: ${verdict}\n`;
const validation = "## Validation\n\n`4s pattern set kick x---` then `4s state` shows the kick on step 1.\n\n";
const body = (cls, extra = "") =>
  `## Change class\n- [${cls === "fix" ? "x" : " "}] Fix\n- [${cls === "ext" ? "x" : " "}] Extension\n- [${cls === "arch" ? "x" : " "}] Architecture / UX (RFC required)\n\n${validation}${extra}`;
const rfc = (statuses) => (n) => statuses[n] ?? null;
const run = (b, opts = {}) => checkBody({ body: b, headSha: HEAD, diffSha256: DIFF, rfcStatus: rfc({}), ...opts });

test("valid fix passes", () => {
  assert.deepEqual(run(body("fix", report())), { errors: [], warnings: [] });
});

test("change class must be exactly one", () => {
  assert.equal(checkedClasses(body("none")).length, 0);
  assert.match(run(body("none", report())).errors[0], /exactly one/);
});

test("architecture needs an accepted RFC on main", () => {
  assert.match(run(body("arch", report())).errors.join(), /must link an accepted RFC/);
  const linked = body("arch", "RFC: docs/rfcs/0002-routing.md\n" + report());
  assert.match(run(linked).errors.join(), /0002: not on main/);
  assert.match(run(linked, { rfcStatus: rfc({ "0002": "proposed" }) }).errors.join(), /0002: proposed/);
  assert.deepEqual(run(linked, { rfcStatus: rfc({ "0002": "accepted" }) }).errors, []);
});

test("template link to 0000 does not count as an RFC", () => {
  const b = body("arch", "see docs/rfcs/0000-template.md\n" + report());
  assert.match(run(b).errors.join(), /must link an accepted RFC/);
});

test("missing report and failing verdicts are errors", () => {
  assert.match(run(body("fix")).errors.join(), /paste the gate report/);
  assert.match(run(body("fix", report("changes-requested"))).errors.join(), /must be 'pass'/);
  assert.match(run(body("fix", report("needs-rfc"))).errors.join(), /needs-rfc/);
});

test("stale review fails unless the diff is unchanged", () => {
  const stale = body("fix", report("pass", "b".repeat(40), "e".repeat(64)));
  assert.match(run(stale).errors.join(), /stale/);
  const rebased = body("fix", report("pass", "b".repeat(40), DIFF));
  assert.deepEqual(run(rebased).errors, [], "same diff after a rebase is still reviewed");
});

test("empty validation section warns", () => {
  const b = `## Change class\n- [x] Fix\n\n## Validation\n\n<!-- commands -->\n\n## Docs\n${report()}`;
  const r = run(b);
  assert.deepEqual(r.errors, []);
  assert.match(r.warnings.join(), /Validation section looks empty/);
});

test("the PR template's placeholder RFC link does not count", () => {
  const template = readFileSync(new URL("../../.github/pull_request_template.md", import.meta.url), "utf8");
  const arch = template.replace("- [ ] Architecture / UX", "- [x] Architecture / UX") + report();
  const accepted = rfc({ "0002": "accepted" });
  assert.match(run(arch, { rfcStatus: accepted }).errors.join(), /must link an accepted RFC/);
  const linked = arch.replace("RFC: <!--", "RFC: docs/rfcs/0002-audio-routing.md <!--");
  assert.deepEqual(run(linked, { rfcStatus: accepted }).errors, []);
});

test("a verdict inside a comment does not count", () => {
  const b = body("fix", "<!-- gate-report -->\n<!-- VERDICT: pass -->\n");
  assert.match(run(b).errors.join(), /no independent review verdict/);
});
