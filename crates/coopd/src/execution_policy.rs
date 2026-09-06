use coopd_core::{CoreError, Hen, HenState, LeaseStatus, Result};

const DEFAULT_MAX_PROMPT_BYTES: usize = 256 * 1024;

pub(crate) fn check_prompt_len(prompt: &str) -> Result<()> {
    let max = std::env::var("COOP_MAX_PROMPT_BYTES")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_PROMPT_BYTES);
    check_prompt_limit(prompt, max)
}

fn check_prompt_limit(prompt: &str, max: usize) -> Result<()> {
    if max != 0 && prompt.len() > max {
        return Err(CoreError::PayloadTooLarge(format!(
            "prompt is {} bytes; limit is {max} (set COOP_MAX_PROMPT_BYTES to adjust)",
            prompt.len()
        )));
    }
    Ok(())
}

pub(crate) fn enforce_lease_topic(hen: &Hen, prompt: &str) -> std::result::Result<(), String> {
    if !matches!(hen.lease, LeaseStatus::Owner)
        && let Some(filter) = hen
            .manifest
            .lease
            .as_ref()
            .and_then(|lease| lease.topic_filter.as_ref())
    {
        filter.check(prompt)?;
    }
    Ok(())
}

pub(crate) fn ensure_hatch_supported(hen: &Hen) -> Result<()> {
    if let Some(net) = &hen.manifest.network
        && net.policy.requires_enforcement()
    {
        if !coopd_tools::sandbox::net_isolation_available() {
            return Err(CoreError::PermissionDenied(format!(
                "refusing to hatch {}: network policy `{}` cannot be enforced on this host \
                 (no user namespaces / Seatbelt). Set network.policy: open to run without \
                 egress isolation, or run on a supported host.",
                hen.id,
                net.policy.as_str()
            )));
        }
        if hen.manifest.agent_kind.is_tmux_agent() {
            return Err(CoreError::PermissionDenied(format!(
                "refusing to hatch {}: network policy `{}` is not yet enforceable for \
                 agent_kind `{}` (tmux CLI agents are an unconfined egress surface in v1). \
                 Use agent_kind: anthropic, or network.policy: open.",
                hen.id,
                net.policy.as_str(),
                hen.manifest.agent_kind.as_str()
            )));
        }
    }
    Ok(())
}

pub(crate) fn check_job(hen: &Hen, prompt: &str) -> Result<()> {
    check_prompt_len(prompt)?;
    if prompt.trim().is_empty() {
        return Err(CoreError::InvalidInput("prompt is empty".into()));
    }
    if hen.state == HenState::Archived {
        return Err(CoreError::Conflict(
            "archived Hens cannot accept new jobs".into(),
        ));
    }
    ensure_hatch_supported(hen)?;
    enforce_lease_topic(hen, prompt).map_err(CoreError::PermissionDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::{AgentKind, AgentManifest, HenId, NetPolicy, NetworkSpec};

    fn hen() -> Hen {
        Hen::new(
            HenId::parse("local.coop/aria").unwrap(),
            AgentManifest::minimal("aria".into()),
        )
    }

    #[test]
    fn prompt_limit_counts_bytes_and_zero_disables_the_cap() {
        assert!(check_prompt_limit("test", 4).is_ok());
        assert!(matches!(
            check_prompt_limit("tests", 4),
            Err(CoreError::PayloadTooLarge(_))
        ));
        assert!(check_prompt_limit("tests", 0).is_ok());
    }

    #[test]
    fn jobs_reject_empty_prompts_and_archived_hens() {
        let mut hen = hen();
        assert!(matches!(
            check_job(&hen, " \n\t"),
            Err(CoreError::InvalidInput(_))
        ));
        hen.state = HenState::Archived;
        assert!(matches!(
            check_job(&hen, "do work"),
            Err(CoreError::Conflict(_))
        ));
    }

    #[test]
    fn automatic_job_hatch_preserves_strict_network_policy() {
        let mut hen = hen();
        hen.manifest.agent_kind = AgentKind::Shell;
        hen.manifest.network = Some(NetworkSpec {
            policy: NetPolicy::Off,
            allow: Vec::new(),
        });
        assert!(matches!(
            check_job(&hen, "do work"),
            Err(CoreError::PermissionDenied(_))
        ));
    }
}
