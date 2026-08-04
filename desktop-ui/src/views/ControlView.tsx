import { useEffect, useRef, useState } from "react";
import {
  clipDurationLabel,
  clipOut,
  clipRuntimeMs,
  formatTime,
  isTrimmed,
  mediaUrl,
  videoGeometryStyle,
} from "../format";
import { comboFromEvent, comboLabel } from "../hotkeys";
import {
  useDisplayPosition,
  useTicker,
  type Snapshot,
  type Store,
} from "../store";
import { isTauri } from "../transport";
import type {
  AppState,
  Aspect,
  Clip,
  Command,
  EndMode,
  HotkeyAction,
} from "../types";

type Send = (cmd: Command) => void;

interface ModalRequest {
  title: string;
  body: string;
  confirmLabel: string;
  onConfirm: () => void;
}

export function ControlView({ store }: { store: Store }) {
  const { snap, connected, send } = store;
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [modal, setModal] = useState<ModalRequest | null>(null);

  useTicker(!!snap && snap.state.transport.playing);
  const displayPos = useDisplayPosition(snap);

  // Files dropped anywhere on the control window join the playlist. Only the
  // native window can produce real paths; web remotes use the app to add.
  useEffect(() => {
    if (!isTauri) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    import("@tauri-apps/api/window")
      .then(({ appWindow }) =>
        appWindow.onFileDropEvent((e) => {
          if (e.payload.type === "drop" && e.payload.paths.length) {
            send({ type: "addClips", payload: { paths: e.payload.paths } });
          }
        })
      )
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [send]);

  // Configurable hotkeys (default Space = take, K = kill, P = pause…).
  // Ignored while typing in a field; a recording HotkeyRow swallows the
  // event in the capture phase before this bubble-phase handler runs.
  useEffect(() => {
    if (!snap) return;
    const hk = snap.state.hotkeys;
    const handler = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "SELECT" || t.tagName === "TEXTAREA")
        return;
      const combo = comboFromEvent(e);
      if (!combo) return;
      const cmd: Command | null =
        combo === hk.take
          ? { type: "take", payload: { id: null } }
          : combo === hk.playPause
          ? { type: "playPause" }
          : combo === hk.kill
          ? { type: "kill" }
          : combo === hk.restart
          ? { type: "restart" }
          : combo === hk.prevClip
          ? { type: "selectDelta", payload: { delta: -1 } }
          : combo === hk.nextClip
          ? { type: "selectDelta", payload: { delta: 1 } }
          : combo === hk.jumpBack
          ? { type: "jump", payload: { deltaMs: -10000 } }
          : combo === hk.jumpFwd
          ? { type: "jump", payload: { deltaMs: 10000 } }
          : combo === hk.markIn
          ? { type: "markIn" }
          : combo === hk.markOut
          ? { type: "markOut" }
          : combo === hk.launchOutput
          ? { type: "launchOutput" }
          : combo === hk.toggleFullscreen
          ? { type: "toggleFullscreen" }
          : null;
      if (cmd) {
        e.preventDefault();
        send(cmd);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [snap, send]);

  if (!snap) {
    return <div className="output-view output-waiting">Connecting…</div>;
  }
  const state = snap.state;

  const confirmClear = () =>
    setModal({
      title: "Clear Playlist",
      body: "This removes every clip from the playlist and cuts the output to black. The files themselves are not touched. Continue?",
      confirmLabel: "Clear Playlist",
      onConfirm: () => send({ type: "clearPlaylist" }),
    });

  return (
    <div className="control-root">
      <div className="columns">
        <div className="col">
          <div className="col-label">Playlist</div>
          <div className="panel">
            <PlaylistSection
              snap={snap}
              posMs={displayPos}
              send={send}
              onClear={confirmClear}
            />
          </div>
        </div>

        <div className="col">
          <div className="col-label">Output</div>
          <div className="panel">
            <div className="launch-row">
              <button
                className="btn darkred"
                title="Open the program output window; it opens on the selected external display when one is configured"
                onClick={() => send({ type: "launchOutput" })}
              >
                Launch Output
              </button>
              <button
                className="btn darkred"
                title="Toggle the output window between fullscreen and windowed. Does nothing if it isn't open"
                onClick={() => send({ type: "toggleFullscreen" })}
              >
                Toggle Fullscreen
              </button>
              <button
                className={"cog-btn" + (advancedOpen ? " selected" : "")}
                title="Open advanced settings: output display, hotkeys, network and remote access"
                onClick={() => setAdvancedOpen((v) => !v)}
              >
                ⚙
              </button>
            </div>
            {advancedOpen ? (
              <AdvancedSettings
                state={state}
                send={send}
                onClose={() => setAdvancedOpen(false)}
                setModal={setModal}
              />
            ) : (
              <>
                <NowPlayingSection snap={snap} posMs={displayPos} send={send} />
                <TransportSection snap={snap} send={send} />
                <EndModeSection state={state} send={send} />
                <GeometrySection state={state} send={send} />
                <VolumeSection state={state} send={send} />
              </>
            )}
          </div>
        </div>
      </div>

      <div className="footer">
        <div className="footer-actions">
          <button
            className="btn gray"
            title="Open the user guide in your browser — quick start, hotkeys, remote URLs"
            onClick={() => {
              if (isTauri) send({ type: "openHelp" });
              else window.open("/help", "_blank");
            }}
          >
            Help
          </button>
        </div>
        <div className="wordmark" aria-hidden>
          <span className="wordmark-mono">▶</span>PlayBack
        </div>
        <div className="server-info">
          Server IP: {state.network.ip}
          <br />
          Version: {state.version}
        </div>
      </div>

      {isTauri && <DurationProber state={state} send={send} />}

      {!connected && <div className="disconnected-banner">Reconnecting…</div>}

      {modal && (
        <div className="modal-backdrop" onClick={() => setModal(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>{modal.title}</h2>
            <p>{modal.body}</p>
            <div className="modal-actions">
              <button className="btn gray" onClick={() => setModal(null)}>
                Cancel
              </button>
              <button
                className="btn red"
                onClick={() => {
                  modal.onConfirm();
                  setModal(null);
                }}
              >
                {modal.confirmLabel}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function SectionBar({ children }: { children: React.ReactNode }) {
  return <div className="section-bar">{children}</div>;
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

/** Sum of the as-played (trimmed) runtimes of a set of clips. */
const totalMs = (clips: Clip[]) =>
  clips.reduce((sum, c) => sum + clipRuntimeMs(c), 0);

/**
 * Time left in the show if it plays through: the rest of the current clip
 * plus every clip after it (or from the cued clip when nothing is loaded).
 */
function remainingMs(snap: Snapshot, posMs: number): number {
  const { clips, transport, selected } = snap.state;
  const anchor = transport.current ?? selected;
  if (!anchor) return totalMs(clips);
  const i = clips.findIndex((c) => c.id === anchor);
  if (i < 0) return totalMs(clips);
  const rest = totalMs(clips.slice(i + 1));
  if (transport.current) {
    const out = clipOut(clips[i], transport.durationMs);
    const left = Math.max(0, out - posMs);
    return left + rest;
  }
  return clipRuntimeMs(clips[i]) + rest;
}

function PlaylistSection({
  snap,
  posMs,
  send,
  onClear,
}: {
  snap: Snapshot;
  posMs: number;
  send: Send;
  onClear: () => void;
}) {
  const state = snap.state;
  const { clips, selected, transport } = state;
  const selIndex = clips.findIndex((c) => c.id === selected);

  const move = (delta: number) => {
    if (selIndex < 0) return;
    const to = selIndex + delta;
    if (to < 0 || to >= clips.length) return;
    send({ type: "moveClip", payload: { id: clips[selIndex].id, to } });
  };

  return (
    <>
      <SectionBar>
        Playlist
        <span className="bar-note">
          {clips.length} {clips.length === 1 ? "clip" : "clips"} · total{" "}
          {formatTime(totalMs(clips))} · remaining{" "}
          {formatTime(remainingMs(snap, posMs))}
        </span>
      </SectionBar>

      <div className="clip-list">
        {clips.length === 0 && (
          <div className="clip-list-empty">
            Playlist is empty.
            <br />
            {isTauri
              ? "Drop video files anywhere on this window, or click “Add Files…”."
              : "Add files from the PlayBack app on the playback machine."}
          </div>
        )}
        {clips.map((clip, i) => {
          const isCurrent = transport.current === clip.id;
          const isSelected = selected === clip.id;
          return (
            <div
              key={clip.id}
              className={
                "clip-row" +
                (isSelected ? " selected" : "") +
                (isCurrent ? " playing" : "") +
                (clip.missing ? " missing" : "")
              }
              title={
                clip.missing
                  ? `File not found: ${clip.path}`
                  : `${clip.path}\nClick to cue · double-click to TAKE`
              }
              onClick={() => send({ type: "selectClip", payload: { id: clip.id } })}
              onDoubleClick={() => {
                if (!clip.missing) send({ type: "take", payload: { id: clip.id } });
              }}
            >
              <span className="clip-index">{i + 1}</span>
              <span className="clip-marker">
                {isCurrent ? (transport.playing ? "▶" : "❚❚") : isSelected ? "●" : ""}
              </span>
              <span className="clip-name">{clip.name}</span>
              <label
                className="clip-loop"
                title="Loop this clip when it ends (overrides the end mode)"
                onClick={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
              >
                <input
                  type="checkbox"
                  checked={clip.loop}
                  onChange={(e) =>
                    send({
                      type: "setClipLoop",
                      payload: { id: clip.id, enabled: e.target.checked },
                    })
                  }
                />
                ⟳
              </label>
              <span
                className="clip-duration"
                title={
                  isTrimmed(clip)
                    ? `Trimmed: in ${formatTime(clip.inMs)} · out ${
                        clip.outMs !== null ? formatTime(clip.outMs) : "end"
                      }`
                    : undefined
                }
              >
                {clipDurationLabel(clip)}
              </span>
            </div>
          );
        })}
      </div>

      <ClipLevelStrip state={state} send={send} />

      <div className="playlist-toolbar">
        <button
          className="btn blue"
          disabled={!isTauri}
          title={
            isTauri
              ? "Pick video files to append to the playlist"
              : "Files can only be added from the playback machine"
          }
          onClick={() => send({ type: "browseFiles" })}
        >
          Add Files…
        </button>
        <button
          className="btn gray"
          disabled={selIndex < 0}
          title="Remove the highlighted clip from the playlist"
          onClick={() => {
            if (selected) send({ type: "removeClip", payload: { id: selected } });
          }}
        >
          Remove
        </button>
        <button
          className="btn gray"
          disabled={selIndex <= 0}
          title="Move the highlighted clip up"
          onClick={() => move(-1)}
        >
          ▲
        </button>
        <button
          className="btn gray"
          disabled={selIndex < 0 || selIndex >= clips.length - 1}
          title="Move the highlighted clip down"
          onClick={() => move(1)}
        >
          ▼
        </button>
        <span className="toolbar-spacer" />
        <button
          className="btn darkred"
          disabled={clips.length === 0}
          title="Remove every clip from the playlist"
          onClick={onClear}
        >
          Clear
        </button>
      </div>
    </>
  );
}

/**
 * Audio level for the highlighted clip, multiplied with the master volume
 * on the output. Lives with the playlist so a clip can be trimmed to level
 * while another one plays.
 */
function ClipLevelStrip({ state, send }: { state: AppState; send: Send }) {
  const clip = state.selected
    ? state.clips.find((c) => c.id === state.selected) ?? null
    : null;
  if (!clip) return null;
  return (
    <div className="clip-level-strip">
      <span className="clip-level-label" title="Audio level for the highlighted clip">
        Clip audio · <strong>{clip.name}</strong>
      </span>
      <input
        type="range"
        min={0}
        max={1}
        step={0.02}
        value={clip.gain}
        title={`Audio level for “${clip.name}” (× master volume)`}
        onChange={(e) =>
          send({
            type: "setClipGain",
            payload: { id: clip.id, gain: Number(e.target.value) },
          })
        }
      />
      <span className="clip-level-value">{Math.round(clip.gain * 100)}%</span>
    </div>
  );
}

/**
 * Invisible probe that fills in playlist durations one clip at a time, by
 * loading each unprobed file's metadata from the media server. Runs only in
 * the native control window so web remotes don't pull media over the LAN.
 */
function DurationProber({ state, send }: { state: AppState; send: Send }) {
  const clip = state.clips.find((c) => c.durationMs === null && !c.missing);
  if (!clip) return null;
  const done = (durationMs: number) =>
    send({ type: "setClipDuration", payload: { id: clip.id, durationMs } });
  return (
    <video
      key={clip.id}
      style={{ display: "none" }}
      preload="metadata"
      muted
      src={mediaUrl(state, clip.id)}
      onLoadedMetadata={(e) => {
        const d = e.currentTarget.duration;
        done(Number.isFinite(d) ? d * 1000 : 0);
      }}
      onError={() => done(0)}
    />
  );
}

// ---------------------------------------------------------------------------
// Now playing — big timecodes + progress
// ---------------------------------------------------------------------------

function NowPlayingSection({
  snap,
  posMs,
  send,
}: {
  snap: Snapshot;
  posMs: number;
  send: Send;
}) {
  const state = snap.state;
  const t = state.transport;
  const clip = t.current
    ? state.clips.find((c) => c.id === t.current) ?? null
    : null;
  const pos = posMs;
  // All show timecodes are trim-aware: elapsed/remaining/duration describe
  // the in→out range, not the raw file.
  const inMs = clip?.inMs ?? 0;
  const out = clip ? clipOut(clip, t.durationMs) : 0;
  const durEff = Math.max(0, out - inMs);
  const elapsed = Math.min(Math.max(0, pos - inMs), durEff);
  const remaining = Math.min(Math.max(0, out - pos), durEff);
  const frac = t.durationMs > 0 ? Math.min(1, pos / t.durationMs) : 0;
  const ready = !!clip && out > 0;

  const status = !clip
    ? "BLACK"
    : t.playing
    ? "PLAYING"
    : pos <= inMs
    ? "CUED"
    : "PAUSED";

  // PlaybackPro-style urgency: amber inside 30s, flashing red inside 10s.
  const urgency =
    clip && t.playing && ready
      ? remaining <= 10000
        ? " flash-red"
        : remaining <= 30000
        ? " amber"
        : ""
      : "";

  return (
    <>
      <SectionBar>
        Now Playing
        <span className={"status-chip " + status.toLowerCase()}>{status}</span>
      </SectionBar>
      <ControlPreview snap={snap} posMs={posMs} />
      <div
        className="now-playing-name"
        title={clip ? clip.path : "Output is black — TAKE plays the cued clip"}
      >
        {clip ? clip.name : "—"}
      </div>
      <div className="timecodes">
        <div className="timecode-group">
          <span className="timecode-caption">Elapsed</span>
          <span className="timecode" title="Time since the clip's in point">
            {ready ? formatTime(elapsed) : "--:--"}
          </span>
        </div>
        <div className="timecode-group">
          <span className="timecode-caption">Remaining</span>
          <span
            className={"timecode timecode-remaining" + urgency}
            title="Time left until the clip's out point"
          >
            {ready ? formatTime(remaining) : "--:--"}
          </span>
        </div>
        <div className="timecode-group">
          <span className="timecode-caption">Duration</span>
          <span className="timecode" title="As-played length (in point to out point)">
            {ready ? formatTime(durEff) : "--:--"}
          </span>
        </div>
      </div>
      <div
        className={"progress" + (clip && t.durationMs > 0 ? "" : " disabled")}
        title="Click to jump to a point in the clip"
        onClick={(e) => {
          if (!clip || t.durationMs <= 0) return;
          const rect = e.currentTarget.getBoundingClientRect();
          const f = (e.clientX - rect.left) / rect.width;
          send({ type: "seek", payload: { ms: f * t.durationMs } });
        }}
      >
        <div className="progress-fill" style={{ width: `${frac * 100}%` }} />
        {clip && t.durationMs > 0 && inMs > 0 && (
          <div
            className="progress-mark in"
            title={`In point ${formatTime(inMs)}`}
            style={{ left: `${(inMs / t.durationMs) * 100}%` }}
          />
        )}
        {clip && t.durationMs > 0 && clip.outMs !== null && (
          <div
            className="progress-mark out"
            title={`Out point ${formatTime(clip.outMs)}`}
            style={{ left: `${Math.min(100, (clip.outMs / t.durationMs) * 100)}%` }}
          />
        )}
      </div>
    </>
  );
}

/**
 * Preview monitor: a muted local replica of the program output. It free-runs
 * on its own decode clock — constantly chasing the reported position caused
 * a visible seek-stutter — and re-aligns only on load/seek/pause changes or
 * when it has genuinely drifted (>1.5s, checked every 2s).
 */
function ControlPreview({ snap, posMs }: { snap: Snapshot; posMs: number }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const state = snap.state;
  const t = state.transport;
  const clip = t.current
    ? state.clips.find((c) => c.id === t.current) ?? null
    : null;
  const src = clip ? mediaUrl(state, clip.id, t.loadSeq) : null;
  const playing = t.playing && !!src;

  const posRef = useRef(0);
  posRef.current = posMs;

  // Hard alignment on load and commanded seeks.
  useEffect(() => {
    const v = videoRef.current;
    if (!v || !src) return;
    const sync = () => {
      v.currentTime = posRef.current / 1000;
    };
    if (v.readyState >= 1) sync();
    else v.addEventListener("loadedmetadata", sync, { once: true });
    return () => v.removeEventListener("loadedmetadata", sync);
  }, [src, t.seekSeq]);

  useEffect(() => {
    const v = videoRef.current;
    if (!v || !src) return;
    if (playing) {
      v.play().catch(() => {});
    } else {
      v.pause();
      // Land on the frame the output is holding (end of clip, pause point).
      if (v.readyState >= 1) v.currentTime = posRef.current / 1000;
    }
  }, [playing, src, t.seekSeq]);

  // Occasional drift correction; otherwise leave the element alone. Never
  // seek a paused element (e.g. autoplay still blocked) — that's pure thrash.
  useEffect(() => {
    if (!src || !playing) return;
    const id = setInterval(() => {
      const v = videoRef.current;
      if (!v) return;
      if (v.paused && !v.ended) v.play().catch(() => {});
      else if (Math.abs(v.currentTime * 1000 - posRef.current) > 1500) {
        v.currentTime = posRef.current / 1000;
      }
    }, 2000);
    return () => clearInterval(id);
  }, [src, playing]);

  return (
    <div className="control-preview" title="Muted preview of the program output">
      {src && (
        <video
          // React doesn't always reflect `muted` as an attribute; set it
          // imperatively too so autoplay heuristics see a muted element.
          ref={(v) => {
            videoRef.current = v;
            if (v) {
              v.muted = true;
              v.defaultMuted = true;
            }
          }}
          src={src}
          style={videoGeometryStyle(state.geometry)}
          muted
          playsInline
          preload="auto"
        />
      )}
      <span className="preview-badge">PREVIEW</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

function TransportSection({ snap, send }: { snap: Snapshot; send: Send }) {
  const state = snap.state;
  const t = state.transport;
  const cued = state.selected
    ? state.clips.find((c) => c.id === state.selected) ?? null
    : null;

  return (
    <>
      <div className="cued-row" title="The clip TAKE plays next">
        Cued:{" "}
        <strong>{cued ? cued.name : "— nothing —"}</strong>
      </div>
      <div className="transport-row">
        <button
          className="btn green btn-take"
          disabled={state.clips.length === 0}
          title="Play the cued clip on the output (Space). Opens the output window if it isn't open"
          onClick={() => send({ type: "take", payload: { id: null } })}
        >
          TAKE
        </button>
        <button
          className="btn blue btn-pause"
          disabled={!t.current}
          title="Pause or resume the current clip (P)"
          onClick={() => send({ type: "playPause" })}
        >
          {t.playing ? "Pause" : "Resume"}
        </button>
        <button
          className="btn red btn-kill"
          disabled={!t.current}
          title="Cut the output to black immediately (K)"
          onClick={() => send({ type: "kill" })}
        >
          KILL
        </button>
      </div>
      <div className="transport-row secondary">
        <button
          className="btn gray"
          disabled={!t.current}
          title="Jump back to the clip's in point (R)"
          onClick={() => send({ type: "restart" })}
        >
          ⟲ Restart
        </button>
        {[30, 20, 10].map((s) => (
          <button
            key={s}
            className="btn gray"
            disabled={!t.current}
            title={`Jump to the last ${s} seconds of the clip — rehearse the ending`}
            onClick={() =>
              send({ type: "jumpToEnd", payload: { offsetMs: s * 1000 } })
            }
          >
            Last {s}
          </button>
        ))}
      </div>
      <TrimRow snap={snap} send={send} />
    </>
  );
}

/**
 * In/out points for the loaded clip: play or scrub to the spot, hit Mark In
 * (I) / Mark Out (O). Timecodes, playlist totals and the end-of-clip switch
 * all follow the trim.
 */
function TrimRow({ snap, send }: { snap: Snapshot; send: Send }) {
  const t = snap.state.transport;
  const clip = t.current
    ? snap.state.clips.find((c) => c.id === t.current) ?? null
    : null;
  return (
    <div className="transport-row trim">
      <button
        className="btn gray"
        disabled={!clip}
        title="Set the clip's in point to the current position (I)"
        onClick={() => send({ type: "markIn" })}
      >
        Mark In
      </button>
      <button
        className="btn gray"
        disabled={!clip}
        title="Set the clip's out point to the current position (O)"
        onClick={() => send({ type: "markOut" })}
      >
        Mark Out
      </button>
      <button
        className="btn gray"
        disabled={!clip || !isTrimmed(clip)}
        title="Remove the clip's in and out points"
        onClick={() => send({ type: "clearTrim" })}
      >
        Clear
      </button>
      <span className="trim-readout" title="In and out points of the loaded clip">
        {clip
          ? `In ${formatTime(clip.inMs)} · Out ${
              clip.outMs !== null ? formatTime(clip.outMs) : "end"
            }`
          : "In —:—— · Out —:——"}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// End-of-clip behavior
// ---------------------------------------------------------------------------

const END_MODES: { mode: EndMode; label: string; title: string }[] = [
  { mode: "hold", label: "Hold", title: "Pause on the last frame of the clip" },
  { mode: "black", label: "Black", title: "Cut to black when the clip ends" },
  { mode: "next", label: "Play Next", title: "Play the next clip in the list immediately" },
  { mode: "cueNext", label: "Cue Next", title: "Load the next clip paused on its first frame" },
];

function EndModeSection({ state, send }: { state: AppState; send: Send }) {
  return (
    <>
      <SectionBar>
        When A Clip Ends
        <span className="bar-note">clips with ⟳ loop instead</span>
      </SectionBar>
      <div className="endmode-row">
        {END_MODES.map(({ mode, label, title }) => (
          <button
            key={mode}
            className={
              "btn blue endmode-btn" + (state.endMode === mode ? " selected" : "")
            }
            title={title}
            onClick={() => send({ type: "setEndMode", payload: { mode } })}
          >
            {label}
          </button>
        ))}
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// Output geometry
// ---------------------------------------------------------------------------

const ASPECTS: { aspect: Aspect; label: string; title: string }[] = [
  { aspect: "fit", label: "Fit", title: "Letterbox/pillarbox, preserving the file's aspect (default)" },
  { aspect: "fill", label: "Fill", title: "Fill the screen, cropping edges" },
  { aspect: "stretch", label: "Stretch", title: "Stretch to the screen's aspect" },
  { aspect: "r16x9", label: "16:9", title: "Force a 16:9 frame, fitted to the screen" },
  { aspect: "r4x3", label: "4:3", title: "Force a 4:3 frame, fitted to the screen" },
];

function GeometrySection({ state, send }: { state: AppState; send: Send }) {
  const g = state.geometry;
  return (
    <>
      <SectionBar>
        Output Geometry
        <span className="bar-note">applies to output and preview</span>
      </SectionBar>
      <div className="geometry-row">
        <select
          value={g.aspect}
          title="How video is mapped onto the output screen"
          onChange={(e) =>
            send({
              type: "setAspect",
              payload: { aspect: e.target.value as Aspect },
            })
          }
        >
          {ASPECTS.map((a) => (
            <option key={a.aspect} value={a.aspect} title={a.title}>
              {a.label}
            </option>
          ))}
        </select>
        <span className="geometry-caption">Scale</span>
        <input
          type="range"
          min={0.25}
          max={2}
          step={0.05}
          value={g.scale}
          title="Scale the video on the output screen"
          onChange={(e) =>
            send({
              type: "setOutputScale",
              payload: { scale: Number(e.target.value) },
            })
          }
        />
        <span className="geometry-value">{Math.round(g.scale * 100)}%</span>
        <button
          className="btn gray"
          disabled={g.scale === 1 && g.aspect === "fit"}
          title="Reset geometry to Fit at 100%"
          onClick={() => {
            send({ type: "setOutputScale", payload: { scale: 1 } });
            send({ type: "setAspect", payload: { aspect: "fit" } });
          }}
        >
          Reset
        </button>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

function VolumeSection({ state, send }: { state: AppState; send: Send }) {
  const t = state.transport;
  return (
    <div className="volume-row">
      <span className="volume-caption">Master Volume</span>
      <input
        type="range"
        min={0}
        max={1}
        step={0.02}
        value={t.volume}
        title="Program audio level on the output"
        onChange={(e) =>
          send({ type: "setVolume", payload: { volume: Number(e.target.value) } })
        }
      />
      <span className="volume-value">{Math.round(t.volume * 100)}%</span>
      <button
        className={"btn " + (t.muted ? "red" : "gray")}
        title={t.muted ? "Unmute the output audio" : "Mute the output audio"}
        onClick={() => send({ type: "setMuted", payload: { enabled: !t.muted } })}
      >
        {t.muted ? "Muted" : "Mute"}
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Advanced settings
// ---------------------------------------------------------------------------

const HOTKEY_ROWS: { action: HotkeyAction; label: string }[] = [
  { action: "take", label: "Take" },
  { action: "playPause", label: "Pause / Resume" },
  { action: "kill", label: "Kill to Black" },
  { action: "restart", label: "Restart Clip" },
  { action: "prevClip", label: "Cue Previous Clip" },
  { action: "nextClip", label: "Cue Next Clip" },
  { action: "jumpBack", label: "Jump Back 10s" },
  { action: "jumpFwd", label: "Jump Forward 10s" },
  { action: "markIn", label: "Mark In Point" },
  { action: "markOut", label: "Mark Out Point" },
  { action: "launchOutput", label: "Launch Output" },
  { action: "toggleFullscreen", label: "Toggle Fullscreen" },
];

function AdvancedSettings({
  state,
  send,
  onClose,
  setModal,
}: {
  state: AppState;
  send: Send;
  onClose: () => void;
  setModal: (m: ModalRequest) => void;
}) {
  const [monitorSel, setMonitorSel] = useState<number>(
    state.selectedMonitor ?? -1
  );
  const [ifaceSel, setIfaceSel] = useState<string>(state.network.iface ?? "");
  const [portText, setPortText] = useState<string>(String(state.network.port));

  // Monitors and network interfaces can change at any time; refresh on open.
  useEffect(() => {
    send({ type: "refreshSystem" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const openUrl = (url: string, cmd: Command) => {
    if (isTauri) send(cmd);
    else window.open(url, "_blank");
  };

  return (
    <div className="advanced">
      <div className="advanced-header">
        <h2>Advanced Settings</h2>
        <button
          className="btn darkred"
          title="Close the advanced settings panel"
          onClick={onClose}
        >
          CLOSE
        </button>
      </div>

      <SectionBar>Output Display Select</SectionBar>
      <div className="advanced-row">
        <span className="advanced-label">Select External Display</span>
        <select
          value={monitorSel}
          title="Display the output window should open on"
          onChange={(e) => setMonitorSel(Number(e.target.value))}
        >
          <option value={-1}>Automatic</option>
          {state.monitors.map((m, i) => (
            <option key={i} value={i}>
              {m.name} ({m.width}×{m.height})
            </option>
          ))}
        </select>
        <button
          className="btn blue"
          title="Confirm the selected external display"
          onClick={() =>
            send({
              type: "selectMonitor",
              payload: { index: monitorSel < 0 ? null : monitorSel },
            })
          }
        >
          Update
        </button>
      </div>
      <div className="advanced-row">
        <span className="advanced-label">Launch Fullscreen by Default</span>
        <input
          type="checkbox"
          className="advanced-check"
          checked={state.launchFullscreen}
          title="Open the output window fullscreen as soon as it launches"
          onChange={(e) =>
            send({
              type: "setLaunchFullscreen",
              payload: { enabled: e.target.checked },
            })
          }
        />
      </div>

      <SectionBar>Hotkeys</SectionBar>
      {HOTKEY_ROWS.map(({ action, label }) => (
        <HotkeyRow
          key={action}
          label={label}
          action={action}
          binding={state.hotkeys[action]}
          send={send}
        />
      ))}

      <SectionBar>Network Configuration</SectionBar>
      <div className="advanced-row">
        <span className="advanced-label">Current Server IP</span>
        <span className="advanced-value">{state.network.ip}</span>
      </div>
      <div className="advanced-row">
        <span className="advanced-label">Select Network Interface</span>
        <select
          value={ifaceSel}
          title="Network interface the web server listens on"
          onChange={(e) => setIfaceSel(e.target.value)}
        >
          <option value="">All interfaces</option>
          {state.network.interfaces.map((i) => (
            <option key={i.name} value={i.name}>
              {i.name} ({i.ip})
            </option>
          ))}
        </select>
      </div>
      <div className="advanced-row">
        <span className="advanced-label">Server Port (Default 3100)</span>
        <input
          className="port-input"
          inputMode="numeric"
          value={portText}
          title="Port used by the web server. Ports below 1024 require administrator rights"
          onChange={(e) =>
            setPortText(e.target.value.replace(/\D/g, "").slice(0, 5))
          }
        />
        <button
          className="btn blue"
          title="Apply the network interface and port; the web server restarts"
          onClick={() =>
            send({
              type: "applyNetwork",
              payload: {
                iface: ifaceSel || null,
                port: Math.min(65535, Number(portText) || 0),
              },
            })
          }
        >
          Update
        </button>
      </div>
      {state.network.error && (
        <div className="network-error">{state.network.error}</div>
      )}

      <SectionBar>Remote Access</SectionBar>
      <div className="advanced-row remote-buttons">
        <button
          className="btn blue"
          title="Open the control surface in the default web browser"
          onClick={() =>
            openUrl(state.network.controlUrl, { type: "openWebControl" })
          }
        >
          Open Web Control
        </button>
        <button
          className="btn blue"
          title="Open the program output in the default web browser (confidence monitor)"
          onClick={() =>
            openUrl(state.network.outputUrl, { type: "openWebOutput" })
          }
        >
          Open Web Output
        </button>
      </div>
      <div className="advanced-row">
        <span className="advanced-label">Control URL</span>
        <span className="advanced-value">{state.network.controlUrl}</span>
      </div>
      <div className="advanced-row">
        <span className="advanced-label">Output URL</span>
        <span className="advanced-value">{state.network.outputUrl}</span>
      </div>

      <SectionBar>Other Settings</SectionBar>
      <div className="advanced-row">
        <button
          className="btn blue"
          title="Remove every clip from the playlist"
          onClick={() =>
            setModal({
              title: "Clear Playlist",
              body: "This removes every clip from the playlist and cuts the output to black. Continue?",
              confirmLabel: "Clear Playlist",
              onConfirm: () => send({ type: "clearPlaylist" }),
            })
          }
        >
          Clear Playlist
        </button>
      </div>
    </div>
  );
}

/**
 * One configurable hotkey. Click the binding button, press the new combo
 * (Escape cancels). While recording, the capture-phase listener swallows the
 * event so the global hotkey handler and focused controls never see it.
 */
function HotkeyRow({
  label,
  action,
  binding,
  send,
}: {
  label: string;
  action: HotkeyAction;
  binding: string | null;
  send: Send;
}) {
  const [recording, setRecording] = useState(false);

  useEffect(() => {
    if (!recording) return;
    const handler = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(false);
        return;
      }
      const combo = comboFromEvent(e);
      if (!combo) return; // bare modifier — keep waiting for the full combo
      send({ type: "setHotkey", payload: { action, binding: combo } });
      setRecording(false);
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [recording, action, send]);

  return (
    <div className="advanced-row">
      <span className="advanced-label">{label}</span>
      <button
        className={"btn blue hotkey-btn" + (recording ? " recording" : "")}
        title={`Click, then press the key combination for “${label}”. Escape cancels`}
        onClick={() => setRecording((v) => !v)}
        onBlur={() => setRecording(false)}
      >
        {recording ? "Press a key…" : binding ? comboLabel(binding) : "None"}
      </button>
      <button
        className="btn gray"
        disabled={!binding}
        title={`Remove the “${label}” hotkey`}
        onClick={() =>
          send({ type: "setHotkey", payload: { action, binding: null } })
        }
      >
        Clear
      </button>
    </div>
  );
}
