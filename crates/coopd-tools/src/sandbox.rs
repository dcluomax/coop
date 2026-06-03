//! OS-aware process sandbox for the `bash` tool.
//!
//! Every hen instance runs shell commands confined to **its own workdir** with
//! a **scrubbed environment**, isolating instances from one another and from
//! host secrets. The environment scrub is unconditional and portable; strong
//! filesystem confinement uses an OS-native sandbox when one is available and
//! verified to work:
//!
//! - **macOS**: `sandbox-exec` (Seatbelt) — deny writes outside the workdir,
//!   deny reads of sibling workdirs, re-allow the hen's own workdir.
//! - **Linux**: `bwrap` (Bubblewrap) — read-only bind of `/`, a `tmpfs` over
//!   the workdirs root that masks sibling hens, and a rw bind of the hen's own
//!   workdir.
//! - **Windows / no sandbox tool**: environment-scrub + `cwd` confinement only
//!   (a documented v0.1 limitation — see `SECURITY.md`).
//!
//! Sandboxing can be disabled with `COOP_SANDBOX=0` (escape hatch for debugging
//! or unsupported hosts). When the OS sandbox is unavailable or fails its
//! one-time capability probe, we degrade to the portable env-scrub path and log
//! a single warning so operators know isolation is reduced.

use std::path::Path;
use std::sync::Once;

use coopd_core::ResolvedNetPolicy;
use tokio::process::Command;
use tracing::warn;

/// Fixed `PATH` used inside every sandboxed shell.
///
/// We deliberately do **not** inherit the operator's host `PATH`: it can contain
/// user-writable directories (e.g. `~/bin`, project-local `node_modules/.bin`)
/// that a hostile hen could exploit to shadow real tools (GAP-5). Locking to a
/// fixed set of system directories closes that vector while still resolving the
/// standard coreutils every tool relies on.
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// CPU-time cap (seconds) applied to each sandboxed shell via `ulimit -t`.
/// A backstop above the per-call wall-clock timeout that blunts CPU-spinning
/// commands (GAP-7). Fork-bomb (`RLIMIT_NPROC`) limiting is intentionally not
/// set here — it is per-real-UID on Linux and would starve the shared daemon;
/// that needs cgroups and is tracked for a later phase.
const RLIMIT_CPU_SECS: u64 = 300;

/// Max file-size cap (1024-byte blocks) applied via `ulimit -f` — ~4 GiB.
/// Prevents a single command from filling the disk (GAP-7).
const RLIMIT_FSIZE_BLOCKS: u64 = 4 * 1024 * 1024;

/// Whether sandboxing is enabled. Default on; `COOP_SANDBOX=0` (or `false`)
/// turns it off.
fn sandbox_enabled() -> bool {
    !matches!(
        std::env::var("COOP_SANDBOX").ok().as_deref(),
        Some("0") | Some("false")
    )
}

/// Build a [`tokio::process::Command`] that runs `command` via `bash -c`,
/// confined to `workdir` with a scrubbed environment.
///
/// The caller is responsible for applying a timeout and collecting output.
/// `workdir` is expected to already exist; if it cannot be canonicalized the
/// command still runs with `cwd = workdir` and a scrubbed environment, just
/// without the OS-native filesystem confinement.
#[must_use]
pub fn bash_command(
    workdir: &Path,
    hen_id: &str,
    command: &str,
    net: &ResolvedNetPolicy,
) -> Command {
    // Prepend resource limits so they apply on every path (OS-sandboxed or the
    // degraded fallback) and to whatever the model asked to run.
    let hardened = harden_command(command);
    let mut cmd = if sandbox_enabled() {
        wrapped_command(workdir, &hardened, net)
    } else {
        plain_command(&hardened)
    };
    scrub_env(&mut cmd, workdir, hen_id);
    cmd.current_dir(workdir);
    cmd
}

/// Prefix `command` with `ulimit` resource caps. Failures (e.g. a non-POSIX
/// shell, or an already-lower limit) are silenced so they can never abort the
/// user's command; the caps only ever lower limits, which is always permitted
/// for an unprivileged process.
fn harden_command(command: &str) -> String {
    format!(
        "ulimit -t {RLIMIT_CPU_SECS} 2>/dev/null; ulimit -f {RLIMIT_FSIZE_BLOCKS} 2>/dev/null; {command}"
    )
}

/// Whether **OS-native filesystem confinement** (not just the env scrub) is in
/// effect on this host: sandboxing is enabled and the platform's sandbox tool
/// passed its capability probe.
///
/// Returns `false` on Windows/unsupported hosts, when the probe fails (e.g. CI
/// without user namespaces), or when `COOP_SANDBOX=0`. Useful for tests and for
/// operators auditing whether instance isolation is fully active.
#[must_use]
pub fn isolation_active() -> bool {
    if !sandbox_enabled() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        seatbelt_works()
    }
    #[cfg(target_os = "linux")]
    {
        bwrap_works()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// A plain `bash -c <command>` with no OS wrapper.
fn plain_command(command: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    cmd
}

/// Replace the inherited environment with a minimal, isolated one.
///
/// This prevents host secrets (vault passphrase, API keys, bearer tokens) and
/// one hen's variables from bleeding into another hen's shell.
fn scrub_env(cmd: &mut Command, workdir: &Path, hen_id: &str) {
    cmd.env_clear();
    // Lock PATH to a fixed system value (see FALLBACK_PATH) rather than
    // inheriting the host's, which may contain user-writable directories.
    cmd.env("PATH", FALLBACK_PATH);
    cmd.env("HOME", workdir);
    cmd.env("TMPDIR", workdir);
    cmd.env("TERM", "dumb");
    cmd.env("COOP_HEN_ID", hen_id);
    cmd.env("COOP_HEN_WORKDIR", workdir);
    // Locale / timezone are not secrets and keep tool output sane.
    for key in ["LANG", "LC_ALL", "TZ"] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
}

/// Emit the "isolation degraded" warning at most once per process.
fn warn_degraded_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        warn!(
            "bash tool running without OS-native filesystem sandbox \
             (env is still scrubbed and cwd is confined to the hen workdir); \
             install bubblewrap (Linux) / ensure sandbox-exec (macOS) for \
             full instance isolation, or set COOP_SANDBOX=0 to silence"
        );
    });
}

// ---------------------------------------------------------------------------
// macOS: Seatbelt via sandbox-exec
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn wrapped_command(workdir: &Path, command: &str, net: &ResolvedNetPolicy) -> Command {
    let Ok(canon) = workdir.canonicalize() else {
        warn_degraded_once();
        return plain_command(command);
    };
    if !seatbelt_works() {
        warn_degraded_once();
        return plain_command(command);
    }
    let profile = seatbelt_profile(&canon, net.bash_egress_denied());
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p")
        .arg(profile)
        .arg("bash")
        .arg("-c")
        .arg(command);
    cmd
}

/// Build a Seatbelt profile confining writes to `wd` (+ `/dev`), hiding
/// sibling hen workdirs from reads, and — when `deny_net` is set — denying
/// **all** network egress (`off`/`allowlist` policies). Later rules override
/// earlier ones.
#[cfg(target_os = "macos")]
fn seatbelt_profile(wd: &Path, deny_net: bool) -> String {
    let esc = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    let wd_s = esc(wd);
    let mut profile = String::from("(version 1)\n(allow default)\n");
    profile.push_str("(deny file-write* (subpath \"/\"))\n");
    profile.push_str(&format!("(allow file-write* (subpath \"{wd_s}\"))\n"));
    profile.push_str("(allow file-write* (subpath \"/dev\"))\n");
    if let Some(root) = wd.parent() {
        let root_s = esc(root);
        profile.push_str(&format!("(deny file-read* (subpath \"{root_s}\"))\n"));
        profile.push_str(&format!("(allow file-read* (subpath \"{wd_s}\"))\n"));
    }
    if deny_net {
        // macOS has no per-host network allowlisting and a shared loopback, so
        // strict policies deny ALL direct socket egress for bash/tmux. Host-
        // scoped egress is delivered only via the in-process `http` tool. See
        // docs/net-isolation.md (§4, honesty contract).
        profile.push_str("(deny network*)\n");
    }
    profile
}

/// One-time probe: does `sandbox-exec` actually run on this host?
#[cfg(target_os = "macos")]
fn seatbelt_works() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Linux: Bubblewrap (bwrap)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn wrapped_command(workdir: &Path, command: &str, net: &ResolvedNetPolicy) -> Command {
    let Ok(canon) = workdir.canonicalize() else {
        warn_degraded_once();
        return plain_command(command);
    };
    if !bwrap_works() {
        warn_degraded_once();
        return plain_command(command);
    }
    let mut cmd = Command::new("bwrap");
    cmd.arg("--die-with-parent")
        // Run in a fresh session with no controlling terminal so a hostile
        // command cannot inject keystrokes into the operator's tty via the
        // TIOCSTI ioctl (CVE-2017-5226).
        .arg("--new-session")
        .arg("--unshare-pid")
        .args(["--ro-bind", "/", "/"])
        .args(["--dev", "/dev"])
        .args(["--proc", "/proc"])
        .args(["--tmpfs", "/tmp"]);
    // Network isolation: strict policies (off/allowlist) get an empty network
    // namespace, so the hen's bash/tmux has NO route to anywhere — raw sockets,
    // `curl --noproxy`, direct-IP connects all fail with ENETUNREACH. Under
    // `allowlist`, host-scoped egress is delivered via the in-process `http`
    // tool (v1). See docs/net-isolation.md.
    if net.bash_egress_denied() {
        cmd.arg("--unshare-net");
    }
    // Mask sibling hen workdirs with an empty tmpfs, then re-expose our own.
    if let Some(root) = canon.parent() {
        cmd.arg("--tmpfs").arg(root);
    }
    cmd.arg("--bind").arg(&canon).arg(&canon);
    cmd.arg("--chdir").arg(&canon);
    cmd.arg("bash").arg("-c").arg(command);
    cmd
}

/// One-time probe: can `bwrap` create a namespace on this host? (Many CI and
/// container environments disable unprivileged user namespaces.)
#[cfg(target_os = "linux")]
fn bwrap_works() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::process::Command::new("bwrap")
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "/bin/sh",
                "-c",
                "exit 0",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Other platforms (Windows, etc.): env-scrub + cwd only
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wrapped_command(_workdir: &Path, command: &str, _net: &ResolvedNetPolicy) -> Command {
    warn_degraded_once();
    plain_command(command)
}

/// Whether this host can enforce a network policy stricter than `open`
/// (`off`/`allowlist`) for the `bash`/tmux egress surface. Mirrors
/// [`isolation_active`]: Linux needs a working `bwrap` (user namespaces for
/// `--unshare-net`); macOS needs `sandbox-exec` (Seatbelt `(deny network*)`).
///
/// The hatch path consults this and **refuses to hatch** a strict-policy hen
/// when it returns `false`, rather than silently running with open egress.
#[must_use]
pub fn net_isolation_available() -> bool {
    if !sandbox_enabled() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        seatbelt_works()
    }
    #[cfg(target_os = "linux")]
    {
        bwrap_works()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::{NetAllow, NetPolicy, NetworkSpec};
    use tempfile::tempdir;

    /// Default (open) policy for tests that don't exercise network isolation.
    fn open_net() -> ResolvedNetPolicy {
        ResolvedNetPolicy::default()
    }

    #[tokio::test]
    async fn env_is_scrubbed_of_host_state() {
        // env_clear replaces the inherited environment: HOME inside the hen
        // shell must be the workdir, not the operator's real home. This proves
        // host env (which may carry secrets) does not leak into the instance.
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "echo $HOME", &open_net())
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let canon = dir.path().canonicalize().unwrap();
        assert!(
            stdout.trim() == dir.path().to_string_lossy()
                || stdout.trim() == canon.to_string_lossy(),
            "HOME not confined to workdir: {stdout}"
        );
        let host_home = std::env::var("HOME").unwrap_or_default();
        if !host_home.is_empty() {
            assert_ne!(stdout.trim(), host_home, "host HOME leaked into hen shell");
        }
    }

    #[tokio::test]
    async fn hen_env_vars_are_present() {
        let dir = tempdir().unwrap();
        let out = bash_command(
            dir.path(),
            "bob.coop/worker",
            "echo $COOP_HEN_ID",
            &open_net(),
        )
        .output()
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("bob.coop/worker"), "got: {stdout}");
    }

    #[tokio::test]
    async fn basic_command_runs() {
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "echo hello", &open_net())
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
    }

    #[tokio::test]
    async fn path_is_locked_to_system_value() {
        // The host PATH must not leak in: a hostile hen could otherwise rely on
        // a user-writable dir on the operator's PATH to shadow real binaries.
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "echo \"$PATH\"", &open_net())
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), FALLBACK_PATH, "PATH not locked: {stdout}");
    }

    #[tokio::test]
    async fn hardening_preserves_exit_code() {
        // The ulimit prologue must be transparent to the command's exit status.
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "exit 42", &open_net())
            .output()
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(42), "prologue altered exit code");
    }

    #[tokio::test]
    async fn cpu_rlimit_is_applied() {
        // ulimit -t should report our cap (or a lower host-imposed one), never
        // "unlimited".
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "ulimit -t", &open_net())
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let reported = stdout.trim();
        assert_ne!(reported, "unlimited", "CPU rlimit not applied");
        let secs: u64 = reported.parse().expect("numeric ulimit -t");
        assert!(secs <= RLIMIT_CPU_SECS, "CPU rlimit too high: {secs}");
    }

    // ---- End-to-end OS-sandbox isolation -------------------------------
    // These assert real cross-instance confinement through the actual
    // bash_command() path. They only run when the OS sandbox is active
    // (skipped on Windows / CI without userns), so they never false-fail.

    #[tokio::test]
    async fn sibling_workdir_is_unreadable() {
        if !isolation_active() {
            eprintln!("skip: OS sandbox inactive on this host");
            return;
        }
        let root = tempdir().unwrap();
        let alice = root.path().join("alice-coop__aria");
        let bob = root.path().join("bob-coop__aria");
        std::fs::create_dir_all(&alice).unwrap();
        std::fs::create_dir_all(&bob).unwrap();
        std::fs::write(bob.join("secret.txt"), "bob-secret").unwrap();

        // alice tries to read bob's secret via an absolute path.
        let bob_secret = bob.join("secret.txt");
        let out = bash_command(
            &alice,
            "alice.coop/aria",
            &format!("cat '{}'", bob_secret.display()),
            &open_net(),
        )
        .output()
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("bob-secret"),
            "ISOLATION BREACH: alice read bob's workdir: {stdout}"
        );
        assert!(
            !out.status.success(),
            "expected non-zero exit when reading sibling workdir"
        );
    }

    #[tokio::test]
    async fn write_outside_workdir_is_denied() {
        if !isolation_active() {
            eprintln!("skip: OS sandbox inactive on this host");
            return;
        }
        let root = tempdir().unwrap();
        let wd = root.path().join("alice-coop__aria");
        std::fs::create_dir_all(&wd).unwrap();
        let escape = root.path().join("escape.txt");

        let out = bash_command(
            &wd,
            "alice.coop/aria",
            &format!("echo pwned > '{}'", escape.display()),
            &open_net(),
        )
        .output()
        .await
        .unwrap();
        assert!(
            !escape.exists(),
            "ISOLATION BREACH: wrote outside workdir at {}",
            escape.display()
        );
        assert!(
            !out.status.success(),
            "expected non-zero exit on denied write"
        );
    }

    #[tokio::test]
    async fn write_inside_own_workdir_is_allowed() {
        if !isolation_active() {
            eprintln!("skip: OS sandbox inactive on this host");
            return;
        }
        let root = tempdir().unwrap();
        let wd = root.path().join("alice-coop__aria");
        std::fs::create_dir_all(&wd).unwrap();

        let out = bash_command(
            &wd,
            "alice.coop/aria",
            "echo ok > mine.txt && cat mine.txt",
            &open_net(),
        )
        .output()
        .await
        .unwrap();
        assert!(out.status.success(), "own-workdir write should succeed");
        assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
        assert_eq!(
            std::fs::read_to_string(wd.join("mine.txt")).unwrap().trim(),
            "ok"
        );
    }

    // ---- Per-hen network isolation (adversarial) -----------------------
    // A hen under `off`/`allowlist` must have NO direct egress from bash:
    // the OS sandbox (Linux empty netns / macOS Seatbelt deny network*)
    // makes raw sockets, curl --noproxy, direct-IP connects all fail.

    fn allowlist(host: &str) -> ResolvedNetPolicy {
        ResolvedNetPolicy::from_spec(Some(&NetworkSpec {
            policy: NetPolicy::Allowlist,
            allow: vec![NetAllow {
                host: host.to_string(),
                ports: vec![443],
            }],
        }))
    }

    fn off() -> ResolvedNetPolicy {
        ResolvedNetPolicy::from_spec(Some(&NetworkSpec {
            policy: NetPolicy::Off,
            allow: vec![],
        }))
    }

    /// A raw TCP `connect()` to a public IP must fail when the hen's policy is
    /// `off`. Skipped unless the OS network sandbox is enforceable here.
    #[tokio::test]
    async fn off_policy_denies_bash_egress() {
        if !net_isolation_available() {
            eprintln!("skip: OS network sandbox unavailable on this host");
            return;
        }
        let root = tempdir().unwrap();
        let wd = root.path().join("alice-coop__aria");
        std::fs::create_dir_all(&wd).unwrap();
        // bash has no curl guarantee; use bash's own /dev/tcp pseudo-device,
        // which performs a real connect(2) via the shell. A blocked netns
        // returns non-zero ("Network is unreachable").
        let out = bash_command(
            &wd,
            "alice.coop/aria",
            "exec 3<>/dev/tcp/1.1.1.1/443 && echo REACHED || echo BLOCKED",
            &off(),
        )
        .output()
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("REACHED"),
            "ISOLATION BREACH: off-policy hen reached the network: {stdout}"
        );
    }

    /// Under `allowlist`, bash still gets NO direct egress (host-scoped egress
    /// is delivered only via the in-process `http` tool in v1).
    #[tokio::test]
    async fn allowlist_policy_denies_direct_bash_egress() {
        if !net_isolation_available() {
            eprintln!("skip: OS network sandbox unavailable on this host");
            return;
        }
        let root = tempdir().unwrap();
        let wd = root.path().join("alice-coop__aria");
        std::fs::create_dir_all(&wd).unwrap();
        let out = bash_command(
            &wd,
            "alice.coop/aria",
            "exec 3<>/dev/tcp/1.1.1.1/443 && echo REACHED || echo BLOCKED",
            &allowlist("api.anthropic.com"),
        )
        .output()
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("REACHED"),
            "ISOLATION BREACH: allowlist hen got direct bash egress: {stdout}"
        );
    }

    /// `open` policy leaves bash egress unchanged (no network args added).
    #[test]
    fn open_policy_adds_no_network_restriction() {
        assert!(!open_net().bash_egress_denied());
        assert!(off().bash_egress_denied());
        assert!(allowlist("x.com").bash_egress_denied());
    }
}
