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
    name: "analyze_audio",
    description:
      "Listen to the audio file behind a sampler pad (the whole song a pad was cut from, if the engine host still has it; else the pad's own file): its tempo and bar grid, key, level, its sections (runs of bars that sound alike; alike sections share a letter), and a table a bar a row. Columns: level in dBFS; low/mid/high, the shares of energy below 200 Hz (kick, bass), 200 Hz-2 kHz (body, voice, most instruments) and above 2 kHz (hats, cymbals); onsets (note or hit starts); punch 0..1 (how hard they hit: drums high, pads low); tonal 0..1 (chords and melody high, drums and noise low). Compare rows within the file, not against fixed numbers. With start/end, rows are numbered from the span's start, while `cut_sample` reports bars counted from the file's start; go by the times. Use it to find a part of a song by description, e.g. a drum break is bars with many onsets, high punch and high-band share, and low tonal; then cut it with `cut_sample`. Give start/end (seconds) to look at a span only.",
    input_schema: {
      type: "object",
      properties: {
        instrument: { type: "string", description: "The sampler's id (default: the first sampler)" },
        pad: { type: "integer", description: "The pad whose file to analyze, 1..16" },
        start: { type: "number", description: "Seconds into the file (default 0)" },
        end: { type: "number", description: "Seconds into the file (default: its end)" },
      },
      required: ["pad"],
      additionalProperties: false,
    },
  },
  {
    name: "cut_sample",
    description:
      "Load a part of the file behind a sampler pad (the same file `analyze_audio` reads) onto a pad: start and end in seconds of that file, snapped to the nearest bar lines of its beat grid (so it loops in time) unless snap is false. At most 60 s. Returns the bars (rows of the whole file's analysis: a pickup before the first bar line, if any, is bar 1) and the exact times; tell the user both. One undo step with the rest of the turn.",
    input_schema: {
      type: "object",
      properties: {
        instrument: { type: "string", description: "The sampler's id (default: the first sampler)" },
        pad: { type: "integer", description: "The pad whose file to cut from, 1..16" },
        to_pad: { type: "integer", description: "The pad to load the cut onto (default: the same pad)" },
        start: { type: "number", description: "Seconds into the file" },
        end: { type: "number", description: "Seconds into the file" },
        snap: { type: "boolean", description: "Snap to bar lines (default true)" },
      },
      required: ["pad", "start", "end"],
      additionalProperties: false,
    },
  },
  {
    name: "rpc",
    description:
      "Call any allowed 4S RPC method, for structural edits and reads that the other tools do not cover: add or remove devices (`device.add`), edit clips and notes (`clip.*`), instruments, channels, routing, sends, modulation, seats and MIDI. Use `describe` first if unsure of a method's params. Refused: project file operations (new/load/import/save), undo history, the daemon and connection, journal export, uploads, renders, and file paths on the engine host (e.g. loading a sample by path; use `analyze_audio` and `cut_sample` for files already on a pad); ask the user to do those.",
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
    /** Whole-file analyses by path, for `cut_sample`'s bar lines (a
     * project's files do not change under it in a chat). @type {Map<string, any>} */
    this.analyses = new Map();
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
      case "analyze_audio":
        return this.analyzeAudio(input);
      case "cut_sample":
        return this.cutSample(input);
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

  /**
   * The file behind a pad, which `analyze_audio` and `cut_sample` work on:
   * the file it was cut from if the engine host still has it (its
   * `origin`, the file the sample editor opens too), else its own. Only
   * files the project names for a pad, never a path from the model.
   * @param {any} input
   * @returns {Promise<{ instrument: string, path: string, name: string }>}
   */
  async padFile(input) {
    if (input.instrument != null && typeof input.instrument !== "string") throw new ToolError("instrument must be a string");
    if (!Number.isInteger(input.pad)) throw new ToolError("pad must be an integer 1..16");
    let info;
    try {
      info = await this.daemon.call("sampler.get", { instrument: input.instrument ?? null });
    } catch (e) {
      throw new ToolError(`sampler.get: ${/** @type {Error} */ (e).message}`);
    }
    const pad = info.pads.find((/** @type {any} */ p) => p.pad === input.pad);
    if (!pad) {
      const loaded = info.pads.map((/** @type {any} */ p) => p.pad).join(", ") || "none";
      throw new ToolError(`${info.instrument} pad ${input.pad} is empty (loaded pads: ${loaded}); ask the user to load the song onto a pad in the app`);
    }
    if (pad.origin) {
      const there = await this.daemon.call("sample.info", { path: pad.origin.path }).then(() => true, () => false);
      if (there) return { instrument: info.instrument, path: pad.origin.path, name: pad.origin.path.split(/[\\/]/).pop() };
    }
    return { instrument: info.instrument, path: pad.path, name: pad.name };
  }

  /**
   * `sample.analyze` of a file, whole files kept for snapping.
   * @param {string} path @param {number | null} start @param {number | null} end
   */
  async analysis(path, start, end) {
    const whole = start == null && end == null;
    // Keyed by what the file is now, so a file replaced at the same path
    // is analyzed again.
    let key = path;
    if (whole) {
      const info = await this.daemon.call("sample.info", { path }).catch(() => null);
      key = info ? `${path}|${info.seconds}|${info.format}` : "";
      if (key && this.analyses.has(key)) return this.analyses.get(key);
    }
    let a;
    try {
      a = await this.daemon.call("sample.analyze", { path, start, end, beats_per_bar: null, onsets: false, beats: false });
    } catch (e) {
      throw new ToolError(`sample.analyze: ${/** @type {Error} */ (e).message}`);
    }
    if (key && whole) this.analyses.set(key, a);
    return a;
  }

  /** @param {any} input */
  async analyzeAudio(input) {
    for (const k of ["start", "end"])
      if (input[k] != null && (typeof input[k] !== "number" || !Number.isFinite(input[k]))) throw new ToolError(`${k} must be seconds`);
    const f = await this.padFile(input);
    const a = await this.analysis(f.path, input.start ?? null, input.end ?? null);
    return { text: analysisText(a, f), chips: [`listened to ${f.name}`] };
  }

  /** @param {any} input */
  async cutSample(input) {
    for (const k of ["start", "end"])
      if (typeof input[k] !== "number" || !Number.isFinite(input[k])) throw new ToolError(`${k} must be seconds`);
    const to = input.to_pad ?? input.pad;
    if (!Number.isInteger(to)) throw new ToolError("to_pad must be an integer 1..16");
    const f = await this.padFile(input);
    let [start, end] = [input.start, input.end];
    let bars = "";
    if (input.snap !== false) {
      const a = await this.analysis(f.path, null, null);
      if (a.row_unit !== "bar") throw new ToolError("no steady beat in this file to snap to; call again with snap: false");
      // Bar lines: each row's start, and the last row's end.
      const lines = [...a.rows.map((/** @type {any} */ r) => r.start), a.rows.at(-1).end];
      const nearest = (/** @type {number} */ t) => lines.reduce((best, x, i) => (Math.abs(x - t) < Math.abs(lines[best] - t) ? i : best), 0);
      const i = nearest(start);
      let j = nearest(end);
      if (j <= i) j = Math.min(i + 1, lines.length - 1);
      if (j <= i) throw new ToolError("that is past the last bar line");
      [start, end] = [lines[i], lines[j]];
      bars = j - i === 1 ? `bar ${i + 1}` : `bars ${i + 1}-${j}`;
    }
    if (end - start > 60) throw new ToolError(`that is ${(end - start).toFixed(1)} s${bars ? ` (${bars})` : ""}; a pad plays at most 60 s: choose fewer bars`);
    try {
      await this.daemon.call("sampler.load", { instrument: f.instrument, pad: to, path: f.path, upload: null, start, end });
    } catch (e) {
      throw new ToolError(`sampler.load: ${/** @type {Error} */ (e).message}`);
    }
    const span = `${clock(start)} - ${clock(end)}`;
    const text = `${f.instrument} pad ${to} now plays ${f.name} ${bars ? `${bars}, ` : ""}${span} (${(end - start).toFixed(3)} s; start ${start}, end ${end})`;
    return { text, chips: [`pad ${to} <- ${bars ? `${bars} ` : ""}(${span})`] };
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

/** Seconds as `m:ss.mmm`. @param {number} t */
function clock(t) {
  const ms = Math.round(Math.max(0, t) * 1000);
  return `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}.${String(ms % 1000).padStart(3, "0")}`;
}

/** `analyze_audio`'s text: the overview, sections, and a compact table a
 * row (bars, or seconds with no steady beat).
 * @param {any} a @param {{ name: string }} f */
function analysisText(a, f) {
  const out = [`${f.name}: ${clock(a.start)} - ${clock(a.end)}, peak ${a.peak_db} dB, level ${a.rms_db} dB, ${a.onset_count} onsets`];
  const t = a.tempo;
  out.push(t ? `tempo ${t.bpm} bpm (confidence ${t.confidence}), ${t.beats_per_bar} beats a bar, first bar line ${t.first_downbeat} s` : "no steady beat: rows are seconds");
  out.push(a.key ? `key ${a.key.name} (confidence ${a.key.confidence})` : "key: none");
  const unit = a.row_unit === "bar" ? "bars" : "secs";
  out.push(`sections (${unit}; level dB, low/mid/high, onsets a second, punch, tonal):`);
  for (const s of a.sections)
    out.push(`${s.label} ${unit} ${s.first}-${s.last} ${s.start}-${s.end}s ${s.rms_db} ${s.low}/${s.mid}/${s.high} ${s.onset_rate}/s p${s.punch} t${s.tonal}`);
  out.push(`${unit} (n start-end s, level dB, low/mid/high, onsets, punch, tonal):`);
  for (const r of a.rows) out.push(`${r.n} ${r.start}-${r.end} ${r.rms_db} ${r.low}/${r.mid}/${r.high} ${r.onsets} p${r.punch} t${r.tonal}`);
  return out.join("\n");
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
