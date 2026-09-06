//! Static Farm UI: durable job history, hens, tmux sessions and CLI tasks.
//!
//! Served from `/` and `/farm`. HTML is embedded; terminal assets use a pinned CDN.

use axum::{Router, response::Html, routing::get};

const FARM_HTML: &str = include_str!("ui/farm.html");
const CHAT_PREVIEW_HTML: &str = include_str!("ui/chat-preview.html");

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/farm", get(index))
        .route("/chat-preview", get(chat_preview))
}

async fn index() -> Html<&'static str> {
    Html(FARM_HTML)
}

async fn chat_preview() -> Html<&'static str> {
    Html(CHAT_PREVIEW_HTML)
}

#[cfg(test)]
mod tests {
    use super::FARM_HTML;

    #[test]
    fn farm_keeps_jobs_tasks_sessions_and_agents_distinct() {
        for section in ["jobs", "tasks", "sessions", "agents"] {
            assert!(FARM_HTML.contains(&format!("id=\"tab-{section}\"")));
            assert!(FARM_HTML.contains(&format!("aria-controls=\"tab-{section}\"")));
        }
        assert!(FARM_HTML.contains("tmux session"));
        assert!(FARM_HTML.contains("id=\"tmux-host\""));
        assert!(FARM_HTML.contains("id=\"yard-grid\""));
        assert!(FARM_HTML.contains("DISPATCHED means delivered, not finished"));
    }

    #[test]
    fn farm_jobs_have_bounded_history_and_independent_completion_tracking() {
        assert!(FARM_HTML.contains("const JOB_PAGE_SIZE = 50;"));
        assert!(FARM_HTML.contains("limit: String(JOB_PAGE_SIZE + 1)"));
        assert!(FARM_HTML.contains("order: 'desc'"));
        assert!(FARM_HTML.contains("const trackedJobs = new Map();"));
        assert!(FARM_HTML.contains("pendingJobIds.add(ev.job_id)"));
        assert!(FARM_HTML.contains("job.status !== 'QUEUED'"));
        assert!(FARM_HTML.contains("['FAILED', 'CANCELLED'].includes(job.status)"));
        assert!(FARM_HTML.contains("Retry as new job"));
        assert!(FARM_HTML.contains("id=\"job-result-section\""));
    }

    #[test]
    fn farm_http_uses_one_bounded_cookie_authenticated_json_path() {
        assert_eq!(FARM_HTML.matches("await fetch(").count(), 1);
        assert!(FARM_HTML.contains("credentials: 'same-origin'"));
        assert!(FARM_HTML.contains("Accept: 'application/json'"));
        assert!(FARM_HTML.contains("const REQUEST_TIMEOUT_MS = 15000;"));
        assert!(FARM_HTML.contains("const MAX_JSON_BYTES = 8 * 1024 * 1024;"));
        assert!(FARM_HTML.contains("if (!response.ok)"));
        assert!(FARM_HTML.contains("response.status === 401"));
        assert!(FARM_HTML.contains("href=\"/login\" target=\"_blank\" rel=\"noopener\""));
    }

    #[test]
    fn farm_exposes_stale_data_and_real_usage_without_estimates() {
        assert!(FARM_HTML.contains("Last farm sync:"));
        assert!(FARM_HTML.contains("Recorded Grain spent"));
        assert!(FARM_HTML.contains("Recorded turns"));
        assert!(FARM_HTML.contains("prefers-reduced-motion: reduce"));
        assert!(!FARM_HTML.contains("savings-pill"));
        assert!(!FARM_HTML.contains("minSaved"));
        assert!(!FARM_HTML.contains("dollarsSpent"));
    }

    #[test]
    fn farm_lifecycle_controls_do_not_claim_to_interrupt_jobs() {
        assert!(FARM_HTML.contains("Lifecycle changes do not interrupt running jobs."));
        assert!(FARM_HTML.contains("Running or queued jobs block culling"));
        assert!(FARM_HTML.contains("aria-describedby=\"dc-lifecycle-hint\""));
        assert!(FARM_HTML.contains("requestJSON('/api/v1/readyz', { validate: validOk })"));
    }
}
