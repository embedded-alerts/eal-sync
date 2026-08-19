use eal_crawl_runtime::{queue::CrawlQueue, types::CrawlCommandOutput};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    Some(
        PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .expect("connect integration test database"),
    )
}

#[tokio::test]
async fn one_ready_job_is_leased_once_and_completed_transactionally() {
    let Some(pool) = test_pool().await else {
        eprintln!("DATABASE_URL is not configured; skipping PostgreSQL integration test");
        return;
    };
    let queue = CrawlQueue::new(pool.clone());
    queue.migrate().await.expect("apply runtime migration");

    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let job_id = queue
        .seed(
            tenant_id,
            source_id,
            "https://docs.example.com/releases",
            3600,
            12,
            true,
        )
        .await
        .expect("seed crawl job");

    let (first, second) = tokio::join!(
        queue.lease("integration-worker-a", 120),
        queue.lease("integration-worker-b", 120),
    );
    let first = first.expect("first lease query");
    let second = second.expect("second lease query");
    assert_ne!(
        first.is_some(),
        second.is_some(),
        "exactly one worker must win the lease"
    );
    let leased = first.or(second).expect("one leased job");
    assert_eq!(leased.id, job_id);
    assert_eq!(leased.tenant_id, tenant_id);
    assert_eq!(leased.source_id, source_id);

    let output = CrawlCommandOutput {
        page_ingest: json!({"source_id": source_id}),
        diagnostic: json!({"robots_snapshot_id": Uuid::new_v4()}),
    };
    let receipt = json!({"revision_id": Uuid::new_v4(), "created": true});
    queue
        .complete(&leased, &output, &receipt)
        .await
        .expect("complete leased job");

    let job_row = sqlx::query(
        "SELECT lease_token, attempt_count, last_error_code, next_run_at > now() AS scheduled FROM eal_crawl_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("read completed job");
    assert!(
        job_row
            .try_get::<Option<Uuid>, _>("lease_token")
            .expect("lease token")
            .is_none()
    );
    assert_eq!(
        job_row
            .try_get::<i32, _>("attempt_count")
            .expect("attempt count"),
        0
    );
    assert!(
        job_row
            .try_get::<Option<String>, _>("last_error_code")
            .expect("error code")
            .is_none()
    );
    assert!(
        job_row
            .try_get::<bool, _>("scheduled")
            .expect("scheduled flag")
    );

    let attempt_row = sqlx::query(
        "SELECT status, finished_at IS NOT NULL AS finished, api_receipt FROM eal_crawl_attempts WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("read successful attempt");
    assert_eq!(
        attempt_row.try_get::<String, _>("status").expect("status"),
        "succeeded"
    );
    assert!(
        attempt_row
            .try_get::<bool, _>("finished")
            .expect("finished flag")
    );
    assert_eq!(
        attempt_row
            .try_get::<serde_json::Value, _>("api_receipt")
            .expect("receipt"),
        receipt
    );

    sqlx::query("DELETE FROM eal_crawl_jobs WHERE id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("clean completed job");
}

#[tokio::test]
async fn expired_lease_is_recovered_and_attempt_is_abandoned() {
    let Some(pool) = test_pool().await else {
        eprintln!("DATABASE_URL is not configured; skipping PostgreSQL integration test");
        return;
    };
    let queue = CrawlQueue::new(pool.clone());
    queue.migrate().await.expect("apply runtime migration");

    let tenant_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let job_id = queue
        .seed(
            tenant_id,
            source_id,
            "https://status.example.com/incidents",
            3600,
            12,
            true,
        )
        .await
        .expect("seed crawl job");
    let leased = queue
        .lease("integration-crash-worker", 120)
        .await
        .expect("lease query")
        .expect("leased job");

    sqlx::query(
        "UPDATE eal_crawl_jobs SET lease_expires_at = now() - interval '1 second' WHERE id = $1 AND lease_token = $2",
    )
    .bind(leased.id)
    .bind(leased.lease_token)
    .execute(&pool)
    .await
    .expect("expire lease");

    let reaped = queue.reap_expired().await.expect("reap expired lease");
    assert_eq!(reaped, 1);

    let job_row = sqlx::query(
        "SELECT lease_token, attempt_count, last_error_code FROM eal_crawl_jobs WHERE id = $1",
   )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("read recovered job");
    assert!(
        job_row
            .try_get::<Option<Uuid>, _>("lease_token")
            .expect("lease token")
            .is_none()
    );
    assert_eq!(
        job_row
            .try_get::<i32, _>("attempt_count")
            .expect("attempt count"),
        1
    );
    assert_eq!(
        job_row
            .try_get::<Option<String>, _>("last_error_code")
            .expect("error code")
            .as_deref(),
        Some("lease_expired")
    );

    let attempt_row = sqlx::query(
        "SELECT status, error_code, finished_at IS NOT NULL AS finished FROM eal_crawl_attempts WHERE id = $1",
    )
    .bind(leased.attempt_id)
    .fetch_one(&pool)
    .await
    .expect("read abandoned attempt");
    assert_eq!(
        attempt_row.try_get::<String, _>("status").expect("status"),
        "abandoned"
   );
    assert_eq!(
        attempt_row
            .try_get::<Option<String>, _>("error_code")
            .expect("error code")
            .as_deref(),
        Some("lease_expired")
    );
    assert!(
        attempt_row
            .try_get::<bool, _>("finished")
            .expect("finished flag")
    );

    sqlx::query("DELETE FROM eal_crawl_jobs WHERE id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("clean recovered job");
}
