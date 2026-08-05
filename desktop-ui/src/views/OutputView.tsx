import { useEffect, useRef, useState } from "react";
import { mediaUrl, videoGeometryStyle } from "../format";
import { type Store } from "../store";
import { isTauri } from "../transport";

/**
 * Full-screen program output: the native output window and the /output web
 * page both render this. The <video> element here is the media clock — it
 * obeys transport intent from state (load, seek, play/pause) and reports
 * position/duration back with mediaStatus commands. It never reports play
 * state: intent belongs to the operator, and a buffering or still-loading
 * element must not cancel it.
 */
export function OutputView({ store }: { store: Store }) {
  const { snap, connected, send } = store;
  const videoRef = useRef<HTMLVideoElement>(null);
  const [needsGesture, setNeedsGesture] = useState(false);
  const [mediaError, setMediaError] = useState(false);

  const t = snap?.state.transport ?? null;
  const clip =
    snap && t?.current
      ? snap.state.clips.find((c) => c.id === t.current) ?? null
      : null;
  // loadSeq in the URL makes re-taking the same clip a fresh load.
  const src = snap && t && clip ? mediaUrl(snap.state, clip.id, t.loadSeq) : null;
  const playing = (t?.playing ?? false) && !!src;
  const seekSeq = t?.seekSeq ?? 0;
  const inMs = clip?.inMs ?? 0;
  const outMs = clip && t ? clip.outMs ?? 0 : 0; // 0 = no out point enforced

  // Report the element's real position/duration back to the state core. The
  // loadSeq tag lets the reducer drop reports that raced a clip change.
  const loadSeqRef = useRef(0);
  loadSeqRef.current = t?.loadSeq ?? 0;
  const lastReport = useRef(0);
  const report = (ended: boolean) => {
    const v = videoRef.current;
    if (!v || !src) return;
    lastReport.current = performance.now();
    send({
      type: "mediaStatus",
      payload: {
        loadSeq: loadSeqRef.current,
        positionMs: v.currentTime * 1000,
        durationMs: Number.isFinite(v.duration) ? v.duration * 1000 : 0,
        ended,
        native: isTauri,
      },
    });
  };

  // A new load invalidates seeks commanded against the previous one.
  const appliedSeek = useRef(seekSeq);
  useEffect(() => {
    appliedSeek.current = seekSeq;
    setMediaError(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src]);

  // Commanded seeks (restart, rehearsal jumps, progress-bar clicks, loop).
  useEffect(() => {
    const v = videoRef.current;
    if (!v || !t || !src) return;
    if (seekSeq !== appliedSeek.current) {
      appliedSeek.current = seekSeq;
      v.currentTime = t.seekMs / 1000;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seekSeq, src]);

  // Play/pause intent. seekSeq is a dependency so a loop restart (seek to
  // the in point while "playing") starts the ended element again.
  useEffect(() => {
    const v = videoRef.current;
    if (!v || !src) return;
    if (playing) {
      v.play()
        .then(() => setNeedsGesture(false))
        .catch(() => setNeedsGesture(true));
    } else {
      v.pause();
    }
  }, [playing, src, seekSeq]);

  // Master volume × per-clip gain.
  const gain = clip?.gain ?? 1;
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.volume = (t?.volume ?? 1) * gain;
    v.muted = t?.muted ?? false;
  }, [t?.volume, t?.muted, gain, src]);

  // Self-healing backup: if intent is "playing" but the element fell paused
  // (autoplay unblock, buffering), poke it. Progress itself is reported from
  // the timeupdate handler — media-engine events keep firing when macOS naps
  // an occluded window's timers, so this interval must never be the only
  // source of position reports.
  useEffect(() => {
    if (!src || !playing) return;
    const id = setInterval(() => {
      const v = videoRef.current;
      if (v && v.paused && !v.ended) v.play().catch(() => {});
      if (performance.now() - lastReport.current >= 900) report(false);
    }, 1000);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src, playing]);

  if (!snap) {
    return <div className="output-view output-waiting">Connecting…</div>;
  }

  return (
    <div
      className="output-view"
      title={isTauri ? "Double-click to toggle fullscreen" : undefined}
      onDoubleClick={() => {
        if (isTauri) send({ type: "toggleFullscreen" });
      }}
    >
      {src && (
        <video
          ref={videoRef}
          className="output-video"
          style={videoGeometryStyle(snap.state.geometry)}
          src={src}
          preload="auto"
          playsInline
          onLoadedMetadata={() => {
            // Fresh loads start at the clip's in point.
            const v = videoRef.current;
            if (v && inMs > 0 && v.currentTime * 1000 < inMs) {
              v.currentTime = inMs / 1000;
            }
            report(false);
          }}
          onTimeUpdate={() => {
            // Out-point enforcement: reaching it counts as the clip ending.
            const v = videoRef.current;
            if (v && !v.paused && outMs > 0 && v.currentTime * 1000 >= outMs) {
              v.pause();
              report(true);
              return;
            }
            // Regular progress reports ride the media engine's own events.
            if (performance.now() - lastReport.current >= 450) report(false);
          }}
          onPlay={() => report(false)}
          onPause={() => {
            const v = videoRef.current;
            if (v && !v.ended) report(false);
          }}
          onSeeked={() => report(false)}
          onEnded={() => report(true)}
          onError={() => {
            setMediaError(true);
            // Tell the control window why nothing is rolling.
            const code = videoRef.current?.error?.code;
            const msg =
              code === 4
                ? "format not supported by this machine's media engine"
                : code === 3
                ? "decode failed — corrupt file or unsupported codec"
                : code === 2
                ? "file could not be read"
                : "playback error";
            send({
              type: "mediaStatus",
              payload: {
                loadSeq: loadSeqRef.current,
                positionMs: 0,
                durationMs: 0,
                ended: false,
                native: isTauri,
                error: msg,
              },
            });
          }}
        />
      )}
      {mediaError && clip && (
        <div className="output-error">
          Cannot play “{clip.name}” — unsupported format or unreadable file
        </div>
      )}
      {needsGesture && playing && (
        <button
          className="gesture-overlay"
          onClick={() => {
            videoRef.current
              ?.play()
              .then(() => setNeedsGesture(false))
              .catch(() => {});
          }}
        >
          Click to start playback
        </button>
      )}
      {!connected && <div className="disconnected-banner">Reconnecting…</div>}
    </div>
  );
}
