//! Per-hen interactive session backend selection.
//!
//! Coop needs two distinct capabilities:
//! - a PTY stream for the browser shell (portable across Unix/macOS/Windows);
//! - a persistent session that survives browser disconnects and accepts
//!   out-of-band `send-keys` task injection.
//!
//! Today the persistent backend is tmux on Unix-like hosts. Native Windows
//! gets a plain ConPTY-backed shell through `portable-pty`, but it is
//! intentionally marked non-persistent and task-dispatch-disabled until a
//! WSL/tmux or native supervisor backend exists.

use std::path::{Path, PathBuf};

use coopd_core::HenId;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionBackendKind {
    Tmux,
    PlainPty,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCapabilities {
    pub shell: bool,
    pub persistent_session: bool,
    pub task_dispatch: bool,
    pub backend: SessionBackendKind,
    pub os: &'static str,
    pub note: &'static str,
}

#[must_use]
pub fn capabilities() -> SessionCapabilities {
    if tmux_available() {
        SessionCapabilities {
            shell: true,
            persistent_session: true,
            task_dispatch: true,
            backend: SessionBackendKind::Tmux,
            os: std::env::consts::OS,
            note: "persistent tmux session",
        }
    } else {
        SessionCapabilities {
            shell: true,
            persistent_session: false,
            task_dispatch: false,
            backend: SessionBackendKind::PlainPty,
            os: std::env::consts::OS,
            note: plain_pty_note(),
        }
    }
}

#[must_use]
pub fn detect_backend() -> SessionBackendKind {
    if tmux_available() {
        SessionBackendKind::Tmux
    } else {
        SessionBackendKind::PlainPty
    }
}

#[must_use]
pub fn tmux_available() -> bool {
    tmux_available_impl()
}

#[cfg(windows)]
fn tmux_available_impl() -> bool {
    false
}

#[cfg(not(windows))]
fn tmux_available_impl() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn plain_pty_note() -> &'static str {
    "native Windows uses ephemeral ConPTY; use WSL+tmux for persistent sessions"
}

#[cfg(not(windows))]
fn plain_pty_note() -> &'static str {
    "tmux not found; shell is ephemeral and task dispatch is disabled"
}

#[must_use]
pub fn default_shell() -> String {
    default_shell_impl()
}

#[cfg(windows)]
fn default_shell_impl() -> String {
    std::env::var("COMSPEC")
        .or_else(|_| std::env::var("SHELL"))
        .unwrap_or_else(|_| "powershell.exe".to_string())
}

#[cfg(not(windows))]
fn default_shell_impl() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

#[must_use]
pub fn tmux_session_name(hen_id: &HenId) -> String {
    let short = hen_id
        .to_string()
        .rsplit('/')
        .next()
        .unwrap_or("hen")
        .to_string();
    let mut s = String::with_capacity(short.len() + 5);
    s.push_str("coop-");
    for c in short.chars() {
        s.push(if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            c
        } else {
            '_'
        });
    }
    s
}

/// Pick a TMUX_TMPDIR that keeps the resulting socket path
/// (`$TMUX_TMPDIR/tmux-$UID/coop`) below platform Unix-socket path limits.
#[must_use]
pub fn tmux_socket_dir(sess_name: &str, scope: &Path) -> PathBuf {
    let leaf = tmux_socket_leaf(sess_name, scope);
    if let Some(base) = std::env::var_os("COOP_TMUX_TMPDIR") {
        return PathBuf::from(base).join(leaf);
    }
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|p| p.as_os_str().len() < 40)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(leaf)
}

fn tmux_socket_leaf(sess_name: &str, scope: &Path) -> String {
    let prefix: String = sess_name.chars().take(32).collect();
    format!("coop-{prefix}-{:016x}", stable_hash(scope))
}

fn stable_hash(path: &Path) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in path.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn ensure_tmux_socket_dir(tmux_dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(tmux_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_session_name_is_tmux_safe() {
        let id = "local.coop/aria-dev1".parse::<HenId>().unwrap();
        assert_eq!(tmux_session_name(&id), "coop-aria-dev1");
    }

    #[test]
    fn tmux_socket_dir_is_short_and_stable() {
        let scope = PathBuf::from("/tmp/coop-test/workdirs/aria");
        let p = tmux_socket_dir("coop-aria", &scope);
        assert!(p.to_string_lossy().contains("coop-aria"));
        assert_eq!(p, tmux_socket_dir("coop-aria", &scope));
    }

    #[test]
    fn capabilities_are_internally_consistent() {
        let c = capabilities();
        assert!(c.shell);
        assert_eq!(c.persistent_session, c.backend == SessionBackendKind::Tmux);
        assert_eq!(c.task_dispatch, c.backend == SessionBackendKind::Tmux);
    }
}
