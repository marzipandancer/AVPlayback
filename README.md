# PlayBack

Playlist video playback for live events — a PlaybackPro-style clip player
built with Tauri + React, sharing RunTime's architecture and UI language.
An operator cues clips on the **control window** and punches **TAKE**; the
**output window** plays them full screen on a second display. A built-in web
server makes both views available to any browser on the network.

## Architecture

```
┌────────────────────────  Rust (src-tauri)  ───────────────────────┐
│  state.rs    single source of truth: playlist, transport intent,   │
│              end-of-clip behavior, network settings + reducer      │
│  server.rs   axum web server: serves the built UI at /control      │
│              and /output, syncs state over /ws, streams playlist   │
│              media at /media/:id (range requests for seeking)      │
│  display.rs  native output-window management (monitors,            │
│              fullscreen, positioning)                              │
└────────────────────────────────────────────────────────────────────┘
          ▲ Tauri IPC + events                ▲ WebSocket
┌─────────┴──────────────┐        ┌───────────┴───────────────┐
│  native windows        │        │  web remote               │
│  control / output      │        │  http://<ip>:3100/control │
└────────────────────────┘        │  http://<ip>:3100/output  │
                                  └───────────────────────────┘
```

- **All state lives in Rust**, but the *media clock* lives in the output
  window's `<video>` element: it obeys transport intent (load / seek /
  play / pause, versioned with `loadSeq`/`seekSeq` so echoes can't replay
  stale commands) and reports real position/duration back twice a second
  via `mediaStatus`. Between reports the Rust side extrapolates
  (`TransportState::rebase`), so every snapshot is current and every
  client renders smooth timecodes from local `requestAnimationFrame`
  extrapolation — no tick traffic.
- **Media over HTTP.** Clips are streamed from the built-in server by
  playlist id. That gives native and web outputs the same path, seeking via
  range requests, and means only playlisted files are ever reachable.
- **Durations** are probed by the native control window with a hidden
  metadata-only `<video>`, one clip at a time.
- **Persistence.** Playlist (paths, names, durations), end mode, hotkeys,
  volume, display and network settings are saved to the app config dir
  (`~/Library/Application Support/com.avplayback.app/state.json`) and
  restored on launch. Files that vanished show as *missing*.

## Codecs

Playback uses the OS media engine (WKWebView on macOS, WebView2 on
Windows, WebKitGTK on Linux). On macOS that's AVFoundation, so **ProRes
.mov files play natively with hardware decode** (verified with ProRes HQ
1080p), alongside H.264/HEVC. H.264/AAC is the cross-platform safe bet —
it's also what web-remote previews (Chrome) can show; ProRes previews
render only in the native windows. Prefer `-movflags +faststart` files.

## Show features

- Per-clip **loop** checkbox in the playlist (overrides the end mode).
- Per-clip **in/out trim**: play or scrub to the spot, Mark In (I) /
  Mark Out (O). Timecodes, totals and end-of-clip behavior follow the
  trim; markers show on the progress bar.
- **Rehearsal jumps**: Last 30 / Last 20 / Last 10 seconds of the clip.
- Per-clip **audio level** × master volume.
- **Output geometry**: Fit / Fill / Stretch / forced 16:9 / forced 4:3,
  plus 25–200% scale — applied to the output and the control preview.

## Development

```sh
pnpm install                      # once, in / and in desktop-ui/
pnpm dev                          # = tauri dev (control window + vite HMR)
pnpm build                        # = tauri build (bundled .app)
```

Notes:

- The web server serves `desktop-ui/dist`, so run
  `pnpm --dir desktop-ui build` once before testing the web remote against a
  dev build (debug builds read the folder from disk; release builds embed it).
- The vite dev server (port 5174) proxies `/ws` and `/media` to
  `localhost:3100`, so `http://localhost:5174/control` works in a plain
  browser during development.
- Default server port is 3100 so PlayBack and RunTime can share a machine.
