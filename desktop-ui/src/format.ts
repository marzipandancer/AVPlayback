import type { CSSProperties } from "react";
import { isTauri } from "./transport";
import type { AppState, Clip, Geometry } from "./types";

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

export const isTrimmed = (clip: Clip) => clip.inMs > 0 || clip.outMs !== null;

/**
 * Effective end of a clip: its out point, else the live file duration from
 * the transport, else the probed duration.
 */
export function clipOut(clip: Clip, fileDurationMs: number): number {
  if (clip.outMs !== null) return clip.outMs;
  if (fileDurationMs > 0) return fileDurationMs;
  return clip.durationMs ?? 0;
}

/** Runtime of a clip as the show will play it (trim applied). */
export function clipRuntimeMs(clip: Clip): number {
  if (clip.missing || !clip.durationMs) return 0;
  return Math.max(0, (clip.outMs ?? clip.durationMs) - clip.inMs);
}

/** Duration cell for a clip: probing and unreadable states show placeholders. */
export function clipDurationLabel(clip: Clip): string {
  if (clip.missing) return "missing";
  if (clip.durationMs === null) return "…";
  if (clip.durationMs <= 0) return "—";
  return (isTrimmed(clip) ? "✂ " : "") + formatTime(clipRuntimeMs(clip));
}

/**
 * Style mapping the video onto its container for the selected geometry.
 * Sizes use container-query units, so the same style works on the output
 * window and the control preview (both containers set container-type).
 */
export function videoGeometryStyle(g: Geometry): CSSProperties {
  const base: CSSProperties =
    g.scale !== 1 ? { transform: `scale(${g.scale})` } : {};
  switch (g.aspect) {
    case "fill":
      return { ...base, width: "100%", height: "100%", objectFit: "cover" };
    case "stretch":
      return { ...base, width: "100%", height: "100%", objectFit: "fill" };
    case "r16x9":
      return {
        ...base,
        width: "min(100cqw, calc(100cqh * 16 / 9))",
        aspectRatio: "16 / 9",
        height: "auto",
        objectFit: "fill",
        margin: "auto",
      };
    case "r4x3":
      return {
        ...base,
        width: "min(100cqw, calc(100cqh * 4 / 3))",
        aspectRatio: "4 / 3",
        height: "auto",
        objectFit: "fill",
        margin: "auto",
      };
    case "fit":
    default:
      return { ...base, width: "100%", height: "100%", objectFit: "contain" };
  }
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
