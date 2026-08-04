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
