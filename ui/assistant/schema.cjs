// @ts-check
// What the assistant knows about the RPC API, read from the generated
// schema (schema/4s.schema.json, written by gen-bindings): each method's
// params, and the method classes its `rpc` tool allows methods by.

const fs = require("node:fs");
const path = require("node:path");

/** The schema file: next to the app when packaged, else in the repo. */
function schemaPath() {
  const candidates = [
    process.env.FOURS_SCHEMA,
    // @ts-ignore resourcesPath exists in Electron
    process.resourcesPath && path.join(process.resourcesPath, "schema", "4s.schema.json"),
    path.join(__dirname, "..", "..", "schema", "4s.schema.json"),
  ];
  for (const c of candidates) if (c && fs.existsSync(c)) return c;
  throw new Error("schema/4s.schema.json not found (set FOURS_SCHEMA)");
}

/** @typedef {{ protocol_version: number, read_only_methods: string[], batchable_methods: string[], request: any }} Schema */

/** @type {Schema | null} */
let cached = null;

/** @returns {Schema} */
function loadSchema() {
  if (!cached) cached = JSON.parse(fs.readFileSync(schemaPath(), "utf8"));
  return /** @type {Schema} */ (cached);
}

/**
 * Every method name, with its one-line description.
 * @param {Schema} s
 * @returns {{ name: string, doc: string, params: any }[]}
 */
function methods(s) {
  return s.request.oneOf.map((/** @type {any} */ v) => ({
    name: v.properties.method.const,
    doc: v.description ?? "",
    params: v.properties.params,
  }));
}

/** Transport moves that are not edits but are safe to do on request. */
const TRANSPORT = ["transport.play", "transport.stop", "transport.locate"];
/** Whole namespaces the assistant never calls: the connection, the
 * daemon, the project file (new, load, import, save), and undo history
 * (the panel wraps each turn in an undo group itself). */
const REFUSED_PREFIXES = ["session.", "events.", "daemon.", "project.", "history."];
/** Read-only in the daemon's sense (not journaled) but not for the
 * assistant: they hand out the whole session, write files on the engine
 * host, or render to a path (the `listen` tool will render to a temporary
 * file instead, RFC 0013 phase B). */
const REFUSED = ["journal.export", "sample.upload", "render.offline"];

/**
 * Whether the `rpc` tool may call `method`. The rule (RFC 0013): a method
 * is allowed if the daemon classes it read-only or `batch` accepts it
 * (journaled, undoable edits), plus transport play/stop/locate; minus the
 * namespaces and methods above. A new method is refused unless the daemon
 * classes it as one of those, so new methods are safe by default.
 * @param {Schema} s
 * @param {string} method
 */
function rpcAllowed(s, method) {
  if (REFUSED.includes(method) || REFUSED_PREFIXES.some((p) => method.startsWith(p))) return false;
  return s.read_only_methods.includes(method) || s.batchable_methods.includes(method) || TRANSPORT.includes(method);
}

/** Params that name a file on the engine host. The assistant may not pass
 * them: a name it read from the project (or was talked into using) must
 * not load or read arbitrary files there. A sample you loaded yourself can
 * still be edited; a pad can still be loaded from an upload id. */
const PATH_PARAMS = {
  "sampler.load": "path",
  "sample.info": "path",
  "sample.peaks": "path",
  "sample.play": "path",
};

/**
 * Why the `rpc` tool refuses this call, or null if it may make it.
 * @param {Schema} s
 * @param {string} method
 * @param {Record<string, unknown>} params
 */
function rpcRefusal(s, method, params) {
  if (!rpcAllowed(s, method)) return `'${method}' is not allowed from the assistant; ask the user to do it in the app or the CLI`;
  const key = PATH_PARAMS[/** @type {keyof typeof PATH_PARAMS} */ (method)];
  if (key && params[key] != null)
    return `'${method}' with a file path is not allowed from the assistant (files on the engine host); ask the user to load the file in the app`;
  return null;
}

/**
 * A method's params schema with its `$ref`s inlined (to a depth), for the
 * `describe` tool.
 * @param {Schema} s
 * @param {string} method
 */
function describeMethod(s, method) {
  const m = methods(s).find((x) => x.name === method);
  if (!m) return null;
  const defs = s.request.$defs ?? {};
  /** @param {any} node @param {number} depth @returns {any} */
  const inline = (node, depth) => {
    if (Array.isArray(node)) return node.map((n) => inline(n, depth));
    if (!node || typeof node !== "object") return node;
    if (typeof node.$ref === "string") {
      const name = node.$ref.replace("#/$defs/", "");
      if (depth > 4 || !defs[name]) return { type: name };
      return inline(defs[name], depth + 1);
    }
    /** @type {Record<string, any>} */
    const out = {};
    for (const [k, v] of Object.entries(node)) out[k] = inline(v, depth);
    return out;
  };
  return { method, description: m.doc, allowed: rpcAllowed(s, method), params: inline(m.params, 0) };
}

module.exports = { loadSchema, methods, rpcAllowed, rpcRefusal, describeMethod, PATH_PARAMS };
