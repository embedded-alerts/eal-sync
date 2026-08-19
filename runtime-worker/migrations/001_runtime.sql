-- Serialize concurrent startup/test migration calls for this schema. sqlx::raw_sql
-- sends this batch as one PostgreSQL simple-query transaction, so the xact lock is
-- held through all catalog changes and released automatically on success or error.
SELECT pg_advisory_xact_lock(3615202608190001);

CREATE TABLE IF NOT EXISTS eal_crawl_jobs (
    id uuid PRIMARY KEY,
    tenant_id uuid NOT NULL,
    source_id uuid NOT NULL,
    start_url text NOT NULL,
    enabled boolean NOT NULL DEFAULT false,
    interval_seconds bigint NOT NULL DEFAULT 3600,
    next_run_at timestamptz NOT NULL DEFAULT now(),
    lease_token uuid,
    lease_owner text,
    lease_expires_at timestamptz,
    attempt_count integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 12,
    last_error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT eal_crawl_jobs_https_only CHECK (start_url ~ '^https://'),
    CONSTRAINT eal_crawl_jobs_interval_bounds CHECK (interval_seconds BETWEEN 60 AND 2592000),
    CONSTRAINT eal_crawl_jobs_attempt_bounds CHECK (attempt_count >= 0 AND max_attempts BETWEEN 1 AND 100),
    CONSTRAINT eal_crawl_jobs_lease_shape CHECK (
        (lease_token IS NULL AND lease_owner IS NULL AND lease_expires_at IS NULL)
        OR
        (lease_token IS NOT NULL AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)
    ),
    UNIQUE (tenant_id, source_id, start_url)
);

CREATE INDEX IF NOT EXISTS eal_crawl_jobs_ready_idx
    ON eal_crawl_jobs (next_run_at, id)
    WHERE enabled = true AND attempt_count < max_attempts;

CREATE INDEX IF NOT EXISTS eal_crawl_jobs_expired_lease_idx
    ON eal_crawl_jobs (lease_expires_at)
    WHERE lease_token IS NOT NULL;

CREATE TABLE IF NOT EXISTS eal_crawl_attempts (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES eal_crawl_jobs(id) ON DELETE CASCADE,
    lease_token uuid NOT NULL,
    status text NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    error_code text,
    api_receipt jsonb,
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT eal_crawl_attempts_status CHECK (status IN ('leased', 'succeeded', 'failed', 'abandoned')),
    CONSTRAINT eal_crawl_attempts_terminal_shape CHECK (
        (status = 'leased' AND finished_at IS NULL)
        OR
        (status <> 'leased' AND finished_at IS NOT NULL)
    ),
    UNIQUE (job_id, lease_token)
);

CREATE INDEX IF NOT EXISTS eal_crawl_attempts_job_started_idx
    ON eal_crawl_attempts (job_id, started_at DESC);

ALTER TABLE eal_crawl_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE eal_crawl_attempts ENABLE ROW LEVEL SECURITY;

COMMENT ON TABLE eal_crawl_jobs IS
    'Internal domain-scoped crawl schedule. Rows are leased with FOR UPDATE SKIP LOCKED; no notification delivery occurs here.';
COMMENT ON TABLE eal_crawl_attempts IS
    'Immutable operational receipts for crawl lease outcomes and API ingestion handoff.';
