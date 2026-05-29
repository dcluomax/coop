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

use tokio::process::Command;
use tracing::warn;

/// Minimal `PATH` used inside the sandbox when the host `PATH` is unset.
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

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
pub fn bash_command(workdir: &Path, hen_id: &str, command: &str) -> Command {
    let mut cmd = if sandbox_enabled() {
        wrapped_command(workdir, command)
    } else {
        plain_command(command)
    };
    scrub_env(&mut cmd, workdir, hen_id);
    cmd.current_dir(workdir);
    cmd
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
    let path = std::env::var("PATH").unwrap_or_else(|_| FALLBACK_PATH.to_string());
    cmd.env("PATH", path);
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
fn wrapped_command(workdir: &Path, command: &str) -> Command {
    let Ok(canon) = workdir.canonicalize() else {
        warn_degraded_once();
        return plain_command(command);
    };
    if !seatbelt_works() {
        warn_degraded_once();
        return plain_command(command);
    }
    let profile = seatbelt_profile(&canon);
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p")
        .arg(profile)
        .arg("bash")
        .arg("-c")
        .arg(command);
    cmd
}

/// Build a Seatbelt profile confining writes to `wd` (+ `/dev`) and hiding
/// sibling hen workdirs from reads. Later rules override earlier ones.
#[cfg(target_os = "macos")]
fn seatbelt_profile(wd: &Path) -> String {
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
fn wrapped_command(workdir: &Path, command: &str) -> Command {
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
        .arg("--unshare-pid")
        .args(["--ro-bind", "/", "/"])
        .args(["--dev", "/dev"])
        .args(["--proc", "/proc"])
        .args(["--tmpfs", "/tmp"]);
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
fn wrapped_command(_workdir: &Path, command: &str) -> Command {
    warn_degraded_once();
    plain_command(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn env_is_scrubbed_of_host_state() {
        // env_clear replaces the inherited environment: HOME inside the hen
        // shell must be the workdir, not the operator's real home. This proves
        // host env (which may carry secrets) does not leak into the instance.
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "echo $HOME")
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
        let out = bash_command(dir.path(), "bob.coop/worker", "echo $COOP_HEN_ID")
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("bob.coop/worker"), "got: {stdout}");
    }

    #[tokio::test]
    async fn basic_command_runs() {
        let dir = tempdir().unwrap();
        let out = bash_command(dir.path(), "alice.coop/aria", "echo hello")
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello"));
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

        let out = bash_command(&wd, "alice.coop/aria", "echo ok > mine.txt && cat mine.txt")
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
}
