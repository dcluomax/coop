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
}

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
                    "timed_out": { "type": "boolean" }
                },
                "required": ["stdout", "stderr", "exit_code", "timed_out"]
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

        let dur = std::time::Duration::from_secs(inp.timeout_s.min(600));
        let result = tokio::time::timeout(dur, cmd.output()).await;
        let out = match result {
            Ok(Ok(o)) => Output {
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
                exit_code: o.status.code().unwrap_or(-1),
                timed_out: false,
            },
            Ok(Err(e)) => return Err(CoreError::Io(format!("bash spawn: {e}"))),
            Err(_) => Output {
                stdout: String::new(),
                stderr: format!("timeout after {}s", inp.timeout_s),
                exit_code: -1,
                timed_out: true,
            },
        };
        Ok(serde_json::to_value(out)?)
    }
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
}
