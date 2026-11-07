// A fake Anthropic Messages API for tests (RFC 0013): replays scripted
// replies as SSE streams, so the assistant runs end to end with no key.
//
// A script is a list of rules. The first rule whose `match` is a substring
// of the turn's user message answers it; its `replies` are given in order,
// one per request in that turn (a reply after each round of tool results).
//
//   { match: "punchier", replies: [
//       { content: [{ type: "text", text: "Tightening the kick." },
//                   { type: "tool_use", name: "set_params", input: {...} }] },
//       { content: [{ type: "text", text: "Done." }] } ] }
//
// A reply can set `delayMs` (between streamed chunks) or `hang: true`
// (never finish: for testing Stop). Requests are recorded for assertions.

const http = require("node:http");

/** @param {any} body The turn's user text and how many replies it had. */
function turnOf(body) {
  const msgs = body.messages ?? [];
  // The turn starts at the last user message with a text block.
  let start = -1;
  for (let i = msgs.length - 1; i >= 0; i--) {
    const c = msgs[i].content;
    if (msgs[i].role === "user" && (typeof c === "string" || c.some((b) => b.type === "text"))) {
      start = i;
      break;
    }
  }
  if (start < 0) return { text: "", index: 0 };
  const c = msgs[start].content;
  const text = typeof c === "string" ? c : c.filter((b) => b.type === "text").map((b) => b.text).join("\n");
  const index = msgs.slice(start + 1).filter((m) => m.role === "assistant").length;
  return { text, index };
}

/**
 * @param {{ match: string, replies: any[] }[]} script
 * @returns {Promise<{ url: string, requests: any[], close: () => Promise<void> }>}
 */
function startFakeAnthropic(script) {
  /** @type {any[]} */
  const requests = [];
  let ids = 0;
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (d) => (raw += d));
    req.on("end", async () => {
      if (req.method !== "POST" || !req.url?.startsWith("/v1/messages")) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ type: "error", error: { type: "not_found_error", message: "not found" } }));
        return;
      }
      const body = JSON.parse(raw);
      requests.push({ headers: req.headers, body });
      const { text, index } = turnOf(body);
      const rule = script.find((r) => text.includes(r.match));
      const reply = rule?.replies[Math.min(index, rule.replies.length - 1)] ?? {
        content: [{ type: "text", text: "(no scripted reply)" }],
      };
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      const send = (event, data) => res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
      const wait = (ms) => new Promise((r) => setTimeout(r, ms));
      let closed = false;
      res.on("close", () => (closed = true));
      const delay = reply.delayMs ?? 0;
      send("message_start", {
        message: {
          id: `msg_fake_${++ids}`,
          type: "message",
          role: "assistant",
          model: body.model,
          content: [],
          stop_reason: null,
          stop_sequence: null,
          usage: { input_tokens: 1000, output_tokens: 1, cache_read_input_tokens: index > 0 ? 900 : 0, cache_creation_input_tokens: 0 },
        },
      });
      let n = 0;
      let usedTool = false;
      for (const [i, block] of reply.content.entries()) {
        if (block.type === "text") {
          send("content_block_start", { index: i, content_block: { type: "text", text: "" } });
          const words = block.text.split(/(?<= )/);
          for (const w of words) {
            if (closed) return;
            send("content_block_delta", { index: i, delta: { type: "text_delta", text: w } });
            if (delay) await wait(delay);
          }
          if (reply.hang) {
            // Keep streaming nothing until the client goes away.
            while (!closed) await wait(50);
            return;
          }
        } else if (block.type === "tool_use") {
          usedTool = true;
          const id = block.id ?? `toolu_fake_${ids}_${++n}`;
          send("content_block_start", { index: i, content_block: { type: "tool_use", id, name: block.name, input: {} } });
          send("content_block_delta", { index: i, delta: { type: "input_json_delta", partial_json: JSON.stringify(block.input ?? {}) } });
        }
        send("content_block_stop", { index: i });
      }
      send("message_delta", { delta: { stop_reason: reply.stop_reason ?? (usedTool ? "tool_use" : "end_turn"), stop_sequence: null }, usage: { output_tokens: 50 } });
      send("message_stop", {});
      res.end();
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const addr = /** @type {import("node:net").AddressInfo} */ (server.address());
      resolve({
        url: `http://127.0.0.1:${addr.port}`,
        requests,
        close: () => new Promise((r) => { server.closeAllConnections(); server.close(() => r(undefined)); }),
      });
    });
  });
}

module.exports = { startFakeAnthropic };
