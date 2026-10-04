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

  const p = await rpc("pattern.get", { voice: null });
  const kick = p.tracks.find((t) => t.voice === "kick")!.steps;
  const snare = p.tracks.find((t) => t.voice === "snare")!.steps;
  expect(kick.slice(0, 8)).toEqual([1, 0, 0, 0, 1, 0, 0, 0]);
  expect(snare[4]).toBe(2);
});

test("daemon-side changes show up in the UI", async () => {
  const { page, rpc } = h;
  await rpc("param.set", { path: "mixer.3.volume", value: 0.35 });
  await expect(page.getByTestId("volume-3")).toHaveText("35%");

  await rpc("pattern.set", { voice: "closed_hat", steps: [1, 0, 1, 0, 2] });
  await expect(page.getByTestId("step-closed_hat-2")).toHaveAttribute("data-level", "1");
  await expect(page.getByTestId("step-closed_hat-4")).toHaveAttribute("data-level", "2");

  await rpc("param.set", { path: "mixer.2.mute", value: 1 });
  await expect(page.getByTestId("mute-2")).toHaveAttribute("data-on", "true");
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
  await rpc("pattern.set_step", { voice: "cowbell", step: 7, level: 1 });
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
  await rpc("pattern.set", { voice: "kick", steps: [2, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0] });
  await rpc("pattern.set", { voice: "snare", steps: [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0] });
  await rpc("pattern.set", { voice: "closed_hat", steps: [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 2] });
  await rpc("pattern.set", { voice: "cowbell", steps: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2] });
  await rpc("transport.play", {});
  await page.waitForTimeout(600);
  await page.screenshot({ path: "test-results/groove.png", fullPage: true });
  await rpc("transport.stop", {});
});
