//! `delegate` tool — hand a subtask to another Hen on the same farm.
//!
//! This is the building block of the "organization" tier: a manager Hen calls
//! `delegate` to dispatch work to a specialist Hen and receives its result.
//! The actual dispatch + wait is provided by the host via
//! [`coopd_core::Delegator`] (carried on [`ToolCtx`]); this tool only validates
//! inputs and shapes the result.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use coopd_core::{
    CoopTool, CoreError, DelegationRequest, HenId, Result, ToolCapability, ToolCtx, ToolSchema,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Default seconds the caller waits for a delegated sub-job to finish.
const DEFAULT_DELEGATE_TIMEOUT_SECS: u64 = 180;

/// Delegate a subtask to another Hen and wait for its result.
#[derive(Debug, Default)]
pub struct Delegate;

#[derive(Debug, Deserialize)]
struct Input {
    /// Target Hen id, e.g. `local.coop/scout`.
    hen: String,
    /// The subtask to perform.
    prompt: String,
}

/// Effective delegation wait, from `COOP_DELEGATE_TIMEOUT_SECS`.
fn delegate_timeout() -> Duration {
    let secs = std::env::var("COOP_DELEGATE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_DELEGATE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

const CAPS: &[ToolCapability] = &[];

#[async_trait]
impl CoopTool for Delegate {
    fn name(&self) -> &'static str {
        "delegate"
    }
    fn version(&self) -> &'static str {
        "v1.0.0"
    }
    fn capabilities(&self) -> &'static [ToolCapability] {
        CAPS
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            description: "Delegate a subtask to another hen on this farm and wait for its result. \
                 Use this to coordinate specialist hens (e.g. a research or writing hen). \
                 Returns the sub-job's status and output."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "hen": {
                        "type": "string",
                        "description": "Target hen id, e.g. local.coop/scout"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The subtask for the target hen to perform"
                    }
                },
                "required": ["hen", "prompt"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "hen": { "type": "string" },
                    "job_id": { "type": "string" },
                    "status": { "type": "string" },
                    "output": { "type": "string" }
                },
                "required": ["hen", "job_id", "status", "output"]
            }),
            examples: vec![],
        }
    }
    async fn invoke(&self, ctx: &ToolCtx, input: Value) -> Result<Value> {
        let inp: Input = serde_json::from_value(input)?;
        let Some(delegator) = ctx.delegator.clone() else {
            return Err(CoreError::Other(
                "delegation is not available in this context".to_string(),
            ));
        };
        if inp.prompt.trim().is_empty() {
            return Err(CoreError::Other(
                "delegate: prompt must not be empty".to_string(),
            ));
        }
        let from = HenId::parse(&ctx.agent_id)?;
        let to = HenId::parse(&inp.hen)
            .map_err(|e| CoreError::Other(format!("invalid target hen `{}`: {e}", inp.hen)))?;
        // Fast local check for nicer errors; the orchestrator re-validates
        // authoritatively (self-delegation, depth cap).
        coopd_core::validate_delegation(&from, &to, ctx.delegation_depth + 1)?;

        // Prefer the remaining job deadline, but never wait longer than the
        // configured delegation timeout.
        let remaining = ctx.deadline.saturating_duration_since(Instant::now());
        let timeout = remaining
            .min(delegate_timeout())
            .max(Duration::from_secs(1));

        let outcome = delegator
            .delegate(DelegationRequest {
                from,
                to: to.clone(),
                prompt: inp.prompt,
                parent_depth: ctx.delegation_depth,
                timeout,
            })
            .await?;
        Ok(json!({
            "hen": to.to_string(),
            "job_id": outcome.job_id,
            "status": format!("{:?}", outcome.status),
            "output": outcome.output,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use coopd_core::{DelegationOutcome, Delegator, JobStatus};
    use std::sync::Arc;

    struct MockDelegator {
        status: JobStatus,
        output: String,
    }

    #[async_trait]
    impl Delegator for MockDelegator {
        async fn delegate(&self, req: DelegationRequest) -> Result<DelegationOutcome> {
            // Echo the request back so the test can assert wiring.
            Ok(DelegationOutcome {
                job_id: "job-xyz".to_string(),
                status: self.status,
                output: format!("{} :: {}", self.output, req.prompt),
                depth: req.parent_depth + 1,
            })
        }
    }

    fn ctx_with(delegator: Option<Arc<dyn Delegator>>, depth: u32) -> ToolCtx {
        ToolCtx {
            agent_id: "alice.coop/aria".into(),
            session_id: "test".into(),
            lease_id: None,
            workdir: std::env::temp_dir(),
            net_policy: coopd_core::ResolvedNetPolicy::default(),
            deadline: Instant::now() + Duration::from_secs(30),
            delegation_depth: depth,
            delegator,
        }
    }

    #[tokio::test]
    async fn delegates_and_shapes_output() {
        let mock = Arc::new(MockDelegator {
            status: JobStatus::Done,
            output: "did it".into(),
        });
        let ctx = ctx_with(Some(mock), 0);
        let out = Delegate
            .invoke(
                &ctx,
                json!({ "hen": "alice.coop/scout", "prompt": "summarize X" }),
            )
            .await
            .unwrap();
        assert_eq!(out["hen"], "alice.coop/scout");
        assert_eq!(out["job_id"], "job-xyz");
        assert_eq!(out["status"], "Done");
        assert_eq!(out["output"], "did it :: summarize X");
    }

    #[tokio::test]
    async fn rejects_self_delegation() {
        let mock = Arc::new(MockDelegator {
            status: JobStatus::Done,
            output: "x".into(),
        });
        let ctx = ctx_with(Some(mock), 0);
        let err = Delegate
            .invoke(&ctx, json!({ "hen": "alice.coop/aria", "prompt": "loop" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("itself"));
    }

    #[tokio::test]
    async fn rejects_when_depth_would_exceed() {
        let mock = Arc::new(MockDelegator {
            status: JobStatus::Done,
            output: "x".into(),
        });
        let ctx = ctx_with(Some(mock), coopd_core::MAX_DELEGATION_DEPTH);
        let err = Delegate
            .invoke(&ctx, json!({ "hen": "alice.coop/scout", "prompt": "deep" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("exceeds the maximum"));
    }

    #[tokio::test]
    async fn errors_without_delegator() {
        let ctx = ctx_with(None, 0);
        let err = Delegate
            .invoke(&ctx, json!({ "hen": "alice.coop/scout", "prompt": "x" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not available"));
    }

    #[tokio::test]
    async fn rejects_empty_prompt() {
        let mock = Arc::new(MockDelegator {
            status: JobStatus::Done,
            output: "x".into(),
        });
        let ctx = ctx_with(Some(mock), 0);
        let err = Delegate
            .invoke(&ctx, json!({ "hen": "alice.coop/scout", "prompt": "   " }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("must not be empty"));
    }
}
