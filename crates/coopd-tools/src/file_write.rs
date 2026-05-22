//! `file_write` tool.

use async_trait::async_trait;
use coopd_core::{CoopTool, CoreError, Result, ToolCapability, ToolCtx, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Write a UTF-8 string to a file (create or overwrite, with optional append).
#[derive(Debug, Default)]
pub struct FileWrite;

#[derive(Debug, Deserialize)]
struct Input {
    path: String,
    content: String,
    #[serde(default)]
    append: bool,
}

#[derive(Debug, Serialize)]
struct Output {
    bytes_written: usize,
    path: String,
}

const CAPS: &[ToolCapability] = &[ToolCapability::FsWrite];

#[async_trait]
impl CoopTool for FileWrite {
    fn name(&self) -> &'static str {
        "file_write"
    }
    fn version(&self) -> &'static str {
        "v1.0.0"
    }
    fn capabilities(&self) -> &'static [ToolCapability] {
        CAPS
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            description: "Write a UTF-8 string to a file. Creates parent dirs.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "append": { "type": "boolean", "default": false }
                },
                "required": ["path", "content"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "bytes_written": { "type": "integer" },
                    "path": { "type": "string" }
                },
                "required": ["bytes_written", "path"]
            }),
            examples: vec![],
        }
    }
    async fn invoke(&self, ctx: &ToolCtx, input: Value) -> Result<Value> {
        let inp: Input = serde_json::from_value(input)?;
        // Need a writable parent — create the requested subdirs *inside*
        // the workdir first (validated path-segment by path-segment), then
        // run the strict safe_resolve which checks the canonicalized parent.
        let user_path = std::path::Path::new(&inp.path);
        if user_path.is_absolute() {
            return Err(CoreError::Other(format!(
                "absolute paths are not allowed: {}",
                inp.path
            )));
        }
        for c in user_path.components() {
            if matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            ) {
                return Err(CoreError::Other(format!(
                    "path traversal not allowed: {}",
                    inp.path
                )));
            }
        }
        let joined = ctx.workdir.join(user_path);
        if let Some(parent) = joined.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", parent.display())))?;
        }
        let p = crate::safe_path::safe_resolve(&ctx.workdir, &inp.path, false)?;
        let bytes = inp.content.as_bytes();
        if inp.append {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&p)
                .await
                .map_err(|e| CoreError::Io(format!("open {}: {e}", p.display())))?;
            f.write_all(bytes)
                .await
                .map_err(|e| CoreError::Io(format!("write: {e}")))?;
        } else {
            tokio::fs::write(&p, bytes)
                .await
                .map_err(|e| CoreError::Io(format!("write {}: {e}", p.display())))?;
        }
        Ok(serde_json::to_value(Output {
            bytes_written: bytes.len(),
            path: p.display().to_string(),
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn write_then_read() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let out = FileWrite
            .invoke(&ctx, json!({ "path": "sub/x.txt", "content": "hi" }))
            .await
            .unwrap();
        assert_eq!(out["bytes_written"], 2);
        let body = tokio::fs::read_to_string(dir.path().join("sub/x.txt"))
            .await
            .unwrap();
        assert_eq!(body, "hi");
    }

    #[tokio::test]
    async fn rejects_absolute_write() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let err = FileWrite
            .invoke(&ctx, json!({ "path": "/tmp/coop-pwn", "content": "x" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("absolute"));
    }

    #[tokio::test]
    async fn rejects_traversal_write() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let err = FileWrite
            .invoke(&ctx, json!({ "path": "../escape.txt", "content": "x" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("traversal") || format!("{err}").contains(".."));
    }
}
