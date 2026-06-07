//! Farmhand — remote monitor & steer for the farm (L2 Federation, phase-0).
//!
//! [`RemoteBridge`] is an **outbound** abstraction. Rather than opening an
//! inbound port (hostile to a Pi behind NAT, and at odds with Coop's
//! `safe_origin` / `COOP_PUBLIC` posture), the daemon dials *out* to a relay,
//! pushes [`FarmEvent`]s, and polls the relay for [`RemoteCommand`]s. This is
//! the same shape GitHub Copilot CLI uses for its `/remote` feature, but Coop
//! keeps the relay a **dumb, zero-knowledge pipe**: events and commands are
//! meant to be end-to-end encrypted between the daemon and the operator's
//! device, so even a hosted relay never sees session content or keys (the E2E
//! envelope lands in a later phase; this module defines the seam).
//!
//! ## Fail-open bypass (NOT fail-closed)
//!
//! Unlike the network egress policy (which fails *closed* — a hen that cannot
//! enforce its policy refuses to hatch), the remote bridge is a **side
//! channel**. A relay that is unreachable, slow, or misconfigured must **never**
//! block local execution: hens keep running and the local terminal stays fully
//! authoritative. Callers therefore treat every [`RemoteBridge`] error as
//! non-fatal (log + continue).
//!
//! ## Three-tier posture
//!
//! [`RemoteMode`] mirrors the `off` / `allowlist` / `open` shape of
//! [`crate::net::NetPolicy`]:
//!
//! | mode      | publishes events | accepts commands |
//! |-----------|------------------|------------------|
//! | `off`     | no               | no               |
//! | `view`    | yes (read-only)  | no               |
//! | `control` | yes              | yes (steer)      |
//!
//! ## Open-core boundary
//!
//! The trait, the event/command schema, and the in-process [`LoopbackBridge`]
//! reference implementation are **OSS (Apache-2.0)** — anyone can self-host a
//! relay that speaks this contract. A hosted, multi-tenant relay-as-a-service
//! (push notifications, mobile app) is a separate concern and is **not** part
//! of this crate. This is Federation (L2), wholly distinct from the Market.

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::error::{CoreError, Result};
use crate::hen::HenState;
use crate::ids::HenId;
use crate::job::JobStatus;

/// Remote-control posture for the farm.
///
/// Farm-wide (not per-hen): it governs the whole flock's exposure to the
/// remote interface. Defaults to [`RemoteMode::Off`] — opt-in, like every
/// other egress surface in Coop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteMode {
    /// No farm state ever leaves the daemon; the bridge is inert.
    #[default]
    Off,
    /// Publish a read-only stream of flock/hen events. Inbound commands are
    /// ignored — the remote interface can watch but cannot steer.
    View,
    /// Publish events **and** accept steer commands (gate approvals, prompts,
    /// cancellation, mode switches).
    Control,
}

impl RemoteMode {
    /// Stable identifier used in serialized config and log lines.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::View => "view",
            Self::Control => "control",
        }
    }

    /// Whether this mode emits [`FarmEvent`]s outward.
    #[must_use]
    pub fn publishes_events(self) -> bool {
        matches!(self, Self::View | Self::Control)
    }

    /// Whether this mode honours inbound [`RemoteCommand`]s.
    #[must_use]
    pub fn accepts_commands(self) -> bool {
        matches!(self, Self::Control)
    }
}

/// Farm-wide remote-control settings (the `remote:` block / `COOP_REMOTE_*`).
///
/// Parsed and enforced by the daemon; kept here in the I/O-free core so the
/// validation rules live next to the types they constrain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteSpec {
    /// Posture. Defaults to [`RemoteMode::Off`].
    #[serde(default)]
    pub mode: RemoteMode,
    /// Relay endpoint. Either the `"loopback"` sentinel (the in-process
    /// [`LoopbackBridge`], for development and tests) or a relay URL.
    #[serde(default)]
    pub relay_url: Option<String>,
}

impl RemoteSpec {
    /// Validate the spec.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidManifest`] when:
    /// - `mode` is `view`/`control` but `relay_url` is absent;
    /// - `relay_url` is neither `"loopback"` nor a `ws(s)`/`http(s)` URL;
    /// - a plaintext `http`/`ws` relay points at a non-loopback host (only
    ///   `https`/`wss` may leave the machine — no cleartext session data on the
    ///   wire, consistent with the rest of Coop's egress posture).
    pub fn validate(&self) -> Result<()> {
        match self.mode {
            RemoteMode::Off => Ok(()),
            RemoteMode::View | RemoteMode::Control => {
                let url = self.relay_url.as_deref().ok_or_else(|| {
                    CoreError::InvalidManifest(format!(
                        "remote.relay_url is required when remote.mode is {}",
                        self.mode.as_str()
                    ))
                })?;
                validate_relay_url(url)
            }
        }
    }
}

fn validate_relay_url(url: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(CoreError::InvalidManifest(format!(
            "remote.relay_url {url:?} is invalid: {why}"
        )))
    };
    if url == "loopback" {
        return Ok(());
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return bad("must be \"loopback\" or a ws(s)/http(s) URL");
    };
    if rest.is_empty() {
        return bad("missing host");
    }
    // Host = up to the first '/', ':' (port), or '?'.
    let host = rest
        .split(['/', ':', '?'])
        .next()
        .unwrap_or(rest)
        .trim_start_matches('[')
        .trim_end_matches(']');
    let is_loopback_host = host == "localhost" || host == "::1" || host.starts_with("127.");
    match scheme {
        "https" | "wss" => Ok(()),
        "http" | "ws" => {
            if is_loopback_host {
                Ok(())
            } else {
                bad(
                    "plaintext http/ws is only permitted to a loopback host; use https/wss for a remote relay",
                )
            }
        }
        other => Err(CoreError::InvalidManifest(format!(
            "remote.relay_url {url:?} has unsupported scheme {other:?} (want https/wss, or http/ws to loopback)"
        ))),
    }
}

/// A secret-free snapshot of one hen for the remote flock view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HenSummary {
    /// Hen identity (`<coop_id>/<name>`).
    pub id: HenId,
    /// Display name.
    pub name: String,
    /// Current lifecycle state.
    pub state: HenState,
}

/// The kind of approval a hen is blocked on, mirrored to the remote interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionKind {
    /// A tool/shell-command execution.
    Tool,
    /// A filesystem path access outside the workdir.
    Path,
    /// A network URL fetch.
    Url,
    /// A plan-mode approval.
    Plan,
}

/// Session interaction mode a hen can be switched into from the remote side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    /// Default ask/execute mode.
    Interactive,
    /// Build a plan before acting.
    Plan,
}

/// An event published outward to the remote interface.
///
/// Carries no vault material and no model keys. Payloads such as a pending
/// question are user/session content and are expected to travel inside the
/// (future) E2E envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FarmEvent {
    /// Full flock snapshot (sent on connect / resync).
    FlockSnapshot {
        /// Every known hen, lightweight.
        hens: Vec<HenSummary>,
    },
    /// A hen changed lifecycle state.
    HenStateChanged {
        /// Hen identity.
        id: HenId,
        /// Previous state.
        from: HenState,
        /// New state.
        to: HenState,
    },
    /// A hen is blocked on a permission gate and needs an approve/deny.
    PermissionRequested {
        /// Correlates with the [`RemoteCommand`] that resolves it.
        request_id: String,
        /// Hen awaiting the decision.
        hen_id: HenId,
        /// What is being requested.
        kind: PermissionKind,
        /// Human-readable one-line summary (e.g. the command to be run).
        summary: String,
    },
    /// A hen is waiting for the operator to answer a question.
    AwaitingInput {
        /// Hen awaiting input.
        hen_id: HenId,
        /// The question posed to the operator.
        question: String,
    },
    /// A job changed status.
    JobStatusChanged {
        /// Job identifier.
        job_id: String,
        /// Owning hen.
        hen_id: HenId,
        /// New status.
        status: JobStatus,
    },
}

/// A command received from the remote interface (only honoured in
/// [`RemoteMode::Control`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteCommand {
    /// Approve a pending permission gate.
    ApprovePermission {
        /// The `request_id` from [`FarmEvent::PermissionRequested`].
        request_id: String,
    },
    /// Deny a pending permission gate, optionally steering with feedback.
    DenyPermission {
        /// The `request_id` from [`FarmEvent::PermissionRequested`].
        request_id: String,
        /// Optional guidance on what to do instead.
        feedback: Option<String>,
    },
    /// Submit a new prompt/instruction to a hen.
    SubmitPrompt {
        /// Target hen.
        hen_id: HenId,
        /// The prompt text.
        prompt: String,
    },
    /// Cancel a hen's current operation.
    Cancel {
        /// Target hen.
        hen_id: HenId,
    },
    /// Switch a hen's session mode.
    SwitchMode {
        /// Target hen.
        hen_id: HenId,
        /// Desired mode.
        mode: SessionMode,
    },
}

/// Outbound remote bridge: publish farm events, poll for steer commands.
///
/// Implementations dial *out* to a relay; they never accept inbound
/// connections. Every method is fallible, and callers **must** treat errors as
/// non-fatal (see the module-level "fail-open bypass" note).
#[async_trait]
pub trait RemoteBridge: Send + Sync {
    /// The configured posture.
    fn mode(&self) -> RemoteMode;

    /// Publish an event outward. A no-op (returning `Ok`) when the mode does
    /// not publish events.
    async fn publish(&self, event: FarmEvent) -> Result<()>;

    /// Poll for pending remote commands. Returns an empty vector when the mode
    /// does not accept commands.
    async fn poll(&self) -> Result<Vec<RemoteCommand>>;

    /// Liveness probe for the relay connection.
    async fn health(&self) -> Result<()>;
}

/// In-process reference bridge (phase-0).
///
/// Pure and I/O-free: it buffers published events and inbound commands in
/// memory. It is the local "loopback" relay used for development and tests,
/// and the executable specification of the [`RemoteBridge`] contract that
/// real (networked, E2E-encrypted) bridges must honour:
///
/// - [`RemoteMode::Off`] drops events and yields no commands;
/// - [`RemoteMode::View`] buffers events but yields no commands;
/// - [`RemoteMode::Control`] buffers events and drains injected commands.
#[derive(Debug)]
pub struct LoopbackBridge {
    mode: RemoteMode,
    outbox: Mutex<Vec<FarmEvent>>,
    inbox: Mutex<VecDeque<RemoteCommand>>,
}

impl LoopbackBridge {
    /// Create a loopback bridge in the given mode.
    #[must_use]
    pub fn new(mode: RemoteMode) -> Self {
        Self {
            mode,
            outbox: Mutex::new(Vec::new()),
            inbox: Mutex::new(VecDeque::new()),
        }
    }

    /// Simulate a command arriving from the remote interface. The command is
    /// only ever surfaced by [`RemoteBridge::poll`] when the mode accepts
    /// commands.
    pub fn inject(&self, cmd: RemoteCommand) {
        self.inbox.lock().push_back(cmd);
    }

    /// Take everything published so far (clears the outbox).
    pub fn drain_published(&self) -> Vec<FarmEvent> {
        std::mem::take(&mut *self.outbox.lock())
    }
}

#[async_trait]
impl RemoteBridge for LoopbackBridge {
    fn mode(&self) -> RemoteMode {
        self.mode
    }

    async fn publish(&self, event: FarmEvent) -> Result<()> {
        if self.mode.publishes_events() {
            self.outbox.lock().push(event);
        }
        Ok(())
    }

    async fn poll(&self) -> Result<Vec<RemoteCommand>> {
        if !self.mode.accepts_commands() {
            return Ok(Vec::new());
        }
        Ok(self.inbox.lock().drain(..).collect())
    }

    async fn health(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hen() -> HenId {
        HenId::parse("alice.coop/aria").unwrap()
    }

    #[test]
    fn mode_predicates() {
        assert_eq!(RemoteMode::default(), RemoteMode::Off);
        assert!(!RemoteMode::Off.publishes_events());
        assert!(!RemoteMode::Off.accepts_commands());
        assert!(RemoteMode::View.publishes_events());
        assert!(!RemoteMode::View.accepts_commands());
        assert!(RemoteMode::Control.publishes_events());
        assert!(RemoteMode::Control.accepts_commands());
        assert_eq!(RemoteMode::Control.as_str(), "control");
    }

    #[test]
    fn spec_off_needs_no_url() {
        let s = RemoteSpec::default();
        assert_eq!(s.mode, RemoteMode::Off);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn spec_view_control_require_url() {
        for mode in [RemoteMode::View, RemoteMode::Control] {
            let s = RemoteSpec {
                mode,
                relay_url: None,
            };
            assert!(
                s.validate().is_err(),
                "{} must require a url",
                mode.as_str()
            );
        }
    }

    #[test]
    fn spec_accepts_loopback_and_tls() {
        for url in [
            "loopback",
            "https://relay.example.com",
            "wss://relay.example.com/farm",
            "http://localhost:9700",
            "ws://127.0.0.1:9700/ws",
            "https://relay.example.com:8443/path?x=1",
        ] {
            let s = RemoteSpec {
                mode: RemoteMode::Control,
                relay_url: Some(url.to_string()),
            };
            assert!(s.validate().is_ok(), "{url} should be valid");
        }
    }

    #[test]
    fn spec_rejects_plaintext_to_remote_and_bad_schemes() {
        for url in [
            "http://relay.example.com",  // cleartext to the world
            "ws://relay.example.com/ws", // cleartext to the world
            "ftp://relay.example.com",   // unsupported scheme
            "relay.example.com",         // no scheme
            "https://",                  // no host
        ] {
            let s = RemoteSpec {
                mode: RemoteMode::Control,
                relay_url: Some(url.to_string()),
            };
            assert!(s.validate().is_err(), "{url} should be rejected");
        }
    }

    #[tokio::test]
    async fn loopback_off_drops_everything() {
        let b = LoopbackBridge::new(RemoteMode::Off);
        b.publish(FarmEvent::AwaitingInput {
            hen_id: hen(),
            question: "ok?".into(),
        })
        .await
        .unwrap();
        b.inject(RemoteCommand::Cancel { hen_id: hen() });
        assert!(b.drain_published().is_empty());
        assert!(b.poll().await.unwrap().is_empty());
        b.health().await.unwrap();
    }

    #[tokio::test]
    async fn loopback_view_publishes_but_ignores_commands() {
        let b = LoopbackBridge::new(RemoteMode::View);
        b.publish(FarmEvent::HenStateChanged {
            id: hen(),
            from: HenState::Idle,
            to: HenState::Working,
        })
        .await
        .unwrap();
        b.inject(RemoteCommand::Cancel { hen_id: hen() });
        assert_eq!(b.drain_published().len(), 1);
        // view is read-only: injected commands are never surfaced.
        assert!(b.poll().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn loopback_control_round_trips() {
        let b = LoopbackBridge::new(RemoteMode::Control);
        b.publish(FarmEvent::PermissionRequested {
            request_id: "r1".into(),
            hen_id: hen(),
            kind: PermissionKind::Tool,
            summary: "rm -rf build/".into(),
        })
        .await
        .unwrap();
        b.inject(RemoteCommand::DenyPermission {
            request_id: "r1".into(),
            feedback: Some("use cargo clean".into()),
        });
        b.inject(RemoteCommand::SubmitPrompt {
            hen_id: hen(),
            prompt: "retry".into(),
        });

        assert_eq!(b.drain_published().len(), 1);
        let cmds = b.poll().await.unwrap();
        assert_eq!(cmds.len(), 2);
        // draining empties the inbox.
        assert!(b.poll().await.unwrap().is_empty());
    }

    #[test]
    fn farm_event_is_tagged() {
        let ev = FarmEvent::JobStatusChanged {
            job_id: "j1".into(),
            hen_id: hen(),
            status: JobStatus::Running,
        };
        let v: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "job_status_changed");
        let back: FarmEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn remote_command_is_tagged() {
        let cmd = RemoteCommand::SwitchMode {
            hen_id: hen(),
            mode: SessionMode::Plan,
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v["type"], "switch_mode");
        assert_eq!(v["mode"], "plan");
        let back: RemoteCommand = serde_json::from_value(v).unwrap();
        assert_eq!(back, cmd);
    }
}
