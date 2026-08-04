import { useEffect, useRef, useState } from "react";
import { mediaUrl } from "../format";
import { type Store } from "../store";
import { isTauri } from "../transport";

/**
 * Full-screen program output: the native output window and the /output web
 * page both render this. The <video> element here is the authority for
 * position/duration — it obeys the transport intent from state (load, seek,
 * play/pause) and reports progress back with mediaStatus commands.
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
  // loadSeq in the URL makes re-taking the same clip a fresh load from 0:00.
  const src = snap && t && clip ? mediaUrl(snap.state, clip.id, t.loadSeq) : null;
  const playing = (t?.playing ?? false) && !!src;
  const seekSeq = t?.seekSeq ?? 0;

  // Report the element's real position/duration back to the state core. The
  // loadSeq tag lets the reducer drop reports that raced a clip change.
  const loadSeqRef = useRef(0);
  loadSeqRef.current = t?.loadSeq ?? 0;
  const report = (ended: boolean) => {
    const v = videoRef.current;
    if (!v || !src) return;
    send({
      type: "mediaStatus",
      payload: {
        loadSeq: loadSeqRef.current,
        positionMs: v.currentTime * 1000,
        durationMs: Number.isFinite(v.duration) ? v.duration * 1000 : 0,
        playing: !v.paused && !v.ended,
        ended,
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

  // Commanded seeks (restart, ±10s, progress-bar clicks, loop).
  useEffect(() => {
    const v = videoRef.current;
    if (!v || !t || !src) return;
    if (seekSeq !== appliedSeek.current) {
      appliedSeek.current = seekSeq;
      v.currentTime = t.seekMs / 1000;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seekSeq, src]);

  // Play/pause intent. seekSeq is a dependency so a loop restart (seek to 0
  // while "playing") starts the ended element again.
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

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.volume = t?.volume ?? 1;
    v.muted = t?.muted ?? false;
  }, [t?.volume, t?.muted, src]);

  // Periodic progress reports; extrapolation on the other views corrects
  // itself against these, so a modest rate is plenty.
  useEffect(() => {
    if (!src || !playing) return;
    const id = setInterval(() => report(false), 500);
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
          src={src}
          preload="auto"
          playsInline
          onLoadedMetadata={() => report(false)}
          onPlay={() => report(false)}
          onPause={() => {
            const v = videoRef.current;
            if (v && !v.ended) report(false);
          }}
          onSeeked={() => report(false)}
          onEnded={() => report(true)}
          onError={() => setMediaError(true)}
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
