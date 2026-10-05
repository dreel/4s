// Reusable controls: rotary knob, vertical fader, level meter, toggle button.

import { useRef } from "react";

function clamp(v: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, v));
}

/** Vertical-drag value control. Shift for fine adjustment, double-click resets. */
function useDrag(value: number, min: number, max: number, onChange: (v: number) => void, pixels = 160) {
  const start = useRef<{ y: number; v: number } | null>(null);
  return {
    onPointerDown: (e: React.PointerEvent) => {
      (e.target as Element).setPointerCapture(e.pointerId);
      start.current = { y: e.clientY, v: value };
    },
    onPointerMove: (e: React.PointerEvent) => {
      if (!start.current) return;
      const scale = (max - min) / (e.shiftKey ? pixels * 5 : pixels);
      const next = clamp(start.current.v + (start.current.y - e.clientY) * scale, min, max);
      if (next !== value) onChange(next);
    },
    onPointerUp: () => {
      start.current = null;
    },
  };
}

export function Knob(props: {
  label: string;
  value: number;
  min: number;
  max: number;
  defaultValue: number;
  format?: (v: number) => string;
  onChange: (v: number) => void;
  testId?: string;
  size?: number;
}) {
  const { value, min, max, size = 34 } = props;
  const drag = useDrag(value, min, max, props.onChange);
  const t = (clamp(value, min, max) - min) / (max - min || 1);
  const angle = -135 + t * 270;
  const r = size / 2 - 3;
  const arc = (a: number) => {
    const rad = ((a - 90) * Math.PI) / 180;
    return [size / 2 + r * Math.cos(rad), size / 2 + r * Math.sin(rad)];
  };
  const [sx, sy] = arc(-135);
  const [ex, ey] = arc(angle);
  const large = angle + 135 > 180 ? 1 : 0;
  const [px, py] = arc(angle);
  return (
    <div
      className="flex flex-col items-center gap-0.5 cursor-ns-resize touch-none"
      data-testid={props.testId}
      data-value={value}
      title={`${props.label}: ${props.format ? props.format(value) : value.toFixed(2)}`}
      onDoubleClick={() => props.onChange(props.defaultValue)}
      {...drag}
    >
      <svg width={size} height={size}>
        <circle cx={size / 2} cy={size / 2} r={r} className="fill-zinc-900 stroke-zinc-700" strokeWidth={2} />
        {t > 0.001 && (
          <path
            d={`M ${sx} ${sy} A ${r} ${r} 0 ${large} 1 ${ex} ${ey}`}
            className="stroke-amber-400 fill-none"
            strokeWidth={3}
            strokeLinecap="round"
          />
        )}
        <line x1={size / 2} y1={size / 2} x2={px} y2={py} className="stroke-zinc-200" strokeWidth={2} strokeLinecap="round" />
      </svg>
      <div className="text-[10px] leading-none text-zinc-400">{props.label}</div>
      <div className="text-[10px] leading-none text-zinc-300 tabular-nums">
        {props.format ? props.format(value) : value.toFixed(2)}
      </div>
    </div>
  );
}

export function Fader(props: {
  value: number;
  onChange: (v: number) => void;
  testId?: string;
  height?: number;
  /** Double-click resets to this (default 0.8). */
  defaultValue?: number;
}) {
  const h = props.height ?? 110;
  const drag = useDrag(props.value, 0, 1, props.onChange, h);
  return (
    <div
      className="relative w-7 rounded bg-zinc-900 border border-zinc-700 cursor-ns-resize touch-none"
      style={{ height: h }}
      data-testid={props.testId}
      data-value={props.value}
      title={`${Math.round(props.value * 100)}%`}
      onDoubleClick={() => props.onChange(props.defaultValue ?? 0.8)}
      {...drag}
    >
      <div className="absolute bottom-0 left-0 right-0 rounded-b bg-amber-500/30" style={{ height: `${props.value * 100}%` }} />
      <div className="absolute left-0 right-0 h-1.5 -mb-0.75 bg-zinc-100 rounded" style={{ bottom: `calc(${props.value * 100}% - 3px)` }} />
    </div>
  );
}

/** Peak meter; `level` is linear 0..1 (shown on a -48..0 dB scale). */
export function Meter(props: { level: number; height?: number; horizontal?: boolean }) {
  const db = 20 * Math.log10(Math.max(props.level, 1e-6));
  const t = clamp((db + 48) / 48, 0, 1);
  const color = db > -1 ? "bg-red-500" : db > -9 ? "bg-amber-400" : "bg-emerald-500";
  if (props.horizontal) {
    return (
      <div className="h-1.5 w-full rounded bg-zinc-900 overflow-hidden">
        <div className={`h-full ${color} transition-[width] duration-75`} style={{ width: `${t * 100}%` }} />
      </div>
    );
  }
  return (
    <div className="relative w-1.5 rounded bg-zinc-900 overflow-hidden" style={{ height: props.height ?? 110 }}>
      <div className={`absolute bottom-0 w-full ${color} transition-[height] duration-75`} style={{ height: `${t * 100}%` }} />
    </div>
  );
}

export function Toggle(props: {
  on: boolean;
  label: string;
  onClick: () => void;
  activeClass?: string;
  testId?: string;
  title?: string;
}) {
  return (
    <button
      data-testid={props.testId}
      data-on={props.on}
      title={props.title}
      onClick={props.onClick}
      className={`h-5 min-w-5 px-1 rounded text-[10px] font-semibold border ${
        props.on ? (props.activeClass ?? "bg-amber-500 text-zinc-950 border-amber-400") : "bg-zinc-900 text-zinc-400 border-zinc-700 hover:border-zinc-500"
      }`}
    >
      {props.label}
    </button>
  );
}
