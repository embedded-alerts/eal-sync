# Embedded Alerts ingestion architecture

## Pipeline

1. `eal-api` authenticates the user through Shared-Auth and stores a tenant-owned
   source, immutable rule revision, delivery targets, and an exact embedding-space
   identifier.
2. A scheduler claims due `eal_sources` rows with a lease and bounded tenant/host
   budget. Search-query sources call a configured provider API; feed/page/API
   sources start from the registered endpoint.
3. `HttpFetcher` validates source scope, DNS results, every redirect, MIME type, and
   response size, then extracts normalized visible text.
4. The worker canonicalizes the page URL and computes a SHA-256 digest over the
   normalized text. Conditional 304 responses and equal hashes are no-ops.
5. A changed document is stored as a new immutable `eal_source_revisions` row linked
   to its predecessor, and an outbox row records `source.ingested` in the same
   transaction.
6. An embedding worker consumes `EmbeddingWorkItem`, calls the configured local or
   remote model, verifies dimensions and model-space provenance, and stores one
   vector per `(source_revision_id, embedding_space_id)`.
7. The deterministic `eal-semantic` core evaluates active immutable rule revisions.
   A unique match row is inserted before cooldown and delivery decisions.
8. A delivery worker creates immutable attempts, uses provider idempotency keys,
   retries with capped backoff, dead-letters terminal failures, and emits filtered
   user events.
9. Mash/Maud/HTMX, Leptos, and Dioxus clients read the same authenticated API and use
   WebSocket events only as an acceleration path.

## Registered discovery, not open crawling

The initial production source types are licensed search-provider APIs, RSS/Atom/JSON
Feed, sitemaps, explicitly registered pages, and structured APIs. Discovery links are
canonicalized and deduplicated, but workers must enforce configured path/depth/link
scope before enqueueing them. The library does not recursively fetch every link it
sees.

Robots policy is enforced by the scheduler/fetch-plan layer so decisions can be
cached per host and audited with the source configuration. Provider APIs and feeds
remain the preferred discovery path.

## SSRF and redirect control

A registered source creates an exact root-host scope. Subdomains and cross-host
redirects are denied unless explicitly configured. Every hop is parsed as HTTP(S),
resolved through DNS, filtered to public addresses, and pinned in a fresh reqwest
client with automatic redirects disabled. The same validation repeats after each
redirect.

This is intentionally conservative. Split-horizon/private destinations require a
separate trusted-connector design with network policy and are not accepted by the
public ingestion worker.

## Content and revision identity

Transport bytes are bounded after decompression. Ingestible MIME types are limited
to HTML/XHTML, plain text, JSON/JSON Feed, and XML/feed variants. HTML script/style
blocks and markup are removed; JSON keys and strings are flattened under depth and
fragment-count limits. The normalized visible text becomes the content hash input.

The database identity is:

- source document: `(tenant_id, canonical_url)`;
- source revision: `(document_id, content_sha256)`; and
- embedding: `(source_revision_id, embedding_space_id)`.

A changed hash creates a linked revision. An unchanged hash updates observation and
scheduler metadata only. This prevents poll retries from manufacturing new pages.

## Transaction and queue boundaries

Network fetching and model calls occur outside long PostgreSQL transactions. A
worker holds a short lease, performs bounded external work, then opens a transaction,
sets `SET LOCAL app.tenant_id`, upserts document/source observation, inserts the
revision if new, and writes an outbox event atomically. The lease is completed or
rescheduled afterward.

No process-local queue is authoritative. Restarts must recover due sources, pending
outbox rows, embedding work, match candidates, and delivery attempts from durable
storage.

## Observability

Metrics and logs may include source kind, normalized host, status class, MIME type,
byte bucket, latency, outcome, embedding space, and error class. They must not include
fetched page text, user query text, webhook secrets, provider credentials, or
high-cardinality full URLs by default. Correlate using tenant-safe opaque IDs and
trace IDs.

## Certification gates

Before enabling production fetch or notification delivery, certify:

- private/reserved DNS and redirect targets are rejected;
- an allowed cross-host provider redirect works only when configured;
- oversized compressed and uncompressed responses are terminated;
- 304 and equal-content polling create no new revision;
- changed content links exactly one new revision;
- vectors cannot cross embedding spaces or dimensions;
- concurrent workers create one logical match and one provider notification;
- restart recovers leases/outbox/retries; and
- authenticated users cannot observe another tenant through HTTP, PostgreSQL RLS, or
  WebSocket events.
