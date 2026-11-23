// Tests for the assistant's tool layer and turn loop against a real
// headless daemon and a fake Anthropic API (RFC 0013 validation plan).
// Run: node --test ui/assistant/ (needs `cargo build` first).

const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { DaemonClient } = require("./daemon.cjs");
const { loadSchema, methods, rpcAllowed, rpcRefusal, PATH_PARAMS } = require("./schema.cjs");
const { Tools } = require("./tools.cjs");
const { Assistant, MAX_TOOL_CALLS } = require("./loop.cjs");
const { Settings } = require("./settings.cjs");
const { SessionLog } = require("./log.cjs");
const { startFakeAnthropic } = require("../e2e/fake-anthropic.cjs");

const ROOT = path.resolve(__dirname, "..", "..");
const CLI = path.join(ROOT, "target", "debug", "4s");
const USER = "tester";
const schema = loadSchema();
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "4s-assistant-"));
const env = { ...process.env, FOURS_DATA_DIR: dataDir, FOURS_USER: USER };
/** @type {DaemonClient} */
let me;

test.before(() => {
  const r = spawnSync(CLI, ["daemon", "start", "--no-audio", "--no-midi", "--listen", "127.0.0.1:0"], { env, encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
});
test.after(() => {
  me?.close();
  spawnSync(CLI, ["daemon", "stop", "--force"], { env });
});

const url = () => JSON.parse(fs.readFileSync(path.join(dataDir, "4sd.json"), "utf8")).url;
const client = (name) => new DaemonClient({ url: url(), user: USER, protocolVersion: schema.protocol_version, clientName: name });

test.beforeEach(async () => {
  me ??= client("test");
  await me.call("project.new");
});

test("the rpc allowlist follows the daemon's method classes", () => {
  for (const m of ["param.set", "device.add", "clip.add", "state.get", "param.list", "transport.play", "transport.locate", "seat.bind", "midi.connect"])
    assert.ok(rpcAllowed(schema, m), `${m} should be allowed`);
  for (const m of ["daemon.shutdown", "session.hello", "events.subscribe", "project.new", "project.load", "project.import", "project.save", "project.list",
    "history.undo", "history.group_begin", "history.group_end", "history.get", "journal.export", "render.offline", "sample.upload", "batch", "transport.record", "made.up"])
    assert.ok(!rpcAllowed(schema, m), `${m} should be refused`);
  // Every method is classified by the rule, from the generated lists.
  const all = methods(schema).map((m) => m.name);
  assert.ok(all.length > 100);
  assert.ok(schema.read_only_methods.every((m) => all.includes(m)));
  assert.ok(schema.batchable_methods.every((m) => all.includes(m)));
});

test("file paths on the engine host are refused, for every method that takes one", () => {
  assert.match(rpcRefusal(schema, "sampler.load", { pad: 1, path: "/etc/passwd" }), /file path is not allowed/);
  assert.match(rpcRefusal(schema, "sample.info", { path: "../../secret.wav" }), /file path is not allowed/);
  assert.equal(rpcRefusal(schema, "sampler.load", { pad: 1, upload: "abc" }), null);
  assert.equal(rpcRefusal(schema, "param.set", { path: "drums.kick.decay", value: 1 }), null);
  // Any method whose params mention files or paths (by their schema
  // description) is refused outright or has its path param listed.
  const defs = schema.request.$defs;
  for (const v of schema.request.oneOf) {
    const m = v.properties.method.const;
    const props = defs[(v.properties.params.$ref ?? "").replace("#/$defs/", "")]?.properties ?? {};
    for (const [k, p] of Object.entries(props)) {
      if (!/(relative paths|engine-side|bundle path|file)/i.test(p.description ?? "")) continue;
      if (!rpcAllowed(schema, m)) continue;
      if (/upload|start|end|name|offset|model|profile/.test(k)) continue; // not paths
      assert.equal(PATH_PARAMS[m], k, `${m}.${k} looks like an engine-host path; refuse it in schema.cjs`);
    }
  }
});

test("tools: overview, find_params, set_params, rpc, describe", async () => {
  const tools = new Tools(me, schema, USER);
  const o = await tools.run("overview", {});
  assert.match(o.text, /drums \(tr808, "Drums"\) -> ch 1/);
  assert.match(o.text, /ch 1 "Drums": vol 100% pan C/);
  const f = await tools.run("find_params", { prefix: "drums.kick.", query: "decay" });
  assert.match(f.text, /^drums\.kick\.decay = [\d.]+ {2}\[0\.\.1/);
  const before = (await me.call("param.get", { path: "drums.kick.decay" })).value;
  const s = await tools.run("set_params", { changes: [{ path: "drums.kick.decay", value: 0.25 }], why: "tighter" });
  assert.deepEqual(s.chips, [`drums.kick.decay ${before} -> 0.25`]);
  assert.equal((await me.call("param.get", { path: "drums.kick.decay" })).value, 0.25);
  const r = await tools.run("rpc", { method: "device.add", params: { type: "compressor", channel: 1 } });
  assert.deepEqual(r.chips, ["+ compressor on ch 1"]);
  await assert.rejects(tools.run("rpc", { method: "project.load", params: { name: "x" } }), /not allowed from the assistant/);
  await assert.rejects(tools.run("set_params", { changes: [{ path: "drums.kick.decay" }] }), /finite number/);
  const d = JSON.parse((await tools.run("describe", { method: "device.add" })).text);
  assert.equal(d.allowed, true);
  assert.ok(d.params.properties.type);
  // What a step is, and the constants' values, reach the model.
  const steps = JSON.parse((await tools.run("describe", { method: "pattern.set" })).text).params.properties.steps;
  assert.match(steps.description, /0 off, 1 on, 2 accent/);
  assert.equal(steps.items.maximum, 2);
  const length = JSON.parse((await tools.run("describe", { method: "clip.length" })).text).params.properties.length;
  assert.match(length.description, /`MAX_CLIP_TICKS` \(\d+\)/);
  assert.equal(length.minimum, 1);
});

/** A made-up band at 126 bpm, 12 bars of 4/4, as a 16-bit mono WAV at
 * 22.05 kHz: bars 1-4 and 9-12 an A minor chord over a kick on 1 and 3,
 * bars 5-8 a drum break (kick, snare on 2 and 4, eighth-note hats). */
function bandWav(file) {
  const sr = 22050;
  const beat = 60 / 126;
  const n = Math.floor(sr * beat * 4 * 12);
  const x = new Float64Array(n);
  let seed = 7;
  const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32) - 0.5;
  const hit = (at, len, f) => {
    const a = Math.floor(at * sr);
    for (let i = 0; i < Math.min(Math.floor(len * sr), n - a); i++) x[a + i] += f(i / sr);
  };
  for (let b = 0; b < 48; b++) {
    const bar = Math.floor(b / 4), k = b % 4, t = b * beat, brk = bar >= 4 && bar < 8;
    if (k % 2 === 0) hit(t, 0.15, (u) => 0.6 * Math.sin(2 * Math.PI * (50 + 100 * Math.exp(-u * 30)) * u) * Math.exp(-u * 20));
    if (brk && k % 2 === 1) hit(t, 0.12, (u) => 0.4 * rnd() * Math.exp(-u * 25));
    if (brk) for (const h of [0, 0.5]) hit(t + h * beat, 0.04, (u) => 0.25 * rnd() * Math.exp(-u * 80));
  }
  for (let i = 0; i < n; i++) {
    const bar = Math.floor(i / sr / (beat * 4));
    if (bar < 4 || bar >= 8) for (const f of [110, 220, 261.63, 329.63]) x[i] += 0.08 * Math.sin((2 * Math.PI * f * i) / sr);
  }
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
  for (let i = 0; i < n; i++) buf.writeInt16LE(Math.max(-32767, Math.min(32767, Math.round(x[i] * 32767))), 44 + i * 2);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, buf);
  return { bar: (4 * 60) / 126 };
}

/** A sampler with the band on pad 1. */
async function bandOnPad() {
  const file = path.join(dataDir, "samples", "band.wav");
  const { bar } = bandWav(file);
  await me.call("instrument.add", { type: "sampler", id: "smp", name: null, channel: null, no_channel: false });
  await me.call("sampler.load", { instrument: "smp", pad: 1, path: file, upload: null, start: null, end: null });
  return { file, bar };
}

test("tools: analyze_audio hears a pad's song; cut_sample loads whole bars of it; no paths from the model", async () => {
  const { file, bar } = await bandOnPad();
  const tools = new Tools(me, schema, USER);
  const a = await tools.run("analyze_audio", { pad: 1 });
  assert.match(a.text, /^band\.wav: 0:00\.000 - 0:22\.857/);
  assert.match(a.text, /tempo 12[56]\.\d+ bpm/);
  assert.match(a.text, /key A minor/);
  // The break is a section of its own, bars 5-8, the most punch.
  const sections = a.text.split("\n").filter((l) => /^[A-Z] bars \d+-\d+ /.test(l));
  const punch = (l) => Number(/ p([\d.]+) /.exec(l)[1]);
  const brk = sections.reduce((x, y) => (punch(y) > punch(x) ? y : x));
  assert.match(brk, /^[A-Z] bars 5-8 /, a.text);
  assert.deepEqual(a.chips, ["listened to band.wav"]);
  // A rough region snaps to bar lines: bars 5-8 onto pad 2.
  const c = await tools.run("cut_sample", { pad: 1, to_pad: 2, start: 4 * bar - 0.3, end: 8 * bar + 0.2 });
  assert.match(c.text, /^smp pad 2 now plays band\.wav bars 5-8, /);
  const p2 = (await me.call("sampler.get", { instrument: "smp" })).pads.find((p) => p.pad === 2);
  assert.equal(p2.path, file);
  assert.ok(Math.abs(p2.start - 4 * bar) < 0.06 && Math.abs(p2.end - 8 * bar) < 0.06, `${p2.start}..${p2.end}`);
  // Its own times are the grid's, exactly.
  assert.match(c.text, new RegExp(`start ${p2.start}, end ${p2.end}\\)$`));
  // Unsnapped, the times as given; an empty pad; paths from the model are refused.
  await tools.run("cut_sample", { pad: 1, to_pad: 3, start: 1.234, end: 2.5, snap: false });
  const p3 = (await me.call("sampler.get", { instrument: "smp" })).pads.find((p) => p.pad === 3);
  assert.ok(Math.abs(p3.start - 1.234) < 0.001 && Math.abs(p3.end - 2.5) < 0.001, `${p3.start}..${p3.end}`);
  await assert.rejects(tools.run("analyze_audio", { pad: 9 }), /pad 9 is empty \(loaded pads: 1, 2, 3\)/);
  await assert.rejects(tools.run("rpc", { method: "sample.analyze", params: { path: file } }), /file path is not allowed/);
  await assert.rejects(tools.run("rpc", { method: "sampler.load", params: { instrument: "smp", pad: 4, path: "/etc/hosts" } }), /file path is not allowed/);
});

/** An assistant against the fake API, collecting its events. */
async function harness(script, opts = {}) {
  const fake = await startFakeAnthropic(script);
  const events = [];
  const daemon = client("assistant");
  const a = new Assistant({
    daemon,
    schema,
    user: USER,
    apiKey: () => ("key" in opts ? opts.key : "test-key"),
    model: () => opts.model ?? "claude-sonnet-5-5",
    emit: (e) => events.push(e),
    baseURL: fake.url,
    ...(opts.log ? { log: opts.log } : {}),
  });
  return { a, fake, events, close: async () => { daemon.close(); await fake.close(); } };
}

const PUNCHY = [
  {
    match: "punchier kick",
    replies: [
      {
        content: [
          { type: "text", text: "Shortening the decay and adding a compressor. " },
          { type: "tool_use", name: "set_params", input: { changes: [{ path: "drums.kick.decay", value: 0.3 }, { path: "drums.kick.tone", value: 0.7 }], why: "punch" } },
          { type: "tool_use", name: "rpc", input: { method: "device.add", params: { type: "compressor", channel: 1 } } },
        ],
      },
      { content: [{ type: "text", text: "Done: decay 0.5 -> 0.3, tone up, compressor on the drums." }] },
    ],
  },
];

test("a turn changes params, is one undo step, and undo reverts all of it", async () => {
  const h = await harness(PUNCHY);
  try {
    const decay = (await me.call("param.get", { path: "drums.kick.decay" })).value;
    await h.a.send("make the punchier kick", { panel: "drums" });
    const done = h.events.find((e) => e.type === "done");
    assert.equal(done.stop, "end_turn", done.error);
    assert.equal(done.label, "assistant: make the punchier kick");
    assert.deepEqual(done.changed.sort(), ["chain:1", "device:comp", "param:drums.kick.decay", "param:drums.kick.tone"].sort());
    const tools = h.events.filter((e) => e.type === "tool");
    assert.deepEqual(tools.map((e) => e.chips).flat(), [`drums.kick.decay ${decay} -> 0.3`, "drums.kick.tone 0.5 -> 0.7", "+ compressor on ch 1"]);
    assert.match(h.events.filter((e) => e.type === "text").map((e) => e.delta).join(""), /Shortening the decay.*Done: decay/);
    assert.equal((await me.call("param.get", { path: "drums.kick.decay" })).value, 0.3);
    // One undo step, labelled with the turn.
    const hist = await me.call("history.get");
    assert.equal(hist.undo[0], "assistant: make the punchier kick");
    assert.equal(hist.undo.length, 1);
    const journal = await me.call("journal.get", { limit: 10 });
    assert.ok(journal.entries.some((e) => e.origin === "assistant" && e.method === "batch"));
    await me.call("history.undo");
    assert.equal((await me.call("param.get", { path: "drums.kick.decay" })).value, decay);
    assert.equal((await me.call("device.list")).devices.length, 0);
    // The request: cached system prompt and tools, a context header, the
    // fallback beta for Sonnet, and the tool results sent back.
    const first = h.fake.requests[0].body;
    assert.equal(first.system[0].cache_control.type, "ephemeral");
    assert.equal(first.tools.at(-1).cache_control.type, "ephemeral");
    assert.ok(first.tools.every((t) => t.eager_input_streaming));
    assert.equal(first.fallbacks, "default");
    assert.match(h.fake.requests[0].headers["anthropic-beta"], /server-side-fallback-2026-07-01/);
    assert.match(first.messages[0].content[0].text, /^\[4S context\] focused instrument: drums \(tr808\); selected clip: drums clip 1 "1"; transport: stopped, 120 bpm; open panel: drums\n\nmake the punchier kick$/);
    const results = h.fake.requests[1].body.messages.at(-1).content;
    assert.equal(results.length, 2);
    assert.match(results[0].content, /drums\.kick\.decay .* -> 0\.3/);
    assert.ok(done.usage.cost > 0);
  } finally {
    await h.close();
  }
});

test("a refused method is reported to the model and changes nothing", async () => {
  const h = await harness([
    { match: "load my song", replies: [
      { content: [{ type: "tool_use", name: "rpc", input: { method: "project.load", params: { name: "song" } } }] },
      { content: [{ type: "text", text: "Please open it from the app." }] },
    ] },
  ]);
  try {
    await h.a.send("load my song");
    const tool = h.events.find((e) => e.type === "tool");
    assert.equal(tool.ok, false);
    assert.match(tool.error, /not allowed from the assistant/);
    const result = h.fake.requests[1].body.messages.at(-1).content[0];
    assert.equal(result.is_error, true);
    assert.equal(h.events.find((e) => e.type === "done").changed.length, 0);
    assert.equal((await me.call("history.get")).undo.length, 0);
  } finally {
    await h.close();
  }
});

test("the session log has every tool call, refused and failed ones too, and new chat starts a new file", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "4s-assistant-log-"));
  const h = await harness(
    [
      { match: "four on the floor", replies: [
        { content: [
          { type: "tool_use", name: "rpc", input: { method: "project.load", params: { name: "song" } } },
          { type: "tool_use", name: "rpc", input: { method: "pattern.set", params: { voice: "kick", steps: [255, 0, 0, 0] } } },
          { type: "tool_use", name: "rpc", input: { method: "pattern.set", params: { voice: "kick", steps: [2, 0, 0, 0, 1, 0, 0, 0] } } },
        ] },
        { content: [{ type: "text", text: "Kick on the beat." }] },
      ] },
      { match: "again", replies: [{ content: [{ type: "text", text: "Hello again." }] }] },
    ],
    { log: new SessionLog(dir), key: "sk-secret-test-key" },
  );
  try {
    await h.a.send("four on the floor");
    const files = () => fs.readdirSync(dir).filter((f) => f.endsWith(".jsonl")).sort();
    assert.equal(files().length, 1);
    const read = (f) => fs.readFileSync(path.join(dir, f), "utf8");
    const records = read(files()[0]).trim().split("\n").map((l) => JSON.parse(l));
    assert.deepEqual(records.map((r) => r.type), ["user", "context", "reply", "tool", "tool", "tool", "reply", "done"]);
    assert.equal(records[0].text, "four on the floor");
    assert.match(records[1].header, /^\[4S context\]/);
    const [refused, failed, ok] = records.filter((r) => r.type === "tool");
    assert.equal(refused.ok, false);
    assert.match(refused.error, /not allowed from the assistant/);
    assert.equal(failed.ok, false);
    // The daemon's error says what a step level is, so the model can fix the call.
    assert.match(failed.error, /0 \(off\), 1 \(on\), or 2 \(accent\)/);
    assert.deepEqual(failed.input.params.steps, [255, 0, 0, 0]);
    assert.equal(ok.ok, true);
    assert.equal(records.at(-1).stop, "end_turn");
    assert.ok(!read(files()[0]).includes("sk-secret-test-key"), "the API key is never logged");

    await h.a.reset();
    await h.a.send("again");
    assert.equal(files().length, 2);
  } finally {
    await h.close();
    await me.call("pattern.clear", {});
  }
});

test("stop ends the turn and its undo group, keeping it one undo step", async () => {
  const h = await harness([
    { match: "slowly", replies: [
      { content: [{ type: "tool_use", name: "set_params", input: { changes: [{ path: "drums.kick.decay", value: 0.2 }] } }] },
      { content: [{ type: "text", text: "Thinking about the next change for a long time " }], hang: true, delayMs: 10 },
    ] },
    { match: "after", replies: [{ content: [{ type: "text", text: "ok" }] }] },
  ]);
  try {
    const sent = h.a.send("change it slowly");
    while (!h.events.some((e) => e.type === "text")) await new Promise((r) => setTimeout(r, 20));
    h.a.stop();
    await sent;
    const done = h.events.find((e) => e.type === "done");
    assert.equal(done.stop, "stopped");
    assert.deepEqual(done.changed, ["param:drums.kick.decay"]);
    const journal = await me.call("journal.get", { limit: 1 });
    assert.equal(journal.entries[0].method, "history.group_end");
    assert.equal((await me.call("history.get")).undo[0], "assistant: change it slowly");
    // The next turn still works (the conversation stays valid).
    await h.a.send("after that");
    assert.equal(h.events.filter((e) => e.type === "done")[1].stop, "end_turn");
    const roles = h.fake.requests.at(-1).body.messages.map((m) => m.role);
    assert.deepEqual(roles, ["user", "assistant", "user", "assistant", "user"]);
  } finally {
    await h.close();
  }
});

test("new chat during a running turn waits for it, so the new chat starts clean", async () => {
  const h = await harness([
    { match: "hang", replies: [{ content: [{ type: "text", text: "Working on it for a long time " }], hang: true, delayMs: 10 }] },
    { match: "fresh", replies: [{ content: [{ type: "text", text: "hi" }] }] },
  ]);
  try {
    const sent = h.a.send("hang around");
    while (!h.events.some((e) => e.type === "text")) await new Promise((r) => setTimeout(r, 20));
    await h.a.reset();
    await sent;
    assert.deepEqual(h.a.messages, []);
    await h.a.send("fresh start");
    const msgs = h.fake.requests.at(-1).body.messages;
    assert.equal(msgs.length, 1);
    assert.equal(msgs[0].role, "user");
    assert.equal(h.events.filter((e) => e.type === "done").at(-1).stop, "end_turn");
  } finally {
    await h.close();
  }
});

test("tool calls per turn are capped", async () => {
  const h = await harness([
    { match: "forever", replies: [{ content: [{ type: "tool_use", name: "overview", input: {} }] }] },
  ]);
  try {
    await h.a.send("look forever");
    const ok = h.events.filter((e) => e.type === "tool" && e.ok);
    assert.equal(ok.length, MAX_TOOL_CALLS);
    assert.deepEqual(h.fake.requests.at(-1).body.tool_choice, { type: "none" });
    assert.equal(h.events.find((e) => e.type === "done").stop, "tool_limit");
  } finally {
    await h.close();
  }
});

test("without a key the turn ends with a message and no request", async () => {
  const h = await harness([], { key: null });
  try {
    await h.a.send("hello");
    assert.match(h.events[0].error, /API key/);
    assert.equal(h.fake.requests.length, 0);
  } finally {
    await h.close();
  }
});

test("settings: the key is stored encrypted, or for the session only without encryption", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "4s-settings-"));
  const fakeStorage = (available) => ({
    isEncryptionAvailable: () => available,
    encryptString: (s) => Buffer.from(`enc:${s}`),
    decryptString: (b) => b.toString().replace(/^enc:/, ""),
  });
  const saved = process.env.ANTHROPIC_API_KEY;
  delete process.env.ANTHROPIC_API_KEY;
  try {
    const s = new Settings(dir, fakeStorage(true));
    s.setKey("sk-test-1");
    assert.equal(s.apiKey(), "sk-test-1");
    const file = fs.readFileSync(path.join(dir, "settings.json"), "utf8");
    assert.ok(!file.includes("sk-test-1"));
    assert.equal(new Settings(dir, fakeStorage(true)).apiKey(), "sk-test-1");
    assert.equal(s.view().keySource, "keychain");
    assert.ok(!JSON.stringify(s.view()).includes("sk-test"));
    const plain = new Settings(fs.mkdtempSync(path.join(os.tmpdir(), "4s-settings-")), fakeStorage(false));
    plain.setKey("sk-test-2");
    assert.equal(plain.apiKey(), "sk-test-2");
    assert.equal(plain.view().keySource, "session");
    assert.ok(!fs.readFileSync(plain.file, "utf8").includes("sk-test-2"));
    assert.throws(() => s.setModel("gpt"), /unknown model/);
    s.setModel("claude-opus-5-5");
    assert.equal(new Settings(dir, fakeStorage(true)).model(), "claude-opus-5-5");
  } finally {
    if (saved != null) process.env.ANTHROPIC_API_KEY = saved;
  }
});

test("the clip finder: a drum break pulled out of a song on a pad, in one undo step", async () => {
  const { bar } = await bandOnPad();
  const h = await harness([
    { match: "drum break", replies: [
      { content: [{ type: "tool_use", name: "analyze_audio", input: { pad: 1 } }] },
      { content: [{ type: "tool_use", name: "cut_sample", input: { pad: 1, to_pad: 2, start: 7.7, end: 15.1 } }] },
      { content: [{ type: "text", text: "Pad 2 has the break, bars 5-8." }] },
    ] },
  ]);
  try {
    await h.a.send("put the drum break on pad 2");
    const done = h.events.find((e) => e.type === "done");
    assert.equal(done.stop, "end_turn", done.error);
    const tools = h.events.filter((e) => e.type === "tool");
    assert.deepEqual(tools.map((e) => e.ok), [true, true]);
    // The model saw the timeline, and the cut it made.
    const seen = h.fake.requests[1].body.messages.at(-1).content[0].content;
    assert.match(seen, /bars 5-8 /);
    const cut = h.fake.requests[2].body.messages.at(-1).content[0].content;
    assert.match(cut, /pad 2 now plays band\.wav bars 5-8/);
    const p2 = (await me.call("sampler.get", { instrument: "smp" })).pads.find((p) => p.pad === 2);
    assert.ok(Math.abs(p2.start - 4 * bar) < 0.06 && Math.abs(p2.end - 8 * bar) < 0.06, `${p2.start}..${p2.end}`);
    assert.deepEqual(done.changed, ["sample:smp.2"]);
    assert.equal((await me.call("history.get")).undo[0], "assistant: put the drum break on pad 2");
    await me.call("history.undo");
    assert.equal((await me.call("sampler.get", { instrument: "smp" })).pads.some((p) => p.pad === 2), false);
  } finally {
    await h.close();
  }
});
