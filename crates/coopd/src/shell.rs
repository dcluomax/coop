//! PTY shell over WebSocket — `GET /api/v1/hens/:id/shell`.
//!
//! Opens a real PTY rooted in the Hen's workdir so the farmer can:
//!   * `claude login`, `gh auth login`, `codex auth login`, ...
//!   * inspect files, install per-hen tooling,
//!   * troubleshoot a misbehaving agent.
//!
//! Wire protocol over the WebSocket:
//!
//! | direction | frame   | payload                                                 |
//! |-----------|---------|---------------------------------------------------------|
//! | C → S     | Binary  | raw stdin bytes (typed keys, paste data)                |
//! | C → S     | Text    | JSON `{"type":"resize","cols":N,"rows":N}`              |
//! | S → C     | Binary  | raw stdout/stderr bytes (the terminal stream)           |
//! | S → C     | Text    | JSON `{"type":"exit","code":N}` then the socket closes  |
//!
//! No authentication. v0.1 assumes the daemon binds to loopback only.

use std::io::{Read, Write};
use std::sync::Arc;

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message as WsMsg, WebSocket},
    },
    response::IntoResponse,
};
use coopd_core::HenId;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::orchestrator::OrchHandle;
use crate::session::{self, SessionBackendKind};

const READ_CHUNK: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientCtrl {
    Resize { cols: u16, rows: u16 },
}

/// Axum handler. Upgrades to a WSS bound to a fresh PTY.
pub async fn shell(
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
    State(orch): State<OrchHandle>,
) -> impl IntoResponse {
    // H6: cap inbound frames. A PTY input frame is a few keystrokes or a
    // paste; 256 KiB is far above realistic interactive payloads but well
    // below the 64 MiB default that lets a hostile client OOM the daemon.
    let ws = ws.max_message_size(256 * 1024).max_frame_size(256 * 1024);
    ws.on_upgrade(move |socket| run(socket, orch, id))
}

async fn run(socket: WebSocket, orch: OrchHandle, raw_id: String) {
    let hen_id = match raw_id.parse::<HenId>() {
        Ok(id) => id,
        Err(e) => {
            close_with_error(socket, &format!("invalid hen id: {e}")).await;
            return;
        }
    };
    let hen = match orch.get_hen(hen_id.clone()).await {
        Ok(h) => h,
        Err(e) => {
            close_with_error(socket, &format!("hen lookup failed: {e}")).await;
            return;
        }
    };
    let workdir = orch.workdir_base.join(hen.id.name());
    if let Err(e) = tokio::fs::create_dir_all(&workdir).await {
        close_with_error(socket, &format!("mkdir workdir: {e}")).await;
        return;
    }

    let user_shell = session::default_shell();
    let backend = session::detect_backend();
    let sess_name = session::tmux_session_name(&hen_id);
    let tmux_dir = session::tmux_socket_dir(&sess_name, &workdir);
    let session_existed = if backend == SessionBackendKind::Tmux {
        if let Err(e) = session::ensure_tmux_socket_dir(&tmux_dir) {
            close_with_error(socket, &format!("mkdir tmux socket dir: {e}")).await;
            return;
        }
        std::process::Command::new("tmux")
            .env("TMUX_TMPDIR", &tmux_dir)
            .args(["-L", "coop", "has-session", "-t", &sess_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        false
    };
    let (shell_cmd, shell_args): (String, Vec<String>) = match backend {
        SessionBackendKind::Tmux => (
            "tmux".into(),
            vec![
                "-L".into(),
                "coop".into(),
                "new-session".into(),
                "-A".into(),
                "-s".into(),
                sess_name.clone(),
                user_shell.clone(),
            ],
        ),
        SessionBackendKind::PlainPty => (user_shell.clone(), vec![]),
    };
    let pty_system = NativePtySystem::default();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            close_with_error(socket, &format!("openpty: {e}")).await;
            return;
        }
    };

    let mut cmd = CommandBuilder::new(shell_cmd);
    for a in &shell_args {
        cmd.arg(a);
    }
    cmd.cwd(&workdir);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COOP_HEN_ID", hen_id.to_string());
    cmd.env("COOP_HEN_WORKDIR", workdir.to_string_lossy().to_string());
    cmd.env("TMUX_TMPDIR", tmux_dir.to_string_lossy().to_string());
    // Pass through HOME so user-level config (e.g. ~/.claude) is reachable.
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            close_with_error(socket, &format!("spawn shell: {e}")).await;
            return;
        }
    };
    // Drop slave; only the child holds it now.
    drop(pair.slave);

    let master = pair.master;
    let mut reader = match master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            close_with_error(socket, &format!("clone reader: {e}")).await;
            return;
        }
    };
    let writer = match master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            close_with_error(socket, &format!("take writer: {e}")).await;
            return;
        }
    };
    let writer = Arc::new(std::sync::Mutex::new(writer));
    let master = Arc::new(std::sync::Mutex::new(master));

    info!(
        hen_id = %hen_id,
        workdir = %workdir.display(),
        backend = ?backend,
        persistent = backend == SessionBackendKind::Tmux,
        "shell attached"
    );

    // If this hen has a CLI agent configured (claude, codex, gh copilot) and
    // we haven't yet launched it for this hen this daemon-lifetime, send
    // the launch command into the tmux session ~600ms after attach (giving
    // tmux time to settle). Tracked in `orch.auto_launched` so reconnects
    // don't relaunch.
    if backend == SessionBackendKind::Tmux {
        let key = hen_id.to_string();
        let already = {
            let mut g = orch.auto_launched.lock().await;
            if session_existed || g.contains(&key) {
                g.insert(key.clone());
                true
            } else {
                g.insert(key.clone());
                false
            }
        };
        if let Some(cli) = hen.manifest.agent_kind.launch_cmd() {
            if !already {
                let sess = sess_name.clone();
                let tmux_tmpdir = tmux_dir.clone();
                let cli_cmd = cli.to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = std::process::Command::new("tmux")
                            .env("TMUX_TMPDIR", &tmux_tmpdir)
                            .args(["-L", "coop", "send-keys", "-t", &sess, &cli_cmd, "Enter"])
                            .status();
                    })
                    .await;
                });
            }
        }
    }

    // PTY -> WSS channel (PTY read happens on a blocking thread).
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    debug!(error = %e, "pty read ended");
                    break;
                }
            }
        }
    });

    let (mut ws_tx, mut ws_rx) = socket.split();
    use futures::{SinkExt, StreamExt};

    // Outbound pump.
    let send_master = master.clone();
    let send_task = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if ws_tx.send(WsMsg::Binary(bytes)).await.is_err() {
                break;
            }
        }
        // Try to report exit code if available.
        let code = tokio::task::spawn_blocking(move || child.wait().ok().map(|s| s.exit_code()))
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
        let _ = ws_tx
            .send(WsMsg::Text(format!(r#"{{"type":"exit","code":{code}}}"#)))
            .await;
        let _ = ws_tx.send(WsMsg::Close(None)).await;
        drop(send_master);
    });

    // Inbound pump.
    while let Some(frame) = ws_rx.next().await {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                debug!(error = %e, "ws recv error");
                break;
            }
        };
        match frame {
            WsMsg::Binary(data) => {
                let writer = writer.clone();
                if tokio::task::spawn_blocking(move || writer.lock().unwrap().write_all(&data))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
                    .is_err()
                {
                    warn!("pty write failed");
                    break;
                }
            }
            WsMsg::Text(text) => match serde_json::from_str::<ClientCtrl>(&text) {
                Ok(ClientCtrl::Resize { cols, rows }) => {
                    let m = master.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = m.lock().unwrap().resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    })
                    .await;
                }
                Err(e) => debug!(error = %e, "bad ctrl frame"),
            },
            WsMsg::Close(_) => break,
            WsMsg::Ping(_) | WsMsg::Pong(_) => {}
        }
    }

    // Closing the master signals EOF to the child shell.
    drop(master);
    let _ = send_task.await;
    let _ = reader_task.await;
    info!(hen_id = %hen_id, "shell detached");
}

async fn close_with_error(mut socket: WebSocket, msg: &str) {
    warn!("shell rejected: {msg}");
    let _ = socket
        .send(WsMsg::Text(format!(
            r#"{{"type":"error","message":{}}}"#,
            serde_json::Value::String(msg.to_string())
        )))
        .await;
    let _ = socket.send(WsMsg::Close(None)).await;
}
