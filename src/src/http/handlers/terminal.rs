use std::os::fd::{BorrowedFd, RawFd};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;

use crate::http::auth::constant_time_eq;
use crate::services::instances::AppState;
use crate::infra::tmux::pty::{pty_attach, PtyHandle};
use crate::infra::tmux::session::resize_window;

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

fn token_valid(provided: Option<&str>, expected: &str) -> bool {
    provided.is_some_and(|t| constant_time_eq(t, expected))
}

pub async fn ws_terminal(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !token_valid(params.t.as_deref(), &state.auth_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

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
    config: &crate::infra::tmux::TmuxConfig,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::instances::{AppState, InstancesCache, PendingFocus, PsCache};
    use crate::http::dto::InstanceData;
    use crate::infra::tmux::TmuxConfig;
    use serde_json::json;
    use std::collections::HashMap;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::sync::Mutex;

    fn make_state() -> Arc<AppState> {
        Arc::new(AppState {
            tmux_config: TmuxConfig {
                tmux_bin: "/nope".into(),
                socket_name: "ciu-test".into(),
                name_prefix: "ciu-".into(),
            },
            auth_token: "tok".into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache { at: 0.0, data: vec![] }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache { at: 0.0, map: HashMap::new() }),
        })
    }

    fn instance(session_id: &str, our_sid: Option<&str>) -> InstanceData {
        InstanceData {
            session_id: session_id.into(),
            pid: None,
            alive: true,
            name: String::new(),
            title: None,
            custom_name: None,
            first_user: None,
            cwd: None,
            kind: None,
            started_at: None,
            command: String::new(),
            status: "idle".into(),
            last_event: None,
            last_tool: None,
            notification_message: None,
            hook_timestamp: None,
            transcript: json!({}),
            summary: json!({}),
            mcps: json!({}),
            subagents: vec![],
            group: None,
            ack_timestamp: 0.0,
            our_sid: our_sid.map(|s| s.to_string()),
            tmux_session: None,
        }
    }

    #[test]
    fn ws_params_deserializes_with_token() {
        let p: WsParams = serde_json::from_str(r#"{"t":"abc"}"#).unwrap();
        assert_eq!(p.t.as_deref(), Some("abc"));
    }

    #[test]
    fn ws_params_deserializes_without_token() {
        let p: WsParams = serde_json::from_str(r#"{}"#).unwrap();
        assert!(p.t.is_none());
    }

    #[test]
    fn ws_message_deserializes_resize() {
        let m: WsMessage =
            serde_json::from_str(r#"{"type":"resize","cols":120,"rows":40}"#).unwrap();
        assert_eq!(m.msg_type, "resize");
        assert_eq!(m.cols, Some(120));
        assert_eq!(m.rows, Some(40));
        assert!(m.data.is_none());
    }

    #[test]
    fn ws_message_deserializes_input() {
        let m: WsMessage = serde_json::from_str(r#"{"type":"input","data":"hi"}"#).unwrap();
        assert_eq!(m.msg_type, "input");
        assert_eq!(m.data.as_deref(), Some("hi"));
        assert!(m.cols.is_none());
    }

    #[test]
    fn ws_message_missing_optional_fields_defaults() {
        let m: WsMessage = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert_eq!(m.msg_type, "ping");
        assert!(m.cols.is_none());
        assert!(m.rows.is_none());
        assert!(m.data.is_none());
    }

    #[test]
    fn resolve_our_sid_returns_none_for_missing_session() {
        let state = make_state();
        assert!(resolve_our_sid("missing", &state).is_none());
    }

    #[test]
    fn resolve_our_sid_returns_none_when_our_sid_absent() {
        let state = make_state();
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.data.push(instance("sess-1", None));
            c.at = f64::MAX;
        }
        assert!(resolve_our_sid("sess-1", &state).is_none());
    }

    #[test]
    fn resolve_our_sid_returns_value_when_present() {
        let state = make_state();
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.data.push(instance("sess-1", Some("ciu-abc")));
            c.at = f64::MAX;
        }
        assert_eq!(
            resolve_our_sid("sess-1", &state),
            Some("ciu-abc".to_string())
        );
    }

    #[test]
    fn write_all_to_fd_writes_bytes_to_pipe() {
        use nix::unistd::{close, pipe, read};
        let (r, w) = pipe().unwrap();
        let w_raw = w.as_raw_fd();
        write_all_to_fd(w_raw, b"hello");
        close(w.into_raw_fd()).unwrap();
        let mut buf = [0u8; 16];
        let n = read(r.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn write_all_to_fd_noop_on_empty_slice() {
        use nix::unistd::{close, pipe};
        let (r, w) = pipe().unwrap();
        let w_raw = w.as_raw_fd();
        write_all_to_fd(w_raw, b"");
        close(w.into_raw_fd()).unwrap();
        close(r.into_raw_fd()).unwrap();
    }

    #[test]
    fn write_all_to_fd_breaks_on_closed_reader() {
        use nix::unistd::{close, pipe};
        let (r, w) = pipe().unwrap();
        let w_raw = w.as_raw_fd();
        close(r.into_raw_fd()).unwrap();
        write_all_to_fd(w_raw, b"x");
        close(w.into_raw_fd()).unwrap();
    }

    fn spawn_dummy_child() -> std::process::Child {
        use nix::unistd::pipe;
        use std::process::{Command, Stdio};
        let (cr, cw) = pipe().unwrap();
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::from(cr))
            .stdout(Stdio::from(cw))
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
    }

    fn pty_on_pipe() -> (Arc<PtyHandle>, std::os::fd::OwnedFd) {
        use nix::unistd::pipe;
        use std::os::fd::FromRawFd;
        let (r, w) = pipe().unwrap();
        let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(w.into_raw_fd()) };
        let pty = PtyHandle::from_parts(master, spawn_dummy_child());
        (Arc::new(pty), r)
    }

    #[tokio::test]
    async fn handle_incoming_close_returns_false() {
        let (pty, _r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(Message::Close(None), &pty, &cfg, "sid").await;
        assert!(!cont);
    }

    #[tokio::test]
    async fn handle_incoming_text_input_writes_to_pty_fd() {
        let (pty, r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let payload = r#"{"type":"input","data":"hi"}"#.to_string();
        let cont = handle_incoming(Message::Text(payload.into()), &pty, &cfg, "sid").await;
        assert!(cont);
        let mut buf = [0u8; 16];
        let n = nix::unistd::read(r.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
    }

    #[tokio::test]
    async fn handle_incoming_binary_writes_to_pty_fd() {
        let (pty, r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(Message::Binary(b"ab".to_vec().into()), &pty, &cfg, "sid").await;
        assert!(cont);
        let mut buf = [0u8; 16];
        let n = nix::unistd::read(r.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], b"ab");
    }

    #[tokio::test]
    async fn handle_incoming_text_invalid_json_is_ignored_and_continues() {
        let (pty, _r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(
            Message::Text("not-json".to_string().into()),
            &pty,
            &cfg,
            "sid",
        )
        .await;
        assert!(cont);
    }

    #[tokio::test]
    async fn handle_incoming_text_unknown_type_is_ignored() {
        let (pty, _r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(
            Message::Text(r#"{"type":"pong"}"#.to_string().into()),
            &pty,
            &cfg,
            "sid",
        )
        .await;
        assert!(cont);
    }

    #[tokio::test]
    async fn handle_incoming_input_without_data_is_ignored() {
        let (pty, _r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(
            Message::Text(r#"{"type":"input"}"#.to_string().into()),
            &pty,
            &cfg,
            "sid",
        )
        .await;
        assert!(cont);
    }

    #[tokio::test]
    async fn handle_incoming_resize_invokes_pty_resize_with_defaults() {
        let pty_sys = nix::pty::openpty(None, None).unwrap();
        let master = pty_sys.master;
        drop(pty_sys.slave);
        let pty = Arc::new(PtyHandle::from_parts(master, spawn_dummy_child()));
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(
            Message::Text(r#"{"type":"resize"}"#.to_string().into()),
            &pty,
            &cfg,
            "sid",
        )
        .await;
        assert!(cont);
    }

    #[tokio::test]
    async fn handle_incoming_resize_applies_cols_rows() {
        let pty_sys = nix::pty::openpty(None, None).unwrap();
        let master = pty_sys.master;
        drop(pty_sys.slave);
        let pty = Arc::new(PtyHandle::from_parts(master, spawn_dummy_child()));
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(
            Message::Text(r#"{"type":"resize","cols":200,"rows":60}"#.to_string().into()),
            &pty,
            &cfg,
            "sid",
        )
        .await;
        assert!(cont);
    }

    #[tokio::test]
    async fn handle_incoming_ping_is_pass_through() {
        let (pty, _r) = pty_on_pipe();
        let cfg = TmuxConfig {
            tmux_bin: "/nope".into(),
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        };
        let cont = handle_incoming(Message::Ping(vec![].into()), &pty, &cfg, "sid").await;
        assert!(cont);
    }

    fn set_nonblocking(fd: RawFd) {
        let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL).unwrap();
        let mut o = nix::fcntl::OFlag::from_bits_truncate(flags);
        o.insert(nix::fcntl::OFlag::O_NONBLOCK);
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(o)).unwrap();
    }

    #[tokio::test]
    async fn pty_read_loop_forwards_bytes_to_channel() {
        use nix::unistd::{close, pipe, write};
        let (r, w) = pipe().unwrap();
        let r_fd = r.as_raw_fd();
        let w_fd = w.as_raw_fd();
        set_nonblocking(r_fd);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
        let handle = tokio::spawn(pty_read_loop(r_fd, tx));
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(w_fd) };
        write(borrowed, b"abc").unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&msg, b"abc");
        close(w.into_raw_fd()).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        close(r.into_raw_fd()).ok();
    }

    #[tokio::test]
    async fn pty_read_loop_exits_when_writer_closes() {
        use nix::unistd::{close, pipe};
        let (r, w) = pipe().unwrap();
        let r_fd = r.as_raw_fd();
        set_nonblocking(r_fd);
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(4);
        let handle = tokio::spawn(pty_read_loop(r_fd, tx));
        close(w.into_raw_fd()).unwrap();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(res.is_ok());
        close(r.into_raw_fd()).ok();
    }

    #[tokio::test]
    async fn pty_read_loop_exits_when_receiver_dropped() {
        use nix::unistd::{close, pipe, write};
        let (r, w) = pipe().unwrap();
        let r_fd = r.as_raw_fd();
        let w_fd = w.as_raw_fd();
        set_nonblocking(r_fd);
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        drop(rx);
        let handle = tokio::spawn(pty_read_loop(r_fd, tx));
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(w_fd) };
        let _ = write(borrowed, b"z");
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(res.is_ok());
        close(w.into_raw_fd()).ok();
        close(r.into_raw_fd()).ok();
    }

    #[test]
    fn token_valid_true_for_matching_token() {
        assert!(token_valid(Some("abc"), "abc"));
    }

    #[test]
    fn token_valid_false_for_wrong_token() {
        assert!(!token_valid(Some("wrong"), "abc"));
    }

    #[test]
    fn token_valid_false_for_missing_token() {
        assert!(!token_valid(None, "abc"));
    }

    async fn spawn_ws_terminal_server() -> (std::net::SocketAddr, Arc<AppState>) {
        use axum::routing::get;
        use axum::Router;
        let state = make_state();
        let app: Router = Router::new()
            .route("/ws/terminal/{session_id}", get(ws_terminal))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, state)
    }

    async fn send_raw_ws_request(addr: std::net::SocketAddr, path: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 128];
        let n = stream.read(&mut buf).await.unwrap();
        let text = std::str::from_utf8(&buf[..n]).unwrap();
        let code = text.split_whitespace().nth(1).unwrap();
        code.parse().unwrap()
    }

    #[tokio::test]
    async fn ws_terminal_returns_401_when_token_missing() {
        let (addr, _state) = spawn_ws_terminal_server().await;
        let code = send_raw_ws_request(addr, "/ws/terminal/abc").await;
        assert_eq!(code, 401);
    }

    #[tokio::test]
    async fn ws_terminal_returns_401_when_token_wrong() {
        let (addr, _state) = spawn_ws_terminal_server().await;
        let code = send_raw_ws_request(addr, "/ws/terminal/abc?t=bad").await;
        assert_eq!(code, 401);
    }

    #[tokio::test]
    async fn ws_terminal_upgrades_with_valid_token() {
        let (addr, _state) = spawn_ws_terminal_server().await;
        let code = send_raw_ws_request(addr, "/ws/terminal/abc?t=tok").await;
        assert_eq!(code, 101);
    }
}
