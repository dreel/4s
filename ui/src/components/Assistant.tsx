// The assistant drawer (RFC 0013): chat with a Claude-powered assistant
// that changes the project through RPC. Each reply is one undo step.

import { useEffect, useRef, useState } from "react";
import {
  newChat,
  saveSettings,
  sendMessage,
  stopTurn,
  toggleAssistant,
  undoTurn,
  useAssistant,
  type Turn,
} from "../assistant";

const MODEL_NAMES: Record<string, string> = {
  "claude-sonnet-5-5": "Sonnet 5.5",
  "claude-opus-5-5": "Opus 5.5",
  "claude-haiku-5-5": "Haiku 5.5",
};

const KEY_SOURCES: Record<string, string> = {
  keychain: "key saved (encrypted)",
  session: "key kept for this session only (no encryption available)",
  env: "using ANTHROPIC_API_KEY",
  none: "no key set",
};

function Settings({ onClose }: { onClose: () => void }) {
  const settings = useAssistant((s) => s.settings);
  const usage = useAssistant((s) => s.usage);
  const [key, setKey] = useState("");
  if (!settings) return null;
  const input = "w-full bg-zinc-900 border border-zinc-700 rounded px-2 py-1 text-xs";
  return (
    <div className="absolute right-2 top-10 z-20 w-72 rounded border border-zinc-700 bg-zinc-950 p-3 text-xs shadow-xl flex flex-col gap-2" data-testid="assistant-settings">
      <label className="flex flex-col gap-1">
        <span className="text-zinc-400">Anthropic API key</span>
        <input
          className={input}
          type="password"
          placeholder={settings.keySource === "none" ? "sk-ant-..." : "(set; type to replace)"}
          value={key}
          onChange={(e) => setKey(e.target.value)}
          data-testid="assistant-key"
        />
      </label>
      <div className="text-zinc-500" data-testid="assistant-key-source" data-source={settings.keySource}>
        {KEY_SOURCES[settings.keySource]}
      </div>
      <div className="flex gap-2">
        <button
          className="px-2 py-0.5 rounded border border-zinc-600 hover:border-zinc-400 disabled:opacity-30"
          disabled={!key.trim()}
          onClick={() => void saveSettings({ key }).then(() => setKey(""))}
          data-testid="assistant-key-save"
        >
          save key
        </button>
        {settings.keySource !== "none" && settings.keySource !== "env" && (
          <button className="px-2 py-0.5 rounded border border-zinc-700 text-zinc-400 hover:border-zinc-500" onClick={() => void saveSettings({ key: null })}>
            forget key
          </button>
        )}
      </div>
      <label className="flex flex-col gap-1">
        <span className="text-zinc-400">Model</span>
        <select className={input} value={settings.model} onChange={(e) => void saveSettings({ model: e.target.value })} data-testid="assistant-model">
          {settings.models.map((m) => (
            <option key={m} value={m}>
              {MODEL_NAMES[m] ?? m}
            </option>
          ))}
        </select>
      </label>
      {usage && (
        <div className="text-zinc-500" data-testid="assistant-cost">
          this session: ~${usage.cost.toFixed(usage.cost < 0.1 ? 4 : 2)} ({Math.round((usage.input + usage.cacheRead + usage.cacheWrite) / 1000)}k in, {Math.round(usage.output / 1000)}k out)
        </div>
      )}
      <div className="text-zinc-600">Your messages and what the assistant reads from the project are sent to Anthropic.</div>
      <button className="self-end text-zinc-400 hover:text-zinc-200" onClick={onClose}>
        close
      </button>
    </div>
  );
}

const STOPS: Record<string, string> = {
  stopped: "stopped",
  refusal: "declined",
  tool_limit: "stopped at the tool call limit",
  max_tokens: "cut off",
};

function Message({ turn, index }: { turn: Turn; index: number }) {
  const done = turn.done;
  return (
    <div className="flex flex-col gap-1.5" data-testid="assistant-turn" data-state={done ? done.stop : "running"}>
      <div className="self-end max-w-[85%] rounded bg-zinc-800 px-2 py-1 text-zinc-100 whitespace-pre-wrap">{turn.user}</div>
      {turn.parts.map((p, i) =>
        p.kind === "text" ? (
          <div key={i} className="text-zinc-300 whitespace-pre-wrap" data-testid="assistant-text">
            {p.text}
          </div>
        ) : (
          <div key={i} className="flex flex-wrap gap-1">
            {p.chips.map((c, j) => (
              <span
                key={j}
                className={`rounded px-1.5 py-0.5 font-mono text-[11px] ${c.ok ? "bg-zinc-900 border border-zinc-700 text-zinc-300" : "bg-red-950 border border-red-900 text-red-300"}`}
                data-testid="assistant-chip"
                data-ok={c.ok}
              >
                {c.text}
              </span>
            ))}
          </div>
        ),
      )}
      {!done && turn.parts.length === 0 && <div className="text-zinc-500">...</div>}
      {done?.error && (
        <div className="text-red-400" data-testid="assistant-error">
          {done.error}
        </div>
      )}
      {done && (STOPS[done.stop] || done.changed.length > 0) && (
        <div className="flex items-center gap-2 text-[11px] text-zinc-500">
          {STOPS[done.stop] && <span>{STOPS[done.stop]}</span>}
          {done.changed.length > 0 &&
            (turn.undone ? (
              <span data-testid="assistant-undone">{turn.undone}</span>
            ) : (
              <button
                className="px-1.5 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500"
                title={`undo this reply (${done.changed.length} changes, one undo step)`}
                onClick={() => void undoTurn(index)}
                data-testid="assistant-undo"
              >
                undo
              </button>
            ))}
        </div>
      )}
    </div>
  );
}

export function AssistantPanel() {
  const turns = useAssistant((s) => s.turns);
  const running = useAssistant((s) => s.running);
  const settings = useAssistant((s) => s.settings);
  const [text, setText] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  const end = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [turns]);
  useEffect(() => {
    box.current?.focus();
  }, []);
  const send = () => {
    if (running || !text.trim()) return;
    void sendMessage(text);
    setText("");
  };
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-xs text-zinc-300 hover:border-zinc-500 disabled:opacity-30";
  return (
    <aside className="relative flex w-96 shrink-0 flex-col border-l border-zinc-800 bg-zinc-950 text-xs" data-testid="assistant-panel">
      <div className="flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
        <div className="font-semibold text-zinc-200">Assistant</div>
        <div className="text-zinc-500">{settings ? (MODEL_NAMES[settings.model] ?? settings.model) : ""}</div>
        <div className="ml-auto flex gap-1">
          <button className={btn} onClick={() => void newChat()} disabled={running} data-testid="assistant-new">
            new chat
          </button>
          <button className={btn} onClick={() => setShowSettings((v) => !v)} data-testid="assistant-settings-toggle">
            settings
          </button>
          <button className={btn} onClick={() => toggleAssistant(false)} title="close (Cmd-K)">
            x
          </button>
        </div>
      </div>
      {showSettings && <Settings onClose={() => setShowSettings(false)} />}
      <div className="flex flex-1 flex-col gap-4 overflow-auto p-3" data-testid="assistant-messages">
        {turns.length === 0 && (
          <div className="text-zinc-500">
            Ask for a change ("make the kick punchier", "darker bass") or a question about 4S. Each reply is one undo step.
            {settings?.keySource === "none" && <div className="mt-2 text-amber-400">Add an API key in settings first.</div>}
          </div>
        )}
        {turns.map((t, i) => (
          <Message key={i} turn={t} index={i} />
        ))}
        <div ref={end} />
      </div>
      <div className="flex flex-col gap-2 border-t border-zinc-800 p-3">
        <textarea
          ref={box}
          className="h-16 w-full resize-none rounded border border-zinc-700 bg-zinc-900 px-2 py-1 text-zinc-100"
          placeholder="Ask the assistant... (Enter to send)"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          data-testid="assistant-input"
        />
        <div className="flex justify-end gap-2">
          {running ? (
            <button className={btn} onClick={stopTurn} data-testid="assistant-stop">
              stop
            </button>
          ) : (
            <button className={btn} onClick={send} disabled={!text.trim()} data-testid="assistant-send">
              send
            </button>
          )}
        </div>
      </div>
    </aside>
  );
}
