// End-to-end: real daemon + real Electron app. Each test drives one side and
// verifies the other, closing the loop in both directions.

import { expect, test } from "@playwright/test";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
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

test("undo and redo: header buttons, the Edit menu, and edits from other local clients", async () => {
  const { page, rpc, app } = h;
  const kick0 = page.getByTestId("step-kick-0");
  await expect(page.getByTestId("undo")).toBeDisabled();
  await expect(page.getByTestId("redo")).toBeDisabled();

  await kick0.click();
  await expect(kick0).toHaveAttribute("data-level", "1");
  await page.getByTestId("undo").click();
  await expect
    .poll(async () => (await rpc("pattern.get", { instrument: null, voice: "kick" })).tracks[0].steps[0])
    .toBe(0);
  await expect(kick0).toHaveAttribute("data-level", "0");
  await page.getByTestId("redo").click();
  await expect(kick0).toHaveAttribute("data-level", "1");

  // Edit > Undo / Redo in the app menu (Cmd/Ctrl+Z, Shift+Cmd+Z / Ctrl+Y).
  const menu = (id: string) => app.evaluate(({ Menu }, id) => Menu.getApplicationMenu()?.getMenuItemById(id)?.click(), id);
  await menu("undo");
  await expect(kick0).toHaveAttribute("data-level", "0");
  await menu("redo");
  await expect(kick0).toHaveAttribute("data-level", "1");

  // A client that names no user (the CLI, a script) shares the host user's
  // history, so its change is undoable here.
  await rpc("param.set", { path: "mixer.1.volume", value: 0.5 });
  await expect(page.getByTestId("undo")).toHaveAttribute("title", "undo param.set mixer.1.volume");
  await page.getByTestId("undo").click();
  await expect.poll(async () => (await rpc("param.get", { path: "mixer.1.volume" })).value).toBe(1);
  await expect(page.getByTestId("history")).toHaveAttribute("data-redo", "1");
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

  // A second 808: the Block follows it once focused, and removing it from
  // the editor hands the focus back to the first instrument, `drums`.
  await rpc("instrument.add", { type: "tr808", id: null, name: null, channel: null, no_channel: false });
  await page.getByTestId("select-drums2").click();
  await page.getByTestId("make-target").click();
  await expect.poll(async () => (await rpc("controller.get", {})).focus).toBe("drums2");
  await expect(page.getByTestId("block-target")).toHaveText("drums2");

  // The tab's remove button asks once before removing.
  await page.getByTestId("remove-drums2").click();
  await expect(page.getByTestId("remove-drums2")).toHaveAttribute("data-armed", "true");
  expect((await rpc("instrument.list", {})).instruments.map((i) => i.id)).toEqual(["drums", "drums2"]);
  await page.getByTestId("remove-drums2").click();
  await expect.poll(async () => (await rpc("instrument.list", {})).instruments.map((i) => i.id)).toEqual(["drums"]);
  await expect.poll(async () => (await rpc("controller.get", {})).focus).toBe("drums");
  await expect(page.getByTestId("select-drums2")).toHaveCount(0);
  await expect(page.getByTestId("strip-2")).toHaveCount(0);
});

test("console: new instruments fill empty channels; strip inputs swap; channels reorder", async () => {
  const { page, rpc } = h;
  // An empty channel is used before a new one is made.
  await page.getByTestId("add-channel").click();
  await expect(page.getByTestId("strip-2")).toBeVisible();
  await page.getByTestId("add-instrument-type").selectOption("tb303");
  await page.getByTestId("add-instrument").click();
  await expect(page.getByTestId("strip-input-2")).toHaveValue("bass");
  let g = (await rpc("state.get", {})).graph;
  expect(g.channels.map((c) => c.n)).toEqual([1, 2]);
  expect(g.routes).toEqual({ drums: 1, bass: 2 });

  // Picking bass as channel 1's input swaps: drums moves to channel 2.
  await page.getByTestId("strip-input-1").selectOption("bass");
  await expect.poll(async () => (await rpc("state.get", {})).graph.routes).toEqual({ drums: 2, bass: 1 });
  await expect(page.getByTestId("strip-input-2")).toHaveValue("drums");

  // The editor's out select does the same from the instrument side.
  await page.getByTestId("select-bass").click();
  await expect(page.getByTestId("instrument-out-bass")).toHaveValue("1");
  await page.getByTestId("instrument-out-bass").selectOption("2");
  await expect.poll(async () => (await rpc("state.get", {})).graph.routes).toEqual({ drums: 1, bass: 2 });

  // Move buttons reorder the strips (display order only; numbers stay).
  await expect(page.getByTestId("channel-left-1")).toBeDisabled();
  await page.getByTestId("channel-right-1").click();
  await expect.poll(async () => (await rpc("state.get", {})).graph.channels.map((c) => c.n)).toEqual([2, 1]);
  await expect(page.getByTestId("channel-right-1")).toBeDisabled();
  await rpc("channel.move", { n: 1, position: 1 });
  await expect(page.getByTestId("channel-left-1")).toBeDisabled();

  // (none) unroutes the channel's input.
  await page.getByTestId("strip-input-2").selectOption("");
  await expect.poll(async () => (await rpc("state.get", {})).graph.routes).toEqual({ drums: 1 });
  await rpc("instrument.remove", { id: "bass", keep_channels: false });
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
  const ch = await rpc("channel.add", { name: "Kick", kind: null });
  await page.getByTestId("out-kick").selectOption(String(ch.n));
  await expect.poll(async () => (await rpc("state.get", {})).graph.routes["drums.kick"]).toBe(ch.n);
  await expect(page.getByTestId(`strip-source-${ch.n}`)).toHaveText("drums.kick");

  await rpc("route.set", { source: "drums.kick", channel: null, swap: false });
  await expect(page.getByTestId("out-kick")).toHaveValue("");

  await rpc("param.set", { path: "drums.kick.level", value: 0.5 });
  await expect(page.getByTestId("knob-drums.kick.level")).toHaveAttribute("data-value", "0.5");
});

test("MIDI panel connects a device by name; with no bindings it plays the seat's focus", async () => {
  const { page, rpc } = h;
  // A real virtual MIDI port, from a separate process (as in the CLI e2e).
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const bin = path.join(root, "target", "debug", "examples", "virtual_block");
  // Built like 4sd before the suite runs (scripts/check.sh builds it).
  if (!existsSync(bin)) throw new Error("build it with: cargo build -p fours-daemon --example virtual_block");
  const name = `PW Keys ${process.pid}`;
  const dev = spawn(bin, [name], { stdio: "pipe" });
  try {
    // Wait for the device to exist, then for CoreMIDI to announce it to the
    // daemon (asynchronous, and slow on CI runners).
    await new Promise<void>((resolve, reject) => {
      let out = "";
      dev.stdout!.on("data", (d) => {
        out += d;
        if (out.includes("ready")) resolve();
      });
      dev.on("exit", (code) => reject(new Error(`virtual_block exited (${code}): ${out}`)));
      setTimeout(() => reject(new Error(`virtual_block not ready: ${out}`)), 10_000);
    });
    await rpc("instrument.add", { type: "tb303", id: "lead", name: null, channel: null, no_channel: false });
    await expect
      .poll(
        async () => {
          await page.getByTestId("midi-refresh").click();
          return page.getByTestId(`midi-connect-${name}`).count();
        },
        { timeout: 20_000 },
      )
      .toBe(1);
    await page.getByTestId("midi-profile").selectOption("generic");
    await page.getByTestId(`midi-connect-${name}`).click();
    const device = `pw_keys_${process.pid}`;
    await expect
      .poll(async () => (await rpc("midi.ports", {})).connections.find((c) => c.input === name))
      .toMatchObject({ device, profile: "generic" });
    await expect(page.getByTestId(`midi-device-${device}`)).toBeVisible();

    // Focus the 303 from its editor; the device's notes then play it.
    await page.getByTestId("select-lead").click();
    await page.getByTestId("make-target").click();
    await expect.poll(async () => (await rpc("controller.get", {})).focus).toBe("lead");
    await rpc("midi.input", { device, data: [0x90, 36, 100], seat: null, profile: null });
    await expect(page.getByTestId("bass-last-note")).toHaveAttribute("data-note", "36");
    await page.getByTestId(`midi-disconnect-${name}`).click();
    await expect.poll(async () => (await rpc("midi.ports", {})).connections.length).toBe(0);
  } finally {
    dev.kill();
  }
});

test("seats: joined automatically, chooser to ignore, rejoin, and re-ask when the seat goes", async () => {
  const { page, rpc } = h;
  // The app's user matches the host seat, so it sat down without asking.
  const host = (await rpc("seat.list", {})).host;
  await expect(page.getByTestId("seat")).toHaveAttribute("data-seat", host);
  await expect(page.getByTestId("seat-chooser")).toHaveCount(0);

  // Ignore: a seat for this session only.
  await page.getByTestId("seat").click();
  await page.getByTestId("seat-ignore").click();
  await expect(page.getByTestId("seat-chooser")).toHaveCount(0);
  await expect(page.getByTestId("seat")).toContainText("session only");
  const temp = await page.getByTestId("seat").getAttribute("data-seat");
  expect((await rpc("seat.list", {})).seats.find((s) => s.name === temp)?.saved).toBe(false);

  // Back to the saved seat; the empty session-only one goes away.
  await page.getByTestId("seat").click();
  await page.getByTestId(`seat-join-${host}`).click();
  await expect(page.getByTestId("seat")).toHaveAttribute("data-seat", host);
  await expect.poll(async () => (await rpc("seat.list", {})).seats.map((s) => s.name)).not.toContain(temp);

  // The seat is deleted elsewhere: the app asks again.
  await rpc("seat.remove", { name: host });
  await expect(page.getByTestId("seat-chooser")).toBeVisible();
  await page.getByTestId("seat-name").fill("solo");
  await page.getByTestId("seat-create").click();
  await expect(page.getByTestId("seat")).toHaveAttribute("data-seat", "solo");
  await expect.poll(async () => (await rpc("seat.list", {})).host).toBe("solo");

  // A project whose only seat is someone else's: the app asks, and can join it.
  const bundle = path.join(mkdtempSync(path.join(tmpdir(), "4s-seats-")), "theirs.4s");
  mkdirSync(bundle);
  const theirs = {
    format_version: 3,
    instruments: [{ id: "drums", type: "tr808", name: "Drums" }],
    channels: [{ n: 1, name: "Drums" }],
    routes: { drums: 1 },
    params: {},
    patterns: {},
    controller: { follow: true },
    seats: { pwother: { focus: "drums", bindings: [{ device: "pads", target: "drums" }] } },
  };
  writeFileSync(path.join(bundle, "project.json"), JSON.stringify(theirs));
  await rpc("project.load", { path: bundle });
  await expect(page.getByTestId("seat-chooser")).toBeVisible();
  await page.getByTestId("seat-join-pwother").click();
  await expect(page.getByTestId("seat")).toHaveAttribute("data-seat", "pwother");
});

test("a known device model plays with its default layout until the seat edits it", async () => {
  const { page, rpc } = h;
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const bin = path.join(root, "target", "debug", "examples", "virtual_block");
  if (!existsSync(bin)) throw new Error("build it with: cargo build -p fours-daemon --example virtual_block");
  const port = `MPK mini IV MIDI Port pw${process.pid}`;
  const dev = spawn(bin, [port], { stdio: "pipe" });
  try {
    await new Promise<void>((resolve, reject) => {
      let out = "";
      dev.stdout!.on("data", (d) => {
        out += d;
        if (out.includes("ready")) resolve();
      });
      setTimeout(() => reject(new Error(`virtual_block not ready: ${out}`)), 10_000);
    });
    await expect
      .poll(async () => (await rpc("midi.ports", {})).inputs.includes(port), { timeout: 20_000 })
      .toBe(true);
    await rpc("midi.connect", { input: port, output: null, name: null, profile: null });
    await expect(page.getByTestId("midi-device-mpk")).toContainText("akai_mpk_mini_iv");
    await expect(page.getByTestId("seat-default-mpk")).toContainText("Akai MPK mini IV");
    await page.getByTestId("seat-apply-mpk").click();
    await expect(page.getByTestId("seat-default-mpk")).toHaveCount(0);
    await expect(page.getByTestId("binding-1")).toContainText("@tr808");
    await rpc("midi.disconnect", { input: port });
  } finally {
    dev.kill();
  }
});

test("clips: a note off the step grid shows as a note in the editor, and steps still edit the clip", async () => {
  const { page, rpc } = h;
  await page.getByTestId("step-kick-0").click();
  await expect.poll(async () => (await rpc("clip.get", { instrument: "drums", clip: null })).events).toEqual([
    { tick: 0, len: 24, note: 36, velocity: 89 },
  ]);
  await expect(page.getByTestId("clip-note-drums")).toHaveCount(0);
  await rpc("clip.add", { instrument: "drums", clip: null, events: [{ tick: 36, len: 6, note: 38, velocity: 100 }] });
  await expect(page.getByTestId("clip-note-drums")).toHaveText("1 note off the step grid (not shown here)");
  // The step editor shows the clip's length (RFC 0009: every clip has one).
  await rpc("clip.length", { instrument: "drums", clip: null, length: 72 });
  await expect(page.getByTestId("step-kick-2")).toBeVisible();
  await expect(page.getByTestId("step-kick-3")).toHaveCount(0);
  await rpc("clip.length", { instrument: "drums", clip: null, length: 70 });
  await expect(page.getByTestId("clip-note-drums")).toContainText("loops every 70 ticks");
  // The length buttons set it in steps.
  await page.getByTestId("length-32").click();
  await expect.poll(async () => (await rpc("clip.get", { instrument: "drums", clip: null })).length).toBe(32 * 24);
  await expect(page.getByTestId("step-kick-31")).toBeVisible();
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

test("record: settings and the take from the UI; notes played elsewhere land quantized in the focus clip", async () => {
  const { page, rpc } = h;
  await page.getByTestId("metronome-toggle").click();
  await expect.poll(async () => (await rpc("param.get", { path: "metronome.on" })).value).toBe(1);
  await page.getByTestId("record-quantize").selectOption("24");
  await page.getByTestId("record-count-in").selectOption("0");
  await expect.poll(async () => (await rpc("state.get", {})).record).toMatchObject({ quantize: 24, count_in: 0 });

  // A 4-step loop (half a second at 120 bpm) so passes come round quickly:
  // the drums' launched clip, which the take records into (RFC 0009).
  await rpc("clip.length", { instrument: "drums", clip: null, length: 96 });
  await page.getByTestId("record-toggle").click();
  await expect(page.getByTestId("record-toggle")).toHaveAttribute("data-recording", "true");
  expect((await rpc("state.get", {})).transport.playing).toBe(true);
  await page.waitForTimeout(150);
  await rpc("voice.trigger", { voice: "snare", instrument: null, note: null, velocity: null });
  // Written one step before the loop's end, so it plays in the next pass.
  await expect
    .poll(async () => (await rpc("clip.get", { instrument: "drums", clip: null })).events, { timeout: 3000 })
    .toEqual([expect.objectContaining({ note: 38 })]);
  const [e] = (await rpc("clip.get", { instrument: "drums", clip: null })).events;
  expect(e.tick % 24).toBe(0);

  await page.getByTestId("record-toggle").click();
  await expect(page.getByTestId("record-toggle")).toHaveAttribute("data-recording", "false");
  expect((await rpc("state.get", {})).transport.playing).toBe(true);
  await rpc("transport.stop", {});
  await rpc("param.set", { path: "metronome.on", value: 0 });
  await rpc("transport.record", {
    arm: null,
    instrument: null,
    mode: null,
    quantize: 0,
    strength: null,
    count_in: 1,
    offset_ms: null,
  });
});

test("clip pool and launching: the step editor edits the selected clip; launch and back; song controls set the song", async () => {
  const { page, rpc } = h;
  const clip = async (id: number) => (await rpc("clip.get", { instrument: "drums", clip: id })).events;
  const track = async () => (await rpc("song.get", {})).tracks.find((t) => t.instrument === "drums")!;
  await page.getByTestId("step-kick-0").click();
  await page.getByTestId("clip-new-drums").click();
  await expect(page.getByTestId("clip-drums-2")).toHaveAttribute("data-selected", "true");
  // The new clip is empty, and step edits go to it.
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "0");
  await page.getByTestId("step-snare-4").click();
  await expect.poll(() => clip(2)).toEqual([{ tick: 96, len: 24, note: 38, velocity: 89 }]);
  expect(await clip(1)).toEqual([{ tick: 0, len: 24, note: 36, velocity: 89 }]);
  // A new project's drums loop their first clip; selecting another does
  // not change that (RFC 0009), launching it does.
  await expect(page.getByTestId("clip-drums-1")).toHaveAttribute("data-launched", "true");
  expect((await track()).launched).toBe(1);
  await page.getByTestId("clip-launch-drums").click();
  await expect.poll(async () => (await track()).launched).toBe(2);
  await expect(page.getByTestId("clip-drums-2")).toHaveAttribute("data-launched", "true");
  await expect(page.getByTestId("back-all")).toBeEnabled();
  // Selecting clip 1 shows it again, still playing clip 2; a daemon-side
  // rename shows up.
  await page.getByTestId("clip-drums-1").click();
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "1");
  expect((await track()).launched).toBe(2);
  await rpc("clip.rename", { instrument: "drums", clip: 2, name: "Fill" });
  await expect(page.getByTestId("clip-drums-2")).toContainText("Fill");
  // Back to the arrangement.
  await page.getByTestId("clip-back-drums").click();
  await expect.poll(async () => (await track()).launched).toBe(null);
  await expect(page.getByTestId("back-all")).toBeDisabled();
  // Deleting asks once.
  await page.getByTestId("clip-drums-2").click();
  await page.getByTestId("clip-delete-drums").click();
  await page.getByTestId("clip-delete-drums").click();
  await expect(page.getByTestId("clip-drums-2")).toHaveCount(0);

  // Launch quantize, a bar loop, and the start point.
  await page.getByTestId("launch-quantize").selectOption("1");
  await expect.poll(async () => (await rpc("param.get", { path: "transport.launch_quantize" })).value).toBe(1);
  await page.getByTestId("song-loop").selectOption("2");
  await page.getByTestId("loop-end").fill("2");
  await expect.poll(async () => (await rpc("state.get", {})).params).toMatchObject({ "song.loop": 2, "song.loop_end": 2 });
  await page.getByTestId("locate").fill("3");
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(768);
  await rpc("transport.locate", { tick: 0 });
  await expect(page.getByTestId("locate")).toHaveValue("1");
  // Playing shows bar.beat.
  await rpc("song.place", { instrument: "drums", clip: 1, start: 0, length: 768, offset: null });
  await rpc("transport.play", {});
  await expect(page.getByTestId("playhead")).toHaveText(/^\d+\.\d$/);
  await rpc("transport.stop", {});
});

test("launching while playing waits for the bar, the editors' playhead follows the clip where it plays, back to the song for all", async () => {
  const { page, rpc } = h;
  const track = async (id: string) => (await rpc("song.get", {})).tracks.find((t) => t.instrument === id)!;
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  // A slow tempo, so the bar takes a while to come.
  await rpc("param.set", { path: "transport.tempo", value: 40 });
  await rpc("transport.play", {});
  await page.getByTestId("select-bass").click();
  await page.getByTestId("clip-launch-bass").click();
  // Waiting: pending, pulsing, and no playhead in its editor yet.
  await expect.poll(async () => (await track("bass")).pending).toBe(true);
  await expect(page.getByTestId("clip-drums-1")).toHaveCount(0);
  await expect(page.getByTestId("clip-bass-1")).toHaveAttribute("data-launched", "true");
  await expect(page.locator('[data-testid^="note-step-"].ring-2')).toHaveCount(0);
  // At 40 bpm a bar is 6 s.
  await expect.poll(async () => (await track("bass")).pending, { timeout: 8000 }).toBe(false);
  await expect(page.locator('[data-testid^="note-step-"].ring-2')).toHaveCount(1);
  // Back for every track.
  await page.getByTestId("back-all").click();
  await expect.poll(async () => (await track("drums")).launched).toBe(null);
  expect((await track("bass")).launched).toBe(null);
  await rpc("transport.stop", {});
  await rpc("param.set", { path: "transport.tempo", value: 120 });
});

test("arrangement: lanes show placements and repeats, headers select and arm, the ruler locates, the playhead follows", async () => {
  const { page, rpc } = h;
  await rpc("pattern.set_step", { instrument: "drums", voice: "kick", step: 0, level: 1 });
  await rpc("song.place", { instrument: "drums", clip: 1, start: 0, length: 4 * 384, offset: null });
  await rpc("clip.new", { instrument: "drums", name: "Fill", length: 384, select: false });
  await rpc("song.place", { instrument: "drums", clip: 2, start: 4 * 384, length: 384, offset: null });
  const arr = page.getByTestId("arrangement");
  await expect(page.getByTestId("placement-drums-0")).toHaveAttribute("data-length", String(4 * 384));
  await expect(page.getByTestId("placement-drums-1536")).toContainText("Fill");
  await expect(arr).toHaveAttribute("data-end", String(5 * 384));
  // A daemon-side move shows up.
  await rpc("song.move", { instrument: "drums", start: 4 * 384, to: 6 * 384 });
  await expect(page.getByTestId("placement-drums-2304")).toBeVisible();
  await expect(page.getByTestId("placement-drums-1536")).toHaveCount(0);

  // Clicking the ruler locates, snapped to the bar.
  const ruler = page.getByTestId("arr-ruler");
  await arr.scrollIntoViewIfNeeded();
  const box = (await ruler.boundingBox())!;
  const pxPerBar = 96; // default zoom
  await ruler.click({ position: { x: 2 * pxPerBar + 30, y: box.height / 2 } });
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(2 * 384);
  await expect(page.getByTestId("arr-start")).toHaveAttribute("data-tick", String(2 * 384));
  await page.getByTestId("arr-snap").selectOption("96");
  await ruler.click({ position: { x: 3 * pxPerBar + pxPerBar / 4 + 2, y: box.height / 2 } });
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(3 * 384 + 96);

  // The loop range shows on the ruler.
  await rpc("param.set", { path: "song.loop", value: 2 });
  await rpc("param.set", { path: "song.loop_end", value: 2 });
  await expect(page.getByTestId("arr-loop")).toBeVisible();

  // Headers: select opens the editor; arm sets the seat's focus.
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  await expect(page.getByTestId("lane-bass")).toBeVisible();
  await page.getByTestId("track-select-bass").click();
  await expect(page.getByTestId("select-bass")).toHaveAttribute("data-selected", "true");
  await expect(page.getByTestId("arm-drums")).toHaveAttribute("data-armed", "true");
  await page.getByTestId("arm-bass").click();
  await expect(page.getByTestId("arm-bass")).toHaveAttribute("data-armed", "true");
  await expect(page.getByTestId("arm-drums")).toHaveAttribute("data-armed", "false");
  const seats = (await rpc("state.get", {})).seats.seats;
  expect(seats.some((s) => s.config.focus === "bass")).toBe(true);

  // The playhead follows (play always plays the arrangement).
  await rpc("param.set", { path: "song.loop", value: 0 });
  await rpc("transport.locate", { tick: 0 });
  await rpc("transport.play", {});
  const head = page.getByTestId("arr-playhead");
  await expect(head).toBeVisible();
  await expect.poll(async () => Number(await head.getAttribute("data-tick"))).toBeGreaterThan(0);
  await arr.screenshot({ path: "test-results/arrangement.png" });
  await rpc("transport.stop", {});
});

test("arrangement launching: a pool clip's triangle launches it and dims the lane; back returns; L loops the selected placements", async () => {
  const { page, rpc } = h;
  const track = async () => (await rpc("song.get", {})).tracks.find((t) => t.instrument === "drums")!;
  await rpc("clip.new", { instrument: "drums", name: "Fill", length: 384, select: false });
  await rpc("song.place", { instrument: "drums", clip: 1, start: 384, length: 2 * 384, offset: null });
  // A new project's drums are launched: the lane is dimmed.
  await expect(page.getByTestId("lane-drums")).toHaveAttribute("data-launched", "true");
  await expect(page.getByTestId("pool-drums-1")).toHaveAttribute("data-launched", "true");
  await page.getByTestId("launch-drums-2").click();
  await expect.poll(async () => (await track()).launched).toBe(2);
  await expect(page.getByTestId("pool-drums-2")).toHaveAttribute("data-launched", "true");
  // Not a placement drag.
  expect((await track()).arrangement).toHaveLength(1);
  await page.getByTestId("track-back-drums").click();
  await expect.poll(async () => (await track()).launched).toBe(null);
  await expect(page.getByTestId("lane-drums")).toHaveAttribute("data-launched", "false");
  await expect(page.getByTestId("track-back-drums")).toBeDisabled();
  // Loop this placement: bars 2-3 (one batch, one undo step).
  await page.getByTestId("placement-drums-384").click();
  await page.keyboard.press("l");
  await expect.poll(async () => (await rpc("state.get", {})).params).toMatchObject({ "song.loop": 2, "song.loop_start": 1, "song.loop_end": 3 });
  await rpc("history.undo", {});
  await expect.poll(async () => (await rpc("state.get", {})).params["song.loop"]).toBe(1);
  // The toolbar button does the same.
  await page.getByTestId("placement-drums-384").click();
  await page.getByTestId("arr-loop-selection").click();
  await expect.poll(async () => (await rpc("state.get", {})).params["song.loop"]).toBe(2);
});

test("arrangement editing: place from the pool, move, resize, copy, and delete by dragging, one undo step each", async () => {
  const { page, rpc } = h;
  type P = { clip: number; start: number; length: number; offset: number };
  const song = async (): Promise<P[]> =>
    (await rpc("song.get", {})).tracks.find((t: { instrument: string }) => t.instrument === "drums")!.arrangement;
  await rpc("clip.new", { instrument: "drums", name: "Fill", length: 384, select: false });
  await expect(page.getByTestId("pool-drums-2")).toBeVisible();
  // 96 pixels per bar, snapped to the bar (an earlier test may have
  // changed either).
  await page.getByTestId("arr-zoom").selectOption("96");
  await page.getByTestId("arr-snap").selectOption("384");
  const bar = 96;
  // The mouse goes to page coordinates: bring the arrangement on screen and
  // measure afresh before each drag (undo/redo clicks scroll the page).
  const lane = async () => {
    await page.getByTestId("arrangement").scrollIntoViewIfNeeded();
    return (await page.getByTestId("lane-drums").boundingBox())!;
  };
  const drag = async (from: { x: number; y: number }, to: { x: number; y: number }, alt = false) => {
    await page.mouse.move(from.x, from.y);
    if (alt) await page.keyboard.down("Alt");
    await page.mouse.down();
    await page.mouse.move((from.x + to.x) / 2, (from.y + to.y) / 2);
    await page.mouse.move(to.x, to.y);
    await page.mouse.up();
    if (alt) await page.keyboard.up("Alt");
  };
  const center = async (testid: string) => {
    await lane();
    const b = (await page.getByTestId(testid).boundingBox())!;
    return { x: b.x + Math.min(20, b.width / 2), y: b.y + b.height / 2 };
  };
  // Each action is one undo step: undo brings back what was before, redo
  // what it made.
  const oneStep = async (before: P[], after: P[]) => {
    await expect.poll(song).toEqual(after);
    await page.getByTestId("undo").click();
    await expect.poll(song).toEqual(before);
    await page.getByTestId("redo").click();
    await expect.poll(song).toEqual(after);
  };

  // Drag the Fill clip from the pool into bar 3.
  const chip = await center("pool-drums-2");
  let l = await lane();
  await drag(chip, { x: l.x + 2 * bar + 40, y: l.y + l.height / 2 });
  const placed = [{ clip: 2, start: 768, length: 384, offset: 0 }];
  await oneStep([], placed);
  // Move it two bars later.
  await expect(page.getByTestId("placement-drums-768")).toBeVisible();
  let from = await center("placement-drums-768");
  await drag(from, { x: from.x + 2 * bar, y: from.y });
  const moved = [{ clip: 2, start: 1536, length: 384, offset: 0 }];
  await oneStep(placed, moved);
  // Drag its right edge two bars longer: it loops the clip.
  await expect(page.getByTestId("placement-drums-1536")).toBeVisible();
  await lane();
  let edge = (await page.getByTestId("placement-edge-drums-1536").boundingBox())!;
  await drag({ x: edge.x + 2, y: edge.y + 10 }, { x: edge.x + 2 + 2 * bar, y: edge.y + 10 });
  const longer = [{ clip: 2, start: 1536, length: 3 * 384, offset: 0 }];
  await oneStep(moved, longer);
  // And a bar shorter (a remove and a place, in one batch).
  await expect(page.getByTestId("placement-drums-1536")).toHaveAttribute("data-length", String(3 * 384));
  await lane();
  edge = (await page.getByTestId("placement-edge-drums-1536").boundingBox())!;
  await drag({ x: edge.x + 2, y: edge.y + 10 }, { x: edge.x + 2 - bar, y: edge.y + 10 });
  const shorter = [{ clip: 2, start: 1536, length: 2 * 384, offset: 0 }];
  await oneStep(longer, shorter);
  // Alt-drag copies it three bars later; the original stays.
  await expect(page.getByTestId("placement-drums-1536")).toHaveAttribute("data-length", String(2 * 384));
  from = await center("placement-drums-1536");
  await drag(from, { x: from.x + 3 * bar, y: from.y }, true);
  const copied = [...shorter, { clip: 2, start: 2688, length: 2 * 384, offset: 0 }];
  await oneStep(shorter, copied);
  // Click the copy to select it; Delete removes it.
  await expect(page.getByTestId("placement-drums-2688")).toBeVisible();
  from = await center("placement-drums-2688");
  await page.mouse.click(from.x, from.y);
  await expect(page.getByTestId("placement-drums-2688")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  await oneStep(copied, shorter);
  // A clip dropped off its own lane places nothing.
  l = await lane();
  const c1 = await center("pool-drums-1");
  await page.mouse.move(c1.x, c1.y);
  await page.mouse.down();
  await page.mouse.move(l.x + 10 * bar, l.y + l.height / 2);
  await expect(page.getByTestId("placement-ghost-drums")).toBeVisible();
  await page.mouse.move(l.x + 10 * bar, l.y - 60);
  await expect(page.getByTestId("placement-ghost-drums")).toHaveCount(0);
  await page.mouse.up();
  // Undo still takes back the delete: nothing else was journaled.
  await page.getByTestId("undo").click();
  await expect.poll(song).toEqual(copied);
  // Nor on another track's lane.
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  await expect(page.getByTestId("lane-bass")).toBeVisible();
  const c2 = await center("pool-drums-1");
  const bassLane = (await page.getByTestId("lane-bass").boundingBox())!;
  await drag(c2, { x: bassLane.x + 10 * bar, y: bassLane.y + bassLane.height / 2 });
  // (Undo is shared by a user's local clients: adding the bass is still
  // the last step.)
  await page.waitForTimeout(100);
  await expect(page.getByTestId("undo")).toHaveAttribute("title", /instrument\.add/);
  expect(await song()).toEqual(copied);
  // Escape, or a click on an empty lane, clears the selection.
  const sel = page.getByTestId("placement-drums-2688");
  from = await center("placement-drums-2688");
  await page.mouse.click(from.x, from.y);
  await expect(sel).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Escape");
  await expect(sel).toHaveAttribute("data-selected", "false");
  await page.mouse.click(from.x, from.y);
  await expect(sel).toHaveAttribute("data-selected", "true");
  l = await lane();
  await page.mouse.click(l.x + 12 * bar + 10, l.y + l.height / 2);
  await expect(sel).toHaveAttribute("data-selected", "false");
  await page.keyboard.press("Delete");
  await page.waitForTimeout(100);
  expect(await song()).toEqual(copied);
});

test("arrangement multi-select: shift-click across tracks; drag moves and alt-drag copies them together; Delete removes them; one undo step each", async () => {
  const { page, rpc } = h;
  type P = { clip: number; start: number; length: number; offset: number };
  const arr = async (id: string): Promise<P[]> =>
    (await rpc("song.get", {})).tracks.find((t: { instrument: string }) => t.instrument === id)!.arrangement;
  const both = async () => ({ drums: await arr("drums"), bass: await arr("bass") });
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  for (const start of [0, 384]) await rpc("song.place", { instrument: "drums", clip: 1, start, length: 384, offset: null });
  await rpc("song.place", { instrument: "bass", clip: 1, start: 0, length: 384, offset: null });
  await page.getByTestId("arr-zoom").selectOption("96");
  await page.getByTestId("arr-snap").selectOption("384");
  const bar = 96;
  const at = async (testid: string) => {
    await page.getByTestId("arrangement").scrollIntoViewIfNeeded();
    const b = (await page.getByTestId(testid).boundingBox())!;
    return { x: b.x + 20, y: b.y + b.height / 2 };
  };
  const drag = async (from: { x: number; y: number }, dx: number, alt = false) => {
    await page.mouse.move(from.x, from.y);
    if (alt) await page.keyboard.down("Alt");
    await page.mouse.down();
    await page.mouse.move(from.x + dx / 2, from.y);
    await page.mouse.move(from.x + dx, from.y);
    await page.mouse.up();
    if (alt) await page.keyboard.up("Alt");
  };
  const shiftClick = async (testid: string) => {
    const p = await at(testid);
    await page.keyboard.down("Shift");
    await page.mouse.click(p.x, p.y);
    await page.keyboard.up("Shift");
  };
  const oneStep = async (before: object, after: object) => {
    await expect.poll(both).toEqual(after);
    await page.getByTestId("undo").click();
    await expect.poll(both).toEqual(before);
    await page.getByTestId("redo").click();
    await expect.poll(both).toEqual(after);
  };
  const one = (start: number) => ({ clip: 1, start, length: 384, offset: 0 });
  const start = { drums: [one(0), one(384)], bass: [one(0)] };
  await expect(page.getByTestId("placement-bass-0")).toBeVisible();
  const sel = (testid: string) => page.getByTestId(testid);

  // Click one, shift-click the next: both selected. Dragging the first a
  // bar right moves both (the second first, so the first, landing where the
  // second was, does not cut it).
  let p = await at("placement-drums-0");
  await page.mouse.click(p.x, p.y);
  await shiftClick("placement-drums-384");
  await expect(sel("placement-drums-0")).toHaveAttribute("data-selected", "true");
  await expect(sel("placement-drums-384")).toHaveAttribute("data-selected", "true");
  await expect(sel("placement-bass-0")).toHaveAttribute("data-selected", "false");
  // A click on a selected one (no drag) selects it alone; select both again.
  p = await at("placement-drums-384");
  await page.mouse.click(p.x, p.y);
  await expect(sel("placement-drums-0")).toHaveAttribute("data-selected", "false");
  await shiftClick("placement-drums-0");
  await expect(sel("placement-drums-0")).toHaveAttribute("data-selected", "true");
  await drag(await at("placement-drums-0"), bar);
  const moved = { drums: [one(384), one(768)], bass: [one(0)] };
  await oneStep(start, moved);
  // (The undo and redo clicks took the focus, and with it the selection.)
  // Select the pair again; alt-drag copies both two bars on.
  p = await at("placement-drums-384");
  await page.mouse.click(p.x, p.y);
  await shiftClick("placement-drums-768");
  await drag(await at("placement-drums-384"), 2 * bar, true);
  const copied = { drums: [one(384), one(768), one(1152), one(1536)], bass: [one(0)] };
  await oneStep(moved, copied);
  // Shift-click adds one and takes it out again, and adds one on another
  // track; Delete removes the selected ones in one step.
  p = await at("placement-drums-384");
  await page.mouse.click(p.x, p.y);
  await shiftClick("placement-drums-1536");
  await expect(sel("placement-drums-1536")).toHaveAttribute("data-selected", "true");
  await shiftClick("placement-drums-1536");
  await expect(sel("placement-drums-1536")).toHaveAttribute("data-selected", "false");
  await shiftClick("placement-bass-0");
  await expect(sel("placement-bass-0")).toHaveAttribute("data-selected", "true");
  await expect(sel("placement-drums-384")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  const deleted = { drums: [one(768), one(1152), one(1536)], bass: [] };
  await oneStep(copied, deleted);
  // Dragged left past the song's start, the pair stops together at bar 1,
  // a bar apart as before.
  p = await at("placement-drums-768");
  await page.mouse.click(p.x, p.y);
  await shiftClick("placement-drums-1152");
  await drag(await at("placement-drums-768"), -4 * bar);
  await oneStep(deleted, { drums: [one(0), one(384), one(1536)], bass: [] });
});

test("arrangement: drag a placement to another track (its clip is copied into that pool), alt-drag copies it there; one undo step each", async () => {
  const { page, rpc } = h;
  type P = { clip: number; start: number; length: number; offset: number };
  const track = async (id: string) => (await rpc("song.get", {})).tracks.find((t: { instrument: string }) => t.instrument === id)!;
  const pool = async (id: string) =>
    (await rpc("state.get", {})).clips
      .filter((c: { instrument: string }) => c.instrument === id)
      .map((c: { id: number; name: string; length: number | null; events: unknown[] }) => ({ id: c.id, name: c.name, length: c.length, events: c.events }));
  const all = async () => ({
    drums: (await track("drums")).arrangement as P[],
    bass: (await track("bass")).arrangement as P[],
    bassPool: await pool("bass"),
    bassSelected: (await track("bass")).selected,
  });
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  const notes = [
    { tick: 0, len: 24, note: 36, velocity: 100 },
    { tick: 96, len: 48, note: 43, velocity: 90 },
  ];
  await rpc("clip.new", { instrument: "drums", name: "Fill", length: 384, select: false });
  await rpc("clip.add", { instrument: "drums", clip: 2, events: notes });
  await rpc("song.place", { instrument: "drums", clip: 2, start: 384, length: 768, offset: 96 });
  await page.getByTestId("arr-zoom").selectOption("96");
  await page.getByTestId("arr-snap").selectOption("384");
  const bar = 96;
  const drag = async (testid: string, dx: number, toLane: string, alt = false) => {
    await page.getByTestId("arrangement").scrollIntoViewIfNeeded();
    const b = (await page.getByTestId(testid).boundingBox())!;
    const lane = (await page.getByTestId(toLane).boundingBox())!;
    const from = { x: b.x + 20, y: b.y + b.height / 2 };
    const to = { x: from.x + dx, y: lane.y + lane.height / 2 };
    await page.mouse.move(from.x, from.y);
    if (alt) await page.keyboard.down("Alt");
    await page.mouse.down();
    await page.mouse.move((from.x + to.x) / 2, (from.y + to.y) / 2);
    await page.mouse.move(to.x, to.y);
    await page.mouse.up();
    if (alt) await page.keyboard.up("Alt");
  };
  const oneStep = async (before: object, after: object) => {
    await expect.poll(all).toEqual(after);
    await page.getByTestId("undo").click();
    await expect.poll(all).toEqual(before);
    await page.getByTestId("redo").click();
    await expect.poll(all).toEqual(after);
  };
  const fill = (start: number, clip: number) => ({ clip, start, length: 768, offset: 96 });
  const bassClip1 = { id: 1, name: "1", length: 384, events: [] };
  const fillIn = (id: number) => ({ id, name: "Fill", length: 384, events: notes });
  const start = { drums: [fill(384, 2)], bass: [], bassPool: [bassClip1], bassSelected: 1 };
  await expect(page.getByTestId("placement-drums-384")).toBeVisible();
  await expect.poll(all).toEqual(start);

  // Drag it down onto the bass lane and a bar later: the bass pool gets a
  // copy of Fill (its notes, length, and the placement's offset), the drums
  // lose the placement, and the bass keeps its selected clip.
  await drag("placement-drums-384", bar, "lane-bass");
  const moved = { drums: [], bass: [fill(768, 2)], bassPool: [bassClip1, fillIn(2)], bassSelected: 1 };
  await oneStep(start, moved);
  // Alt-drag it back up to the drums, three bars later: a copy; the bass
  // keeps its own.
  await expect(page.getByTestId("placement-bass-768")).toBeVisible();
  await drag("placement-bass-768", 3 * bar, "lane-drums", true);
  const copied = { ...moved, drums: [fill(1920, 3)] };
  await oneStep(moved, copied);
  const drumsPool = await pool("drums");
  expect(drumsPool.map((c: { id: number; name: string }) => [c.id, c.name])).toEqual([[1, "1"], [2, "Fill"], [3, "Fill"]]);
  expect(drumsPool[2].events).toEqual(notes);
  // Straight down (no distance in time) still goes to the other track.
  await expect(page.getByTestId("placement-drums-1920")).toBeVisible();
  await drag("placement-drums-1920", 0, "lane-bass");
  await oneStep(copied, { ...copied, drums: [], bass: [fill(768, 2), fill(1920, 3)], bassPool: [bassClip1, fillIn(2), fillIn(3)] });
});

test("arrangement loop brace: draw, move, resize, and toggle by dragging the ruler, one undo step each; clicks still locate", async () => {
  const { page, rpc } = h;
  type L = { on: number; start: number; end: number };
  const loop = async (): Promise<L> => {
    const p = (await rpc("state.get", {})).params;
    return { on: p["song.loop"], start: p["song.loop_start"], end: p["song.loop_end"] };
  };
  await page.getByTestId("arr-zoom").selectOption("96");
  await page.getByTestId("arr-snap").selectOption("384");
  const bar = 96;
  const strip = async () => {
    await page.getByTestId("arrangement").scrollIntoViewIfNeeded();
    return (await page.getByTestId("arr-loop-strip").boundingBox())!;
  };
  const drag = async (from: { x: number; y: number }, to: { x: number; y: number }) => {
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move((from.x + to.x) / 2, from.y);
    await page.mouse.move(to.x, to.y);
    await page.mouse.up();
  };
  // A small window (CI's) shows only part of the ruler: scroll the handle
  // itself into view before measuring it.
  const handle = async (testid: string) => {
    await strip();
    await page.getByTestId(testid).scrollIntoViewIfNeeded();
    const b = (await page.getByTestId(testid).boundingBox())!;
    return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
  };
  const oneStep = async (before: L, after: L) => {
    await expect.poll(loop).toEqual(after);
    await page.getByTestId("undo").click();
    await expect.poll(loop).toEqual(before);
    await page.getByTestId("redo").click();
    await expect.poll(loop).toEqual(after);
  };
  const whole = { on: 1, start: 0, end: 4 }; // the default: the whole song loops
  await expect.poll(loop).toEqual(whole);
  await expect(page.getByTestId("arr-loop")).toHaveAttribute("data-on", "false");

  // Draw a new brace over bars 5 and 6 (past the old one, near the start so
  // a small window shows it): loop on.
  let b = await strip();
  const y = b.y + b.height / 2;
  await drag({ x: b.x + 4 * bar + 40, y }, { x: b.x + 5 * bar + 40, y });
  const drawn = { on: 2, start: 4, end: 6 };
  await oneStep(whole, drawn);
  await expect(page.getByTestId("arr-loop")).toHaveAttribute("data-on", "true");
  // Move it a bar earlier by its body.
  let p = await handle("arr-loop");
  await drag(p, { x: p.x - bar, y: p.y });
  const moved = { on: 2, start: 3, end: 5 };
  await oneStep(drawn, moved);
  // Its end two bars later, its start a bar earlier.
  await expect(page.getByTestId("arr-loop")).toHaveAttribute("data-start", "3");
  p = await handle("arr-loop-end");
  await drag(p, { x: p.x + 2 * bar, y: p.y });
  const longer = { on: 2, start: 3, end: 7 };
  await oneStep(moved, longer);
  await expect(page.getByTestId("arr-loop")).toHaveAttribute("data-end", "7");
  p = await handle("arr-loop-start");
  await drag(p, { x: p.x - bar, y: p.y });
  const earlier = { on: 2, start: 2, end: 7 };
  await oneStep(longer, earlier);
  // A click on the brace turns the loop off (undo turns it on again).
  await expect(page.getByTestId("arr-loop")).toHaveAttribute("data-start", "2");
  p = await handle("arr-loop");
  await page.mouse.click(p.x, p.y);
  await oneStep(earlier, { ...earlier, on: 0 });
  // A click on empty strip, or the ruler, locates and leaves the loop.
  await page.getByTestId("arr-loop-strip").click({ position: { x: 7 * bar + 30, y: 5 } });
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(7 * 384);
  await page.getByTestId("arr-ruler").click({ position: { x: 3 * bar + 30, y: 24 } });
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(3 * 384);
  expect(await loop()).toEqual({ ...earlier, on: 0 });
  await page.getByTestId("arrangement").screenshot({ path: "test-results/arrangement-loop.png" });
});

test("arrangement piano roll: double-click a placement opens its clip below; edits land in that clip; close", async () => {
  const { page, rpc } = h;
  await rpc("clip.rename", { instrument: "drums", clip: 1, name: "Verse" });
  await rpc("clip.new", { instrument: "drums", name: "Fill", length: 384, select: false });
  await rpc("song.place", { instrument: "drums", clip: 1, start: 0, length: 384, offset: null });
  await rpc("song.place", { instrument: "drums", clip: 2, start: 768, length: 384, offset: null });
  const events = async (clip: number) => (await rpc("clip.get", { instrument: "drums", clip })).events;
  const selected = async () => (await rpc("song.get", {})).tracks.find((t: { instrument: string }) => t.instrument === "drums")!.selected;
  const arrangement = async () => (await rpc("song.get", {})).tracks.find((t: { instrument: string }) => t.instrument === "drums")!.arrangement;
  const before = await arrangement();
  await expect(page.getByTestId("placement-drums-768")).toBeVisible();
  await page.getByTestId("arrangement").scrollIntoViewIfNeeded();

  // Double-click the Fill placement: its clip is selected and opens below.
  await page.getByTestId("placement-drums-768").dblclick();
  const panel = page.getByTestId("arr-roll");
  await expect(panel).toHaveAttribute("data-instrument", "drums");
  await expect.poll(selected).toBe(2);
  await expect(panel.getByTestId("pianoroll-drums")).toContainText("Fill");
  // A click in its grid adds a note to Fill, not to clip 1.
  await panel.getByTestId("roll-zoom").selectOption("1");
  await panel.scrollIntoViewIfNeeded();
  const grid = panel.getByTestId("roll-grid");
  const box = (await grid.boundingBox())!;
  await page.mouse.click(box.x + 50, box.y + 16 + 8); // the snare row, step 3
  await expect.poll(() => events(2)).toEqual([{ tick: 48, len: 24, note: 38, velocity: 100 }]);
  expect(await events(1)).toEqual([]);
  // Delete there removes the note, not the placement selected by the
  // double-click.
  await expect(panel.getByTestId("note-48-38")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  await expect.poll(() => events(2)).toEqual([]);
  expect(await arrangement()).toEqual(before);

  // With the editor's roll open on the same clip too, Delete goes only to
  // the roll last clicked.
  await rpc("clip.add", {
    instrument: "drums",
    clip: 2,
    events: [{ tick: 0, len: 24, note: 36, velocity: 100 }, { tick: 192, len: 24, note: 36, velocity: 100 }],
  });
  await page.getByTestId("view-roll").click();
  const editorRoll = page.getByTestId("editor").getByTestId("pianoroll-drums");
  await editorRoll.getByTestId("note-0-36").click();
  await expect(editorRoll.getByTestId("note-0-36")).toHaveAttribute("data-selected", "true");
  await panel.getByTestId("note-192-36").click();
  await expect(panel.getByTestId("note-192-36")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  await expect.poll(() => events(2)).toEqual([{ tick: 0, len: 24, note: 36, velocity: 100 }]);
  // Closed, the panel hands the keys back: the editor's roll deletes its
  // selected note.
  await page.getByTestId("arr-roll-close").click();
  await expect(editorRoll.getByTestId("note-0-36")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  await expect.poll(() => events(2)).toEqual([]);
  await page.getByTestId("view-steps").click();

  // The other placement opens its own clip.
  await page.getByTestId("arrangement").scrollIntoViewIfNeeded();
  await page.getByTestId("placement-drums-0").dblclick();
  await expect.poll(selected).toBe(1);
  await expect(panel.getByTestId("pianoroll-drums")).toContainText("Verse");
  await page.getByTestId("arr-roll-close").click();
  await expect(panel).toHaveCount(0);
});

test("piano roll: add, move, resize, copy, select, delete, velocity, quantize, and take notes", async () => {
  const { page, rpc } = h;
  const clip = async () => (await rpc("clip.get", { instrument: "drums", clip: null })).events;
  await page.getByTestId("view-roll").click();
  await page.getByTestId("roll-zoom").selectOption("1");
  const grid = page.getByTestId("roll-grid");
  await expect(grid).toBeVisible();
  // The mouse goes to page coordinates, so the whole roll (grid and velocity
  // lane) must be on screen first; a small CI display puts it below the fold.
  await page.getByTestId("pianoroll-drums").scrollIntoViewIfNeeded();
  const box = (await grid.boundingBox())!;
  // 1 px per tick, 16 px rows: the 808's voices, kick first.
  const at = (tick: number, row: number) => ({ x: box.x + tick + 2, y: box.y + row * 16 + 8 });
  const drag = async (from: { x: number; y: number }, to: { x: number; y: number }, alt = false) => {
    await page.mouse.move(from.x, from.y);
    if (alt) await page.keyboard.down("Alt");
    await page.mouse.down();
    await page.mouse.move((from.x + to.x) / 2, (from.y + to.y) / 2);
    await page.mouse.move(to.x, to.y);
    await page.mouse.up();
    if (alt) await page.keyboard.up("Alt");
  };

  // Click empty space: a snare (row 1) at step 3.
  let p = at(50, 1);
  await page.mouse.click(p.x, p.y);
  await expect.poll(clip).toEqual([{ tick: 48, len: 24, note: 38, velocity: 100 }]);
  // Drag it a step later and a row down: a clap at step 5. (Each step
  // waits for the UI to show the daemon's result before pointing at it.)
  await expect(page.getByTestId("note-48-38")).toBeVisible();
  await drag(at(55, 1), at(55 + 48, 2));
  await expect.poll(clip).toEqual([{ tick: 96, len: 24, note: 39, velocity: 100 }]);
  // Drag its right edge a step longer.
  await expect(page.getByTestId("note-96-39")).toBeVisible();
  const edge = (await page.getByTestId("note-edge-96-39").boundingBox())!;
  await drag({ x: edge.x + 2, y: edge.y + 4 }, { x: edge.x + 2 + 24, y: edge.y + 4 });
  await expect.poll(clip).toEqual([{ tick: 96, len: 48, note: 39, velocity: 100 }]);
  // Alt-drag copies it a beat later.
  await expect(page.getByTestId("note-96-39")).toHaveAttribute("title", /48 long/);
  await drag(at(100, 2), at(100 + 96, 2), true);
  await expect.poll(async () => (await clip()).length).toBe(2);
  expect((await clip()).map((e: { tick: number }) => e.tick)).toEqual([96, 192]);
  // The velocity lane: drag the first bar up.
  await expect(page.getByTestId("note-192-39")).toBeVisible();
  const vel = (await page.getByTestId("vel-96-39").boundingBox())!;
  await page.keyboard.press("Escape"); // clear the selection
  await drag({ x: vel.x + 2, y: vel.y + 4 }, { x: vel.x + 2, y: vel.y - 20 });
  await expect.poll(async () => (await clip())[0].velocity).toBe(127);
  // Box-select both and delete them: one undo brings both back.
  await expect(page.getByTestId("vel-96-39")).toHaveAttribute("title", "velocity 127");
  await drag({ x: box.x + 80, y: box.y + 20 }, { x: box.x + 300, y: box.y + 40 });
  await expect(page.getByTestId("note-96-39")).toHaveAttribute("data-selected", "true");
  await expect(page.getByTestId("note-192-39")).toHaveAttribute("data-selected", "true");
  await page.keyboard.press("Delete");
  await expect.poll(clip).toEqual([]);
  await page.getByTestId("undo").click();
  await expect.poll(async () => (await clip()).length).toBe(2);
  // Quantize only the selected note: an off-grid kick added elsewhere moves,
  // the selected one's neighbors do not.
  await rpc("clip.add", { instrument: "drums", clip: null, events: [{ tick: 30, len: 12, note: 36, velocity: 90 }, { tick: 250, len: 12, note: 36, velocity: 90 }] });
  await expect(page.getByTestId("note-250-36")).toBeVisible();
  await page.getByTestId("note-30-36").click();
  await page.getByTestId("roll-quantize").click();
  await expect.poll(async () => (await clip()).filter((e: { note: number }) => e.note === 36).map((e: { tick: number }) => e.tick)).toEqual([24, 250]);
  await page.getByTestId("pianoroll-drums").screenshot({ path: "test-results/pianoroll.png" });

  // A take draws the notes it has not written yet.
  await rpc("transport.record", { arm: true, instrument: "drums", mode: null, quantize: 0, strength: null, count_in: 0, offset_ms: null });
  await rpc("voice.note_on", { instrument: "drums", note: 42, velocity: 1 });
  await expect(page.locator('[data-testid^="take-note-"][data-testid$="-42"]')).toHaveCount(1);
  await rpc("voice.note_off", { instrument: "drums", note: 42, velocity: null });
  await rpc("transport.record", { arm: false, instrument: null, mode: null, quantize: null, strength: null, count_in: null, offset_ms: null });
  await rpc("transport.stop", {});
  await expect(page.locator('[data-testid^="take-note-"]')).toHaveCount(0);
  await page.getByTestId("view-steps").click();
});

test("step view: one component for both instruments; resolution; the Block's launch mode and pitch rows", async () => {
  const { page, rpc } = h;
  // The 808 at 1/8: eight steps over its bar, a click at step 2 is tick 48.
  await page.getByTestId("step-res-drums").selectOption("48");
  await expect(page.getByTestId("step-kick-7")).toBeVisible();
  await expect(page.getByTestId("step-kick-8")).toHaveCount(0);
  await page.getByTestId("step-kick-1").click();
  await expect.poll(async () => (await rpc("clip.get", { instrument: "drums", clip: null })).events).toEqual([
    { tick: 48, len: 48, note: 36, velocity: 89 },
  ]);
  // Shift-click: the 808's declared expression, accent.
  await page.getByTestId("step-kick-1").click({ modifiers: ["Shift"] });
  await expect(page.getByTestId("step-kick-1")).toHaveAttribute("data-level", "2");

  // The mirror follows the clip the armed track plays, even when the LEDs
  // do not change (two empty clips).
  await rpc("clip.clear", { instrument: "drums", clip: null });
  await rpc("clip.new", { instrument: "drums", name: null, length: null, select: false });
  await expect(page.getByTestId("block-clip")).toContainText("clip 1");
  await rpc("clip.launch", { instrument: "drums", clip: 2, now: null });
  await expect(page.getByTestId("block-clip")).toContainText("clip 2");
  await rpc("clip.launch", { instrument: "drums", clip: 1, now: null });

  // The Block's launch mode: drums are launched (lit), the last pad goes back.
  await page.getByTestId("block-grid-launch").click();
  await expect.poll(async () => (await rpc("controller.get", {})).grid).toBe("launch");
  await expect(page.getByTestId("block-row-0")).toHaveText("drums");
  await expect(page.getByTestId("pad-0-0")).toHaveAttribute("data-lit", "1");
  await page.getByTestId("pad-0-7").click();
  await expect.poll(async () => (await rpc("song.get", {})).tracks[0].launched).toBe(null);
  await page.getByTestId("block-grid-steps").click();

  // A 303 focus: pitch rows from C2, moved up a semitone.
  await rpc("instrument.add", { type: "tb303", id: "bass", name: null, channel: null, no_channel: false });
  await rpc("seat.focus", { seat: (await rpc("controller.get", {})).seat, instrument: "bass" });
  await page.getByTestId("select-bass").click();
  await expect(page.getByTestId("block-row-7")).toHaveText("C2");
  await page.getByTestId("block-rows-up").click();
  await expect(page.getByTestId("block-row-7")).toHaveText("C#2");
  // A pad sets its row's pitch, and the step view shows it.
  await page.getByTestId("pad-7-0").click();
  await expect(page.getByTestId("note-0")).toHaveValue("37");
});

test("devices: add effects to a strip and the master, their panel's knobs and bypass, reorder and remove", async () => {
  const { page, rpc } = h;
  const devices = async () => (await rpc("device.list", {})).devices.map((d) => `${d.id}@${d.channel}:${d.position}`);
  await page.getByTestId("insert-add-1").selectOption("delay");
  await expect(page.getByTestId("insert-delay")).toBeVisible();
  // Adding opens its panel: a knob per parameter, from the registry.
  await expect(page.getByTestId("device-panel-delay")).toBeVisible();
  await expect(page.getByTestId("knob-delay.feedback")).toBeVisible();
  await page.getByTestId("device-bypass-delay").click();
  await expect.poll(async () => (await rpc("param.get", { path: "delay.bypass" })).value).toBe(1);
  await expect(page.getByTestId("insert-delay")).toHaveAttribute("data-bypassed", "true");
  // A filter after it, moved before it.
  await page.getByTestId("insert-add-1").selectOption("filter");
  await expect(page.getByTestId("device-panel-filter")).toBeVisible();
  await page.getByTestId("device-earlier-filter").click();
  await expect.poll(devices).toEqual(["filter@1:0", "delay@1:1"]);
  // A reverb on the master; a daemon-side add shows up too.
  await page.getByTestId("insert-add-0").selectOption("reverb");
  await expect.poll(devices).toEqual(["reverb@0:0", "filter@1:0", "delay@1:1"]);
  await rpc("device.add", { type: "delay", id: "echo", name: "Echo", channel: 0, position: null });
  await expect(page.getByTestId("insert-echo")).toBeVisible();
  // Remove from the panel, then undo it.
  await page.getByTestId("insert-filter").click();
  await page.getByTestId("device-remove-filter").click();
  await expect(page.getByTestId("insert-filter")).toHaveCount(0);
  await rpc("history.undo", {});
  await expect(page.getByTestId("insert-filter")).toBeVisible();
  await page.screenshot({ path: "test-results/devices.png", fullPage: true });
});

test("returns: a return bus with a reverb, fed by a send knob on a track strip", async () => {
  const { page, rpc } = h;
  await page.getByTestId("add-return").click();
  await expect(page.getByTestId("return-2")).toBeVisible();
  // Every track strip gets a send knob per return; the return has none.
  await expect(page.getByTestId("knob-mixer.1.send.2")).toBeVisible();
  await expect(page.getByTestId("sends-2")).toHaveCount(0);
  await rpc("param.set", { path: "mixer.1.send.2", value: 0.5 });
  await expect(page.getByTestId("knob-mixer.1.send.2")).toHaveAttribute("data-value", "0.5");
  await page.getByTestId("insert-add-2").selectOption("reverb");
  await expect.poll(async () => (await rpc("device.list", {})).devices.map((d) => `${d.id}@${d.channel}`)).toEqual(["reverb@2"]);
  // A return takes no instrument.
  await expect(rpc("route.set", { source: "drums", channel: 2, swap: false })).rejects.toThrow("return bus");
});

test("compressor: a sidechain picked in its panel ducks the bass on the kick, and its meter shows the reduction", async () => {
  const { page, rpc } = h;
  await rpc("instrument.add", { type: "tb303", id: null, name: null, channel: null, no_channel: false });
  await rpc("pattern.set", { instrument: null, voice: "kick", steps: [1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0] });
  const held = Array.from({ length: 16 }, () => ({ note: 36, accent: false, slide: true }));
  await rpc("pattern.set_notes", { instrument: "bass", steps: held });
  await rpc("clip.launch", { instrument: "bass", clip: 1, now: null });
  await page.getByTestId("insert-add-2").selectOption("compressor");
  await expect(page.getByTestId("device-panel-comp")).toBeVisible();
  await expect(page.getByTestId("knob-comp.threshold")).toBeVisible();
  // The sidechain is a channel picker, not a knob: own input, or a track.
  const picker = page.getByTestId("sidechain-comp.sidechain");
  await expect(picker).toHaveValue("0");
  await picker.selectOption("1");
  await expect.poll(async () => (await rpc("param.get", { path: "comp.sidechain" })).value).toBe(1);
  // The kick's channel muted: it still keys the compressor.
  await rpc("param.set", { path: "mixer.1.mute", value: 1 });
  await rpc("param.set", { path: "comp.threshold", value: -30 });
  await rpc("transport.play", {});
  await expect.poll(async () => Number(await page.getByTestId("reduction-comp").getAttribute("data-db")), { timeout: 5000 }).toBeGreaterThan(3);
  await page.screenshot({ path: "test-results/compressor.png", fullPage: true });
  await rpc("transport.stop", {});
  // A daemon-side change shows in the picker.
  await rpc("param.set", { path: "comp.sidechain", value: 0 });
  await expect(picker).toHaveValue("0");
});

test("automation lanes: add a lane in the piano roll, add, drag, and remove points, remove the lane", async () => {
  const { page, rpc } = h;
  const lanes = async () => (await rpc("clip.get", { instrument: "drums", clip: null })).lanes;
  await page.getByTestId("view-roll").click();
  await page.getByTestId("roll-zoom").selectOption("1");
  // A lane on channel 1's pan starts flat at the value set by hand.
  await page.getByTestId("lane-add").fill("mixer.1.pan");
  await page.getByTestId("lane-add").press("Enter");
  await expect.poll(lanes).toEqual([{ path: "mixer.1.pan", points: [{ tick: 0, value: 0 }] }]);
  const strip = page.getByTestId("lane-mixer.1.pan");
  await strip.scrollIntoViewIfNeeded();
  const box = (await strip.boundingBox())!;
  // 1 px per tick; the strip's top is the range's top (pan 1). A click
  // near a beat lands on the 1/16 grid.
  await page.mouse.click(box.x + 190, box.y + 2);
  await expect.poll(lanes).toEqual([{ path: "mixer.1.pan", points: [{ tick: 0, value: 0 }, { tick: 192, value: 1 }] }]);
  // Drag the first point to the bottom (pan -1).
  await expect(page.getByTestId("lane-point-mixer.1.pan-192")).toBeVisible();
  const first = (await page.getByTestId("lane-point-mixer.1.pan-0").boundingBox())!;
  await page.mouse.move(first.x + 4, first.y + 4);
  await page.mouse.down();
  await page.mouse.move(first.x + 4, box.y + 30);
  await page.mouse.move(first.x + 4, box.y + box.height - 2);
  await page.mouse.up();
  await expect.poll(async () => (await lanes())[0].points[0]).toEqual({ tick: 0, value: -1 });
  // The value set by hand stays.
  expect((await rpc("param.get", { path: "mixer.1.pan" })).value).toBe(0);
  await page.getByTestId("pianoroll-drums").screenshot({ path: "test-results/lanes.png" });
  // Alt-click removes a point; the x removes the lane.
  await page.keyboard.down("Alt");
  await page.getByTestId("lane-point-mixer.1.pan-192").click();
  await page.keyboard.up("Alt");
  await expect.poll(async () => (await lanes())[0].points.length).toBe(1);
  await page.getByTestId("lane-remove-mixer.1.pan").click();
  await expect.poll(lanes).toEqual([]);
  await expect(strip).toHaveCount(0);
});

test("automation lanes in the step view: a bar per step, set a point, paint across steps, remove a point and the lane", async () => {
  const { page, rpc } = h;
  const points = async () => (await rpc("clip.get", { instrument: "drums", clip: null })).lanes[0]?.points ?? [];
  await page.getByTestId("step-lane-add-drums").fill("mixer.1.pan");
  await page.getByTestId("step-lane-add-drums").press("Enter");
  await expect(page.getByTestId("step-lane-drums-mixer.1.pan")).toBeVisible();
  // Step 5 (tick 96), pressed near its top: pan near 1.
  const c4 = page.getByTestId("lane-cell-drums-mixer.1.pan-4");
  await c4.scrollIntoViewIfNeeded();
  const b4 = (await c4.boundingBox())!;
  await page.mouse.click(b4.x + b4.width / 2, b4.y + 1);
  await expect.poll(async () => (await points()).find((p: { tick: number }) => p.tick === 96)?.value).toBeGreaterThan(0.85);
  await expect(c4).toHaveAttribute("data-point", "true");
  // Paint steps 9..12 at the bottom in one jump (the steps a fast drag
  // skips are filled): one edit, pan near -1 at ticks 192..264.
  const at = async (s: number) => (await page.getByTestId(`lane-cell-drums-mixer.1.pan-${s}`).boundingBox())!;
  const [b8, b11] = [await at(8), await at(11)];
  await page.mouse.move(b8.x + b8.width / 2, b8.y + b8.height - 1);
  await page.mouse.down();
  await page.mouse.move(b11.x + b11.width / 2, b11.y + b11.height - 1);
  await page.mouse.up();
  await expect.poll(async () => (await points()).filter((p: { value: number }) => p.value < -0.85).map((p: { tick: number }) => p.tick)).toEqual([192, 216, 240, 264]);
  await page.getByTestId("step-view-drums").screenshot({ path: "test-results/step-lanes.png" });
  // Alt-click removes step 5's point; the x removes the lane.
  await page.keyboard.down("Alt");
  await c4.click();
  await page.keyboard.up("Alt");
  await expect.poll(async () => (await points()).some((p: { tick: number }) => p.tick === 96)).toBe(false);
  await page.getByTestId("step-lane-remove-drums-mixer.1.pan").click();
  await expect.poll(async () => (await rpc("clip.get", { instrument: "drums", clip: null })).lanes).toEqual([]);
  // A 303's step columns carry a bar per lane: pressing one sets its step.
  await rpc("instrument.add", { type: "tb303", id: null, name: null, channel: null, no_channel: false });
  await rpc("clip.lane.set", { instrument: "bass", clip: null, path: "bass.cutoff", points: [{ tick: 0, value: 0.2 }] });
  await page.getByTestId("select-bass").click();
  const bass = page.getByTestId("lane-cell-bass-bass.cutoff-2");
  await bass.scrollIntoViewIfNeeded();
  const bb = (await bass.boundingBox())!;
  await page.mouse.click(bb.x + bb.width / 2, bb.y + 1);
  await expect
    .poll(async () => (await rpc("clip.get", { instrument: "bass", clip: null })).lanes[0].points.map((p: { tick: number }) => p.tick))
    .toEqual([0, 48]);
});

test("sample editor: a pad's file as a waveform; select a region by dragging or by times, play it, apply it; a saved cut reopens on its origin", async () => {
  const { page, rpc } = h;
  const mp3 = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "tests", "fixtures", "audio", "song.mp3");
  await rpc("instrument.add", { type: "sampler", id: "ed", name: null, channel: null, no_channel: false });
  await rpc("sampler.load", { instrument: "ed", pad: 1, path: mp3, upload: null, start: null, end: null });
  await page.getByTestId("select-ed").click();
  const wave = page.getByTestId("sample-wave-ed-1");
  await expect(wave).toBeVisible();
  await page.getByTestId("sample-fit-ed-1").click();
  await expect(wave).toHaveAttribute("data-view", /^0\.0000,2\.0/);
  // Drag from 1.5 s to 1.8 s (the whole file is in view).
  await wave.scrollIntoViewIfNeeded();
  const box = (await wave.boundingBox())!;
  const seconds = Number((await wave.getAttribute("data-view"))!.split(",")[1]);
  const x = (t: number) => box.x + (t / seconds) * box.width;
  await page.mouse.move(x(1.5), box.y + 60);
  await page.mouse.down();
  await page.mouse.move(x(1.65), box.y + 60);
  await page.mouse.move(x(1.8), box.y + 60);
  await page.mouse.up();
  const [a, b] = (await wave.getAttribute("data-selection"))!.split(",").map(Number);
  expect(Math.abs(a - 1.5) < 0.02 && Math.abs(b - 1.8) < 0.02).toBe(true);
  // Exact times from the fields; play it; apply it to the pad.
  await page.getByTestId("sel-start-ed-1").fill("1.5");
  await page.getByTestId("sel-start-ed-1").press("Enter");
  await page.getByTestId("sel-end-ed-1").fill("1.8");
  await page.getByTestId("sel-end-ed-1").press("Enter");
  await expect(wave).toHaveAttribute("data-selection", "1.5000,1.8000");
  await page.getByTestId("sample-editor-ed-1").screenshot({ path: "test-results/sample-editor.png" });
  await page.getByTestId("sample-play-ed-1").click();
  await expect(wave).toHaveAttribute("data-played", "1.5000,1.8000");
  await page.getByTestId("sample-stop-ed-1").click();
  await expect(wave).toHaveAttribute("data-playing", "false");
  // The wheel zooms around the pointer (and only the waveform moves).
  const before = (await wave.getAttribute("data-view"))!;
  await page.mouse.move(x(1.65), box.y + 60);
  await page.mouse.wheel(0, -200);
  await expect(wave).not.toHaveAttribute("data-view", before);
  await page.getByTestId("sample-apply-ed-1").click();
  await expect
    .poll(async () => (await rpc("sampler.get", { instrument: "ed" })).pads.map((p) => [p.start, p.end]))
    .toEqual([[1.5, 1.8]]);
  await expect(page.getByTestId("pad-ed-1")).toContainText("song.mp3");
  // Saved, the pad plays the bundle's WAV of the cut; reopened, the editor
  // works on the MP3 it was cut from, at the same region.
  await rpc("project.save", { path: "projects/edited.4s" });
  await rpc("project.new", {});
  await rpc("project.load", { path: "projects/edited.4s" });
  await page.getByTestId("select-ed").click();
  await expect(page.getByTestId("sample-editor-ed-1")).toContainText("the file it was cut from");
  await expect(page.getByTestId("sample-wave-ed-1")).toHaveAttribute("data-selection", "1.5000,1.8000");
});

test("modulators: an LFO from the rack, mapped onto a knob by pressing it, depth and unroute", async () => {
  const { page, rpc } = h;
  const routes = async () => (await rpc("state.get", {})).graph.mod_routes;
  await page.getByTestId("add-lfo").click();
  await expect(page.getByTestId("rack-lfo")).toBeVisible();
  await expect(page.getByTestId("device-panel-lfo")).toBeVisible();
  await expect(page.getByTestId("knob-lfo.1.rate")).toBeVisible();
  // Map mode: press an output, then a knob (the master volume).
  await page.getByTestId("map-lfo.2").click();
  await expect(page.getByTestId("mapping")).toContainText("lfo.2");
  await page.getByTestId("fader-master").scrollIntoViewIfNeeded();
  await page.getByTestId("knob-mixer.1.pan").click();
  await expect.poll(routes).toEqual([{ source: "lfo.2", target: "mixer.1.pan", depth: 0.5 }]);
  await expect(page.getByTestId("mapping")).toHaveCount(0);
  // The knob was not turned, only routed.
  expect((await rpc("param.get", { path: "mixer.1.pan" })).value).toBe(0);
  // Depth from the route's slider, then remove it.
  await page.getByTestId("depth-lfo.2-mixer.1.pan").fill("-25");
  await expect.poll(async () => (await routes())[0]?.depth).toBe(-0.25);
  await page.getByTestId("unroute-lfo.2-mixer.1.pan").click();
  await expect.poll(routes).toEqual([]);
  // Escape cancels map mode.
  await page.getByTestId("map-lfo.1").click();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("mapping")).toHaveCount(0);
});

test("moving values: knobs and faders show where an LFO or a lane has them, beside the value set by hand", async () => {
  const { page, rpc } = h;
  const knob = page.getByTestId("knob-mixer.1.pan");
  await page.getByTestId("fader-master").scrollIntoViewIfNeeded();
  await expect(knob).not.toHaveAttribute("data-moving", /./);
  // An LFO on channel 1's pan: the knob gets a second marker that moves.
  await rpc("device.add", { type: "lfo", id: null, name: null, channel: null, position: null });
  await rpc("mod.route", { source: "lfo.1", target: "mixer.1.pan", depth: 0.5 });
  await expect(knob).toHaveAttribute("data-moving", /./);
  await expect(knob).toHaveAttribute("data-value", "0");
  await expect(knob).toHaveAttribute("title", /now .*modulated or automated/);
  await rpc("mod.unroute", { source: "lfo.1", target: "mixer.1.pan" });
  await expect(knob).not.toHaveAttribute("data-moving", /./);
  // A lane on channel 1's volume while playing: the fader's marker sits at
  // the lane's value, and goes when it stops.
  await rpc("clip.lane.set", { instrument: "drums", clip: null, path: "mixer.1.volume", points: [{ tick: 0, value: 0.25 }] });
  await rpc("transport.play", {});
  await expect(page.getByTestId("fader-1-moving")).toBeVisible();
  expect((await rpc("state.get", {})).moving).toEqual({ "mixer.1.volume": 0.25 });
  await rpc("transport.stop", {});
  await expect(page.getByTestId("fader-1-moving")).toHaveCount(0);
  await rpc("clip.lane.remove", { instrument: "drums", clip: null, path: "mixer.1.volume" });
  await rpc("device.remove", { id: "lfo" });
});

test("sampler: load a WAV into a pad from this computer, play it from the step view, clear it", async () => {
  const { page, rpc } = h;
  // A 0.2 s 220 Hz tone as a 16-bit mono WAV.
  const sr = 44100;
  const n = Math.round(sr * 0.2);
  const buf = Buffer.alloc(44 + n * 2);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + n * 2, 4);
  buf.write("WAVEfmt ", 8);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(1, 20);
  buf.writeUInt16LE(1, 22);
  buf.writeUInt32LE(sr, 24);
  buf.writeUInt32LE(sr * 2, 28);
  buf.writeUInt16LE(2, 32);
  buf.writeUInt16LE(16, 34);
  buf.write("data", 36);
  buf.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) buf.writeInt16LE(Math.round(12000 * Math.sin((2 * Math.PI * 220 * i) / sr)), 44 + i * 2);
  await rpc("instrument.add", { type: "sampler", id: "pads", name: null, channel: null, no_channel: false });
  await page.getByTestId("select-pads").click();
  await expect(page.getByTestId("sampler-editor-pads")).toBeVisible();
  await page.getByTestId("pad-load-pads-1").setInputFiles({ name: "tone.wav", mimeType: "audio/wav", buffer: buf });
  await expect(page.getByTestId("pad-pads-1")).toContainText("tone.wav");
  expect((await rpc("sampler.get", { instrument: "pads" })).pads.map((p) => p.pad)).toEqual([1]);
  // Pads are rows of the step view.
  await page.getByTestId("step-pad1-0").click();
  await expect.poll(async () => (await rpc("clip.get", { instrument: "pads", clip: null })).events.map((e) => e.note)).toEqual([36]);
  // The selected pad's knobs.
  await expect(page.getByTestId("knob-pads.1.tune")).toBeVisible();
  await page.getByTestId("pad-clear-pads-1").click();
  await expect(page.getByTestId("pad-pads-1")).toContainText("empty");
  // An MP3 (RFC 0011), sent in chunks and decoded by the engine.
  const mp3 = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "tests", "fixtures", "audio", "song.mp3");
  await page.getByTestId("pad-load-pads-2").setInputFiles(mp3);
  await expect(page.getByTestId("pad-pads-2")).toContainText("song.mp3");
  const pad2 = (await rpc("sampler.get", { instrument: "pads" })).pads.find((p) => p.pad === 2)!;
  expect(pad2.seconds).toBeGreaterThan(1.9);
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

  // Knob page: decay; knob 1 controls kick decay.
  await page.getByTestId("knob-page-decay").click();
  await expect.poll(async () => (await rpc("controller.get", {})).knob_page).toBe("decay");
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
  await rpc("clip.launch", { instrument: "bass", clip: null, now: null });
  await rpc("transport.play", {});
  await page.getByTestId("select-drums").click();
  await page.waitForTimeout(600);
  await page.screenshot({ path: "test-results/groove.png", fullPage: true });
  await page.getByTestId("select-bass").click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: "test-results/bass.png", fullPage: true });
  await rpc("transport.stop", {});
});
