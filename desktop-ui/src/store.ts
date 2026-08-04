import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createTransport } from "./transport";
import type { AppState, Command } from "./types";

export interface Snapshot {
  state: AppState;
  /** performance.now() when this state arrived; used to extrapolate playback. */
  at: number;
}

export interface Store {
  snap: Snapshot | null;
  connected: boolean;
  send: (cmd: Command) => void;
}

export function useStore(): Store {
  const transportRef = useRef(createTransport());
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [connected, setConnected] = useState(false);

  useEffect(() => {
    const t = transportRef.current;
    return t.start(
      (state) => setSnap({ state, at: performance.now() }),
      setConnected
    );
  }, []);

  const send = useCallback((cmd: Command) => {
    transportRef.current.send(cmd);
  }, []);

  return useMemo(() => ({ snap, connected, send }), [snap, connected, send]);
}

/**
 * Current playback position in ms, extrapolated locally from the last
 * snapshot. The output's periodic MediaStatus reports rebase it, so drift
 * against the real media clock stays within a report interval.
 */
export function positionNow(snap: Snapshot): number {
  const t = snap.state.transport;
  if (!t.playing) return t.positionMs;
  const pos = t.positionMs + (performance.now() - snap.at);
  return t.durationMs > 0 ? Math.min(pos, t.durationMs) : pos;
}

/**
 * Position for on-screen readouts. Raw extrapolation re-bases on every
 * progress report, and each report carries its own transport latency — the
 * jitter makes a 60fps countdown flicker across second boundaries. This
 * clock slews instead: it runs at 0.5×–1.5× media rate until re-aligned
 * with the extrapolated target, so it never visibly jumps or runs
 * backward, and hard-snaps only on load/seek/pause or real desync.
 */
export function useDisplayPosition(snap: Snapshot | null): number {
  const ref = useRef<{
    pos: number;
    at: number;
    loadSeq: number;
    seekSeq: number;
  } | null>(null);
  if (!snap) {
    ref.current = null;
    return 0;
  }
  const t = snap.state.transport;
  const target = positionNow(snap);
  const now = performance.now();
  const prev = ref.current;
  let pos: number;
  if (
    !prev ||
    prev.loadSeq !== t.loadSeq ||
    prev.seekSeq !== t.seekSeq ||
    !t.playing
  ) {
    pos = target;
  } else {
    const dt = Math.max(0, now - prev.at);
    const err = target - (prev.pos + dt);
    if (Math.abs(err) > 700) {
      pos = target;
    } else {
      const rate = err > 40 ? 1.5 : err < -40 ? 0.5 : 1;
      pos = prev.pos + dt * rate;
    }
  }
  if (t.durationMs > 0) pos = Math.min(pos, t.durationMs);
  pos = Math.max(0, pos);
  ref.current = { pos, at: now, loadSeq: t.loadSeq, seekSeq: t.seekSeq };
  return pos;
}

/**
 * Re-render on animation frames while media is playing so the timecodes are
 * smooth without any network traffic.
 */
export function useTicker(active: boolean) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    let raf = 0;
    const loop = () => {
      setTick((t) => t + 1);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, [active]);
}
