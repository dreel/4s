// The sample editor (RFC 0011): a pad's file as a waveform, to choose the
// region the pad plays. It edits the file the pad was cut from when that is
// still on the engine host (a saved project's copy keeps its origin), else
// the pad's own file. Drag on the waveform to select (drag a selection's
// edge to move it), wheel to zoom around the pointer, shift-wheel to
// scroll; play the selection through the engine (`sample.play`, outside
// the mix), and apply it to the pad (`sampler.load` with the region, one
// undo step). The waveform comes from the engine (`sample.peaks`), a bin a
// pixel, so a whole song costs no more than a hit.

import { useEffect, useMemo, useRef, useState } from "react";
import type { SamplerPad } from "../generated/SamplerPad";
import { act, client, useApp } from "../store";

/** A pad plays at most this long (the engine's limit). */
const MAX_REGION = 60;
/** The narrowest view, in seconds. */
const MIN_VIEW = 0.005;

const fmt = (s: number) => {
  const ms = Math.round(Math.max(0, s) * 1000);
  return `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}.${String(ms % 1000).padStart(3, "0")}`;
};

/** The file to edit and the pad's region in it. */
type Source = { path: string; seconds: number; name: string; format: string; start: number; end: number };

/** What the editor works on for a pad: its origin if the engine host still
 * has it, else its own file. */
function useSource(pad: SamplerPad | undefined): Source | null {
  const [source, setSource] = useState<Source | null>(null);
  const key = pad ? `${pad.path}|${pad.start}|${pad.end}|${pad.origin?.path ?? ""}` : "";
  useEffect(() => {
    let live = true;
    setSource(null);
    if (!pad) return;
    void (async () => {
      const o = pad.origin;
      if (o) {
        const info = await client.call("sample.info", { path: o.path }).catch(() => null);
        // The copy is the origin's region at the same pace: the pad's part
        // of the copy is that part of the origin.
        if (info && live) return setSource({ ...info, start: o.start + pad.start, end: Math.min(o.end, o.start + pad.end) });
      }
      const info = await act(client.call("sample.info", { path: pad.path }));
      if (info && live) setSource({ ...info, start: pad.start, end: pad.end });
    })();
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return source;
}

type Drag = { kind: "new"; from: number } | { kind: "start" } | { kind: "end" };

export function SampleEditor({ id, pad: padNumber }: { id: string; pad: number }) {
  const pad = useApp((s) => s.snapshot?.samplers.find((x) => x.instrument === id)?.pads.find((p) => p.pad === padNumber));
  const source = useSource(pad);
  const [view, setView] = useState<[number, number]>([0, 1]);
  const [sel, setSel] = useState<[number, number]>([0, 0]);
  const [peaks, setPeaks] = useState<[number, number][]>([]);
  const [width, setWidth] = useState(800);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [playing, setPlaying] = useState<{ at: number; start: number; end: number } | null>(null);
  /** The last region the engine took to audition. */
  const [played, setPlayed] = useState<string | null>(null);
  const [now, setNow] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const H = 120;

  // A new source: show the pad's region with some room around it.
  useEffect(() => {
    if (!source) return;
    const len = source.end - source.start;
    setSel([source.start, source.end]);
    const pad = Math.max(len, 0.05);
    setView([Math.max(0, source.start - pad), Math.min(source.seconds, source.end + pad)]);
  }, [source]);

  // The canvas follows its box's width (the box is there once the source
  // is).
  const ready = source !== null;
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(Math.max(100, Math.floor(el.clientWidth))));
    ro.observe(el);
    return () => ro.disconnect();
  }, [ready]);

  // The waveform of the view, a bin a pixel, refetched as it moves: one
  // request at a time, the newest view next when it is done.
  const wanted = useRef<{ path: string; start: number; end: number; bins: number } | null>(null);
  const fetching = useRef(false);
  useEffect(() => {
    if (!source || view[1] <= view[0]) return;
    wanted.current = { path: source.path, start: view[0], end: view[1], bins: Math.min(4096, width) };
    if (fetching.current) return;
    fetching.current = true;
    void (async () => {
      while (wanted.current) {
        const q = wanted.current;
        wanted.current = null;
        const r = await client.call("sample.peaks", q).catch(() => null);
        if (r && !wanted.current) setPeaks(r.peaks as [number, number][]);
      }
      fetching.current = false;
    })();
  }, [source, view, width]);

  // The playhead while auditioning.
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const t = playing.start + (performance.now() - playing.at) / 1000;
      if (t >= playing.end) return setPlaying(null);
      setNow(t);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing]);

  const span = view[1] - view[0];
  const xOf = (t: number) => ((t - view[0]) / span) * width;
  const tOf = (x: number) => Math.min(source?.seconds ?? 0, Math.max(0, view[0] + (x / width) * span));

  // Draw.
  useEffect(() => {
    const c = canvas.current;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    c.width = width * dpr;
    c.height = H * dpr;
    const g = c.getContext("2d");
    if (!g) return;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, width, H);
    const [a, b] = [xOf(Math.min(sel[0], sel[1])), xOf(Math.max(sel[0], sel[1]))];
    g.fillStyle = "rgba(56, 189, 248, 0.15)";
    g.fillRect(a, 0, b - a, H);
    g.strokeStyle = "#f59e0b";
    g.beginPath();
    peaks.forEach(([lo, hi], i) => {
      const x = (i + 0.5) * (width / Math.max(1, peaks.length));
      g.moveTo(x, H / 2 - hi * (H / 2 - 2));
      g.lineTo(x, H / 2 - lo * (H / 2 - 2) + 1);
    });
    g.stroke();
    g.strokeStyle = "#38bdf8";
    g.lineWidth = 2;
    for (const x of [a, b]) {
      g.beginPath();
      g.moveTo(x, 0);
      g.lineTo(x, H);
      g.stroke();
    }
    g.lineWidth = 1;
    if (playing) {
      g.strokeStyle = "#f4f4f5";
      g.beginPath();
      g.moveTo(xOf(now), 0);
      g.lineTo(xOf(now), H);
      g.stroke();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [peaks, sel, view, width, playing, now]);

  const ticks = useMemo(() => {
    // About one label each 100 px, on a round step.
    const raw = (span / width) * 100;
    const step = [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300].find((s) => s >= raw) ?? 600;
    const out: number[] = [];
    for (let t = Math.ceil(view[0] / step) * step; t <= view[1]; t += step) out.push(t);
    return out;
  }, [view, span, width]);

  if (!pad) return null;
  if (!source) {
    return (
      <section className="p-3 rounded-lg bg-zinc-900/50 border border-zinc-800 text-xs text-zinc-500" data-testid={`sample-editor-${id}-${padNumber}`}>
        loading the waveform...
      </section>
    );
  }

  const [a, b] = [Math.min(sel[0], sel[1]), Math.max(sel[0], sel[1])];
  const len = b - a;
  const valid = len >= 0.002 && len <= MAX_REGION;
  const changed = Math.abs(a - source.start) > 0.0005 || Math.abs(b - source.end) > 0.0005;
  const at = (e: React.PointerEvent) => e.clientX - canvas.current!.getBoundingClientRect().left;
  const zoom = (factor: number, around: number) => {
    const s = Math.min(source.seconds, Math.max(MIN_VIEW, span * factor));
    const k = (around - view[0]) / span;
    let v0 = around - k * s;
    v0 = Math.min(Math.max(0, v0), Math.max(0, source.seconds - s));
    setView([v0, v0 + s]);
  };
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:opacity-30";
  const timeInput = (value: number, set: (v: number) => void, testId: string) => (
    <input
      className="w-20 px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200 tabular-nums"
      type="number"
      step={0.001}
      min={0}
      max={source.seconds}
      key={`${testId}-${value.toFixed(4)}`}
      defaultValue={value.toFixed(3)}
      data-testid={testId}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
      onBlur={(e) => {
        // A cleared field keeps the time it had.
        const v = e.target.value.trim() === "" ? NaN : Number(e.target.value);
        if (Number.isFinite(v)) set(Math.min(source.seconds, Math.max(0, v)));
        else e.target.value = value.toFixed(3);
      }}
    />
  );

  return (
    <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`sample-editor-${id}-${padNumber}`}>
      <div className="flex items-center gap-3 text-xs text-zinc-400">
        <span className="text-zinc-200">pad {padNumber}</span>
        <span className="truncate max-w-64" title={source.path}>
          {source.name}
        </span>
        <span className="text-zinc-500">
          {fmt(source.seconds)} - {source.format}
        </span>
        {pad.origin && source.path === pad.origin.path && <span className="text-zinc-500">(the file it was cut from)</span>}
      </div>
      <div ref={box} className="relative w-full select-none">
        <div className="relative h-4 text-[9px] text-zinc-500">
          {ticks.map((t) => (
            <span key={t} className="absolute" style={{ left: xOf(t) + 2 }}>
              {fmt(t)}
            </span>
          ))}
        </div>
        <canvas
          ref={canvas}
          className="block rounded bg-zinc-950 border border-zinc-800 cursor-crosshair touch-none"
          style={{ width, height: H }}
          data-testid={`sample-wave-${id}-${padNumber}`}
          data-view={`${view[0].toFixed(4)},${view[1].toFixed(4)}`}
          data-selection={`${a.toFixed(4)},${b.toFixed(4)}`}
          data-played={played ?? undefined}
          onPointerDown={(e) => {
            (e.target as Element).setPointerCapture(e.pointerId);
            const x = at(e);
            // Near an edge: move that edge; else a new selection.
            if (Math.abs(x - xOf(a)) < 6) setDrag({ kind: "start" });
            else if (Math.abs(x - xOf(b)) < 6) setDrag({ kind: "end" });
            else {
              const t = tOf(x);
              setDrag({ kind: "new", from: t });
              setSel([t, t]);
            }
          }}
          onPointerMove={(e) => {
            if (!drag) return;
            const t = tOf(at(e));
            if (drag.kind === "new") setSel([drag.from, t]);
            else if (drag.kind === "start") setSel([t, b]);
            else setSel([a, t]);
          }}
          onPointerUp={() => {
            setDrag(null);
            setSel(([x, y]) => [Math.min(x, y), Math.max(x, y)]);
          }}
          onWheel={(e) => {
            const t = tOf(e.clientX - canvas.current!.getBoundingClientRect().left);
            if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
              const d = ((e.shiftKey ? e.deltaY : e.deltaX) / width) * span;
              const v0 = Math.min(Math.max(0, view[0] + d), Math.max(0, source.seconds - span));
              setView([v0, v0 + span]);
            } else zoom(e.deltaY > 0 ? 1.25 : 0.8, t);
          }}
        />
      </div>
      <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-400">
        <span>from</span>
        {timeInput(a, (v) => setSel([v, b]), `sel-start-${id}-${padNumber}`)}
        <span>to</span>
        {timeInput(b, (v) => setSel([a, v]), `sel-end-${id}-${padNumber}`)}
        <span className={`tabular-nums ${valid ? "text-zinc-500" : "text-red-400"}`}>{len > MAX_REGION ? `${len.toFixed(1)} s: at most ${MAX_REGION} s` : `${(len * 1000).toFixed(0)} ms`}</span>
        <span className="ml-2" />
        <button className={btn} data-testid={`sample-zoom-in-${id}-${padNumber}`} onClick={() => zoom(0.5, (view[0] + view[1]) / 2)}>
          +
        </button>
        <button className={btn} data-testid={`sample-zoom-out-${id}-${padNumber}`} onClick={() => zoom(2, (view[0] + view[1]) / 2)}>
          -
        </button>
        <button className={btn} data-testid={`sample-fit-${id}-${padNumber}`} onClick={() => setView([0, source.seconds])}>
          all
        </button>
        <button
          className={btn}
          disabled={len <= 0}
          data-testid={`sample-zoom-sel-${id}-${padNumber}`}
          onClick={() => {
            const m = Math.max(len * 0.1, 0.002);
            setView([Math.max(0, a - m), Math.min(source.seconds, b + m)]);
          }}
        >
          selection
        </button>
        <span className="ml-2" />
        <button
          className={btn}
          disabled={!valid}
          data-testid={`sample-play-${id}-${padNumber}`}
          onClick={async () => {
            const r = await act(client.call("sample.play", { path: source.path, start: a, end: b }));
            if (r) {
              setPlaying({ at: performance.now(), start: r.start, end: r.end });
              setPlayed(`${r.start.toFixed(4)},${r.end.toFixed(4)}`);
            }
          }}
        >
          play
        </button>
        <button
          className={btn}
          data-testid={`sample-stop-${id}-${padNumber}`}
          onClick={() => {
            setPlaying(null);
            void act(client.call("sample.stop", {}));
          }}
        >
          stop
        </button>
        <button
          className={`${btn} ${changed && valid ? "border-amber-500 text-amber-200" : ""}`}
          disabled={!valid || !changed}
          title="the pad plays this region"
          data-testid={`sample-apply-${id}-${padNumber}`}
          onClick={() =>
            void act(client.call("sampler.load", { instrument: id, pad: padNumber, path: source.path, upload: null, start: a, end: b }))
          }
        >
          apply to pad
        </button>
      </div>
      <div className="text-[10px] text-zinc-600">drag: select - drag an edge: move it - wheel: zoom - shift-wheel: scroll</div>
    </section>
  );
}
