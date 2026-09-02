//! `bash` tool: execute a shell command and capture output.

use async_trait::async_trait;
use coopd_core::{CoopTool, CoreError, Result, ToolCapability, ToolCtx, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `bash` tool — runs a single shell command with a timeout.
#[derive(Debug, Default)]
pub struct Bash;

#[derive(Debug, Deserialize)]
struct Input {
    command: String,
    #[serde(default = "default_timeout_s")]
    timeout_s: u64,
}

fn default_timeout_s() -> u64 {
    30
}

#[derive(Debug, Serialize)]
struct Output {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const CAPS: &[ToolCapability] = &[
    ToolCapability::ProcSpawn,
    ToolCapability::FsRead,
    ToolCapability::FsWrite,
];

#[async_trait]
impl CoopTool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn version(&self) -> &'static str {
        "v1.0.0"
    }
    fn capabilities(&self) -> &'static [ToolCapability] {
        CAPS
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            description: "Execute a bash command and return stdout/stderr/exit_code. \
                          Use for shell tasks, build commands, file inspection."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run." },
                    "timeout_s": { "type": "integer", "default": 30, "minimum": 1, "maximum": 600 }
                },
                "required": ["command"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "stdout": { "type": "string" },
                    "stderr": { "type": "string" },
                    "exit_code": { "type": "integer" },
                    "timed_out": { "type": "boolean" },
                    "stdout_truncated": { "type": "boolean" },
                    "stderr_truncated": { "type": "boolean" }
                },
                "required": ["stdout", "stderr", "exit_code", "timed_out", "stdout_truncated", "stderr_truncated"]
            }),
            examples: vec![],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: Value) -> Result<Value> {
        let inp: Input = serde_json::from_value(input)?;
        // workdir is ALWAYS the runner-supplied hen workdir (H3 fix):
        // never trust model input to pick the cwd. The sandbox confines the
        // command to this workdir and scrubs the environment so hen instances
        // are isolated from each other and from host secrets.
        let workdir = ctx.workdir.clone();

        let mut cmd =
            crate::sandbox::bash_command(&workdir, &ctx.agent_id, &inp.command, &ctx.net_policy);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }
        cmd.kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| CoreError::Io(format!("bash spawn: {e}")))?;
        let process_group_id = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Io("bash stdout pipe unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CoreError::Io("bash stderr pipe unavailable".into()))?;
        let timeout_s = inp.timeout_s.clamp(1, 600);
        let dur = std::time::Duration::from_secs(timeout_s);
        let completed = async {
            let (status, stdout, stderr) =
                tokio::try_join!(child.wait(), read_capped(stdout), read_capped(stderr))?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };
        let out = match tokio::time::timeout(dur, completed).await {
            Ok(Ok((status, (stdout, stdout_truncated), (stderr, stderr_truncated)))) => Output {
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                exit_code: status.code().unwrap_or(-1),
                timed_out: false,
                stdout_truncated,
                stderr_truncated,
            },
            Ok(Err(e)) => {
                terminate_process_tree(&mut child, process_group_id).await;
                return Err(CoreError::Io(format!("bash wait: {e}")));
            }
            Err(_) => {
                terminate_process_tree(&mut child, process_group_id).await;
                Output {
                    stdout: String::new(),
                    stderr: format!("timeout after {timeout_s}s"),
                    exit_code: -1,
                    timed_out: true,
                    stdout_truncated: false,
                    stderr_truncated: false,
                }
            }
        };
        Ok(serde_json::to_value(out)?)
    }
}

async fn read_capped<R>(reader: R) -> std::io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut reader = reader;
    let mut bytes = Vec::with_capacity(64 * 1024);
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(bytes.len());
        if count > remaining {
            bytes.extend_from_slice(&buffer[..remaining]);
            truncated = true;
        } else {
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    Ok((bytes, truncated))
}

async fn terminate_process_tree(child: &mut tokio::process::Child, process_group_id: Option<u32>) {
    #[cfg(unix)]
    if let Some(id) = process_group_id
        && let Ok(raw_id) = i32::try_from(id)
    {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        let _ = killpg(Pid::from_raw(raw_id), Signal::SIGKILL);
    }
    #[cfg(windows)]
    if let Some(id) = process_group_id {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .status()
            .await;
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn echo_works() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let tool = Bash;
        let out = tool
            .invoke(&ctx, json!({ "command": "echo hello" }))
            .await
            .unwrap();
        assert_eq!(out["exit_code"], 0);
        assert!(out["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn nonzero_exit() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let out = Bash
            .invoke(&ctx, json!({ "command": "exit 42" }))
            .await
            .unwrap();
        assert_eq!(out["exit_code"], 42);
    }

    #[tokio::test]
    async fn output_is_bounded() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let out = Bash
            .invoke(
                &ctx,
                json!({ "command": "yes x | head -c 1100000", "timeout_s": 10 }),
            )
            .await
            .unwrap();
        assert_eq!(out["stdout_truncated"], true);
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"].as_str().unwrap().len(), MAX_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn timeout_covers_descendants_holding_output_pipes() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let started = std::time::Instant::now();
        let out = Bash
            .invoke(
                &ctx,
                json!({ "command": "sleep 30 & echo $! > child.pid", "timeout_s": 1 }),
            )
            .await
            .unwrap();
        assert_eq!(out["timed_out"], true);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        #[cfg(unix)]
        {
            use nix::sys::signal::kill;
            use nix::unistd::Pid;

            let pid: i32 = std::fs::read_to_string(dir.path().join("child.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            for _ in 0..50 {
                if kill(Pid::from_raw(pid), None).is_err() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("background descendant {pid} survived timeout");
        }
    }
}
