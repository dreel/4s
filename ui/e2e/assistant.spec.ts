// End-to-end: the assistant panel (RFC 0013) in the real app against a real
// headless daemon and a fake Anthropic API that replays scripted tool
// calls. No real key: the key typed in settings is a dummy, kept for the
// session only (FOURS_ASSISTANT_SESSION_KEY=1, no keychain access).

import { expect, test } from "@playwright/test";
import { mkdtempSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import { startHarness, type Harness } from "./harness";

const require = createRequire(import.meta.url);
const { startFakeAnthropic } = require("./fake-anthropic.cjs") as {
  startFakeAnthropic: (script: unknown[]) => Promise<{ url: string; requests: { body: any }[]; close: () => Promise<void> }>;
};

const SCRIPT = [
  {
    match: "make the kick punchier",
    replies: [
      {
        content: [
          { type: "text", text: "Shortening the kick's decay and adding a compressor. " },
          { type: "tool_use", name: "find_params", input: { prefix: "drums.kick." } },
          { type: "tool_use", name: "set_params", input: { changes: [{ path: "drums.kick.decay", value: 0.3 }, { path: "drums.kick.tone", value: 0.7 }], why: "punch" } },
          { type: "tool_use", name: "rpc", input: { method: "device.add", params: { type: "compressor", channel: 1 } } },
        ],
      },
      { content: [{ type: "text", text: "Done: decay 0.5 -> 0.3, tone 0.5 -> 0.7, and a compressor on the drums." }] },
    ],
  },
  {
    match: "slowly",
    replies: [
      { content: [{ type: "tool_use", name: "set_params", input: { changes: [{ path: "drums.snare.tone", value: 0.9 }] } }] },
      { content: [{ type: "text", text: "Now thinking about the next change for a long time " }], hang: true, delayMs: 20 },
    ],
  },
  {
    match: "load my other song",
    replies: [
      { content: [{ type: "tool_use", name: "rpc", input: { method: "project.load", params: { name: "other" } } }] },
      { content: [{ type: "text", text: "I can't open projects; use the project panel." }] },
    ],
  },
];

let h: Harness;
let fake: Awaited<ReturnType<typeof startFakeAnthropic>>;

test.beforeAll(async () => {
  fake = await startFakeAnthropic(SCRIPT);
  h = await startHarness({
    env: {
      ANTHROPIC_BASE_URL: fake.url,
      FOURS_DATA_DIR: mkdtempSync(path.join(tmpdir(), "4s-e2e-assistant-")),
      FOURS_ASSISTANT_SESSION_KEY: "1",
      ANTHROPIC_API_KEY: "",
    },
  });
});

test.afterAll(async () => {
  await h?.close();
  await fake?.close();
});

test.beforeEach(async () => {
  await h.rpc("transport.stop", {});
  await h.rpc("project.new", {});
  await expect(h.page.getByTestId("history")).toHaveAttribute("data-undo", "0");
});

async function openPanel() {
  const { page } = h;
  if ((await page.getByTestId("assistant-panel").count()) === 0) await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByTestId("assistant-panel")).toBeVisible();
}

async function ask(text: string) {
  const { page } = h;
  await page.getByTestId("assistant-input").fill(text);
  await page.getByTestId("assistant-input").press("Enter");
}

test("settings, then 'punchier kick': params change, chips, one undo step, Undo reverts it all", async () => {
  const { page, rpc } = h;
  // Cmd-K toggles the drawer, as does the header button.
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByTestId("assistant-panel")).toBeVisible();
  await page.getByTestId("assistant-toggle").click();
  await expect(page.getByTestId("assistant-panel")).toHaveCount(0);
  await openPanel();

  // Key and model in the settings popover. The key never comes back.
  await page.getByTestId("assistant-settings-toggle").click();
  await expect(page.getByTestId("assistant-key-source")).toHaveAttribute("data-source", "none");
  await page.getByTestId("assistant-key").fill("sk-ant-dummy-for-tests");
  await page.getByTestId("assistant-key-save").click();
  await expect(page.getByTestId("assistant-key-source")).toHaveAttribute("data-source", "session");
  await expect(page.getByTestId("assistant-key")).toHaveValue("");
  await page.getByTestId("assistant-model").selectOption("claude-opus-5-5");
  await page.getByTestId("assistant-settings-toggle").click();

  const decay = (await rpc("param.get", { path: "drums.kick.decay" })).value;
  await ask("make the kick punchier");
  const turn = page.getByTestId("assistant-turn").last();
  await expect(turn).toHaveAttribute("data-state", "end_turn");
  await expect(turn.getByTestId("assistant-text").last()).toContainText("Done: decay 0.5 -> 0.3");
  const chips = turn.getByTestId("assistant-chip");
  await expect(chips).toHaveText([`drums.kick.decay ${decay} -> 0.3`, "drums.kick.tone 0.5 -> 0.7", "+ compressor on ch 1"]);
  await page.screenshot({ path: "test-results/assistant.png" });

  // The daemon has it, as one undo step labelled with the message.
  expect((await rpc("param.get", { path: "drums.kick.decay" })).value).toBe(0.3);
  expect((await rpc("device.list", {})).devices.map((d) => d.type)).toEqual(["compressor"]);
  const hist = await rpc("history.get", {});
  expect(hist.undo).toEqual(["assistant: make the kick punchier"]);
  const journal = await rpc("journal.get", { limit: 20, since: null, user: null });
  expect(journal.entries.filter((e) => e.origin === "assistant").map((e) => e.method)).toEqual([
    "history.group_begin",
    "batch",
    "device.add",
    "history.group_end",
  ]);
  // The request went out with the chosen model and the key we typed.
  const req = fake.requests.at(-1)!.body;
  expect(req.model).toBe("claude-opus-5-5");
  expect(req.messages[0].content[0].text).toMatch(/^\[4S context\] focused instrument: drums \(tr808\)/);

  await turn.getByTestId("assistant-undo").click();
  await expect(turn.getByTestId("assistant-undone")).toHaveText("undone");
  expect((await rpc("param.get", { path: "drums.kick.decay" })).value).toBe(decay);
  expect((await rpc("param.get", { path: "drums.kick.tone" })).value).toBe(0.5);
  expect((await rpc("device.list", {})).devices).toEqual([]);
  await expect(page.getByTestId("history")).toHaveAttribute("data-undo", "0");
});

test("the per-message Undo says what is in the way", async () => {
  const { page, rpc } = h;
  await openPanel();
  await page.getByTestId("assistant-new").click();
  await ask("make the kick punchier");
  const turn = page.getByTestId("assistant-turn").last();
  await expect(turn).toHaveAttribute("data-state", "end_turn");
  // You turn a knob afterwards: that is your latest step now.
  await rpc("param.set", { path: "mixer.1.pan", value: 0.4 });
  await turn.getByTestId("assistant-undo").click();
  await expect(turn.getByTestId("assistant-undone")).toContainText("first");
  expect((await rpc("param.get", { path: "drums.kick.decay" })).value).toBe(0.3);
});

test("Stop ends the turn; what it did is still one undo step", async () => {
  const { page, rpc } = h;
  await openPanel();
  await ask("brighten the snare slowly");
  const turn = page.getByTestId("assistant-turn").last();
  await expect(turn.getByTestId("assistant-text")).toContainText("Now thinking");
  await page.getByTestId("assistant-stop").click();
  await expect(turn).toHaveAttribute("data-state", "stopped");
  await expect(page.getByTestId("assistant-send")).toBeVisible();
  expect((await rpc("param.get", { path: "drums.snare.tone" })).value).toBe(0.9);
  const journal = await rpc("journal.get", { limit: 1, since: null, user: null });
  expect(journal.entries[0].method).toBe("history.group_end");
  expect((await rpc("history.get", {})).undo).toEqual(["assistant: brighten the snare slowly"]);
  await turn.getByTestId("assistant-undo").click();
  await expect(turn.getByTestId("assistant-undone")).toHaveText("undone");
  expect((await rpc("param.get", { path: "drums.snare.tone" })).value).toBe(0.5);
});

test("a method outside the allowlist is refused and changes nothing", async () => {
  const { page, rpc } = h;
  await openPanel();
  await ask("load my other song");
  const turn = page.getByTestId("assistant-turn").last();
  await expect(turn.getByTestId("assistant-error")).toHaveCount(0);
  await expect(turn).toHaveAttribute("data-state", "end_turn");
  const chip = turn.getByTestId("assistant-chip");
  await expect(chip).toHaveAttribute("data-ok", "false");
  await expect(chip).toContainText("'project.load' is not allowed from the assistant");
  await expect(turn.getByTestId("assistant-undo")).toHaveCount(0);
  expect((await rpc("history.get", {})).undo).toEqual([]);
});
