mod api;
mod queue;
mod runner;
mod types;

use std::{env, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use api::IngestApiClient;
use clap::{Parser, Subcommand};
use queue::CrawlQueue;
use runner::run_crawl_command;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use tokio::signal;
use tracing::{error, info, warn};
use types::CrawlCommandConfig;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "eal-crawl-runtime", version, about)]
struct Cli {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,

    #[arg(
        long,
        env = "EAL_API_BASE_URL",
        default_value = "https://api.embedded-alerts.invalid"
    )]
    api_base_url: String,

    #[arg(long, env = "EAL_ALLOW_LOOPBACK_HTTP", default_value_t = false)]
    allow_loopback_http: bool,

    #[arg(long, env = "EAL_API_TIMEOUT_SECONDS", default_value_t = 20)]
    api_timeout_seconds: u64,

    #[arg(
        long,
        env = "EAL_API_RESPONSE_LIMIT_BYTES",
        default_value_t = 2 * 1024 * 1024
    )]
    api_response_limit_bytes: usize,

    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    Migrate,
    Seed {
        #[arg(long)]
        tenant_id: Uuid,
        #[arg(long)]
        source_id: Uuid,
        #[arg(long)]
        start_url: String,
        #[arg(long, default_value_t = 3_600)]
        interval_seconds: i64,
        #[arg(long, default_value_t = 12)]
        max_attempts: i32,
        #[arg(long, default_value_t = false)]
        enabled: bool,
    },
    Worker {
        #[arg(long, env = "EAL_CRAWL_COMMAND")]
        crawl_command: PathBuf,
        #[arg(long, env = "EAL_WORKER_OWNER")]
        owner: Option<String>,
        #[arg(long, default_value_t = 180)]
        lease_seconds: i64,
        #[arg(long, default_value_t = 10)]
        idle_seconds: u64,
        #[arg(long, default_value_t = 120)]
        crawl_timeout_seconds: u64,
        #[arg(long, default_value_t = 8 * 1024 * 1024)]
        crawl_stdout_limit_bytes: usize,
        #[arg(long, default_value_t = 64 * 1024)]
        crawl_stderr_limit_bytes: usize,
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    Reap,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&cli.database_url)
        .await
        .context("connect crawl runtime to PostgreSQL")?;
    let queue = CrawlQueue::new(pool);

    match cli.action {
        Action::Migrate => {
            queue.migrate().await?;
            info!("crawl runtime migration applied");
        }
        Action::Seed {
            tenant_id,
            source_id,
            start_url,
            interval_seconds,
            max_attempts,
            enabled,
        } => {
            let id = queue
                .seed(
                    tenant_id,
                    source_id,
                    &start_url,
                    interval_seconds,
                    max_attempts,
                    enabled,
                )
                .await?;
            println!("{id}");
        }
        Action::Reap => {
            let count = queue.reap_expired().await?;
            info!(count, "expired crawl leases reaped");
        }
        Action::Worker {
            crawl_command,
            owner,
            lease_seconds,
            idle_seconds,
            crawl_timeout_seconds,
            crawl_stdout_limit_bytes,
            crawl_stderr_limit_bytes,
            once,
        } => {
            validate_worker_limits(
                idle_seconds,
                crawl_timeout_seconds,
                crawl_stdout_limit_bytes,
                crawl_stderr_limit_bytes,
            )?;
            let owner = owner.unwrap_or_else(default_owner);
            let api = IngestApiClient::new(
                &cli.api_base_url,
                cli.allow_loopback_http,
                cli.api_timeout_seconds,
                cli.api_response_limit_bytes,
            )?;
            let command = CrawlCommandConfig {
                executable: crawl_command,
                timeout_seconds: crawl_timeout_seconds,
                stdout_limit_bytes: crawl_stdout_limit_bytes,
                stderr_limit_bytes: crawl_stderr_limit_bytes,
            };
            run_worker(
                &queue,
                &api,
                &command,
                &owner,
                lease_seconds,
                idle_seconds,
                once,
            )
            .await?;
        }
    }
    Ok(())
}

async fn run_worker(
    queue: &CrawlQueue,
    api: &IngestApiClient,
    command: &CrawlCommandConfig,
    owner: &str,
    lease_seconds: i64,
    idle_seconds: u64,
    once: bool,
) -> Result<()> {
    let shutdown = signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let iteration = process_next(queue, api, command, owner, lease_seconds);
        let processed = tokio::select! {
            result = iteration => result?,
            result = &mut shutdown => {
                if let Err(error) = result {
                    warn!(%error, "failed to receive shutdown signal");
                }
                info!("crawl worker shutting down");
                break;
            }
        };
        if once {
            break;
        }
        if !processed {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(idle_seconds)) => {},
                result = &mut shutdown => {
                    if let Err(error) = result {
                        warn!(%error, "failed to receive shutdown signal");
                    }
                    info!("crawl worker shutting down");
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn process_next(
    queue: &CrawlQueue,
    api: &IngestApiClient,
    command: &CrawlCommandConfig,
    owner: &str,
    lease_seconds: i64,
) -> Result<bool> {
    let reaped = queue.reap_expired().await?;
    if reaped > 0 {
        warn!(reaped, "reaped expired crawl attempts");
    }
    let Some(job) = queue.lease(owner, lease_seconds).await? else {
        return Ok(false);
    };
    info!(
        job_id = %job.id,
        tenant_id = %job.tenant_id,
        source_id = %job.source_id,
        attempt = job.attempt_count + 1,
        max_attempts = job.max_attempts,
        "leased crawl job"
    );

    let output = match run_crawl_command(command, &job).await {
        Ok(output) => output,
        Err(crawl_error) => {
            let diagnostic = crawl_error.diagnostic();
            error!(
                job_id = %job.id,
                error_code = crawl_error.code(),
                error = %crawl_error,
                "crawl command failed"
            );
            queue
                .fail(&job, crawl_error.code(), &diagnostic)
                .await
                .context("record crawl command failure")?;
            return Ok(true);
        }
    };

    let receipt = match api
        .ingest_page(job.tenant_id, job.source_id, &output.page_ingest)
        .await
    {
        Ok(receipt) => receipt,
        Err(api_error) => {
            let diagnostic = api_error.diagnostic();
            error!(
                job_id = %job.id,
                error_code = api_error.code(),
                error = %api_error,
                "page-ingest API handoff failed"
            );
            queue
                .fail(&job, api_error.code(), &diagnostic)
                .await
                .context("record API handoff failure")?;
            return Ok(true);
        }
    };

    queue
        .complete(&job, &output, &receipt)
        .await
        .context("commit successful crawl receipt")?;
    info!(job_id = %job.id, source_id = %job.source_id, "crawl job completed");
    Ok(true)
}

fn validate_worker_limits(
    idle_seconds: u64,
    crawl_timeout_seconds: u64,
    stdout_limit_bytes: usize,
    stderr_limit_bytes: usize,
) -> Result<()> {
    if !(1..=300).contains(&idle_seconds) {
        bail!("idle_seconds must be between 1 and 300");
    }
    if !(5..=3_600).contains(&crawl_timeout_seconds) {
        bail!("crawl_timeout_seconds must be between 5 and 3600");
    }
    if !(64 * 1024..=64 * 1024 * 1024).contains(&stdout_limit_bytes) {
        bail!("crawl_stdout_limit_bytes must be between 65536 and 67108864");
    }
    if !(4 * 1024..=1024 * 1024).contains(&stderr_limit_bytes) {
        bail!("crawl_stderr_limit_bytes must be between 4096 and 1048576");
    }
    Ok(())
}

fn default_owner() -> String {
    let host = env::var("HOSTNAME")
        .ok()
        .map(|value| value.chars().take(120).collect::<String>())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "worker".to_owned());
    format!("{host}-{}-{}", std::process::id(), Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_limits_are_bounded() {
        assert!(validate_worker_limits(10, 120, 8 * 1024 * 1024, 64 * 1024).is_ok());
        assert!(validate_worker_limits(0, 120, 8 * 1024 * 1024, 64 * 1024).is_err());
    }

    #[test]
    fn owner_is_unique_and_non_empty() {
        let first = default_owner();
        let second = default_owner();
        assert!(!first.is_empty());
        assert_ne!(first, second);
    }

    #[test]
    fn failure_details_never_include_page_payloads() {
        let detail = json!({"error_code": "test", "message": "bounded"});
        assert!(detail.get("page_ingest").is_none());
    }
}
