//! # coopd
//!
//! The Coop agent farm daemon.
//!
//! Run with `coopd serve` to start the HTTP API on `127.0.0.1:9700`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod api;
mod auth;
mod brain_factory;
mod discord_supervisor;
mod location;
mod orchestrator;
mod runner;
mod safe_origin;
mod session;
mod shell;
mod tasks;
mod ui;

/// Top-level CLI for `coopd`.
#[derive(Parser, Debug)]
#[command(name = "coopd", version, about = "Coop agent farm daemon")]
struct Cli {
    /// Path to coopd data directory. Defaults to `~/.coop`.
    #[arg(long, env = "COOP_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Logging filter (e.g. `info`, `coopd=debug`).
    #[arg(long, env = "COOP_LOG", default_value = "info")]
    log: String,

    /// Subcommand.
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Start the daemon and serve the HTTP API.
    Serve {
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1:9700")]
        addr: String,
    },
    /// Print effective configuration and exit.
    ConfigShow,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(true)
        .compact()
        .init();

    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;
    // H1: confine ~/.coop tree to owner only — it holds sealed vault,
    // redb operational state, and PTY/job traces.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700));
    }

    match cli.cmd {
        Cmd::Serve { addr } => serve(data_dir, addr).await,
        Cmd::ConfigShow => {
            println!("data_dir = {}", data_dir.display());
            Ok(())
        }
    }
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".coop")
}

/// One-shot startup reconciler: any Job left RUNNING from a previous run
/// is marked FAILED with `interrupted`; any Hen stuck in Hatching/Working
/// is forced back to Idle. v0.1 has no crash recovery beyond this.
async fn reconcile_on_startup(orch: &orchestrator::OrchHandle) {
    use coopd_core::{HenState, JobStatus};

    match orch.list_jobs(None).await {
        Ok(jobs) => {
            for mut job in jobs {
                if matches!(job.status, JobStatus::Running | JobStatus::Queued) {
                    job.mark_failed("interrupted at restart".to_string());
                    if let Err(e) = orch.update_job(job.clone()).await {
                        tracing::warn!(job_id = %job.id, error = %e, "reconcile: update_job failed");
                    } else {
                        info!(job_id = %job.id, "reconcile: marked interrupted job FAILED");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "reconcile: list_jobs failed"),
    }
    match orch.list_hens(None).await {
        Ok(hens) => {
            for hen in hens {
                if matches!(hen.state, HenState::Hatching | HenState::Working) {
                    if let Err(e) = orch.transition_hen(hen.id.clone(), HenState::Idle).await {
                        tracing::warn!(id = %hen.id, error = %e, "reconcile: hen reset failed");
                    } else {
                        info!(id = %hen.id, "reconcile: hen reset to IDLE");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "reconcile: list_hens failed"),
    }
}

async fn serve(data_dir: PathBuf, addr: String) -> Result<()> {
    info!(?data_dir, %addr, "starting coopd");

    let state_db = data_dir.join("state.redb");
    let store = coopd_storage::Store::open(&state_db)
        .with_context(|| format!("opening state db {}", state_db.display()))?;

    let workdir_base = data_dir.join("workdirs");
    std::fs::create_dir_all(&workdir_base)?;

    let tools = Arc::new(coopd_tools::Registry::with_builtins());

    // Auto-unlock vault if COOP_VAULT and COOP_PASSPHRASE both set.
    let initial_vault = match (
        std::env::var("COOP_VAULT").ok(),
        std::env::var("COOP_PASSPHRASE").ok(),
    ) {
        (Some(p), Some(pw)) => match coopd_vault::Vault::open(&p, &pw) {
            Ok(v) => {
                info!(path = %p, "vault auto-unlocked");
                Some(v)
            }
            Err(e) => {
                tracing::warn!(path = %p, error = %e, "vault auto-unlock failed");
                None
            }
        },
        _ => None,
    };
    let brain_factory = Arc::new(Mutex::new(brain_factory::BrainFactory::new(initial_vault)));

    let orch = orchestrator::spawn(store, tools, brain_factory, workdir_base);
    reconcile_on_startup(&orch).await;

    // Discord supervisor: loads persisted config (~/.coop/discord.json) or
    // env vars on first boot, then exposed via /api/v1/config/discord.
    let default_api_base = format!("http://{addr}");
    let discord = discord_supervisor::DiscordSupervisor::new(
        &data_dir,
        &orch.coop_id.to_string(),
        default_api_base,
    );
    discord.bootstrap().await;

    let task_svc = tasks::TaskService::new(orch.clone());

    let auth_cfg = auth::AuthConfig::from_env();
    if auth_cfg.enabled() {
        info!("api auth enabled (COOP_API_TOKEN set)");
    } else {
        tracing::warn!("api auth DISABLED — COOP_API_TOKEN unset; do not expose publicly");
    }

    let app = api::router(orch.clone(), discord, task_svc, addr.clone()).merge(ui::router());
    let app = auth::install(app, auth_cfg);
    // C3/C4: refuse any request whose Host/Origin isn't loopback. Runs
    // outermost so it gates auth as well (the login endpoint must also
    // refuse cross-origin POSTs).
    let app = app.layer(axum::middleware::from_fn(safe_origin::require_safe_origin));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(%addr, "coopd listening");

    // ConnectInfo carries the peer SocketAddr so the login throttle can key on
    // client IP.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    orch.shutdown().await;
    info!("coopd stopped");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install ctrl_c");
    };
    #[cfg(unix)]
    let term = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => info!("received ctrl-c"),
        () = term => info!("received SIGTERM"),
    }
}
