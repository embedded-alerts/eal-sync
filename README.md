# eal-sync

opto-sync/syncer.rs JSON reconciliation gateway for Embedded Alerts.

**Product:** Embedded Alerts — Embedding-based alerting for semantically relevant new information.

Define semantic alert rules, ingest source documents, compare embeddings, rank matches, and deliver explainable notifications.

## Safety and production boundary

Similarity scores are ranking signals, not truth guarantees. Production ingestion must respect source terms, robots rules, privacy requirements, retention limits, and notification consent.

This repository is an executable bootstrap, not a production deployment. Before live
use, add authentication, tenant authorization, rate limits, durable migrations,
observability, backups, incident response, dependency review, and secret management.
## Reconciliation contract

`POST /api/v1/reconcile` accepts `{"base":...,"incoming":...}` and delegates to
`opto-sync/syncer.rs` at immutable commit `132a97c77867128656070be85d3046b0cc065cbf`. The default policy is
identity-keyed array merge using `id` plus last-writer-wins selectors
`updated_at,synced_at`.

The gateway is not a durable record by itself. Persist and authorize the result in
the owning API/database transaction, enforce idempotency, and retain conflict audit
metadata.
