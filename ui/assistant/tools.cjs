// @ts-check
// The assistant's tools (RFC 0013): a small curated set on top of the RPC
// API, not one tool per method. Every tool talks to the daemon over RPC
// only, so new instruments and devices work with no changes here.

const { rpcAllowed, rpcRefusal, describeMethod, methods } = require("./schema.cjs");

/** Longest tool result sent back to the model, in characters. */
const MAX_RESULT = 12000;
/** Most params `find_params` lists. */
const MAX_PARAMS = 80;

/** Tool definitions for the Messages API. Order and text are stable, so
 * the prompt cache keeps hitting. @type {any[]} */
const TOOL_DEFS = [
  {
    name: "overview",
    description:
      "A compact text summary of the open project: tempo and transport, instruments and their types, mixer channels with volume, pan and insert devices, routing, clips, seats and their focus, and MIDI devices. Call it before structural edits or when you need more than the context header.",
    input_schema: { type: "object", properties: {}, additionalProperties: false },
  },
  {
    name: "find_params",
    description:
      "Find parameters by path prefix (e.g. `drums.kick.`, `mixer.2.`, `comp.`) and/or a search word matched against path and label (e.g. `decay`, `cutoff`). Returns each match with its current value, range, unit and label.",
    input_schema: {
      type: "object",
      properties: {
        prefix: { type: "string", description: "Path prefix, e.g. `bass.` or `mixer.1.`" },
        query: { type: "string", description: "Word to match in the path or label, case-insensitive" },
      },
      additionalProperties: false,
    },
  },
  {
    name: "set_params",
    description:
      "Set one or more parameters at once (applied together; values are clamped to each parameter's range). Returns `path old -> new` for each. Prefer this over `rpc` for parameter changes.",
    input_schema: {
      type: "object",
      properties: {
        changes: {
          type: "array",
          items: {
            type: "object",
            properties: { path: { type: "string" }, value: { type: "number" } },
            required: ["path", "value"],
            additionalProperties: false,
          },
        },
        why: { type: "string", description: "One short phrase: what this change is for" },
      },
      required: ["changes"],
      additionalProperties: false,
    },
  },
  {
    name: "rpc",
    description:
      "Call any allowed 4S RPC method, for structural edits and reads that the other tools do not cover: add or remove devices (`device.add`), edit clips and notes (`clip.*`), instruments, channels, routing, sends, modulation, seats and MIDI. Use `describe` first if unsure of a method's params. Refused: project file operations (new/load/import/save), undo history, the daemon and connection, journal export, uploads, renders, and file paths on the engine host (e.g. loading a sample by path); ask the user to do those.",
    input_schema: {
      type: "object",
      properties: {
        method: { type: "string", description: "e.g. `device.add`" },
        params: { type: "object", description: "The method's params object" },
      },
      required: ["method"],
      additionalProperties: false,
    },
  },
  {
    name: "describe",
    description:
      "The params schema and description of one RPC method, and whether `rpc` may call it. With no method, lists every method with a one-line description.",
    input_schema: {
      type: "object",
      properties: { method: { type: "string" } },
      additionalProperties: false,
    },
  },
];

class ToolError extends Error {}

/** @param {number} v */
const pct = (v) => `${Math.round(v * 100)}%`;
/** @param {number} p */
const fmtPan = (p) => (Math.abs(p) < 0.005 ? "C" : p < 0 ? `L${Math.round(-p * 100)}` : `R${Math.round(p * 100)}`);
/** @param {number} v */
const num = (v) => (Number.isInteger(v) ? String(v) : String(Number(v.toFixed(4))));

/**
 * The overview text from a snapshot.
 * @param {any} s
 * @param {string} user
 */
function overviewText(s, user) {
  const p = (/** @type {string} */ k) => s.params[k];
  const lines = [];
  const t = s.transport;
  lines.push(
    `tempo ${num(p("transport.tempo") ?? 120)} bpm, swing ${pct(p("transport.swing") ?? 0)}, ${t.playing ? `playing (tick ${t.tick ?? "?"})` : "stopped"}${s.record?.recording ? ", recording" : ""}`,
  );
  lines.push(`project: ${s.project.path ?? "(unsaved)"}${s.project.dirty ? " (unsaved changes)" : ""}`);
  lines.push("instruments:");
  for (const i of s.graph.instruments) {
    const ch = s.graph.routes[i.id];
    const track = s.tracks.find((/** @type {any} */ x) => x.instrument === i.id);
    const clips = track ? `, clips ${track.clips.map((/** @type {any} */ c) => c.id).join(",")} (selected ${track.selected ?? "-"}${track.launched != null ? `, launched ${track.launched}` : ""})` : "";
    lines.push(`  ${i.id} (${i.type}, "${i.name}") -> ${ch != null ? `ch ${ch}` : "unrouted"}${clips}`);
  }
  const extra = Object.entries(s.graph.routes).filter(([src]) => !s.graph.instruments.some((/** @type {any} */ i) => i.id === src));
  for (const [src, ch] of extra) lines.push(`  route ${src} -> ch ${ch}`);
  lines.push("channels:");
  const inserts = (/** @type {number} */ n) =>
    s.graph.devices
      .filter((/** @type {any} */ d) => d.channel === n)
      .sort((/** @type {any} */ a, /** @type {any} */ b) => a.position - b.position)
      .map((/** @type {any} */ d) => `${d.id} (${d.type}${p(`${d.id}.bypass`) ? ", bypassed" : ""})`);
  for (const c of s.graph.channels) {
    const k = `mixer.${c.n}`;
    const flags = [p(`${k}.mute`) ? "muted" : "", p(`${k}.solo`) ? "solo" : ""].filter(Boolean).join(" ");
    const ins = inserts(c.n);
    lines.push(
      `  ch ${c.n} "${c.name}"${c.return ? " (return)" : ""}: vol ${pct(p(`${k}.volume`) ?? 1)} pan ${fmtPan(p(`${k}.pan`) ?? 0)}${flags ? ` ${flags}` : ""}${ins.length ? `; inserts ${ins.join(", ")}` : ""}`,
    );
  }
  const master = inserts(0);
  lines.push(`master inserts: ${master.length ? master.join(", ") : "(none)"}`);
  const others = s.graph.devices.filter((/** @type {any} */ d) => d.channel == null);
  if (others.length) lines.push(`other devices: ${others.map((/** @type {any} */ d) => `${d.id} (${d.type})`).join(", ")}`);
  if (s.graph.mod_routes?.length)
    lines.push(`modulation: ${s.graph.mod_routes.map((/** @type {any} */ r) => `${r.source} -> ${r.target} ${pct(r.depth ?? 0)}`).join(", ")}`);
  lines.push("seats:");
  for (const seat of s.seats.seats) {
    const mine = seat.name.toLowerCase() === user.toLowerCase() ? " (yours)" : "";
    const b = seat.config.bindings.length ? `, ${seat.config.bindings.length} bindings` : "";
    lines.push(`  ${seat.name}${mine}: focus ${seat.config.focus ?? "(first instrument)"}${b}, occupants ${seat.occupants.join(", ") || "none"}`);
  }
  lines.push(
    `midi devices: ${s.midi.length ? s.midi.map((/** @type {any} */ m) => `${m.device} (${m.model ?? m.profile?.kind ?? "generic"}, port ${m.input})`).join("; ") : "(none connected)"}`,
  );
  return lines.join("\n");
}

/**
 * Runs tools against the daemon.
 */
class Tools {
  /**
   * @param {InstanceType<typeof import("./daemon.cjs").DaemonClient>} daemon
   * @param {import("./schema.cjs").Schema} schema
   * @param {string} user
   */
  constructor(daemon, schema, user) {
    this.daemon = daemon;
    this.schema = schema;
    this.user = user;
  }

  /**
   * Run one tool call. Returns the text result and, for the panel, a short
   * summary (chips). Throws ToolError for a bad call or refused method.
   * @param {string} name
   * @param {any} input
   * @returns {Promise<{ text: string, chips: string[] }>}
   */
  async run(name, input) {
    if (!input || typeof input !== "object" || Array.isArray(input)) throw new ToolError("input must be an object");
    switch (name) {
      case "overview": {
        const s = await this.daemon.call("state.get");
        return { text: overviewText(s, this.user), chips: ["looked at the project"] };
      }
      case "find_params":
        return this.findParams(input);
      case "set_params":
        return this.setParams(input);
      case "rpc":
        return this.rpc(input);
      case "describe": {
        if (input.method == null) {
          const list = methods(this.schema)
            .map((m) => `${m.name}${rpcAllowed(this.schema, m.name) ? "" : " (refused)"}: ${m.doc}`)
            .join("\n");
          return { text: list, chips: [] };
        }
        if (typeof input.method !== "string") throw new ToolError("method must be a string");
        const d = describeMethod(this.schema, input.method);
        if (!d) throw new ToolError(`unknown method '${input.method}' (call describe with no method for the list)`);
        return { text: JSON.stringify(d), chips: [] };
      }
      default:
        throw new ToolError(`unknown tool '${name}'`);
    }
  }

  /** @param {any} input */
  async findParams(input) {
    const { prefix, query } = input;
    if (prefix != null && typeof prefix !== "string") throw new ToolError("prefix must be a string");
    if (query != null && typeof query !== "string") throw new ToolError("query must be a string");
    const [list, state] = await Promise.all([
      this.daemon.call("param.list", { prefix: prefix || null }),
      this.daemon.call("state.get"),
    ]);
    const q = (query ?? "").toLowerCase();
    const found = list.params.filter(
      (/** @type {any} */ p) => !q || p.path.toLowerCase().includes(q) || p.label.toLowerCase().includes(q),
    );
    const rows = found.slice(0, MAX_PARAMS).map((/** @type {any} */ p) => {
      const v = state.params[p.path] ?? p.default;
      const range = p.kind === "toggle" ? "on/off" : `${num(p.min)}..${num(p.max)}${p.kind === "integer" ? " int" : ""}`;
      const moving = state.moving?.[p.path] != null ? ` (moving, now ${num(state.moving[p.path])})` : "";
      return `${p.path} = ${num(v)}${moving}  [${range}${p.unit ? ` ${p.unit}` : ""}, default ${num(p.default)}]  ${p.label}`;
    });
    if (found.length > MAX_PARAMS) rows.push(`... ${found.length - MAX_PARAMS} more; narrow the prefix or query`);
    return { text: rows.length ? rows.join("\n") : "no matching parameters", chips: [] };
  }

  /** @param {any} input */
  async setParams(input) {
    const changes = input.changes;
    if (!Array.isArray(changes) || changes.length === 0) throw new ToolError("changes must be a non-empty array");
    for (const c of changes) {
      if (!c || typeof c.path !== "string" || typeof c.value !== "number" || !Number.isFinite(c.value))
        throw new ToolError("each change needs a string path and a finite number value");
    }
    const before = (await this.daemon.call("state.get")).params;
    const requests = changes.map((c) => ({ method: "param.set", params: { path: c.path, value: c.value } }));
    let result;
    try {
      result = await this.daemon.call("batch", { requests });
    } catch (e) {
      throw new ToolError(/** @type {Error} */ (e).message);
    }
    const lines = changes.map((/** @type {any} */ c, /** @type {number} */ i) => {
      const r = result.results[i];
      const now = typeof r?.value === "number" ? r.value : c.value;
      const old = before[c.path];
      return `${c.path} ${old == null ? "?" : num(old)} -> ${num(now)}`;
    });
    return { text: lines.join("\n"), chips: lines };
  }

  /** @param {any} input */
  async rpc(input) {
    const { method } = input;
    const params = input.params ?? {};
    if (typeof method !== "string") throw new ToolError("method must be a string");
    if (typeof params !== "object" || Array.isArray(params)) throw new ToolError("params must be an object");
    if (!methods(this.schema).some((m) => m.name === method)) throw new ToolError(`unknown method '${method}'`);
    const refused = rpcRefusal(this.schema, method, params);
    if (refused) throw new ToolError(refused);
    let result;
    try {
      result = await this.daemon.call(method, params);
    } catch (e) {
      throw new ToolError(`${method}: ${/** @type {Error} */ (e).message}`);
    }
    let text = JSON.stringify(result ?? null);
    if (text.length > MAX_RESULT) text = `${text.slice(0, MAX_RESULT)} ... (truncated; ask for less)`;
    const readOnly = this.schema.read_only_methods.includes(method);
    return { text, chips: readOnly ? [] : [rpcChip(method, params)] };
  }
}

/** A short chip for an edit made through `rpc`, e.g. `+ compressor on ch 1`.
 * @param {string} method @param {any} params */
function rpcChip(method, params) {
  if (method === "device.add") {
    const on = params.channel == null ? "" : params.channel === 0 ? " on master" : ` on ch ${params.channel}`;
    return `+ ${params.type ?? "device"}${on}`;
  }
  if (method === "device.remove") return `- ${params.id ?? "device"}`;
  if (method === "instrument.add") return `+ ${params.type ?? "instrument"}${params.id ? ` (${params.id})` : ""}`;
  if (method === "instrument.remove") return `- ${params.id ?? "instrument"}`;
  if (method === "param.set") return `${params.path} -> ${params.value}`;
  const brief = JSON.stringify(params);
  return `${method} ${brief.length > 40 ? `${brief.slice(0, 40)}...` : brief}`;
}

module.exports = { TOOL_DEFS, Tools, ToolError, overviewText };
