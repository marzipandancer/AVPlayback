import { invoke } from "@tauri-apps/api/tauri";
import { listen } from "@tauri-apps/api/event";
import type { AppState, Command } from "./types";

export type ViewKind = "control" | "output";

export const isTauri = "__TAURI_IPC__" in window;

export function detectView(): ViewKind {
  if (isTauri) {
    // The output window is created with the label "output".
    // @tauri-apps/api declares this global; it is set in every Tauri webview.
    return window.__TAURI_METADATA__?.__currentWindow?.label === "output"
      ? "output"
      : "control";
  }
  return window.location.pathname.toLowerCase().startsWith("/output")
    ? "output"
    : "control";
}

export interface Transport {
  /** Subscribe to state updates. Returns an unsubscribe function. */
  start(
    onState: (s: AppState) => void,
    onConnected: (ok: boolean) => void
  ): () => void;
  send(cmd: Command): void;
}

class TauriTransport implements Transport {
  start(onState: (s: AppState) => void, onConnected: (ok: boolean) => void) {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    listen<AppState>("state", (e) => onState(e.payload)).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });
    invoke<AppState>("get_state").then((s) => {
      onState(s);
      onConnected(true);
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  send(cmd: Command) {
    // The arg must not be named `cmd`: invoke() spreads args into the IPC
    // envelope, whose own `cmd` field is the command name (Tauri v1).
    invoke("dispatch", { command: cmd }).catch((e) =>
      console.error("dispatch", e)
    );
  }
}

class WsTransport implements Transport {
  private ws: WebSocket | null = null;
  private stopped = false;

  start(onState: (s: AppState) => void, onConnected: (ok: boolean) => void) {
    // start() may run again after a cleanup (e.g. React StrictMode).
    this.stopped = false;
    const connect = () => {
      if (this.stopped) return;
      const proto = window.location.protocol === "https:" ? "wss" : "ws";
      const ws = new WebSocket(`${proto}://${window.location.host}/ws`);
      this.ws = ws;
      ws.onopen = () => onConnected(true);
      ws.onmessage = (e) => {
        try {
          onState(JSON.parse(e.data as string));
        } catch (err) {
          console.error("bad state payload", err);
        }
      };
      ws.onclose = () => {
        onConnected(false);
        if (!this.stopped) setTimeout(connect, 1500);
      };
      ws.onerror = () => ws.close();
    };
    connect();
    return () => {
      this.stopped = true;
      this.ws?.close();
    };
  }

  send(cmd: Command) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(cmd));
    }
  }
}

export function createTransport(): Transport {
  return isTauri ? new TauriTransport() : new WsTransport();
}
