//! Built-in Simple Sync.
//!
//! Every PlayBack instance beacons its presence over UDP broadcast; every
//! instance listens and keeps a peer list in state. The operator picks one
//! peer as the backup deck, and the main deck opens a websocket to it and
//! mirrors every show command (take/kill/pause/seek/cue, by playlist index
//! so clip ids never need to match). The backup applies them like local
//! commands but never re-forwards, so decks can't loop.

use futures_util::{SinkExt, StreamExt};
use sha2::Digest;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::state::{self, Peer, Shared, SyncClipMeta};

pub const BEACON_PORT: u16 = 3101;
const BEACON_INTERVAL: Duration = Duration::from_secs(2);
const PEER_TIMEOUT: Duration = Duration::from_secs(7);

/// Stable per-process id so an instance can ignore its own beacons.
fn instance_id() -> u64 {
    use std::sync::OnceLock;
    static ID: OnceLock<u64> = OnceLock::new();
    *ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        (std::process::id() as u64) << 32 | nanos
    })
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(beacon_sender(app.clone()));
    tauri::async_runtime::spawn(beacon_listener(app.clone()));
    tauri::async_runtime::spawn(clock_sender(app.clone()));
    start_mirror_worker(app.clone());
    restart_follower(app);
}

/// Once a second, forward the main deck's playhead to the linked backup so
/// it can chase (see Command::SyncClock). Mirrored commands alone leave the
/// backup behind by its own media start latency — several hundred ms on
/// Windows — and then each deck's video element free-runs, so decks drift.
async fn clock_sender(app: AppHandle) {
    loop {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let msg: Option<String> = {
            let shared = app.state::<Shared>();
            if shared.sync_tx.lock().unwrap().is_none() {
                continue;
            }
            let mut st = shared.state.lock().unwrap();
            st.transport.rebase();
            st.transport
                .current
                .as_ref()
                .and_then(|id| st.clips.iter().position(|c| c.id == *id))
                .map(|index| {
                    serde_json::json!({
                        "type": "syncClock",
                        "payload": {
                            "index": index,
                            "positionMs": st.transport.position_ms,
                            "playing": st.transport.playing,
                        }
                    })
                    .to_string()
                })
        };
        if let Some(msg) = msg {
            let shared = app.state::<Shared>();
            let tx = shared.sync_tx.lock().unwrap();
            if let Some(tx) = &*tx {
                let _ = tx.send(msg);
            }
        }
    }
}

async fn beacon_sender(app: AppHandle) {
    let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        eprintln!("sync: could not open beacon sender socket");
        return;
    };
    let _ = sock.set_broadcast(true);
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    loop {
        let port = {
            let shared = app.state::<Shared>();
            let st = shared.state.lock().unwrap();
            st.network.port
        };
        let msg = serde_json::json!({
            "app": "playback",
            "id": instance_id(),
            "name": name,
            "port": port,
        })
        .to_string();
        // Broadcast for the LAN plus loopback for same-machine instances.
        let _ = sock.send_to(msg.as_bytes(), ("255.255.255.255", BEACON_PORT)).await;
        let _ = sock.send_to(msg.as_bytes(), ("127.0.0.1", BEACON_PORT)).await;
        prune_peers(&app);
        tokio::time::sleep(BEACON_INTERVAL).await;
    }
}

/// Listen for peer beacons. REUSEADDR/REUSEPORT so a main and a backup on
/// the same machine (or a dev pair) can both listen.
async fn beacon_listener(app: AppHandle) {
    let sock = match reusable_udp(BEACON_PORT) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sync: could not listen for beacons on {BEACON_PORT}: {e}");
            return;
        }
    };
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, src)) = sock.recv_from(&mut buf).await else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else { continue };
        if v.get("app").and_then(|a| a.as_str()) != Some("playback") {
            continue;
        }
        let Some(id) = v.get("id").and_then(|i| i.as_u64()) else { continue };
        if id == instance_id() {
            continue;
        }
        let Some(port) = v.get("port").and_then(|p| p.as_u64()) else { continue };
        let name = v
            .get("name")
            .and_then(|s| s.as_str())
            .unwrap_or("AVPlayback")
            .to_string();
        let addr = format!("{}:{}", src.ip(), port);
        let is_loopback = src.ip().is_loopback();

        let changed = {
            let shared = app.state::<Shared>();
            let mut st = shared.state.lock().unwrap();
            match st.sync.peers.iter_mut().find(|p| p.id == id) {
                Some(p) => {
                    p.last_seen = Instant::now();
                    let mut c = false;
                    if p.name != name {
                        p.name = name;
                        c = true;
                    }
                    // One instance beacons on several interfaces; keep the
                    // routable address, never downgrading to loopback.
                    let p_loopback = p.addr.starts_with("127.");
                    if p.addr != addr && (!is_loopback || p_loopback) {
                        p.addr = addr;
                        c = true;
                    }
                    c
                }
                None => {
                    st.sync.peers.push(Peer {
                        name,
                        addr,
                        id,
                        last_seen: Instant::now(),
                    });
                    st.sync.peers.sort_by(|a, b| a.name.cmp(&b.name));
                    true
                }
            }
        };
        if changed {
            state::broadcast(&app);
        }
    }
}

fn reusable_udp(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    tokio::net::UdpSocket::from_std(sock.into())
}

fn prune_peers(app: &AppHandle) {
    let changed = {
        let shared = app.state::<Shared>();
        let mut st = shared.state.lock().unwrap();
        let before = st.sync.peers.len();
        st.sync.peers.retain(|p| p.last_seen.elapsed() < PEER_TIMEOUT);
        st.sync.peers.len() != before
    };
    if changed {
        state::broadcast(app);
    }
}

// ---------------------------------------------------------------------------
// Playlist mirroring — the backup is a perfect copy of the main deck
// ---------------------------------------------------------------------------

/// Cheap content fingerprint: sha256 over (size, first 64 KiB, last 64 KiB).
/// Full-file hashing of multi-GB ProRes would take minutes; this reads at
/// most 128 KiB and distinguishes show media reliably.
async fn fingerprint(path: &str) -> std::io::Result<(u64, std::time::SystemTime, String)> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let meta = tokio::fs::metadata(path).await?;
    let (size, mtime) = (meta.len(), meta.modified()?);
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(size.to_le_bytes());
    let mut buf = vec![0u8; 64 * 1024];
    let n = f.read(&mut buf).await?;
    hasher.update(&buf[..n]);
    if size > 128 * 1024 {
        f.seek(std::io::SeekFrom::End(-(64 * 1024))).await?;
        let n = f.read(&mut buf).await?;
        hasher.update(&buf[..n]);
    }
    let fp = hex::encode(&hasher.finalize()[..16]);
    Ok((size, mtime, fp))
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

async fn fingerprint_cached(app: &AppHandle, path: &str) -> std::io::Result<String> {
    let meta = tokio::fs::metadata(path).await?;
    let (size, mtime) = (meta.len(), meta.modified()?);
    {
        let shared = app.state::<Shared>();
        let cache = shared.fp_cache.lock().unwrap();
        if let Some((s, m, fp)) = cache.get(path) {
            if *s == size && *m == mtime {
                return Ok(fp.clone());
            }
        }
    }
    let (size, mtime, fp) = fingerprint(path).await?;
    let shared = app.state::<Shared>();
    shared
        .fp_cache
        .lock()
        .unwrap()
        .insert(path.to_string(), (size, mtime, fp.clone()));
    Ok(fp)
}

/// Debounced worker: any playlist/clip-settings change (or a fresh link)
/// nudges it, and it brings the backup's media and layout in line.
fn start_mirror_worker(app: AppHandle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    *app.state::<Shared>().mirror_tx.lock().unwrap() = Some(tx);
    tauri::async_runtime::spawn(async move {
        while rx.recv().await.is_some() {
            // Coalesce bursts (drag-adding ten files, probe results…).
            tokio::time::sleep(Duration::from_millis(500)).await;
            while rx.try_recv().is_ok() {}
            if let Err(e) = reconcile(&app).await {
                state::set_mirror_status(&app, &format!("mirror error: {e}"));
            }
        }
    });
}

pub fn trigger_mirror(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let tx = shared.mirror_tx.lock().unwrap();
    if let Some(tx) = &*tx {
        let _ = tx.send(());
    }
}

async fn reconcile(app: &AppHandle) -> Result<(), String> {
    let (linked, follower, clips) = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        (st.sync.linked, st.sync.follower.clone(), st.clips.clone())
    };
    let Some(addr) = follower else {
        state::set_mirror_status(app, "");
        return Ok(());
    };
    if !linked {
        return Ok(());
    }

    state::set_mirror_status(app, "scanning…");
    let mut metas: Vec<(SyncClipMeta, String)> = Vec::new(); // (meta, source path)
    for c in &clips {
        if c.missing {
            continue;
        }
        let fp = fingerprint_cached(app, &c.path)
            .await
            .map_err(|e| format!("{}: {e}", c.name))?;
        let file = state::sanitize_filename(
            std::path::Path::new(&c.path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| c.name.clone())
                .as_str(),
        );
        metas.push((
            SyncClipMeta {
                fp,
                file,
                looping: c.looping,
                in_ms: c.in_ms,
                out_ms: c.out_ms,
                gain: c.gain,
                duration_ms: c.duration_ms,
            },
            c.path.clone(),
        ));
    }

    let client = reqwest::Client::new();

    // What does the backup already have?
    let mut needed: Vec<&(SyncClipMeta, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in &metas {
        if !seen.insert(m.0.fp.clone()) {
            continue; // same file twice in the playlist
        }
        let url = format!("http://{addr}/sync/have/{}", m.0.fp);
        let have = client
            .get(&url)
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .map_err(|e| format!("backup unreachable: {e}"))?
            .status()
            .is_success();
        if !have {
            needed.push(m);
        }
    }

    // Push missing media, with byte progress in the status line.
    let total = needed.len();
    for (i, (meta, path)) in needed.iter().enumerate() {
        let size = tokio::fs::metadata(path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let label = format!("uploading {}/{}: {}", i + 1, total, meta.file);
        state::set_mirror_status(app, &label);

        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| format!("{}: {e}", meta.file))?;
        let app2 = app.clone();
        let label2 = label.clone();
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_pct = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stream = tokio_util::io::ReaderStream::new(file).inspect(move |chunk| {
            if let Ok(chunk) = chunk {
                use std::sync::atomic::Ordering;
                let s = sent.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
                if size > 0 {
                    let pct = s * 100 / size;
                    if pct > last_pct.swap(pct, Ordering::Relaxed) && pct % 5 == 0 {
                        state::set_mirror_status(&app2, &format!("{label2} ({pct}%)"));
                    }
                }
            }
        });
        let url = format!(
            "http://{addr}/sync/media/{}?name={}",
            meta.fp,
            urlencode(&meta.file)
        );
        let res = client
            .post(&url)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|e| format!("upload {}: {e}", meta.file))?;
        if !res.status().is_success() {
            return Err(format!("upload {}: HTTP {}", meta.file, res.status()));
        }
    }

    // Ship the layout over the sync socket.
    let payload = serde_json::json!({
        "type": "syncPlaylist",
        "payload": { "clips": metas.iter().map(|m| &m.0).collect::<Vec<_>>() }
    });
    {
        let shared = app.state::<Shared>();
        let tx = shared.sync_tx.lock().unwrap();
        match &*tx {
            Some(tx) => tx.send(payload.to_string()).map_err(|_| "link lost".to_string())?,
            None => return Ok(()), // unlinked mid-reconcile; next link retriggers
        }
    }
    state::set_mirror_status(app, "mirrored");
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// (Re)connect to the selected backup deck. Called at startup and whenever
/// the operator picks a different follower (or None).
pub fn restart_follower(app: AppHandle) {
    let shared = app.state::<Shared>();
    if let Some(task) = shared.sync_task.lock().unwrap().take() {
        task.abort();
    }
    *shared.sync_tx.lock().unwrap() = None;

    let target = {
        let mut st = shared.state.lock().unwrap();
        st.sync.linked = false;
        st.sync.follower.clone()
    };
    state::broadcast(&app);
    let Some(target) = target else { return };

    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        loop {
            let url = format!("ws://{target}/ws?role=sync");
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws, _)) => {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    {
                        let shared = app2.state::<Shared>();
                        *shared.sync_tx.lock().unwrap() = Some(tx);
                        shared.state.lock().unwrap().sync.linked = true;
                    }
                    state::broadcast(&app2);
                    // A fresh link always reconciles the backup's playlist.
                    trigger_mirror(&app2);

                    let (mut sink, mut stream) = ws.split();
                    loop {
                        tokio::select! {
                            cmd = rx.recv() => match cmd {
                                Some(json) => {
                                    if sink
                                        .send(tokio_tungstenite::tungstenite::Message::Text(json))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                None => break,
                            },
                            // The backup pushes its state broadcasts; drain
                            // and ignore them, watching for disconnect.
                            incoming = stream.next() => match incoming {
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                        }
                    }

                    {
                        let shared = app2.state::<Shared>();
                        *shared.sync_tx.lock().unwrap() = None;
                        shared.state.lock().unwrap().sync.linked = false;
                    }
                    state::broadcast(&app2);
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    *shared.sync_task.lock().unwrap() = Some(task);
}
