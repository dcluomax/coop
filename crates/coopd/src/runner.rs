//! Hen runner: per-job task executing the reason / tool loop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coopd_core::{
    BrainAdapter, CoreError, Hen, Job, ReasonRequest, Result, ToolCtx,
    brain::{ContentBlock, Message, MessageContent},
};
use coopd_tools::Registry;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::brain_factory::BrainFactory;
use crate::orchestrator::OrchHandle;

/// Maximum reason/tool turns per job (v0.1 safety cap).
const MAX_TURNS: u32 = 16;

/// Spawn an async task that runs `job` to completion and persists results.
pub fn spawn_job_task(
    orch: OrchHandle,
    tools: Arc<Registry>,
    brain_factory: Arc<Mutex<BrainFactory>>,
    workdir_base: std::path::PathBuf,
    job: Job,
) {
    tokio::spawn(async move {
        let job_id = job.id.clone();
        let hen_id = job.hen_id.clone();
        info!(%job_id, %hen_id, "job runner starting");
        if let Err(e) = run_job(&orch, &tools, &brain_factory, &workdir_base, job).await {
            warn!(%job_id, error = %e, "job runner failed");
        }
    });
}

async fn run_job(
    orch: &OrchHandle,
    tools: &Registry,
    brain_factory: &Mutex<BrainFactory>,
    workdir_base: &std::path::Path,
    mut job: Job,
) -> Result<()> {
    use coopd_core::HenState;

    // Mark hen WORKING + job RUNNING.
    let _ = orch
        .transition_hen(job.hen_id.clone(), HenState::Working)
        .await;
    job.mark_running();
    orch.update_job(job.clone()).await?;

    let outcome: Result<String> = async {
        let hen: Hen = orch.get_hen(job.hen_id.clone()).await?;
        let manifest = hen.manifest.clone();
        let brain = {
            let bf = brain_factory.lock().await;
            bf.build(&manifest).await?
        };
        let workdir = workdir_base.join(hen.id.workdir_key());
        tokio::fs::create_dir_all(&workdir)
            .await
            .map_err(|e| CoreError::Io(format!("mkdir workdir: {e}")))?;
        reason_loop(orch, tools, brain.as_ref(), &hen, &workdir, &mut job).await
    }
    .await;

    match outcome {
        Ok(text) => {
            job.mark_done(text);
            orch.update_job(job.clone()).await?;
            info!(job_id = %job.id, turns = job.turns, "job done");
        }
        Err(e) => {
            job.mark_failed(e.to_string());
            orch.update_job(job.clone()).await?;
        }
    }

    // Persist an episodic memory of this job (success or failure) so the hen
    // continues from context next time. Retention pruning happens in the
    // orchestrator. Best-effort: memory is an enhancement, never fatal.
    if let Some(entry) = coopd_core::MemoryEntry::from_job(&job) {
        if let Err(e) = orch.record_memory(entry).await {
            warn!(job_id = %job.id, error = %e, "failed to record episodic memory");
        }
    }

    let _ = orch
        .transition_hen(job.hen_id.clone(), HenState::Idle)
        .await;
    // Drain the next queued job for this hen, if any. Best-effort.
    let _ = orch.dispatch_next_queued(job.hen_id.clone()).await;
    Ok(())
}

async fn reason_loop(
    orch: &OrchHandle,
    tools: &Registry,
    brain: &dyn BrainAdapter,
    hen: &Hen,
    workdir: &std::path::Path,
    job: &mut Job,
) -> Result<String> {
    let manifest = &hen.manifest;
    let mut system = manifest
        .personality
        .as_ref()
        .and_then(|p| p.system_prompt.clone())
        .unwrap_or_else(|| {
            "You are a hen on a Coop farm. Use tools when needed. Be concise.".to_string()
        });

    // Persistent memory: prepend the hen's recent episodes so it continues
    // from prior context instead of starting from zero. Count is capped and
    // overridable via COOP_MEMORY_CONTEXT_ENTRIES (0 disables injection).
    let mem_limit = std::env::var("COOP_MEMORY_CONTEXT_ENTRIES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(coopd_core::DEFAULT_MEMORY_CONTEXT_ENTRIES);
    if mem_limit > 0 {
        match orch.load_memories(hen.id.clone(), Some(mem_limit)).await {
            Ok(mems) if !mems.is_empty() => {
                let ctx = coopd_core::render_memory_context(&mems);
                system = format!("{system}\n\n{ctx}");
                debug!(hen_id = %hen.id, episodes = mems.len(), "injected memory context");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(hen_id = %hen.id, error = %e, "load memories failed; continuing without")
            }
        }
    }

    let mut messages: Vec<Message> = vec![Message {
        role: "user".into(),
        content: job.prompt.clone().into(),
    }];

    // If the hen is currently leased, advertise only the lease-allowed
    // subset of tools to the brain so the model can't even reason about
    // calling a denied tool. Enforcement at invoke_tool is the hard wall;
    // this is just hygiene.
    let visible_tools: Vec<String> = match &hen.lease {
        coopd_core::LeaseStatus::Owner => manifest.tools.clone(),
        coopd_core::LeaseStatus::LeasedOut { .. } | coopd_core::LeaseStatus::LeasedIn { .. } => {
            match manifest
                .lease
                .as_ref()
                .and_then(|l| l.allowed_tools.as_ref())
            {
                Some(allow) => manifest
                    .tools
                    .iter()
                    .filter(|t| allow.iter().any(|a| a == *t))
                    .cloned()
                    .collect(),
                None => manifest.tools.clone(),
            }
        }
    };

    let tool_schemas = tools
        .schemas_for(&visible_tools)
        .into_iter()
        .map(|e| {
            json!({
                "name": e.name,
                "description": e.description,
                "input_schema": e.input_schema,
            })
        })
        .collect::<Vec<_>>();

    let mut last_text = String::new();
    for turn in 0..MAX_TURNS {
        job.turns = turn + 1;
        let req = ReasonRequest {
            system: system.clone(),
            messages: messages.clone(),
            tools: tool_schemas.clone(),
            temperature: 0.7,
            max_tokens: 4096,
            stop_seq: vec![],
            stream: false,
            metadata: Default::default(),
        };
        let resp = brain.reason(req).await?;
        job.grain_spent = job.grain_spent.saturating_add(resp.cost.grain);

        // Replay the assistant turn structurally: any text plus the exact
        // `tool_use` blocks it emitted, then answer each with a correlated
        // `tool_result` block in the following user turn. This preserves
        // tool-call fidelity across multi-turn conversations (no plaintext
        // re-encoding, ids threaded through).
        let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();
        for block in resp.content {
            match block {
                ContentBlock::Text { text } => {
                    if !text.trim().is_empty() {
                        last_text = text.clone();
                    }
                    assistant_blocks.push(ContentBlock::Text { text });
                }
                ContentBlock::Thinking { .. } | ContentBlock::ToolResult { .. } => {}
                ContentBlock::ToolCall { id, name, input } => {
                    let (result, is_error) =
                        invoke_tool(tools, workdir, hen, job, &name, input.clone()).await;
                    assistant_blocks.push(ContentBlock::ToolCall {
                        id: id.clone(),
                        name,
                        input,
                    });
                    tool_result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: result,
                        is_error,
                    });
                }
            }
        }
        if tool_result_blocks.is_empty() {
            return Ok(last_text);
        }
        messages.push(Message {
            role: "assistant".into(),
            content: MessageContent::Blocks(assistant_blocks),
        });
        messages.push(Message {
            role: "user".into(),
            content: MessageContent::Blocks(tool_result_blocks),
        });
        if resp.finish_reason == "end_turn" {
            return Ok(last_text);
        }
    }
    Err(CoreError::Other(format!(
        "max turns ({MAX_TURNS}) exhausted"
    )))
}

async fn invoke_tool(
    tools: &Registry,
    workdir: &std::path::Path,
    hen: &Hen,
    job: &Job,
    name: &str,
    input: serde_json::Value,
) -> (String, bool) {
    // Lease enforcement (hard wall): if the hen is leased and the policy
    // either restricts the tool list or disallows this name, refuse before
    // we even look the tool up.
    let lease_id = match &hen.lease {
        coopd_core::LeaseStatus::Owner => None,
        coopd_core::LeaseStatus::LeasedOut { lease_id, .. }
        | coopd_core::LeaseStatus::LeasedIn { lease_id, .. } => Some(lease_id.clone()),
    };
    if lease_id.is_some() {
        if let Some(allow) = hen
            .manifest
            .lease
            .as_ref()
            .and_then(|l| l.allowed_tools.as_ref())
        {
            if !allow.iter().any(|t| t == name) {
                warn!(tool = name, %job.id, "lease policy denied tool call");
                return (
                    format!("ERROR: tool `{name}` is not permitted by the active lease policy"),
                    true,
                );
            }
        }
    }
    let Some(tool) = tools.get(name) else {
        return (format!("ERROR: unknown tool `{name}`"), true);
    };
    let ctx = ToolCtx {
        agent_id: job.hen_id.to_string(),
        session_id: job.id.clone(),
        lease_id,
        workdir: workdir.to_path_buf(),
        net_policy: coopd_core::ResolvedNetPolicy::from_spec(hen.manifest.network.as_ref()),
        deadline: Instant::now() + Duration::from_secs(120),
    };
    debug!(tool = name, %job.id, "invoking tool");
    match tool.invoke(&ctx, input).await {
        Ok(v) => (
            serde_json::to_string(&v).unwrap_or_else(|e| format!("ERROR serialize: {e}")),
            false,
        ),
        Err(e) => (format!("ERROR: {e}"), true),
    }
}
