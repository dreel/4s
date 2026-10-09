import type { Voice } from "../generated/Voice";

const LABELS: Record<Voice, string> = {
  kick: "Kick",
  snare: "Snare",
  clap: "Clap",
  closed_hat: "Closed Hat",
  open_hat: "Open Hat",
  low_tom: "Low Tom",
  high_tom: "High Tom",
  cowbell: "Cowbell",
};

const SHORT: Record<Voice, string> = {
  kick: "BD",
  snare: "SD",
  clap: "CP",
  closed_hat: "CH",
  open_hat: "OH",
  low_tom: "LT",
  high_tom: "HT",
  cowbell: "CB",
};

export const voiceLabel = (v: Voice) => LABELS[v];
export const voiceShort = (v: Voice) => SHORT[v];

/** GM drum notes the 808's voices play (the protocol's `Voice::gm_note`). */
export const GM_NOTES = [36, 38, 39, 42, 46, 45, 50, 56];
