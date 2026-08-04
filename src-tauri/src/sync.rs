//! Built-in Simple Sync.
//!
//! Every PlayBack instance beacons its presence over UDP broadcast; every
//! instance listens and keeps a peer list in state. The operator picks one
//! peer as the backup deck, and the main deck opens a websocket to it and
//! mirrors every show command (take/kill/pause/seek/cue, by playlist index
//! so clip ids never need to match). The backup applies them like local
//! commands but never re-forwards, so decks can't loop.

use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::state::{self, Peer, Shared};

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
    restart_follower(app);
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
            .unwrap_or("PlayBack")
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
