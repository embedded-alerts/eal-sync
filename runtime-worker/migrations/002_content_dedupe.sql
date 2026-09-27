-- Dedupe-before-embedding state for the crawl runtime.
-- The runtime computes this SHA-256 itself from normalized extracted content.
-- It is advanced only after a successful downstream API handoff (or a confirmed
-- unchanged result), so a failed embedding/match attempt is retried rather than
-- silently acknowledged.

ALTER TABLE eal_crawl_jobs
    ADD COLUMN IF NOT EXISTS last_content_sha256 TEXT;

DO $$
BEGIN
    ALTER TABLE eal_crawl_jobs
        ADD CONSTRAINT eal_crawl_jobs_last_content_sha256_shape
        CHECK (
            last_content_sha256 IS NULL
            OR last_content_sha256 ~ '^[0-9a-f]{64}$'
        )
        NOT VALID;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

CREATE INDEX IF NOT EXISTS eal_crawl_jobs_content_dedupe_idx
    ON eal_crawl_jobs (tenant_id, source_id, last_content_sha256)
    WHERE last_content_sha256 IS NOT NULL;

COMMENT ON COLUMN eal_crawl_jobs.last_content_sha256 IS
    'SHA-256 of runtime-normalized extracted content from the last successfully acknowledged crawl; used to skip embedding unchanged pages.';
