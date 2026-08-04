import { isTauri } from "./transport";
import type { AppState, Clip } from "./types";

const pad = (n: number) => String(n).padStart(2, "0");

/**
 * Format a millisecond duration as a timecode: "mm:ss" under an hour,
 * "h:mm:ss" above. Used for playlist durations and the big readouts.
 */
export function formatTime(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

/** Duration cell for a clip: probing and unreadable states show placeholders. */
export function clipDurationLabel(clip: Clip): string {
  if (clip.missing) return "missing";
  if (clip.durationMs === null) return "…";
  if (clip.durationMs <= 0) return "—";
  return formatTime(clip.durationMs);
}

/**
 * Base URL for streaming a clip. Native windows go straight to the built-in
 * server; web clients use a relative URL on whatever host served them.
 */
export function mediaUrl(state: AppState, id: string, loadSeq?: number): string {
  const base = isTauri
    ? `http://${state.network.ip}:${state.network.port}`
    : "";
  const bust = loadSeq !== undefined ? `?v=${loadSeq}` : "";
  return `${base}/media/${id}${bust}`;
}
