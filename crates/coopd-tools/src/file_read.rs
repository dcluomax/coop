//! `file_read` tool.

use async_trait::async_trait;
use coopd_core::{CoopTool, CoreError, Result, ToolCapability, ToolCtx, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Read a UTF-8 file from disk, with an optional max byte limit.
#[derive(Debug, Default)]
pub struct FileRead;

#[derive(Debug, Deserialize)]
struct Input {
    path: String,
    #[serde(default = "default_max")]
    max_bytes: usize,
}
fn default_max() -> usize {
    1024 * 1024
}

#[derive(Debug, Serialize)]
struct Output {
    content: String,
    bytes: usize,
    truncated: bool,
}

const CAPS: &[ToolCapability] = &[ToolCapability::FsRead];

#[async_trait]
impl CoopTool for FileRead {
    fn name(&self) -> &'static str {
        "file_read"
    }
    fn version(&self) -> &'static str {
        "v1.0.0"
    }
    fn capabilities(&self) -> &'static [ToolCapability] {
        CAPS
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            description: "Read a UTF-8 file and return its contents.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_bytes": { "type": "integer", "default": 1048576 }
                },
                "required": ["path"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "bytes": { "type": "integer" },
                    "truncated": { "type": "boolean" }
                },
                "required": ["content", "bytes", "truncated"]
            }),
            examples: vec![],
        }
    }
    async fn invoke(&self, ctx: &ToolCtx, input: Value) -> Result<Value> {
        let inp: Input = serde_json::from_value(input)?;
        let p = crate::safe_path::safe_resolve(&ctx.workdir, &inp.path, true)?;
        let bytes = tokio::fs::read(&p)
            .await
            .map_err(|e| CoreError::Io(format!("read {}: {e}", p.display())))?;
        let truncated = bytes.len() > inp.max_bytes;
        let slice = if truncated {
            &bytes[..inp.max_bytes]
        } else {
            &bytes[..]
        };
        let content = String::from_utf8_lossy(slice).into_owned();
        Ok(serde_json::to_value(Output {
            bytes: bytes.len(),
            truncated,
            content,
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, "hello world").await.unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let out = FileRead
            .invoke(&ctx, json!({ "path": "a.txt" }))
            .await
            .unwrap();
        assert_eq!(out["content"], "hello world");
        assert_eq!(out["truncated"], false);
    }

    #[tokio::test]
    async fn truncates() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("big.txt");
        tokio::fs::write(&p, "abcdefghij").await.unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let out = FileRead
            .invoke(&ctx, json!({ "path": "big.txt", "max_bytes": 3 }))
            .await
            .unwrap();
        assert_eq!(out["content"], "abc");
        assert_eq!(out["truncated"], true);
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let err = FileRead
            .invoke(&ctx, json!({ "path": "/etc/passwd" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("absolute"));
    }

    #[tokio::test]
    async fn rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        let err = FileRead
            .invoke(&ctx, json!({ "path": "../../etc/passwd" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains(".."));
    }
}
