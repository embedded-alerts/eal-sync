# eal-sync

Rust ingestion and offline-first reconciliation for **Embedded Alerts**.

The service handles two separate concerns:

- the existing opto-sync JSON reconciliation endpoints used by clients; and
- bounded ingestion of tenant-registered search-provider results, feeds, sitemaps,
  web pages, and APIs before embedding and match evaluation.

It is not an unrestricted web crawler and does not expose an arbitrary-URL fetch
route. A source is created through the authenticated `eal-api` contract, then a
worker claims due source rows from PostgreSQL and supplies explicit host and path
scope to this crate.

## Ingestion safety boundary

`src/ingestion/` implements the network and content boundary used by workers:

- only `http` and `https`, with URL user information rejected;
- canonical DNS hosts only—wildcards, IP literals, local single-label names, and
  non-default ports are rejected by the default scope;
- exact registered-host and path-prefix scope by default, with explicit additions
  required for subdomains, redirect hosts, and additional path prefixes;
- environment proxies disabled so DNS validation and address pinning cannot be
  bypassed through `HTTP_PROXY`, `HTTPS_PROXY`, or `ALL_PROXY`;
- redirect handling disabled in reqwest and revalidated one hop at a time;
- DNS resolution before every hop and rejection of private, loopback, link-local,
  carrier-grade NAT, reserved, multicast, and documentation addresses;
- connection pinning to one validated address to reduce DNS rebinding exposure;
- conditional `ETag`/`Last-Modified` requests;
- per-host concurrency, connect/request timeouts, response-size limits before and
  after decompression, and an ingestible MIME allowlist;
- visible-text extraction for HTML, JSON, feeds/XML, and plain text;
- canonical URL identity and normalized-text SHA-256 content identity;
- deterministic unchanged-versus-new-revision decisions; and
- a validated `EmbeddingWorkItem` handoff containing tenant, source, revision,
  canonical URL, content hash, content text, and exact embedding-space ID.

The scheduler remains responsible for source ownership, robots policy, per-tenant
and per-host fetch budgets, retry timing, leases, PostgreSQL transactions, and the
outbox handoff. Search discovery should use licensed/provider APIs or explicitly
registered feeds rather than scraping search-engine result pages against their
terms.

## One-shot certification

The `crawl_once` binary lets an operator certify one registered source without
creating a public fetch endpoint:

```bash
EAL_CRAWL_URL=https://example.com/feed.xml \
  cargo run --bin crawl_once
```

Optional inputs are `EAL_CRAWL_INCLUDE_SUBDOMAINS`,
`EAL_CRAWL_ALLOWED_HOSTS`, `EAL_CRAWL_ALLOWED_PATH_PREFIXES`,
`EAL_CRAWL_ETAG`, and `EAL_CRAWL_LAST_MODIFIED`. Path prefixes are
comma-separated canonical absolute paths. When they are omitted, the fetch is
restricted to the path represented by `EAL_CRAWL_URL`. Output contains metadata,
the content hash, and at most a 500-character preview—not the full fetched document.

## Service endpoints

The existing Axum server continues to expose:

- `GET /healthz`
- `GET /readyz`
- `POST /v1/reconcile`
- `POST /api/v1/reconcile`

Durable source CRUD, revision persistence, vector search, matching, and notification
delivery belong in `eal-api`/PostgreSQL and the worker loop; they are not process-local
state in this gateway.

## Validation

```bash
python3 scripts/verify_repo.py
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Production crawling and external notifications stay disabled until the migration,
restart, cross-tenant isolation, DNS/redirect, path-scope, proxy-bypass,
unchanged-content, duplicate-match, cooldown, and delivery-idempotency canaries pass
in `embedded-alerts-test`.
