use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, Request, State,
    },
    http::{header, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::collections::HashMap;
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use tauri::{AppHandle, Manager};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::state::{self, Shared};

/// The web remote serves the same built frontend bundle the native windows
/// use; the app decides between control/output views from the URL path.
#[derive(RustEmbed)]
#[folder = "../desktop-ui/dist/"]
struct Assets;

/// User guide, served at /help. The remote-access URLs in the document use
/// this placeholder address; it is swapped for the live server address at
/// request time so the copy buttons yield working URLs.
const HELP_HTML: &str = include_str!("../../docs/PlayBack_UserGuide.html");
const HELP_ADDR_PLACEHOLDER: &str = "10.1.10.140:3100";

/// (Re)start the web server using the network settings currently in state.
/// Any previous server task is aborted first.
pub fn restart(app: AppHandle) {
    let shared = app.state::<Shared>();
    if let Some(handle) = shared.server.lock().unwrap().take() {
        handle.abort();
    }

    let app2 = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        // Give the previous listener a moment to release the port.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let (ip, port) = {
            let shared = app2.state::<Shared>();
            let st = shared.state.lock().unwrap();
            (st.network.bind_ip(), st.network.port)
        };

        let listener = match tokio::net::TcpListener::bind((ip, port)).await {
            Ok(l) => {
                set_error(&app2, None);
                l
            }
            Err(e) => {
                set_error(&app2, Some(format!("Could not listen on {ip}:{port}: {e}")));
                return;
            }
        };

        let router = Router::new()
            .route("/ws", get(ws_handler))
            .route("/media/:id", get(media_handler))
            .route("/help", get(help_handler))
            .fallback(static_handler)
            .with_state(app2.clone());

        if let Err(e) = axum::serve(listener, router).await {
            set_error(&app2, Some(format!("Web server stopped: {e}")));
        }
    });

    *app.state::<Shared>().server.lock().unwrap() = Some(task);
}

fn set_error(app: &AppHandle, error: Option<String>) {
    {
        let shared = app.state::<Shared>();
        let mut st = shared.state.lock().unwrap();
        st.network.error = error;
    }
    state::broadcast(app);
}

// ---------------------------------------------------------------------------
// Media streaming
// ---------------------------------------------------------------------------

/// Stream a playlisted file with range-request support (ServeFile handles
/// the Range headers video elements send when seeking). Only files that are
/// actually in the playlist are reachable — the id is the only key.
async fn media_handler(
    Path(id): Path<String>,
    State(app): State<AppHandle>,
    req: Request,
) -> impl IntoResponse {
    let path = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        st.clips.iter().find(|c| c.id == id).map(|c| c.path.clone())
    };
    match path {
        Some(path) => match ServeFile::new(path).oneshot(req).await {
            Ok(res) => res.into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        None => (StatusCode::NOT_FOUND, "no such clip").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Static files
// ---------------------------------------------------------------------------

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let lower = path.to_ascii_lowercase();

    // App routes (case-insensitive) fall through to the SPA entry point.
    let file = if path.is_empty() || lower == "control" || lower == "output" {
        "index.html".to_string()
    } else {
        path.to_string()
    };

    match Assets::get(&file).or_else(|| Assets::get("index.html")) {
        Some(content) => {
            let mime = mime_guess::from_path(&file).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            "frontend bundle not built — run `pnpm --dir desktop-ui build`",
        )
            .into_response(),
    }
}

async fn help_handler(State(app): State<AppHandle>) -> impl IntoResponse {
    let addr = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        format!("{}:{}", st.network.ip, st.network.port)
    };
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        HELP_HTML.replace(HELP_ADDR_PLACEHOLDER, &addr),
    )
}

// ---------------------------------------------------------------------------
// WebSocket sync
// ---------------------------------------------------------------------------

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(app): State<AppHandle>,
) -> impl IntoResponse {
    // A main deck connects with ?role=sync; commands it mirrors here are
    // applied but never re-forwarded (see dispatch_ext).
    let from_sync = params.get("role").map(|r| r == "sync").unwrap_or(false);
    ws.on_upgrade(move |socket| handle_socket(socket, app, from_sync))
}

async fn handle_socket(socket: WebSocket, app: AppHandle, from_sync: bool) {
    let mut rx = app.state::<Shared>().tx.subscribe();
    let (mut sender, mut receiver) = socket.split();

    // Full state on connect, then push every broadcast.
    let initial = state::state_json(&app).to_string();
    if sender.send(Message::Text(initial)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            update = rx.recv() => {
                match update {
                    Ok(json) => {
                        if sender.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    // Lagged: skip to the most recent state.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<state::Command>(&text) {
                            let app = app.clone();
                            // Dispatch does blocking work (persistence, window
                            // ops); keep it off this reactor thread.
                            tauri::async_runtime::spawn_blocking(move || {
                                state::dispatch_ext(&app, cmd, from_sync);
                            });
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}
