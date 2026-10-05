// End-to-end: real daemon + real Electron app. Each test drives one side and
// verifies the other, closing the loop in both directions.

import { expect, test } from "@playwright/test";
import { startHarness, type Harness } from "./harness";

let h: Harness;

test.beforeAll(async () => {
  h = await startHarness();
});

test.afterAll(async () => {
  await h?.close();
});

test.beforeEach(async () => {
  await h.rpc("transport.stop", {});
  await h.rpc("project.new", {});
  await expect(h.page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "0");
});

test("UI step click edits the daemon pattern", async () => {
  const { page, rpc } = h;
  await page.getByTestId("step-kick-0").click();
  await page.getByTestId("step-kick-4").click();
  await page.getByTestId("step-snare-4").click({ modifiers: ["Shift"] });
  await expect(page.getByTestId("step-snare-4")).toHaveAttribute("data-level", "2");

  const p = await rpc("pattern.get", { instrument: null, voice: null });
  const kick = p.tracks.find((t) => t.voice === "kick")!.steps;
  const snare = p.tracks.find((t) => t.voice === "snare")!.steps;
  expect(kick.slice(0, 8)).toEqual([1, 0, 0, 0, 1, 0, 0, 0]);
  expect(snare[4]).toBe(2);
});

test("daemon-side changes show up in the UI", async () => {
  const { page, rpc } = h;
  await rpc("param.set", { path: "mixer.1.volume", value: 0.35 });
  await expect(page.getByTestId("volume-1")).toHaveText("35%");

  await rpc("pattern.set", { instrument: null, voice: "closed_hat", steps: [1, 0, 1, 0, 2] });
  await expect(page.getByTestId("step-closed_hat-2")).toHaveAttribute("data-level", "1");
  await expect(page.getByTestId("step-closed_hat-4")).toHaveAttribute("data-level", "2");

  await rpc("param.set", { path: "mixer.1.mute", value: 1 });
  await expect(page.getByTestId("mute-1")).toHaveAttribute("data-on", "true");
  await rpc("param.set", { path: "drums.snare.mute", value: 1 });
  await expect(page.getByTestId("voice-mute-snare")).toHaveAttribute("data-on", "true");
});

test("console: add a 303 on its own channel, mute and solo strips", async () => {
  const { page, rpc } = h;
  await page.getByTestId("add-instrument-type").selectOption("tb303");
  await page.getByTestId("add-instrument").click();
  await expect(page.getByTestId("strip-2")).toBeVisible();
  await expect(page.getByTestId("channel-name-2")).toHaveText("Bass");
  await expect(page.getByTestId("bass-editor-bass")).toBeVisible();
  const g = (await rpc("state.get", {})).graph;
  expect(g.instruments.map((i) => i.id)).toEqual(["drums", "bass"]);
  expect(g.routes).toEqual({ drums: 1, bass: 2 });

  await page.getByTestId("mute-2").click();
  await expect.poll(async () => (await rpc("param.get", { path: "mixer.2.mute" })).value).toBe(1);
  await page.getByTestId("solo-1").click();
  await expect.poll(async () => (await rpc("param.get", { path: "mixer.1.solo" })).value).toBe(1);

  // Clicking the Drums strip's source selects the 808 in the editor.
  await page.getByTestId("strip-source-1").click();
  await expect(page.getByTestId("drum-editor-drums")).toBeVisible();
  await expect(page.getByTestId("strip-1")).toHaveAttribute("data-selected", "true");

  // Removing the instrument removes its now-empty channel.
  await rpc("instrument.remove", { id: "bass", keep_channels: false });
  await expect(page.getByTestId("strip-2")).toHaveCount(0);
});

test("console: add, rename, and remove channels; remove an instrument from the editor", async () => {
  const { page, rpc } = h;
  await page.getByTestId("add-channel").click();
  await expect(page.getByTestId("strip-2")).toBeVisible();
  await expect(page.getByTestId("channel-name-2")).toHaveText("Ch 2");
  expect((await rpc("state.get", {})).graph.channels.map((c) => c.n)).toEqual([1, 2]);

  await page.getByTestId("channel-name-2").dblclick();
  await page.getByTestId("channel-name-input-2").fill("Hats");
  await page.getByTestId("channel-name-input-2").press("Enter");
  await expect
    .poll(async () => (await rpc("state.get", {})).graph.channels.find((c) => c.n === 2)?.name)
    .toBe("Hats");
  await expect(page.getByTestId("channel-name-2")).toHaveText("Hats");

  await page.getByTestId("channel-remove-2").click();
  await expect(page.getByTestId("strip-2")).toHaveCount(0);
  expect((await rpc("state.get", {})).graph.channels.map((c) => c.n)).toEqual([1]);

  // A second 808: the Block follows it after "control with Block", and
  // removing it from the editor hands the Block back to `drums`.
  await rpc("instrument.add", { type: "tr808", id: null, name: null, channel: null, no_channel: false });
  await page.getByTestId("select-drums2").click();
  await page.getByTestId("make-target").click();
  await expect.poll(async () => (await rpc("controller.get", {})).target).toBe("drums2");
  await expect(page.getByTestId("block-target")).toHaveText("drums2");

  await page.getByTestId("remove-drums2").click();
  await expect.poll(async () => (await rpc("instrument.list", {})).instruments.map((i) => i.id)).toEqual(["drums"]);
  await expect.poll(async () => (await rpc("controller.get", {})).target).toBe("drums");
  await expect(page.getByTestId("select-drums2")).toHaveCount(0);
  await expect(page.getByTestId("strip-2")).toHaveCount(0);
});

test("303 note editor edits the daemon's note pattern, and back", async () => {
  const { page, rpc } = h;
  await rpc("instrument.add", { type: "tb303", id: null, name: null, channel: null, no_channel: false });
  await page.getByTestId("select-bass").click();
  await page.getByTestId("note-0").selectOption("36");
  await page.getByTestId("accent-0").click();
  await page.getByTestId("note-2").selectOption("43");
  await page.getByTestId("slide-2").click();
  await expect
    .poll(async () => (await rpc("pattern.get_notes", { instrument: "bass" })).steps.slice(0, 3))
    .toEqual([
      { note: 36, accent: true, slide: false },
      { note: null, accent: false, slide: false },
      { note: 43, accent: false, slide: true },
    ]);

  const steps = (await rpc("pattern.get_notes", { instrument: "bass" })).steps;
  steps[5] = { note: 48, accent: false, slide: false };
  await rpc("pattern.set_notes", { instrument: "bass", steps });
  await expect(page.getByTestId("note-5")).toHaveValue("48");

  await page.getByTestId("bass-audition").click();
  await expect(page.getByTestId("bass-last-note")).toHaveAttribute("data-note", "36");

  await page.getByTestId("bass-waveform").click();
  await expect.poll(async () => (await rpc("param.get", { path: "bass.waveform" })).value).toBe(1);
});

test("808 voice output: route a voice to its own channel from the UI and over RPC", async () => {
  const { page, rpc } = h;
  const ch = await rpc("channel.add", { name: "Kick" });
  await page.getByTestId("out-kick").selectOption(String(ch.n));
  await expect.poll(async () => (await rpc("state.get", {})).graph.routes["drums.kick"]).toBe(ch.n);
  await expect(page.getByTestId(`strip-source-${ch.n}`)).toHaveText("drums.kick");

  await rpc("route.set", { source: "drums.kick", channel: null });
  await expect(page.getByTestId("out-kick")).toHaveValue("");

  await rpc("param.set", { path: "drums.kick.level", value: 0.5 });
  await expect(page.getByTestId("knob-drums.kick.level")).toHaveAttribute("data-value", "0.5");
});

test("transport: play from UI, playhead moves, stop", async () => {
  const { page, rpc } = h;
  await page.getByTestId("transport-toggle").click();
  await expect(page.getByTestId("transport-toggle")).toHaveAttribute("data-playing", "true");
  expect((await rpc("state.get", {})).transport.playing).toBe(true);

  const first = await page.getByTestId("playhead").textContent();
  await expect(page.getByTestId("playhead")).not.toHaveText(first ?? "");

  await page.getByTestId("transport-toggle").click();
  await expect(page.getByTestId("transport-toggle")).toHaveAttribute("data-playing", "false");
  expect((await rpc("state.get", {})).transport.playing).toBe(false);
});

test("virtual Livid Block pads, LEDs, and knobs", async () => {
  const { page, rpc } = h;
  // Pad row 1 (snare), column 3 -> snare step 2.
  await page.getByTestId("pad-1-2").click();
  await expect(page.getByTestId("pad-1-2")).toHaveAttribute("data-lit", "1");
  await expect(page.getByTestId("step-snare-2")).toHaveAttribute("data-level", "1");
  const c = await rpc("controller.get", {});
  expect(c.leds[1][2]).toBe(1);

  // A daemon-side pattern change lights the mirror.
  await rpc("pattern.set_step", { instrument: null, voice: "cowbell", step: 7, level: 1 });
  await expect(page.getByTestId("pad-7-7")).toHaveAttribute("data-lit", "1");

  // Knob mode: decay; knob 1 controls kick decay.
  await page.getByTestId("knob-mode-decay").click();
  await expect.poll(async () => (await rpc("controller.get", {})).knob_mode).toBe("decay");
  await rpc("controller.knob", { index: 0, value: 1 });
  await expect(page.getByTestId("knob-drums.kick.decay")).toHaveAttribute("data-value", "1");
});

test("project save and reload round trip through the UI", async () => {
  const { page, rpc } = h;
  await page.getByTestId("step-kick-0").click();
  await rpc("param.set", { path: "transport.tempo", value: 133 });
  await page.getByTestId("project-path").fill("e2e-beat");
  await page.getByTestId("project-save").click();
  await expect(page.getByTestId("project-name")).toHaveText("e2e-beat.4s");

  await page.getByTestId("project-new").click();
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "0");
  await page.getByRole("button", { name: "e2e-beat.4s" }).click();
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "1");
  await expect(page.getByTestId("tempo-input")).toHaveValue("133");
});

test("show-in-Finder button reveals the saved project bundle", async () => {
  const { app, page, rpc } = h;
  // Record reveals instead of opening real Finder windows.
  await app.evaluate(({ shell }) => {
    (globalThis as { revealed?: string[] }).revealed = [];
    shell.showItemInFolder = (p: string) => {
      (globalThis as { revealed?: string[] }).revealed!.push(p);
    };
  });
  await expect(page.getByTestId("project-reveal")).toBeDisabled();

  await page.getByTestId("project-path").fill("reveal-me");
  await page.getByTestId("project-save").click();
  await expect(page.getByTestId("project-name")).toHaveText("reveal-me.4s");
  await expect(page.getByTestId("project-reveal")).toBeEnabled();
  await page.getByTestId("project-reveal").click();

  const saved = (await rpc("state.get", {})).project.path!;
  expect(saved).toMatch(/projects\/reveal-me\.4s$/);
  await expect
    .poll(() => app.evaluate(() => (globalThis as { revealed?: string[] }).revealed))
    .toEqual([saved]);
});

test("offline render from the UI reports detected hits", async () => {
  const { page } = h;
  for (const s of [0, 4, 8, 12]) await page.getByTestId(`step-kick-${s}`).click();
  await expect(page.getByTestId("step-kick-12")).toHaveAttribute("data-level", "1");
  await page.getByTestId("render-button").click();
  await expect(page.getByTestId("render-result")).toContainText("4 hits, 4 onsets");
});

test("screenshot of a full groove for visual review", async () => {
  const { page, rpc } = h;
  await rpc("pattern.set", { instrument: null, voice: "kick", steps: [2, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0] });
  await rpc("pattern.set", { instrument: null, voice: "snare", steps: [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0] });
  await rpc("pattern.set", { instrument: null, voice: "closed_hat", steps: [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 2] });
  await rpc("pattern.set", { instrument: null, voice: "cowbell", steps: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2] });
  await rpc("instrument.add", { type: "tb303", id: null, name: null, channel: null, no_channel: false });
  const notes = Array.from({ length: 64 }, () => ({ note: null as number | null, accent: false, slide: false }));
  for (const [i, n, a, sl] of [
    [0, 36, true, false],
    [2, 36, false, false],
    [3, 39, false, true],
    [4, 43, false, false],
    [7, 48, true, false],
    [10, 34, false, false],
    [12, 36, false, true],
    [13, 31, true, false],
  ] as const)
    notes[i] = { note: n, accent: a, slide: sl };
  await rpc("pattern.set_notes", { instrument: "bass", steps: notes });
  await rpc("transport.play", {});
  await page.getByTestId("select-drums").click();
  await page.waitForTimeout(600);
  await page.screenshot({ path: "test-results/groove.png", fullPage: true });
  await page.getByTestId("select-bass").click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: "test-results/bass.png", fullPage: true });
  await rpc("transport.stop", {});
});
