# Embedded Alerts durable crawl runtime

This crate closes the scheduling and handoff gap between registered source policies, the bounded one-shot crawler, and the canonical page-ingest API.

## What it does

- Stores explicit HTTPS crawl jobs in PostgreSQL.
- Leases one ready job at a time with `FOR UPDATE SKIP LOCKED`.
- Records an immutable attempt receipt for every lease.
- Invokes a configured crawler executable directly, never through a shell.
- Sends a versioned JSON request on stdin and accepts one bounded JSON result on stdout.
- Verifies source identity, canonical public HTTPS URL, content hash, embedding metadata, dimensions, and finite vector values.
- Posts the opaque `PageIngestRequest` object to `POST /v1/sources/{source_id}/pages` with tenant context.
- Commits success receipts or bounded exponential retry/backoff receipts.
- Reaps expired worker leases after crashes.

It does **not** discover sources by itself, accept arbitrary user fetches, score alert rules, or send notifications. Discovery/robots/DNS/redirect/fetch/extraction/embedding remain inside the approved one-shot crawler adapter. Matching remains in `eal-api`; delivery remains in DEN-3460.

## Crawler command protocol

The worker starts the absolute path supplied by `--crawl-command` and writes one newline-terminated request:

```json
{
  "protocol_version": "eal-crawl-command/v1",
  "job_id": "...",
  "attempt_id": "...",
  "tenant_id": "...",
  "source_id": "...",
  "start_url": "https://docs.example.com/",
  "leased_at": "2026-08-10T00:00:00Z"
}
```

The crawler must return one JSON document on stdout:

```json
{
  "protocol_version": "eal-crawl-result/v1",
  "page_ingest": {
    "source_id": "...",
    "canonical_url": "https://docs.example.com/releases/1",
    "content_hash": "...",
    "embedding": {
      "model": "fixed-model",
      "model_version": "2026-08-10",
      "dimensions": 768,
      "normalization": "unit_length",
      "values": []
    }
  },
  "diagnostic": {
    "robots_snapshot_id": "...",
    "redirect_count": 0
  }
}
```

The runtime treats `page_ingest` as the API contract payload after enforcing the safety-critical fields above. Stderr is diagnostic only, bounded, control-character filtered, and never forwarded to the API.

## Database lifecycle

Apply the reviewed migration before starting workers:

```bash
cargo run -- migrate
```

Register an explicit job disabled by default:

```bash
cargo run -- seed \
  --tenant-id "$TENANT_ID" \
  --source-id "$SOURCE_ID" \
  --start-url https://docs.example.com/ \
  --interval-seconds 3600
```

Enable only after the source policy and crawler adapter have been certified:

```bash
cargo run -- seed \
  --tenant-id "$TENANT_ID" \
  --source-id "$SOURCE_ID" \
  --start-url https://docs.example.com/ \
  --interval-seconds 3600 \
  --enabled
```

Run one lease for a canary:

```bash
cargo run -- worker \
  --crawl-command /absolute/path/to/eal-crawl-adapter \
  --once
```

## Required environment

- `DATABASE_URL`
- `EAL_API_BASE_URL`
- `EAL_CRAWL_COMMAND` for worker mode

Loopback HTTP for a local API requires the explicit `EAL_ALLOW_LOOPBACK_HTTP=true` opt-in. Remote API endpoints must use HTTPS. Database URLs and child-process environment values are never logged.

## Validation

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
python3 scripts/verify_contract.py
```

Production sending remains disabled until DEN-3459 authentication/storage gates and DEN-3460 transactional delivery canaries pass.
