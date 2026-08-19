use anyhow::{Context, Result, bail};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::types::{CrawlCommandOutput, CrawlJob, validate_public_https_url};

const MIGRATION: &str = include_str!("../migrations/001_runtime.sql");

#[derive(Clone)]
pub struct CrawlQueue {
    pool: PgPool,
}

impl CrawlQueue {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::raw_sql(MIGRATION)
            .execute(&self.pool)
            .await
            .context("apply crawl runtime migration")?;
        Ok(())
    }

    pub async fn seed(
        &self,
        tenant_id: Uuid,
        source_id: Uuid,
        start_url: &str,
        interval_seconds: i64,
        max_attempts: i32,
        enabled: bool,
    ) -> Result<Uuid> {
        let start_url = validate_public_https_url(start_url)?.to_string();
        if !(60..=2_592_000).contains(&interval_seconds) {
            bail!("interval_seconds must be between 60 and 2592000");
        }
        if !(1..=100).contains(&max_attempts) {
            bail!("max_attempts must be between 1 and 100");
        }
        let id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
            INSERT INTO eal_crawl_jobs (
                id, tenant_id, source_id, start_url, enabled,
                interval_seconds, max_attempts, next_run_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            ON CONFLICT (tenant_id, source_id, start_url)
            DO UPDATE SET
                enabled = EXCLUDED.enabled,
                interval_seconds = EXCLUDED.interval_seconds,
                max_attempts = EXCLUDED.max_attempts,
                next_run_at = LEAST(eal_crawl_jobs.next_run_at, now()),
                updated_at = now()
            RETURNING id
            "#,
        )
        .bind(id)
        .bind(tenant_id)
        .bind(source_id)
        .bind(start_url)
        .bind(enabled)
        .bind(interval_seconds)
        .bind(max_attempts)
        .fetch_one(&self.pool)
        .await
        .context("seed crawl job")?;
        row.try_get("id").context("decode crawl job ID")
    }

    pub async fn lease(&self, owner: &str, lease_seconds: i64) -> Result<Option<CrawlJob>> {
        if owner.trim().is_empty() || owner.len() > 200 {
            bail!("lease owner must contain 1 to 200 bytes");
        }
        if !(30..=3_600).contains(&lease_seconds) {
            bail!("lease_seconds must be between 30 and 3600");
        }
        let lease_token = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await.context("begin lease transaction")?;
        let row = sqlx::query(
            r#"
            WITH candidate AS (
                SELECT id
                FROM eal_crawl_jobs
                WHERE enabled = true
                  AND next_run_at <= now()
                  AND attempt_count < max_attempts
                  AND (lease_token IS NULL OR lease_expires_at < now())
                ORDER BY next_run_at ASC, id ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE eal_crawl_jobs AS job
            SET lease_token = $1,
                lease_owner = $2,
                lease_expires_at = now() + ($3::bigint * interval '1 second'),
                updated_at = now()
            FROM candidate
            WHERE job.id = candidate.id
            RETURNING job.id, job.tenant_id, job.source_id, job.start_url,
                      job.interval_seconds, job.attempt_count, job.max_attempts
            "#,
        )
        .bind(lease_token)
        .bind(owner.trim())
        .bind(lease_seconds)
        .fetch_optional(&mut *transaction)
        .await
        .context("lease next crawl job")?;

        let Some(row) = row else {
            transaction.commit().await.context("commit empty lease")?;
            return Ok(None);
        };
        let job_id: Uuid = row.try_get("id").context("decode job id")?;
        sqlx::query(
            r#"
            INSERT INTO eal_crawl_attempts (id, job_id, lease_token, status)
            VALUES ($1, $2, $3, 'leased')
            "#,
        )
        .bind(attempt_id)
        .bind(job_id)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .context("record crawl lease attempt")?;
        transaction.commit().await.context("commit crawl lease")?;

        Ok(Some(CrawlJob {
            id: job_id,
            tenant_id: row.try_get("tenant_id").context("decode tenant id")?,
            source_id: row.try_get("source_id").context("decode source id")?,
            start_url: row.try_get("start_url").context("decode start URL")?,
            interval_seconds: row.try_get("interval_seconds").context("decode interval")?,
            attempt_count: row
                .try_get("attempt_count")
                .context("decode attempt count")?,
            max_attempts: row.try_get("max_attempts").context("decode max attempts")?,
            lease_token,
            attempt_id,
        }))
    }

    pub async fn complete(
        &self,
        job: &CrawlJob,
        output: &CrawlCommandOutput,
        api_receipt: &Value,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin completion transaction")?;
        let attempt = sqlx::query(
            r#"
            UPDATE eal_crawl_attempts
            SET status = 'succeeded', finished_at = now(), api_receipt = $3, details = $4
            WHERE id = $1 AND lease_token = $2 AND status = 'leased'
            "#,
        )
        .bind(job.attempt_id)
        .bind(job.lease_token)
        .bind(api_receipt)
        .bind(&output.diagnostic)
        .execute(&mut *transaction)
        .await
        .context("complete crawl attempt")?;
        if attempt.rows_affected() != 1 {
            bail!("crawl attempt lease was lost before completion");
        }
        let job_update = sqlx::query(
            r#"
            UPDATE eal_crawl_jobs
            SET next_run_at = now() + (interval_seconds * interval '1 second'),
                lease_token = NULL,
                lease_owner = NULL,
                lease_expires_at = NULL,
                attempt_count = 0,
                last_error_code = NULL,
                updated_at = now()
            WHERE id = $1 AND lease_token = $2
            "#,
        )
        .bind(job.id)
        .bind(job.lease_token)
        .execute(&mut *transaction)
        .await
        .context("release successful crawl job")?;
        if job_update.rows_affected() != 1 {
            bail!("crawl job lease was lost before completion");
        }
        transaction
            .commit()
            .await
            .context("commit crawl completion")?;
        Ok(())
    }

    pub async fn fail(&self, job: &CrawlJob, error_code: &str, details: &Value) -> Result<()> {
        let error_code = sanitize_code(error_code);
        let backoff_seconds = retry_backoff_seconds(job.attempt_count.saturating_add(1));
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("begin failure transaction")?;
        let attempt = sqlx::query(
            r#"
            UPDATE eal_crawl_attempts
            SET status = 'failed', finished_at = now(), error_code = $3, details = $4
            WHERE id = $1 AND lease_token = $2 AND status = 'leased'
            "#,
        )
        .bind(job.attempt_id)
        .bind(job.lease_token)
        .bind(&error_code)
        .bind(details)
        .execute(&mut *transaction)
        .await
        .context("fail crawl attempt")?;
        if attempt.rows_affected() != 1 {
            bail!("crawl attempt lease was lost before failure recording");
        }
        let job_update = sqlx::query(
            r#"
            UPDATE eal_crawl_jobs
            SET next_run_at = now() + ($3::bigint * interval '1 second'),
                lease_token = NULL,
                lease_owner = NULL,
                lease_expires_at = NULL,
                attempt_count = attempt_count + 1,
                last_error_code = $4,
                updated_at = now()
            WHERE id = $1 AND lease_token = $2
            "#,
        )
        .bind(job.id)
        .bind(job.lease_token)
        .bind(backoff_seconds)
        .bind(&error_code)
        .execute(&mut *transaction)
        .await
        .context("release failed crawl job")?;
        if job_update.rows_affected() != 1 {
            bail!("crawl job lease was lost before failure recording");
        }
        transaction
            .commit()
            .await
            .context("commit crawl failure")?;
        Ok(())
    }

    pub async fn reap_expired(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            WITH expired AS (
                SELECT id, lease_token
                FROM eal_crawl_jobs
                WHERE lease_token IS NOT NULL AND lease_expires_at < now()
                FOR UPDATE
            ), abandoned AS (
                UPDATE eal_crawl_attempts AS attempt
                SET status = 'abandoned',
                    finished_at = now(),
                    error_code = 'lease_expired',
                    details = jsonb_build_object('reason', 'worker lease expired')
                FROM expired
                WHERE attempt.job_id = expired.id
                  AND attempt.lease_token = expired.lease_token
                  AND attempt.status = 'leased'
                RETURNING expired.id AS job_id, expired.lease_token AS lease_token
            )
            UPDATE eal_crawl_jobs AS job
            SET lease_token = NULL,
                lease_owner = NULL,
                lease_expires_at = NULL,
                attempt_count = attempt_count + 1,
                next_run_at = now() + interval '60 seconds',
                last_error_code = 'lease_expired',
                updated_at = now()
            FROM abandoned
            WHERE job.id = abandoned.job_id
              AND job.lease_token = abandoned.lease_token
            "#,
        )
        .execute(&self.pool)
        .await
        .context("reap expired crawl leases")?;
        Ok(result.rows_affected())
    }
}

pub fn retry_backoff_seconds(attempt: i32) -> i64 {
    let exponent = attempt.clamp(1, 16) as u32;
    (30_i64.saturating_mul(2_i64.saturating_pow(exponent - 1))).min(86_400)
}

fn sanitize_code(value: &str) -> String {
    let code: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(128)
        .collect();
    if code.is_empty() {
        "crawl_failed".to_owned()
    } else {
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_backoff_seconds(1), 30);
        assert_eq!(retry_backoff_seconds(2), 60);
        assert_eq!(retry_backoff_seconds(100), 86_400);
    }

    #[test]
    fn error_codes_are_sanitized() {
        assert_eq!(
            sanitize_code("provider timeout\nsecret"),
            "providertimeoutsecret"
        );
        assert_eq!(sanitize_code("***"), "crawl_failed");
    }
}
