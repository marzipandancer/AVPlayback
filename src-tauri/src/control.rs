//! PlaybackPro-compatible external control.
//!
//! Speaks DT Videolabs PlaybackPro Plus's published network protocol so
//! existing controllers — notably Bitfocus Companion's
//! "DT Videolabs PlaybackPro Plus" module — drive PlayBack unmodified:
//! raw ASCII two-character commands, no terminators, fire-and-forget, over
//! TCP port 4647 or UDP port 7000. `GO`/`GT` carry a 1-based clip number.
//!
//! Program-side commands map onto PlayBack's transport. Preview-transport
//! and timing-trim commands that have no PlayBack equivalent are accepted
//! and ignored, so controller salvos never error out.

use tauri::{AppHandle, Manager};

use crate::state::{self, Command, Shared};

pub const TCP_PORT: u16 = 4647;
pub const UDP_PORT: u16 = 7000;

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(tcp_listener(app.clone()));
    tauri::async_runtime::spawn(udp_listener(app));
}

async fn tcp_listener(app: AppHandle) {
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", TCP_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("control: could not listen on TCP {TCP_PORT}: {e}");
            return;
        }
    };
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 256];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        handle_payload(&app, &text);
                    }
                }
            }
        });
    }
}

async fn udp_listener(app: AppHandle) {
    let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", UDP_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("control: could not listen on UDP {UDP_PORT}: {e}");
            return;
        }
    };
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, _)) = sock.recv_from(&mut buf).await else { continue };
        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
        handle_payload(&app, &text);
    }
}

/// Parse a chunk that may hold several concatenated commands (controllers
/// send them with no delimiter). Two characters each; `GO`/`GT` swallow the
/// digits that follow.
fn handle_payload(app: &AppHandle, text: &str) {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let code: String = chars[i..i + 2].iter().collect();
        i += 2;
        let mut num = String::new();
        if code == "GO" || code == "GT" {
            while i < chars.len() && chars[i].is_ascii_digit() {
                num.push(chars[i]);
                i += 1;
            }
        }
        apply(app, &code, num.parse::<usize>().ok());
    }
}

fn apply(app: &AppHandle, code: &str, num: Option<usize>) {
    // A few mappings depend on current state (toggle vs. explicit).
    let (playing, has_current, current_loop, current_id) = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        let id = st.transport.current.clone();
        let looping = id
            .as_ref()
            .and_then(|i| st.clips.iter().find(|c| &c.id == i))
            .map(|c| c.looping)
            .unwrap_or(false);
        (st.transport.playing, id.is_some(), looping, id)
    };

    let cmd: Option<Command> = match code {
        // Program transport
        "TA" => Some(Command::Take { id: None }),
        "KL" => Some(Command::Kill),
        "OP" => has_current.then_some(Command::PlayPause), // pause/play toggle
        "PA" => (has_current && playing).then_some(Command::PlayPause),
        "PL" => (has_current && !playing).then_some(Command::PlayPause),
        // Preview / cue list
        "PR" => Some(Command::SelectDelta { delta: -1 }),
        "NX" => Some(Command::SelectDelta { delta: 1 }),
        "CL" => Some(Command::ClearSelection),
        // Clip addressing (1-based in the PlaybackPro protocol)
        "GO" => num
            .filter(|n| *n >= 1)
            .map(|n| Command::SelectIndex { index: n - 1 }),
        "GT" => num
            .filter(|n| *n >= 1)
            .map(|n| Command::TakeIndex { index: n - 1 }),
        // Rehearsal jumps
        "10" => Some(Command::JumpToEnd { offset_ms: 10_000.0 }),
        "20" => Some(Command::JumpToEnd { offset_ms: 20_000.0 }),
        "30" => Some(Command::JumpToEnd { offset_ms: 30_000.0 }),
        // Program positioning
        "OI" => Some(Command::Restart), // goto in point
        "OO" => Some(Command::JumpToEnd { offset_ms: 1_000.0 }), // goto out point
        "O-" => Some(Command::Jump { delta_ms: -33.0 }), // step one frame back
        "O+" => Some(Command::Jump { delta_ms: 33.0 }),  // step one frame fwd
        "OR" => Some(Command::Jump { delta_ms: -5_000.0 }), // fast reverse (approx.)
        "OF" => Some(Command::Jump { delta_ms: 5_000.0 }),  // fast forward (approx.)
        // Trim
        "ES" => Some(Command::MarkIn),
        "EE" => Some(Command::MarkOut),
        // Loop temp maps onto the current clip's loop checkbox
        "LT" => current_id.map(|id| Command::SetClipLoop { id, enabled: !current_loop }),
        "SL" => current_id.map(|id| Command::SetClipLoop { id, enabled: true }),
        "EL" => current_id.map(|id| Command::SetClipLoop { id, enabled: false }),
        // Accepted no-ops: preview transport, fades and timing trims that
        // have no PlayBack equivalent (IT FT EP EI EO ER EF E- E+ K± I± L±
        // F± T± SA and anything unknown).
        _ => None,
    };

    if let Some(cmd) = cmd {
        state::dispatch(app, cmd);
    }
}
