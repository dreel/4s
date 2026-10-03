// Typed JSON-RPC client for 4sd. Method names, params, and results all come
// from generated/methods.ts, which is generated from the Rust protocol crate.

import type { EventEnvelope } from "./generated/EventEnvelope";
import type { MethodName, Methods } from "./generated/methods";

export type ConnectionState = "connecting" | "open" | "closed";

type Pending = { resolve: (v: unknown) => void; reject: (e: Error) => void; method: string };

export class RpcError extends Error {
  constructor(public method: string, public code: number, message: string) {
    super(`${method}: ${message}`);
  }
}

export class RpcClient {
  private ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private eventListeners = new Set<(e: EventEnvelope) => void>();
  private stateListeners = new Set<(s: ConnectionState) => void>();
  private retry: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  state: ConnectionState = "closed";
  /** Called after every (re)connect, before the state flips to "open". */
  onConnect: (() => Promise<void>) | null = null;

  constructor(public readonly url: string) {}

  connect() {
    this.closed = false;
    this.setState("connecting");
    const ws = new WebSocket(this.url);
    this.ws = ws;
    ws.onopen = async () => {
      try {
        await this.onConnect?.();
        this.setState("open");
      } catch (e) {
        console.error("connect handshake failed", e);
        ws.close();
      }
    };
    ws.onmessage = (m) => this.handle(JSON.parse(String(m.data)));
    ws.onclose = () => {
      for (const p of this.pending.values()) p.reject(new Error("connection closed"));
      this.pending.clear();
      this.setState("closed");
      if (!this.closed) this.retry = setTimeout(() => this.connect(), 1000);
    };
  }

  close() {
    this.closed = true;
    if (this.retry) clearTimeout(this.retry);
    this.ws?.close();
  }

  call<M extends MethodName>(method: M, params: Methods[M]["params"]): Promise<Methods[M]["result"]> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error(`${method}: not connected`));
    }
    const id = this.nextId++;
    ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, method });
    });
  }

  onEvent(fn: (e: EventEnvelope) => void) {
    this.eventListeners.add(fn);
    return () => this.eventListeners.delete(fn);
  }

  onState(fn: (s: ConnectionState) => void) {
    this.stateListeners.add(fn);
    return () => this.stateListeners.delete(fn);
  }

  private setState(s: ConnectionState) {
    this.state = s;
    for (const fn of this.stateListeners) fn(s);
  }

  private handle(msg: { id?: number; result?: unknown; error?: { code: number; message: string }; method?: string; params?: unknown }) {
    if (typeof msg.id === "number") {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.reject(new RpcError(p.method, msg.error.code, msg.error.message));
      else p.resolve(msg.result);
    } else if (msg.method === "event") {
      for (const fn of this.eventListeners) fn(msg.params as EventEnvelope);
    }
  }
}
