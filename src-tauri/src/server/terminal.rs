use std::os::fd::{BorrowedFd, RawFd};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;

use crate::auth::constant_time_eq;
use crate::server::instances::AppState;
use crate::tmux::pty::{pty_attach, PtyHandle};
use crate::tmux::session::resize_window;

#[derive(Deserialize)]
pub struct WsParams {
    pub t: Option<String>,
}

#[derive(Deserialize)]
struct WsMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
    #[serde(default)]
    data: Option<String>,
}

fn resolve_our_sid(session_id: &str, state: &AppState) -> Option<String> {
    let instances = state.cached_instances();
    instances
        .iter()
        .find(|i| i.session_id == session_id)
        .and_then(|i| i.our_sid.clone())
}

pub async fn ws_terminal(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let _token = match params.t {
        Some(ref t) if constant_time_eq(t, &state.auth_token) => t.clone(),
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };

    ws.on_upgrade(move |socket| handle_ws(socket, session_id, state))
}

async fn handle_ws(mut socket: WebSocket, session_id: String, state: Arc<AppState>) {
    let our_sid = match resolve_our_sid(&session_id, &state) {
        Some(sid) => sid,
        None => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let (cols, rows) = match wait_for_resize(&mut socket).await {
        Some(dims) => dims,
        None => return,
    };

    let config = state.tmux_config.clone();
    let our_sid_clone = our_sid.clone();
    let pty_handle = match tokio::task::spawn_blocking(move || {
        pty_attach(&config, &our_sid_clone, cols, rows)
    })
    .await
    {
        Ok(Ok(handle)) => Arc::new(handle),
        _ => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let config = state.tmux_config.clone();
    let our_sid_clone = our_sid.clone();
    let _ = tokio::task::spawn_blocking(move || {
        resize_window(&config, &our_sid_clone, cols as u32, rows as u32)
    })
    .await;

    let fd = pty_handle.raw_fd();
    let (output_tx, mut output_rx) = mpsc::channel::<Vec<u8>>(64);

    let reader_handle = tokio::spawn(pty_read_loop(fd, output_tx));

    let pty_for_input = Arc::clone(&pty_handle);
    let config = state.tmux_config.clone();
    let our_sid_for_input = our_sid.clone();

    loop {
        tokio::select! {
            Some(data) = output_rx.recv() => {
                if socket.send(Message::Binary(data.into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(msg)) => {
                        if !handle_incoming(
                            msg,
                            &pty_for_input,
                            &config,
                            &our_sid_for_input,
                        ).await {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }

    reader_handle.abort();
    drop(pty_handle);
}

async fn wait_for_resize(socket: &mut WebSocket) -> Option<(u16, u16)> {
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            if let Ok(parsed) = serde_json::from_str::<WsMessage>(&text) {
                if parsed.msg_type == "resize" {
                    let cols = parsed.cols.unwrap_or(80);
                    let rows = parsed.rows.unwrap_or(24);
                    return Some((cols, rows));
                }
            }
        }
    }
    None
}

async fn pty_read_loop(fd: RawFd, tx: mpsc::Sender<Vec<u8>>) {
    let async_fd = match AsyncFd::new(fd) {
        Ok(afd) => afd,
        Err(_) => return,
    };

    let mut buf = vec![0u8; 4096];

    loop {
        let mut guard = match async_fd.readable().await {
            Ok(g) => g,
            Err(_) => break,
        };

        match nix::unistd::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
            Err(nix::errno::Errno::EAGAIN) => {
                guard.clear_ready();
            }
            Err(_) => break,
        }
    }

    std::mem::forget(async_fd);
}

async fn handle_incoming(
    msg: Message,
    pty: &Arc<PtyHandle>,
    config: &crate::tmux::TmuxConfig,
    our_sid: &str,
) -> bool {
    match msg {
        Message::Text(text) => {
            let parsed: WsMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => return true,
            };
            match parsed.msg_type.as_str() {
                "input" => {
                    if let Some(data) = parsed.data {
                        let fd = pty.raw_fd();
                        let bytes = data.as_bytes();
                        write_all_to_fd(fd, bytes);
                    }
                }
                "resize" => {
                    let cols = parsed.cols.unwrap_or(80);
                    let rows = parsed.rows.unwrap_or(24);
                    pty.resize(cols, rows);
                    let config = config.clone();
                    let our_sid = our_sid.to_string();
                    let _ = tokio::task::spawn_blocking(move || {
                        resize_window(&config, &our_sid, cols as u32, rows as u32)
                    })
                    .await;
                }
                _ => {}
            }
            true
        }
        Message::Binary(data) => {
            let fd = pty.raw_fd();
            write_all_to_fd(fd, &data);
            true
        }
        Message::Close(_) => false,
        _ => true,
    }
}

fn write_all_to_fd(fd: RawFd, mut data: &[u8]) {
    while !data.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match nix::unistd::write(borrowed, data) {
            Ok(n) => data = &data[n..],
            Err(nix::errno::Errno::EAGAIN) => continue,
            Err(_) => break,
        }
    }
}
