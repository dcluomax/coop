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

const MAX_WRITE_BYTES: usize = 4 * 1024 * 1024;
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
        if inp.content.len() > MAX_WRITE_BYTES {
            return Err(CoreError::Other(format!(
                "file_write content exceeds {MAX_WRITE_BYTES} bytes"
            )));
        }
        crate::safe_path::validate_relative_path(&inp.path)?;
        let bytes_written = inp.content.len();
        let display_path = ctx.workdir.join(&inp.path);
        let base = ctx.workdir.clone();
        let user_path = inp.path;
        let content = inp.content;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;

            let dir = cap_std::fs::Dir::open_ambient_dir(&base, cap_std::ambient_authority())
                .map_err(|e| CoreError::Io(format!("open workdir {}: {e}", base.display())))?;
            if let Some(parent) = std::path::Path::new(&user_path).parent()
                && !parent.as_os_str().is_empty()
            {
                dir.create_dir_all(parent)
                    .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", parent.display())))?;
            }
            let mut options = cap_std::fs::OpenOptions::new();
            options.write(true).create(true);
            if inp.append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            let mut file = dir
                .open_with(&user_path, &options)
                .map_err(|e| CoreError::Io(format!("open {user_path}: {e}")))?;
            file.write_all(content.as_bytes())
                .map_err(|e| CoreError::Io(format!("write {user_path}: {e}")))
        })
        .await
        .map_err(|e| CoreError::Io(format!("file_write task: {e}")))??;
        Ok(serde_json::to_value(Output {
            bytes_written,
            path: display_path.display().to_string(),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_relative_open_rejects_symlink_escape() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let ctx = crate::test_ctx(dir.path().to_path_buf());
        assert!(
            FileWrite
                .invoke(&ctx, json!({ "path": "escape/pwned", "content": "nope" }),)
                .await
                .is_err()
        );
        assert!(!outside.path().join("pwned").exists());
    }
}
