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
  const ch = await rpc("channel.add", { name: "Kick" });
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
  await rpc("clip.length", { instrument: "drums", clip: null, length: 72 });
  await expect(page.getByTestId("clip-note-drums")).toContainText("loops every 3 steps");
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

  // A 4-step loop (half a second at 120 bpm) so passes come round quickly.
  await rpc("param.set", { path: "sequencer.length", value: 4 });
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

test("clip pool and song mode: the step editor edits the selected clip; song controls set the song", async () => {
  const { page, rpc } = h;
  const clip = async (id: number) => (await rpc("clip.get", { instrument: "drums", clip: id })).events;
  await page.getByTestId("step-kick-0").click();
  await page.getByTestId("clip-new-drums").click();
  await expect(page.getByTestId("clip-drums-2")).toHaveAttribute("data-selected", "true");
  // The new clip is empty, and step edits go to it.
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "0");
  await page.getByTestId("step-snare-4").click();
  await expect.poll(() => clip(2)).toEqual([{ tick: 96, len: 24, note: 38, velocity: 89 }]);
  expect(await clip(1)).toEqual([{ tick: 0, len: 24, note: 36, velocity: 89 }]);
  // Selecting clip 1 shows it again; a daemon-side rename shows up.
  await page.getByTestId("clip-drums-1").click();
  await expect(page.getByTestId("step-kick-0")).toHaveAttribute("data-level", "1");
  await rpc("clip.rename", { instrument: "drums", clip: 2, name: "Fill" });
  await expect(page.getByTestId("clip-drums-2")).toContainText("Fill");
  // Deleting asks once.
  await page.getByTestId("clip-drums-2").click();
  await page.getByTestId("clip-delete-drums").click();
  await page.getByTestId("clip-delete-drums").click();
  await expect(page.getByTestId("clip-drums-2")).toHaveCount(0);

  // Song mode, a bar loop, and the start point.
  await page.getByTestId("mode-toggle").click();
  await expect.poll(async () => (await rpc("param.get", { path: "song.mode" })).value).toBe(1);
  await page.getByTestId("song-loop").selectOption("2");
  await page.getByTestId("loop-end").fill("2");
  await expect.poll(async () => (await rpc("state.get", {})).params).toMatchObject({ "song.loop": 2, "song.loop_end": 2 });
  await page.getByTestId("locate").fill("3");
  await expect.poll(async () => (await rpc("state.get", {})).transport.start).toBe(768);
  await rpc("transport.locate", { tick: 0 });
  await expect(page.getByTestId("locate")).toHaveValue("1");
  // Playing in song mode shows bar.beat.
  await rpc("song.place", { instrument: "drums", clip: 1, start: 0, length: 768, offset: null });
  await rpc("transport.play", {});
  await expect(page.getByTestId("playhead")).toHaveText(/^\d+\.\d$/);
  await rpc("transport.stop", {});
  await rpc("param.set", { path: "song.mode", value: 0 });
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

  // In song mode the playhead follows.
  await rpc("param.set", { path: "song.loop", value: 0 });
  await rpc("transport.locate", { tick: 0 });
  await rpc("param.set", { path: "song.mode", value: 1 });
  await rpc("transport.play", {});
  const head = page.getByTestId("arr-playhead");
  await expect(head).toBeVisible();
  await expect.poll(async () => Number(await head.getAttribute("data-tick"))).toBeGreaterThan(0);
  await arr.screenshot({ path: "test-results/arrangement.png" });
  await rpc("transport.stop", {});
  await rpc("param.set", { path: "song.mode", value: 0 });
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
  await rpc("transport.play", {});
  await page.getByTestId("select-drums").click();
  await page.waitForTimeout(600);
  await page.screenshot({ path: "test-results/groove.png", fullPage: true });
  await page.getByTestId("select-bass").click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: "test-results/bass.png", fullPage: true });
  await rpc("transport.stop", {});
});
