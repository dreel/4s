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
const { loadSchema, methods, rpcAllowed } = require("./schema.cjs");
const { Tools } = require("./tools.cjs");
const { Assistant, MAX_TOOL_CALLS } = require("./loop.cjs");
const { Settings } = require("./settings.cjs");
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
