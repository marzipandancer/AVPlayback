// Mirrors the serde model in src-tauri/src/state.rs.

export interface Clip {
  id: string;
  path: string;
  name: string;
  /** null = not yet probed; 0 = unreadable file. */
  durationMs: number | null;
  missing: boolean;
  /** Loop this clip when it ends, overriding the global end mode. */
  loop: boolean;
  /** Trim: playback starts here (Mark In). */
  inMs: number;
  /** Trim: playback ends here (Mark Out); null = play to the file's end. */
  outMs: number | null;
  /** Per-clip audio level, multiplied with the master volume. */
  gain: number;
}

/**
 * What the output does when the playing clip reaches its end (a clip with
 * its Loop checkbox set loops instead, regardless of this mode).
 */
export type EndMode = "hold" | "black" | "next" | "cueNext";

/** How the output maps video onto the screen. */
export type Aspect = "fit" | "fill" | "stretch" | "r16x9" | "r4x3";

export interface Geometry {
  scale: number;
  aspect: Aspect;
}

export interface TransportState {
  /** Clip loaded in the output, or null = black. */
  current: string | null;
  playing: boolean;
  positionMs: number;
  durationMs: number;
  /** Bumped whenever the output must (re)load media. */
  loadSeq: number;
  /** Bumped to command a seek to seekMs. */
  seekSeq: number;
  seekMs: number;
  volume: number;
  muted: boolean;
}

export interface Iface {
  name: string;
  ip: string;
}

export interface Network {
  port: number;
  iface: string | null;
  ip: string;
  interfaces: Iface[];
  controlUrl: string;
  outputUrl: string;
  error: string | null;
}

export interface MonitorInfo {
  name: string;
  width: number;
  height: number;
  x: number;
  y: number;
  scale: number;
}

/** Combo strings from KeyboardEvent.code with modifier prefixes (see hotkeys.ts). */
export interface Hotkeys {
  take: string | null;
  playPause: string | null;
  kill: string | null;
  restart: string | null;
  prevClip: string | null;
  nextClip: string | null;
  jumpBack: string | null;
  jumpFwd: string | null;
  markIn: string | null;
  markOut: string | null;
  launchOutput: string | null;
  toggleFullscreen: string | null;
}

export type HotkeyAction = keyof Hotkeys;

export interface AppState {
  version: string;
  clips: Clip[];
  /** The cued clip: what TAKE plays next. Independent of transport.current. */
  selected: string | null;
  endMode: EndMode;
  geometry: Geometry;
  transport: TransportState;
  hotkeys: Hotkeys;
  /** Open the output window fullscreen as soon as it launches. */
  launchFullscreen: boolean;
  network: Network;
  monitors: MonitorInfo[];
  selectedMonitor: number | null;
}

export type Command =
  | { type: "addClips"; payload: { paths: string[] } }
  | { type: "browseFiles" }
  | { type: "removeClip"; payload: { id: string } }
  | { type: "moveClip"; payload: { id: string; to: number } }
  | { type: "clearPlaylist" }
  | { type: "selectClip"; payload: { id: string } }
  | { type: "selectDelta"; payload: { delta: number } }
  | { type: "setClipDuration"; payload: { id: string; durationMs: number } }
  | { type: "setClipLoop"; payload: { id: string; enabled: boolean } }
  | { type: "setClipGain"; payload: { id: string; gain: number } }
  | { type: "take"; payload: { id: string | null } }
  | { type: "playPause" }
  | { type: "kill" }
  | { type: "restart" }
  | { type: "jump"; payload: { deltaMs: number } }
  | { type: "jumpToEnd"; payload: { offsetMs: number } }
  | { type: "seek"; payload: { ms: number } }
  | { type: "markIn" }
  | { type: "markOut" }
  | { type: "clearTrim" }
  | { type: "setEndMode"; payload: { mode: EndMode } }
  | { type: "setAspect"; payload: { aspect: Aspect } }
  | { type: "setOutputScale"; payload: { scale: number } }
  | { type: "setVolume"; payload: { volume: number } }
  | { type: "setMuted"; payload: { enabled: boolean } }
  | {
      type: "mediaStatus";
      payload: {
        loadSeq: number;
        positionMs: number;
        durationMs: number;
        ended: boolean;
      };
    }
  | {
      type: "setHotkey";
      payload: { action: HotkeyAction; binding: string | null };
    }
  | { type: "setLaunchFullscreen"; payload: { enabled: boolean } }
  | { type: "launchOutput" }
  | { type: "toggleFullscreen" }
  | { type: "selectMonitor"; payload: { index: number | null } }
  | { type: "applyNetwork"; payload: { iface: string | null; port: number } }
  | { type: "refreshSystem" }
  | { type: "openWebControl" }
  | { type: "openWebOutput" }
  | { type: "openHelp" };
