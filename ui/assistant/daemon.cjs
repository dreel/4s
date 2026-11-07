// @ts-check
// The assistant's own connection to 4sd (RFC 0013): a JSON-RPC client over
// WebSocket that says hello as client "assistant" with the app's user, so
// its edits are the user's (Cmd-Z undoes them) and the journal shows
// `origin: assistant`.

class RpcError extends Error {
  /** @param {string} method @param {{ code?: number, message?: string }} e */
  constructor(method, e) {
    super(e.message || "request failed");
    this.method = method;
    this.code = e.code;
  }
}

class DaemonClient {
  /**
   * @param {{ url: string, user: string, token?: string | null, protocolVersion: number, clientName?: string }} opts
   */
  constructor(opts) {
    this.opts = opts;
    /** @type {WebSocket | null} */
    this.ws = null;
    this.nextId = 1;
    /** @type {Map<number, { method: string, resolve: (v: any) => void, reject: (e: Error) => void }>} */
    this.pending = new Map();
    /** @type {Promise<void> | null} */
    this.opening = null;
  }

  /** Connect (once; again after a disconnect) and say hello. */
  async connect() {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) return;
    if (this.opening) return this.opening;
    this.opening = (async () => {
      const ws = new WebSocket(this.opts.url);
      await new Promise((resolve, reject) => {
        ws.onopen = () => resolve(undefined);
        ws.onerror = () => reject(new Error(`cannot reach 4sd at ${this.opts.url}`));
      });
      ws.onmessage = (m) => this.onMessage(String(m.data));
      ws.onclose = () => {
        if (this.ws === ws) this.ws = null;
        for (const p of this.pending.values()) p.reject(new Error("the connection to 4sd closed"));
        this.pending.clear();
      };
      this.ws = ws;
      await this.call("session.hello", {
        client_name: this.opts.clientName ?? "assistant",
        protocol_version: this.opts.protocolVersion,
        token: this.opts.token ?? null,
        user: this.opts.user,
      });
    })();
    try {
      await this.opening;
    } finally {
      this.opening = null;
    }
  }

  /** @param {string} text */
  onMessage(text) {
    let msg;
    try {
      msg = JSON.parse(text);
    } catch {
      return;
    }
    if (msg.id == null) return; // events: this client does not subscribe
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    if (msg.error) p.reject(new RpcError(p.method, msg.error));
    else p.resolve(msg.result);
  }

  /**
   * One request. Connects first if needed (except for the hello itself).
   * @param {string} method
   * @param {unknown} [params]
   * @returns {Promise<any>}
   */
  async call(method, params = {}) {
    if (method !== "session.hello") await this.connect();
    const ws = this.ws;
    if (!ws) throw new Error("not connected to 4sd");
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { method, resolve, reject });
      ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params: params ?? {} }));
    });
  }

  close() {
    this.ws?.close();
    this.ws = null;
  }
}

module.exports = { DaemonClient, RpcError };
